//! The Jev arm of `harness friction suggest` (ticket friction-suggest-jev, unix only).
//!
//! With no `--jev` flag the verb is free: no model, no network. With `--jev-send <program>` the arm builds one
//! request from the cleaned query and the cleaned best row of at most 20 candidate groups, writes it to a fresh
//! private temp folder, runs the named send program with the path of that file (and the ledger path, the answer
//! path and the key file path), and prints one more line: the pick of the model or a skip line with a closed
//! reason token. The engine itself opens no socket and reads no key file: the send program makes the call and
//! only it can spend; the key file path is passed to it and is not opened here.

use crate::friction::jstr;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The deadline default, in milliseconds.
pub(crate) const DEFAULT_DEADLINE_MS: u64 = 12000;
/// How many candidate groups are offered, at most (the letters A to T).
const MAX_LETTERS: usize = 20;
/// A request over this many bytes is refused before anything is sent.
const REQUEST_CAP: usize = 199999;
/// An answer file over this many bytes is bad-answer.
const ANSWER_CAP: u64 = 1048576;
/// How much of the program's stderr the engine keeps (the last bytes).
const STDERR_KEEP: usize = 4096;
/// The cost written to the record for a run whose real cost is unknown (an upper estimate).
const ESTIMATE_COST: f64 = 0.0014;
/// The model the send program is asked for.
const MODEL: &str = "typesafe/jev-1.13";
/// The fixed choice prompt (324 bytes, no line feed at its end).
pub(crate) const PROMPT: &str = "The query above is a new friction report: a problem someone hit while working, with why it matters. Each option is an earlier friction report that stands for one group of reports about the same underlying problem. Which option describes the same underlying problem that the new report repeats? Choose the single best option.";

/// One candidate group, shown by its best row (the row whose text is sent).
pub(crate) struct JevCandidate {
    pub group: String,
    pub row: String,
    pub text: String,
    pub why: String,
}

/// The four `--jev` flags, checked by the verb before the config is loaded.
pub(crate) struct JevFlags<'a> {
    pub send: &'a str,
    pub key_file: &'a str,
    pub ledger_dir: &'a str,
    pub deadline_ms: u64,
}

/// Everything the arm needs from the verb, borrowed.
pub(crate) struct JevRun<'a> {
    pub text: &'a str,
    pub why: &'a str,
    pub flags: JevFlags<'a>,
    pub list: &'a crate::overlay::List,
    pub query_sha256: &'a str,
    pub candidates: &'a [JevCandidate],
    pub record: Option<&'a str>,
    pub ts: &'a str,
}

// ---------------------------------------------------------------- the clean rule

fn digits_at(b: &[u8], i: usize, n: usize) -> bool {
    i + n <= b.len() && b[i..i + n].iter().all(|c| c.is_ascii_digit())
}

/// The text as stored with the three id shapes (`f-YYYYMMDD-NN`, `issue-N`, `#N`, ASCII case folded) each replaced
/// by one blank, then each run of 2 or more blanks or tabs folded to one blank, then blanks, tabs and line ends cut
/// from both ends. The text is NOT lower-cased: each other byte is kept as it is.
fn clean(s: &str) -> String {
    let b = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let low = |c: u8| c.to_ascii_lowercase();
        if low(b[i]) == b'f' && b.get(i + 1) == Some(&b'-') && digits_at(b, i + 2, 8) && b.get(i + 10) == Some(&b'-') && digits_at(b, i + 11, 2) {
            out.push(b' ');
            i += 13;
            continue;
        }
        if b.len() >= i + 6 && b[i..i + 6].iter().zip(b"issue-").all(|(c, w)| low(*c) == *w) {
            let mut j = i + 6;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 6 {
                out.push(b' ');
                i = j;
                continue;
            }
        }
        if b[i] == b'#' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 {
                out.push(b' ');
                i = j;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    let mut folded: Vec<u8> = Vec::with_capacity(out.len());
    let mut i = 0;
    while i < out.len() {
        if out[i] == b' ' || out[i] == b'\t' {
            let start = i;
            while i < out.len() && (out[i] == b' ' || out[i] == b'\t') {
                i += 1;
            }
            if i - start == 1 {
                folded.push(out[start]);
            } else {
                folded.push(b' ');
            }
        } else {
            folded.push(out[i]);
            i += 1;
        }
    }
    let blank = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | b'\r');
    let start = folded.iter().position(|c| !blank(*c)).unwrap_or(folded.len());
    let end = folded.iter().rposition(|c| !blank(*c)).map(|e| e + 1).unwrap_or(start);
    // Only ASCII bytes were dropped or folded, so what is left is still valid UTF-8.
    String::from_utf8(folded[start..end].to_vec()).unwrap_or_default()
}

