//! `harness friction suggest` (ask A32): free BM25 ranking of the inbox groups for a row that is not stored yet.
//!
//! The caller has a lesson text (and maybe a cause) it is about to file; this verb says which existing groups it
//! looks most like, so the new row can be filed with `--breaks <row id>` (or left alone). It is read-only on the
//! inbox; with no `--jev` flag it is pure given the inbox and the query; it prints ids, numbers and (with the Jev
//! arm) one reason token: no row text reaches stdout.
//! With no `--jev` flag the verb is free: no model, no network. With `--jev-send <program>` the Jev arm (its flag
//! checks and candidate list are here, the rest is in `friction_suggest_jev`) sends the cleaned query and
//! cleaned candidate rows to that program only; the engine itself opens no socket and reads no key file.
//!
//! The score, exactly (the contract is `docs/friction-inbox.md`):
//! * a token is a run of `[a-z0-9]` in the lowercased text after `f-YYYYMMDD-NN`, `issue-N` and `#N` are removed,
//!   kept when longer than 2 characters and not all digits;
//! * the query is the DISTINCT tokens of `text + ' ' + why`; each ranked row is a document over its own
//!   `text + ' ' + why`; N, df and the average length are taken over the ranked rows only (never the query);
//! * row score = sum over query tokens of `ln(1 + (N - df + 0.5)/(df + 0.5)) * tf * (K1 + 1) /
//!   (tf + K1 * (1 - B + B * len/avg))` with K1 = 1.2 and B = 0.75;
//! * group score = its best row's score; ties go to the group seen first in inbox order.
//!
//! Exit codes as the family: 0 ok; 1 a refusal (a privacy hit in the query, a damaged inbox, an unknown
//! `--before` row); 2 a usage or setup error (bad flags, no config, no privacy list, a `--record` the record guard refuses).

use crate::config::Config;
use crate::friction::{self, Row};
use crate::friction_loop::groups;
use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::Path;

/// BM25 term-frequency saturation.
const K1: f64 = 1.2;
/// BM25 length normalisation.
const B: f64 = 0.75;
/// The default number of suggested groups.
const DEFAULT_TOP: usize = 3;

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {}", m);
    2
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: friction refused: {}", m);
    1
}

fn digits_at(b: &[u8], i: usize, n: usize) -> bool {
    i + n <= b.len() && b[i..i + n].iter().all(|c| c.is_ascii_digit())
}