/// The shown text of a row or of the query: `clean(text)`, one line feed, `Why: `, `clean(why)`; the Why part is
/// there also when why is empty.
fn shown(text: &str, why: &str) -> String {
    format!("{}\nWhy: {}", clean(text), clean(why))
}

/// The code rule: three grave accents in a row, or a line (line feeds split; the first line counts) that starts
/// with 4 blanks or with a tab.
fn code_in(s: &str) -> bool {
    let fence: &[u8] = &[0x60, 0x60, 0x60];
    s.as_bytes().windows(3).any(|w| w == fence) || s.lines().any(|l| l.starts_with("    ") || l.starts_with('\t'))
}

// ---------------------------------------------------------------- the request

/// The letter order key: the lower-case hex sha256 of `<query sha256 hex>:<group id>`.
fn letter_hash(query_sha256: &str, group: &str) -> String {
    crate::launcher::sha256_hex(format!("{query_sha256}:{group}").as_bytes())
}

/// One JSON object, no blanks between tokens, no final line feed, keys in the fixed order.
fn build_request(run: &JevRun, lettered: &[(char, &JevCandidate)]) -> String {
    let mut r = format!(
        "{{\"model\":{},\"state\":{{\"query\":{}}},\"questions\":{{\"group\":{{\"type\":{},\"instructions\":{},\"criteria\":{{",
        jstr(MODEL),
        jstr(&shown(run.text, run.why)),
        jstr("choice"),
        jstr(PROMPT)
    );
    for (i, (letter, c)) in lettered.iter().enumerate() {
        if i > 0 {
            r.push(',');
        }
        r.push_str(&format!("{}:{}", jstr(&letter.to_string()), jstr(&shown(&c.text, &c.why))));
    }
    r.push_str("}}}}");
    r
}

// ---------------------------------------------------------------- the answer reader

#[derive(Debug, Clone, PartialEq)]
enum Jv {
    Str(String),
    Num(f64),
    Bool(bool),
    Null,
    Arr(Vec<Jv>),
    Obj(Vec<(String, Jv)>),
}

/// A strict whole-value JSON reader over bytes; `Err` on a bad shape, on nesting deeper than 16 and on a \u escape
/// that is not one scalar value (a surrogate pair is refused).
fn parse_json(b: &[u8]) -> Result<Jv, ()> {
    let mut p = P { b, i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != b.len() {
        return Err(());
    }
    Ok(v)
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while matches!(self.b.get(self.i), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.i += 1;
        }
    }

    fn value(&mut self, depth: u32) -> Result<Jv, ()> {
        if depth > 16 {
            return Err(());
        }
        self.ws();
        match self.b.get(self.i).copied() {
            Some(b'"') => Ok(Jv::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(Jv::Arr(out));
                }
                loop {
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(Jv::Arr(out));
                        }
                        _ => return Err(()),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(Jv::Obj(out));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err(());
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(());
                    }
                    self.i += 1;
                    out.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(Jv::Obj(out));
                        }
                        _ => return Err(()),
                    }
                }
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(b't') if self.b[self.i..].starts_with(b"true") => {
                self.i += 4;
                Ok(Jv::Bool(true))
            }
            Some(b'f') if self.b[self.i..].starts_with(b"false") => {
                self.i += 5;
                Ok(Jv::Bool(false))
            }
            Some(b'n') if self.b[self.i..].starts_with(b"null") => {
                self.i += 4;
                Ok(Jv::Null)
            }
            _ => Err(()),
        }
    }

    /// A JSON number read as a 64-bit float (sign, digits, fraction, exponent).
    fn number(&mut self) -> Result<Jv, ()> {
        let s = self.i;
        if self.b.get(self.i) == Some(&b'-') {
            self.i += 1;
        }
        match self.b.get(self.i).copied() {
            Some(b'0') => self.i += 1,
            Some(c) if c.is_ascii_digit() => {
                while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                    self.i += 1;
                }
            }
            _ => return Err(()),
        }
        if self.b.get(self.i) == Some(&b'.') {
            self.i += 1;
            let d = self.i;
            while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == d {
                return Err(());
            }
        }
        if matches!(self.b.get(self.i), Some(b'e' | b'E')) {
            self.i += 1;
            if matches!(self.b.get(self.i), Some(b'+' | b'-')) {
                self.i += 1;
            }
            let d = self.i;
            while matches!(self.b.get(self.i), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
            if self.i == d {
                return Err(());
            }
        }
        let t = std::str::from_utf8(&self.b[s..self.i]).map_err(|_| ())?;
        t.parse::<f64>().map(Jv::Num).map_err(|_| ())
    }

    fn string(&mut self) -> Result<String, ()> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or(())?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| ()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or(())?;
                    self.i += 1;
                    match e {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let hex = self.b.get(self.i..self.i + 4).ok_or(())?;
                            let n = u32::from_str_radix(std::str::from_utf8(hex).map_err(|_| ())?, 16).map_err(|_| ())?;
                            let ch = char::from_u32(n).ok_or(())?;
                            let mut buf = [0u8; 4];
                            out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                            self.i += 4;
                        }
                        _ => return Err(()),
                    }
                }
                c if c < 0x20 => return Err(()),
                c => out.push(c),
            }
        }
    }
}

fn dup_key(obj: &[(String, Jv)]) -> bool {
    let mut seen = std::collections::HashSet::new();
    obj.iter().any(|(k, _)| !seen.insert(k.as_str()))
}

/// What the answer file said: the values per letter (None = bad-answer) and the usage cost.
struct AnswerRead {
    values: Option<Vec<f64>>,
    usage_cost: Option<f64>,
}

/// Read `<temp>/answer.json` under the cap and the five-object rules; a bad shape is bad-answer, not a panic.
fn read_answer(path: &Path, letters: &[char]) -> AnswerRead {
    let bad = AnswerRead { values: None, usage_cost: None };
    let md = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => return bad,
    };
    if !md.file_type().is_file() || md.len() > ANSWER_CAP {
        return bad;
    }
    let mut buf = Vec::new();
    if fs::File::open(path).and_then(|f| f.take(ANSWER_CAP + 1).read_to_end(&mut buf)).is_err() {
        return bad;
    }
    if buf.len() as u64 > ANSWER_CAP {
        return bad;
    }
    let v = match parse_json(&buf) {
        Ok(v) => v,
        Err(_) => return bad,
    };
    let Jv::Obj(top) = &v else { return bad };
    if dup_key(top) {
        return bad;
    }
    // usage, when there, is read for the cost first: a good cost counts also when the probabilities part is bad;
    // a duplicate key there is bad-answer with no cost.
    let usage = top.iter().find(|(k, _)| k == "usage").and_then(|(_, v)| match v {
        Jv::Obj(kv) => Some(kv),
        _ => None,
    });
    if let Some(u) = usage {
        if dup_key(u) {
            return bad;
        }
    }
    let usage_cost = usage
        .and_then(|u| u.iter().find(|(k, _)| k == "cost").map(|(_, v)| v))
        .and_then(|v| match v {
            Jv::Num(f) if f.is_finite() && (0.0..=1.0).contains(f) => Some(*f),
            _ => None,
        });
    let bad = AnswerRead { values: None, usage_cost };
    // answers, then group, then probabilities: each must be an object with no key given twice.
    fn as_obj(o: &Jv) -> Option<&Vec<(String, Jv)>> {
        match o {
            Jv::Obj(kv) => Some(kv),
            _ => None,
        }
    }
    fn level<'a>(parent: &'a [(String, Jv)], key: &str) -> Option<&'a Vec<(String, Jv)>> {
        parent.iter().find(|(k, _)| k == key).map(|(_, v)| v).and_then(as_obj)
    }
    let Some(answers) = level(top, "answers") else { return bad };
    if dup_key(answers) {
        return bad;
    }
    let Some(group) = level(answers, "group") else { return bad };
    if dup_key(group) {
        return bad;
    }
    let Some(probs) = level(group, "probabilities") else { return bad };
    if dup_key(probs) {
        return bad;
    }
    // The keys must be exactly the asked letters, byte-equal, and each value a number from 0 to 1.
    let mut values = vec![f64::NAN; letters.len()];
    let mut seen = vec![false; letters.len()];
    for (k, val) in probs {
        let Some(i) = letters.iter().position(|l| k == &l.to_string()) else { return bad };
        seen[i] = true;
        let Jv::Num(f) = val else { return bad };
        if !f.is_finite() || !(0.0..=1.0).contains(f) {
            return bad;
        }
        values[i] = *f;
    }
    if !seen.iter().all(|s| *s) {
        return bad;
    }
    AnswerRead { values: Some(values), usage_cost }
}