/// The lowercased text with every `f-YYYYMMDD-NN`, `issue-N` and `#N` replaced by one space.
fn strip_ids(low: &str) -> String {
    let b = low.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'f' && b.get(i + 1) == Some(&b'-') && digits_at(b, i + 2, 8) && b.get(i + 10) == Some(&b'-') && digits_at(b, i + 11, 2) {
            out.push(b' ');
            i += 13;
            continue;
        }
        let mut skip = 0;
        if b[i..].starts_with(b"issue-") {
            skip = 6;
        } else if b[i] == b'#' {
            skip = 1;
        }
        if skip > 0 {
            let mut j = i + skip;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + skip {
                out.push(b' ');
                i = j;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    // Only ASCII bytes were dropped or replaced, so what is left is still valid UTF-8.
    String::from_utf8(out).unwrap_or_default()
}

/// The tokens of one text, repeats kept, in order.
pub fn tokens(text: &str) -> Vec<String> {
    let cleaned = strip_ids(&text.to_lowercase());
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut flush = |cur: &mut String| {
        if cur.len() > 2 && !cur.bytes().all(|c| c.is_ascii_digit()) {
            out.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for c in cleaned.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            cur.push(c);
        } else {
            flush(&mut cur);
        }
    }
    flush(&mut cur);
    out
}

/// The score of every document for the distinct `query` tokens; the statistics are over `docs` only.
pub fn bm25(docs: &[Vec<String>], query: &[String]) -> Vec<f64> {
    let n = docs.len() as f64;
    let total: usize = docs.iter().map(|d| d.len()).sum();
    if docs.is_empty() || total == 0 {
        return vec![0.0; docs.len()];
    }
    let avg = total as f64 / n;
    let idf: Vec<f64> = query
        .iter()
        .map(|q| {
            let df = docs.iter().filter(|d| d.iter().any(|t| t == q)).count() as f64;
            (1.0 + (n - df + 0.5) / (df + 0.5)).ln()
        })
        .collect();
    docs.iter()
        .map(|d| {
            let len = d.len() as f64;
            let mut s = 0.0;
            for (q, w) in query.iter().zip(&idf) {
                let tf = d.iter().filter(|t| *t == q).count() as f64;
                if tf > 0.0 {
                    s += w * tf * (K1 + 1.0) / (tf + K1 * (1.0 - B + B * len / avg));
                }
            }
            s
        })
        .collect()
}

/// `YYYY-MM-DDTHH:MM:SSZ` now (the record's only clock read).
fn now_ts() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let rem = secs % 86400;
    format!("{}T{:02}:{:02}:{:02}Z", friction::today_utc(), rem / 3600, rem % 3600 / 60, rem % 60)
}

fn lint_rows(cfg: &Config, ctx: &crate::scan::Ctx) -> Result<Vec<Row>, i32> {
    // The read takes the shared inbox lock and lets it go before it returns, so the Jev call later never holds it.
    let inbox = friction::read_inbox(Path::new(cfg.friction_inbox())).map_err(|e| fail(&e))?;
    if inbox.recovery_copy {
        eprintln!("harness: {}", friction::RECOVERY_MSG);
        return Err(1);
    }
    if let Some(n) = inbox.torn_tail {
        eprintln!("harness: inbox has a torn tail at line {n} (no trailing newline); not read as a row");
        return Err(1);
    }
    let mut problems = inbox.problems;
    problems.extend(friction::privacy_problems(ctx, &inbox.rows));
    if !problems.is_empty() {
        for (n, m) in &problems {
            eprintln!("harness: inbox line {n}: {m}");
        }
        return Err(1);
    }
    Ok(inbox.rows.into_iter().map(|(_, r)| r).collect())
}

/// One suggested group.
struct Suggestion {
    group: String,
    row: usize,
    score: f64,
    first: usize,
}

/// `harness friction suggest --text <t> [--why <w>] [--top <k>] [--before <row id>] [--record <file>] [--config <file>]`
/// plus the Jev flags `[--jev-send <program> --jev-key-file <file> --jev-ledger-dir <dir> [--jev-deadline-ms <n>]]`.
pub fn suggest_cmd(rest: &[String], testmode: bool) -> i32 {
    const ALLOWED: [&str; 10] = [
        "--text", "--why", "--top", "--before", "--record", "--config", "--jev-send", "--jev-key-file", "--jev-ledger-dir", "--jev-deadline-ms",
    ];
    let mut flags: Vec<(&str, &str)> = Vec::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        // A constant message: the argument bytes have passed no privacy scan, so they are never echoed.
        let Some(name) = ALLOWED.iter().find(|a| **a == arg.as_str()) else {
            return fail("an unknown option (the friction usage line lists the suggest flags)");
        };
        let Some(v) = it.next() else {
            return fail(&format!("{name} is missing its value"));
        };
        if flags.iter().any(|(k, _)| k == name) {
            return fail(&format!("{name} was given twice"));
        }
        flags.push((name, v.as_str()));
    }
    let flag = |k: &str| flags.iter().find(|(n, _)| *n == k).map(|(_, v)| *v);
    let Some(text) = flag("--text") else {
        return fail("--text is required");
    };
    if text.trim().is_empty() {
        return fail("--text must not be empty");
    }
    let why = flag("--why").unwrap_or("");
    let top = match flag("--top") {
        None => DEFAULT_TOP,
        Some(v) => {
            if v.is_empty() || v.len() > 6 || !v.bytes().all(|c| c.is_ascii_digit()) || v.starts_with('0') {
                return fail("--top must be a positive whole number");
            }
            v.parse().unwrap_or(DEFAULT_TOP)
        }
    };
    // The --jev checks, in this order, right after the --top check and before the config is loaded.
    let (jev_send, jev_key, jev_ledger, jev_deadline) =
        (flag("--jev-send"), flag("--jev-key-file"), flag("--jev-ledger-dir"), flag("--jev-deadline-ms"));
    if jev_send.is_none() && (jev_key.is_some() || jev_ledger.is_some() || jev_deadline.is_some()) {
        return fail("a --jev flag needs --jev-send");
    }
    let mut jev: Option<crate::friction_suggest_jev::JevFlags> = None;
    if let Some(send) = jev_send {
        let (Some(key_file), Some(ledger_dir)) = (jev_key, jev_ledger) else {
            return fail("--jev-send needs --jev-key-file and --jev-ledger-dir");
        };
        for (name, v) in [("--jev-send", send), ("--jev-key-file", key_file), ("--jev-ledger-dir", ledger_dir)] {
            if !v.starts_with('/') {
                return fail(&format!("{name} must be an absolute path"));
            }
        }
        let deadline_ms = match jev_deadline {
            None => crate::friction_suggest_jev::DEFAULT_DEADLINE_MS,
            Some(v) => {
                if v.is_empty()
                    || v.len() > 6
                    || !v.bytes().all(|c| c.is_ascii_digit())
                    || v.starts_with('0')
                    || v.parse::<u64>().unwrap_or(0) > 600000
                {
                    return fail("--jev-deadline-ms must be a whole number from 1 to 600000");
                }
                v.parse().unwrap_or(crate::friction_suggest_jev::DEFAULT_DEADLINE_MS)
            }
        };
        jev = Some(crate::friction_suggest_jev::JevFlags { send, key_file, ledger_dir, deadline_ms });
    }
    let cfg = match friction::config_path(flag("--config")).and_then(|p| Config::load(&p)) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let record = flag("--record");
    if let Some(path) = record {
        if let Err(m) = friction::record_guard(path, flag("--config"), cfg.friction_inbox()) {
            return fail(m);
        }
    }
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned query is never ranked"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    // The query is scanned BEFORE anything is ranked; a hit names the field and the class, never the bytes.
    for (name, value) in [("text", text), ("why", why)] {
        let hits = crate::scan::scan_text(&ctx, "friction.suggest", value);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            return refuse(&format!("field {name} has a privacy hit ({}); nothing was suggested", classes.join(", ")));
        }
    }
    let rows = match lint_rows(&cfg, &ctx) {
        Ok(r) => r,
        Err(c) => return c,
    };
    // `--before <row id>`: only the rows before that row (inbox order) are ranked and counted.
    let limit = match flag("--before") {
        None => rows.len(),
        Some(id) => match rows.iter().position(|r| r.id == id) {
            Some(i) => i,
            None => return refuse("--before names no row in the inbox"),
        },
    };
    let all = match groups(&rows) {
        Ok(g) => g,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let query: Vec<String> = {
        let mut seen: HashSet<String> = HashSet::new();
        tokens(&format!("{text} {why}")).into_iter().filter(|t| seen.insert(t.clone())).collect()
    };
    let docs: Vec<Vec<String>> = rows[..limit].iter().map(|r| tokens(&format!("{} {}", r.text, r.why))).collect();
    let scores = bm25(&docs, &query);
    let mut found: Vec<Suggestion> = Vec::new();
    for g in &all {
        let mut best: Option<usize> = None;
        let mut first: Option<usize> = None;
        for &m in g.members.iter().filter(|&&m| m < limit) {
            first.get_or_insert(m);
            if best.is_none_or(|b| scores[m] > scores[b]) {
                best = Some(m);
            }
        }
        if let (Some(row), Some(first)) = (best, first) {
            found.push(Suggestion { group: g.id.clone(), row, score: scores[row], first });
        }
    }
    found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.first.cmp(&b.first)));
    // The Jev candidates are the BM25 order BEFORE the --top cut, the first 20 of them (--top changes the
    // printed lines only); each candidate stands for one group and is shown by its best row.
    let candidates: Vec<crate::friction_suggest_jev::JevCandidate> = found
        .iter()
        .take(20)
        .map(|s| crate::friction_suggest_jev::JevCandidate {
            group: s.group.clone(),
            row: rows[s.row].id.clone(),
            text: rows[s.row].text.clone(),
            why: rows[s.row].why.clone(),
        })
        .collect();
    found.truncate(top);
    // The first record line is appended BEFORE anything is printed (a failed append is a clean exit 2, empty stdout).
    // The clock is read once: the Jev arm's second line reuses the same ts.
    let mut ts: Option<String> = None;
    if let Some(path) = record {
        let now = now_ts();
        let ids: Vec<String> = found.iter().map(|s| friction::jstr(&s.group)).collect();
        let sha = crate::launcher::sha256_hex(format!("{text}\n{why}").as_bytes());
        let line = format!("{{\"ts\":{},\"query_sha256\":{},\"top\":[{}],\"via\":\"bm25\"}}\n", friction::jstr(&now), friction::jstr(&sha), ids.join(","));
        let mut f = match fs::OpenOptions::new().append(true).create(true).open(Path::new(path)) {
            Ok(f) => f,
            Err(_) => return fail("the suggest record cannot be written"),
        };
        if f.write_all(line.as_bytes()).and_then(|()| f.sync_all()).is_err() {
            return fail("the suggest record cannot be written");
        }
        ts = Some(now);
    }
    if found.is_empty() {
        println!("suggest: none");
    } else {
        for (i, s) in found.iter().enumerate() {
            println!("suggest: rank={} group={} row={} score={:.2} via=bm25", i + 1, s.group, rows[s.row].id, s.score);
        }
    }
    if let Some(flags) = jev {
        let _ = std::io::stdout().flush();
        let qsha = crate::launcher::sha256_hex(format!("{text}\n{why}").as_bytes());
        let run = crate::friction_suggest_jev::JevRun {
            text,
            why,
            flags,
            list: &list,
            query_sha256: &qsha,
            candidates: &candidates,
            record,
            ts: ts.as_deref().unwrap_or(""),
        };
        return crate::friction_suggest_jev::jev_arm(&run);
    }
    0
}