// ---------------------------------------------------------------- the send run

/// How the program ended, with what the engine keeps of its stderr.
enum End {
    SpawnFailed,
    WaitErr,
    Timeout,
    Signal,
    Code(i32, Vec<u8>),
}

fn run_send(run: &JevRun, temp: &Path) -> (End, u64) {
    let ledger = Path::new(run.flags.ledger_dir).join(format!("spend-friction-suggest-{}.jsonl", crate::friction::today_utc()));
    let mut c = Command::new(run.flags.send);
    c.arg("--payload")
        .arg(temp.join("request.json"))
        .arg("--surface")
        .arg("prose")
        .arg("--spend-ledger")
        .arg(&ledger)
        .arg("--out")
        .arg(temp.join("answer.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .current_dir(temp)
        .process_group(0)
        .env_clear();
    for name in ["PATH", "HOME", "LANG", "LC_ALL"] {
        if let Some(v) = std::env::var_os(name) {
            c.env(name, v);
        }
    }
    c.env("TMPDIR", temp).env("OPENROUTER_KEY_FILE", run.flags.key_file);
    let start = Instant::now();
    // A file written just before the run can be briefly busy while another thread forks; retry a few times.
    let mut tries = 0;
    let child = loop {
        match c.spawn() {
            Ok(ch) => break Some(ch),
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && tries < 10 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(_) => break None,
        }
    };
    let Some(mut child) = child else { return (End::SpawnFailed, 0) };
    let mut pipe = child.stderr.take().expect("stderr was piped");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut keep = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    keep.extend_from_slice(&buf[..n]);
                    if keep.len() > STDERR_KEEP {
                        keep.drain(..keep.len() - STDERR_KEEP);
                    }
                }
            }
        }
        let _ = tx.send(keep);
    });
    let deadline = start + Duration::from_millis(run.flags.deadline_ms);
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                // After a normal end the engine waits at most 1000 ms for stderr to close; past that the kept stderr is empty.
                let tail = rx.recv_timeout(Duration::from_millis(1000)).unwrap_or_default();
                let end = match st.code() {
                    Some(code) => End::Code(code, tail),
                    None => End::Signal,
                };
                return (end, start.elapsed().as_millis() as u64);
            }
            Ok(None) => {}
            Err(_) => return (End::WaitErr, start.elapsed().as_millis() as u64),
        }
        if Instant::now() >= deadline {
            crate::launcher::kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            return (End::Timeout, start.elapsed().as_millis() as u64);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The LAST line of the kept stderr that ends with `HTTP `, one blank and exactly 3 digits, as a reason token.
fn http_token(tail: &[u8]) -> String {
    let text = String::from_utf8_lossy(tail);
    for line in text.lines().rev() {
        let b = line.as_bytes();
        if b.len() >= 8 && b[..b.len() - 3].ends_with(b"HTTP ") && b[b.len() - 3..].iter().all(|c| c.is_ascii_digit()) {
            let code = &line[line.len() - 3..];
            return if code == "402" { "no-credit".to_string() } else { format!("http-{code}") };
        }
    }
    "http-unknown".to_string()
}

// ---------------------------------------------------------------- the arm

/// The outcome the record and the print line carry.
struct Verdict {
    status: Status,
    cost: f64,
    latency_ms: u64,
}

enum Status {
    Pick { letter: char, group: String, row: String, p: f64 },
    Skip { reason: String },
}

fn v_skip(reason: &str, cost: f64, latency_ms: u64) -> Verdict {
    Verdict { status: Status::Skip { reason: reason.to_string() }, cost, latency_ms }
}

/// One fresh private folder under the system temp folder; the create fails when the name is taken.
fn make_temp() -> Option<PathBuf> {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for _ in 0..100 {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let p = std::env::temp_dir().join(format!("harness-friction-jev-{}-{}-{}", std::process::id(), nanos, n));
        if fs::DirBuilder::new().mode(0o700).create(&p).is_ok() {
            return Some(p);
        }
    }
    None
}

/// Write `request.json` with mode 0600 into the temp folder.
fn write_request(temp: &Path, request: &str) -> std::io::Result<()> {
    let path = temp.join("request.json");
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
    f.set_permissions(fs::Permissions::from_mode(0o600))?;
    f.write_all(request.as_bytes())
}

/// The second record line (one JSON object, one line, the fixed key order); it holds no text and no path.
fn record_line(run: &JevRun, v: &Verdict) -> String {
    let (status, reason, letter, group, p) = match &v.status {
        Status::Pick { letter, group, p, .. } => ("pick", String::new(), letter.to_string(), group.clone(), format!("{p:.2}")),
        Status::Skip { reason } => ("skipped", reason.clone(), String::new(), String::new(), "null".to_string()),
    };
    format!(
        "{{\"ts\":{},\"query_sha256\":{},\"via\":\"jev\",\"status\":{},\"reason\":{},\"letter\":{},\"group\":{},\"p\":{},\"cost_usd\":{:.6},\"latency_ms\":{}}}\n",
        jstr(run.ts),
        jstr(run.query_sha256),
        jstr(status),
        jstr(&reason),
        jstr(&letter),
        jstr(&group),
        p,
        v.cost,
        v.latency_ms
    )
}

/// Remove the temp folder, append the second record line when asked, then print the one Jev line. The only
/// non-zero exit is a failed second record append (2, after the BM25 lines, the Jev line not printed).
fn finish(run: &JevRun, v: &Verdict, temp: Option<&Path>) -> i32 {
    if let Some(t) = temp {
        let _ = fs::remove_dir_all(t);
    }
    if let Some(path) = run.record {
        let line = record_line(run, v);
        let written = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(Path::new(path))
            .and_then(|mut f| f.write_all(line.as_bytes()).and_then(|()| f.sync_all()));
        if written.is_err() {
            eprintln!("harness: error: the suggest record cannot be written");
            return 2;
        }
    }
    match &v.status {
        Status::Pick { group, row, p, .. } => println!("suggest: jev pick={group} row={row} p={p:.2}"),
        Status::Skip { reason } => println!("jev: skipped reason={reason}"),
    }
    0
}

/// The Jev arm proper. Runs after the BM25 lines were printed and stdout was flushed; prints one more stdout
/// line (the last one), or none when the second record append fails, and returns the verb's exit code.
pub(crate) fn jev_arm(run: &JevRun) -> i32 {
    // (1) code in the query, as given.
    if code_in(run.text) || code_in(run.why) {
        return finish(run, &v_skip("code-skipped", 0.0, 0), None);
    }
    // (2) candidates with code in their row are left out; fewer than 2 left ends the arm.
    let kept: Vec<&JevCandidate> = run.candidates.iter().filter(|c| !code_in(&c.text) && !code_in(&c.why)).collect();
    if kept.len() < 2 {
        return finish(run, &v_skip("no-candidates", 0.0, 0), None);
    }
    // Letters: the candidates sorted by the sha256 of `<query sha256>:<group id>`, smallest first, A upwards.
    let mut hashes: Vec<(String, &JevCandidate)> = kept.into_iter().map(|c| (letter_hash(run.query_sha256, &c.group), c)).collect();
    hashes.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.group.cmp(&b.1.group)));
    let lettered: Vec<(char, &JevCandidate)> = hashes
        .into_iter()
        .enumerate()
        .filter(|(i, _)| *i < MAX_LETTERS)
        .map(|(i, (_, c))| ((b'A' + i as u8) as char, c))
        .collect();
    let letters: Vec<char> = lettered.iter().map(|(l, _)| *l).collect();
    // (3) the byte cap.
    let request = build_request(run, &lettered);
    if request.len() > REQUEST_CAP {
        return finish(run, &v_skip("payload", 0.0, 0), None);
    }
    // (4) the whole request through the privacy engine, test mode OFF whatever the run was given.
    let sctx = crate::scan::Ctx { list: run.list, testmode: false };
    if !crate::scan::scan_text(&sctx, "friction.suggest.jev", &request).is_empty() {
        return finish(run, &v_skip("scan-refused", 0.0, 0), None);
    }
    // (5) the key file must name a regular file (a link to a file counts); the engine does not open it.
    let key_ok = fs::metadata(Path::new(run.flags.key_file)).map(|m| m.is_file()).unwrap_or(false);
    if !key_ok {
        return finish(run, &v_skip("no-key", 0.0, 0), None);
    }
    // (6) the ledger folder: there and a folder, or missing and makable (only the last part, mode 0700).
    let ldir = Path::new(run.flags.ledger_dir);
    let ledger_ok = if ldir.exists() {
        ldir.is_dir()
    } else {
        fs::DirBuilder::new().mode(0o700).create(ldir).is_ok()
    };
    if !ledger_ok {
        return finish(run, &v_skip("ledger", 0.0, 0), None);
    }
    // (7) the temp folder and the request file.
    let Some(temp) = make_temp() else {
        return finish(run, &v_skip("spawn-failed", 0.0, 0), None);
    };
    if write_request(&temp, &request).is_err() {
        return finish(run, &v_skip("spawn-failed", 0.0, 0), Some(&temp));
    }
    // The run.
    let (end, latency_ms) = run_send(run, &temp);
    let answer = temp.join("answer.json");
    let read = read_answer(&answer, &letters);
    let usage_cost = read.usage_cost.filter(|c| (0.0..=1.0).contains(c));
    let v = match end {
        End::SpawnFailed => v_skip("spawn-failed", 0.0, 0),
        End::WaitErr => v_skip("spawn-failed", ESTIMATE_COST, latency_ms),
        End::Timeout => v_skip("timeout", ESTIMATE_COST, latency_ms),
        End::Signal => v_skip("send-signal", ESTIMATE_COST, latency_ms),
        End::Code(0, _) => {
            let cost = usage_cost.unwrap_or(ESTIMATE_COST);
            match decide(&read.values, &letters) {
                Some((letter, p)) => {
                    let c = lettered.iter().find(|(l, _)| *l == letter).expect("the letter was asked").1;
                    Verdict { status: Status::Pick { letter, group: c.group.clone(), row: c.row.clone(), p }, cost, latency_ms }
                }
                None if read.values.is_some() => v_skip("flat", cost, latency_ms),
                None => v_skip("bad-answer", cost, latency_ms),
            }
        }
        End::Code(5, _) => v_skip("no-key", ESTIMATE_COST, latency_ms),
        End::Code(7, _) => v_skip("scan-refused", ESTIMATE_COST, latency_ms),
        End::Code(8, tail) => {
            let text = String::from_utf8_lossy(&tail);
            if text.contains("SPEND-CAP reached") {
                v_skip("cap", ESTIMATE_COST, latency_ms)
            } else {
                v_skip("ledger", ESTIMATE_COST, latency_ms)
            }
        }
        End::Code(9, _) => v_skip("transport", ESTIMATE_COST, latency_ms),
        End::Code(10, tail) => v_skip(&http_token(&tail), ESTIMATE_COST, latency_ms),
        End::Code(11 | 12, _) => v_skip("ledger", ESTIMATE_COST, latency_ms),
        End::Code(n, _) => v_skip(&format!("send-{n}"), ESTIMATE_COST, latency_ms),
    };
    finish(run, &v, Some(&temp))
}

/// The pick: the letter with the highest value, ties to the earlier letter; None (with values Some) = flat,
/// None with values None = bad-answer.
fn decide(values: &Option<Vec<f64>>, letters: &[char]) -> Option<(char, f64)> {
    let vals = values.as_ref()?;
    if vals.windows(2).all(|w| w[0] == w[1]) {
        return None;
    }
    let mut best = 0;
    for i in 1..vals.len() {
        if vals[i] > vals[best] {
            best = i;
        }
    }
    Some((letters[best], vals[best]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_deadline_is_12000_ms() {
        assert_eq!(DEFAULT_DEADLINE_MS, 12000);
    }

    #[test]
    fn the_prompt_constant_hashes_to_its_fixed_value() {
        assert_eq!(PROMPT.len(), 324);
        assert_eq!(
            crate::launcher::sha256_hex(PROMPT.as_bytes()),
            "653296451c2ece330d858b77b5a20578cc4d2aeb62d6ae744f0ccbdbaf8c38ce"
        );
    }

    #[test]
    fn clean_strips_the_three_id_shapes_with_case_folding_and_keeps_case() {
        assert_eq!(clean("See F-20991231-01 and ISSUE-42 and #7 ok"), "See and and ok");
        assert_eq!(clean("f-20991231-01zeta issue-5 zeta #9zeta"), "zeta zeta zeta");
        assert_eq!(clean("  a \t\t b \n"), "a b");
        assert_eq!(clean("one tab\there stays"), "one tab\there stays");
        assert_eq!(clean("two  blanks fold"), "two blanks fold");
        assert_eq!(clean("keep Upper and lower"), "keep Upper and lower");
    }
}
