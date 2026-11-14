//! The friction triage loop (ticket 3601, Leg A): `harness friction rank | triage | bind | page`.
//!
//! `rank` is a pure function of the inbox: rows are grouped by the TRANSITIVE `breaks` closure (a chain, a forward
//! pointer or a cycle are all lint-legal, so the walk is iterative and a revisit is a refusal), scored as
//! impact weight x occurrences, and listed in one total order. `triage` is the loop's one deterministic step: it
//! reads an append-only state file, resolves groups from a closed feed keyed by a BOUND task id, flags
//! regressions, and writes ticket-contract hand-off files for the groups to fix. `bind` is the one writer of the
//! task binding. `page` writes the weekly report text ONLY to its `--out` file.
//!
//! No wall clock runs in `rank`, `triage` or `bind`: the week is an argument. Every verb runs the full inbox lint
//! first (same refusal as `friction lint`), and nothing printed to stdout carries a row's `text`, `why` or `proof`:
//! ids, numbers and dates only. Exit codes as the family: 0 ok; 1 a refusal; 2 a usage or setup error.

use crate::events::token_ok;
use crate::friction::{self, Row};
use crate::config::Config;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

/// The state file is read no further than this (the inbox's own cap); past it the verb refuses.
const STATE_CAP: u64 = 4 * 1024 * 1024;
/// The closed feed is read no further than 1 MiB.
const CLOSED_CAP: u64 = 1024 * 1024;
/// Nesting bound of the closed-feed reader: a deeper document is refused, so no input can recurse far.
const JSON_DEPTH: usize = 16;

// ---------------------------------------------------------------- grouping and score (pure)

/// What a group is rooted at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootKind {
    /// A hand-written friction row.
    Hand,
    /// A `source: event` row; the group key is the row's signature.
    Event,
    /// A `kind: pattern` row (a proven procedure); ranked only when something broke it.
    Pattern,
}

/// One group of the partition: every inbox row lands in exactly one group.
#[derive(Debug, Clone)]
pub struct Group {
    pub id: String,
    pub root: RootKind,
    /// Row indexes (inbox order).
    pub members: Vec<usize>,
    /// Members whose `breaks` is non-empty.
    pub break_count: usize,
    pub occurrences: usize,
    /// Highest-rated member impact, or empty (unrated).
    pub impact: String,
    pub score: u64,
    pub first: String,
    pub last: String,
    /// False for a pattern-rooted group nothing broke (a success, listed by `patterns`).
    pub ranked: bool,
}

fn impact_rank(i: &str) -> u8 {
    match i {
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        "blocker" => 4,
        _ => 0,
    }
}

fn weight(rank: u8) -> u64 {
    match rank {
        0 | 1 => 1,
        2 => 3,
        3 => 9,
        _ => 27,
    }
}

/// The signature of an event row id: the id prefix before `.<yyyy>-w<ww>`.
fn signature(id: &str) -> &str {
    match id.rfind('.') {
        Some(i) if friction::week_ok(&id[i + 1..]) => &id[..i],
        _ => id,
    }
}

/// The closure walk: for every row the index of its `breaks` ROOT, followed iteratively through any depth. A walk
/// that revisits a row is a cycle: the answer is `Err("rank: breaks cycle through <lowest id on the cycle>")`
/// (the lowest over all cycles, so the message is deterministic). Linear: every row is resolved once.
fn root_index(rows: &[Row]) -> Result<Vec<usize>, String> {
    let by_id: HashMap<&str, usize> = rows.iter().enumerate().map(|(i, r)| (r.id.as_str(), i)).collect();
    let mut memo: Vec<Option<usize>> = vec![None; rows.len()];
    let mut cycle_low: Option<&str> = None;
    for start in 0..rows.len() {
        if memo[start].is_some() {
            continue;
        }
        let mut path: Vec<usize> = Vec::new();
        let mut pos: HashMap<usize, usize> = HashMap::new();
        let mut cur = start;
        let root = loop {
            if let Some(r) = memo[cur] {
                break r;
            }
            if let Some(&p) = pos.get(&cur) {
                let low = path[p..].iter().map(|&i| rows[i].id.as_str()).min().unwrap_or("");
                if cycle_low.is_none_or(|c| low < c) {
                    cycle_low = Some(low);
                }
                break cur;
            }
            pos.insert(cur, path.len());
            path.push(cur);
            let b = rows[cur].breaks.as_str();
            match if b.is_empty() { None } else { by_id.get(b) } {
                Some(&next) => cur = next,
                None => break cur,
            }
        };
        for i in path {
            memo[i] = Some(root);
        }
    }
    if let Some(id) = cycle_low {
        return Err(format!("rank: breaks cycle through {id}"));
    }
    Ok(memo.into_iter().map(|m| m.unwrap_or(0)).collect())
}

/// The whole partition, ordered by group id. Pure: no clock, no file.
pub fn groups(rows: &[Row]) -> Result<Vec<Group>, String> {
    let roots = root_index(rows)?;
    let mut map: BTreeMap<String, (RootKind, Vec<usize>)> = BTreeMap::new();
    for (i, _) in rows.iter().enumerate() {
        let root = &rows[roots[i]];
        let (key, kind) = if root.source == "event" {
            (signature(&root.id).to_string(), RootKind::Event)
        } else if root.kind == "pattern" {
            (root.id.clone(), RootKind::Pattern)
        } else {
            (root.id.clone(), RootKind::Hand)
        };
        map.entry(key).or_insert((kind, Vec::new())).1.push(i);
    }
    let mut out = Vec::with_capacity(map.len());
    for (id, (root, members)) in map {
        let break_count = members.iter().filter(|&&m| !rows[m].breaks.is_empty()).count();
        let occurrences = match root {
            RootKind::Hand | RootKind::Event => members.len(),
            RootKind::Pattern => break_count,
        };
        let top = members.iter().map(|&m| impact_rank(&rows[m].impact)).max().unwrap_or(0);
        let impact = match top {
            1 => "low",
            2 => "medium",
            3 => "high",
            4 => "blocker",
            _ => "",
        };
        let first = members.iter().map(|&m| rows[m].date.as_str()).min().unwrap_or("").to_string();
        let last = members.iter().map(|&m| rows[m].date.as_str()).max().unwrap_or("").to_string();
        let ranked = !(root == RootKind::Pattern && break_count == 0);
        out.push(Group {
            id,
            root,
            members,
            break_count,
            occurrences,
            impact: impact.to_string(),
            score: weight(top) * occurrences as u64,
            first,
            last,
            ranked,
        });
    }
    Ok(out)
}

/// The ranked groups in the one total order: score desc, last-seen desc, id asc.
pub fn ranked(all: &[Group]) -> Vec<&Group> {
    let mut v: Vec<&Group> = all.iter().filter(|g| g.ranked).collect();
    v.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| b.last.cmp(&a.last)).then_with(|| a.id.cmp(&b.id)));
    v
}

// ---------------------------------------------------------------- shared plumbing

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {}", m);
    2
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: friction refused: {}", m);
    1
}

/// Every flag is `--name value`, each at most once; an unknown option is a constant message (the argument bytes
/// have passed no privacy scan, so they are never echoed).
fn parse_flags(rest: &[String], allowed: &[&str], verb: &str) -> Result<Vec<(String, String)>, String> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        if !allowed.contains(&arg.as_str()) {
            return Err(format!("an unknown option ({verb} takes only {})", allowed.join(", ")));
        }
        let Some(v) = it.next() else {
            return Err(format!("{arg} is missing its value"));
        };
        if out.iter().any(|(k, _)| k == arg) {
            return Err(format!("{arg} was given twice"));
        }
        out.push((arg.clone(), v.clone()));
    }
    Ok(out)
}

fn flag<'a>(f: &'a [(String, String)], k: &str) -> Option<&'a str> {
    f.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str())
}

fn load_cfg(file: Option<&str>) -> Result<Config, i32> {
    let path = friction::config_path(file).map_err(|e| fail(&e))?;
    Config::load(&path).map_err(|e| fail(&e))
}

fn load_list(what: &str) -> Result<crate::overlay::List, i32> {
    crate::overlay::load().map_err(|_| fail(&format!("no privacy list configured: {what}")))
}

/// The inbox rows after the FULL lint (`friction lint`'s own refusal behaviour: nothing on stdout, same stderr).
fn lint_rows(cfg: &Config, ctx: &crate::scan::Ctx) -> Result<Vec<Row>, i32> {
    let inbox = friction::read_inbox(Path::new(cfg.friction_inbox())).map_err(|e| fail(&e))?;
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

fn parse_top(v: &str) -> Result<usize, i32> {
    if v.is_empty() || v.len() > 6 || !v.bytes().all(|c| c.is_ascii_digit()) || v.starts_with('0') {
        return Err(fail("--top must be a positive whole number"));
    }
    Ok(v.parse().unwrap_or(1))
}

// ---------------------------------------------------------------- rank

/// `harness friction rank [--top K] [--record <file>] [--config <file>]`.
pub fn rank_cmd(rest: &[String], testmode: bool) -> i32 {
    let flags = match parse_flags(rest, &["--top", "--record", "--config"], "rank") {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let cfg = match load_cfg(flag(&flags, "--config")) {
        Ok(c) => c,
        Err(c) => return c,
    };
    let top = match flag(&flags, "--top") {
        Some(v) => match parse_top(v) {
            Ok(k) => k,
            Err(c) => return c,
        },
        None => cfg.triage_top() as usize,
    };
    let record = flag(&flags, "--record");
    if let Some(path) = record {
        if friction::paths_name_one_file(path, cfg.friction_inbox()) {
            return fail("--record must not name the friction inbox");
        }
    }
    let list = match load_list("an unscanned inbox is never ranked") {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let rows = match lint_rows(&cfg, &ctx) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let all = match groups(&rows) {
        Ok(g) => g,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let order = ranked(&all);
    let listed: Vec<&&Group> = order.iter().take(top).collect();
    // The trial line is appended BEFORE anything is printed (a failed append is a clean exit 2, empty stdout).
    if let Some(path) = record {
        let ids: Vec<String> = listed.iter().map(|g| friction::jstr(&g.id)).collect();
        let line = format!("{{\"date\":{},\"groups\":{},\"listed\":[{}]}}\n", friction::jstr(&friction::today_utc()), order.len(), ids.join(","));
        let mut f = match fs::OpenOptions::new().append(true).create(true).open(Path::new(path)) {
            Ok(f) => f,
            Err(_) => return fail("the trial record cannot be written"),
        };
        if f.write_all(line.as_bytes()).and_then(|()| f.sync_all()).is_err() {
            return fail("the trial record cannot be written");
        }
    }
    for g in &listed {
        println!(
            "rank group={} score={} impact={} occurrences={} first={} last={}",
            g.id,
            g.score,
            if g.impact.is_empty() { "unrated" } else { g.impact.as_str() },
            g.occurrences,
            g.first,
            g.last
        );
    }
    println!("rank ok groups={} listed={}", order.len(), listed.len());
    0
}

// ---------------------------------------------------------------- the state file

const STATUSES: [&str; 4] = ["proposed", "resolved", "regressed", "escalated"];
const ST_KEYS: [&str; 9] = ["group", "status", "ticket_file", "proposed_week", "task_id", "task_session", "resolved_pr", "resolved_date", "attempts"];

/// One state row (every value is stored as a string; `attempts` is digits).
#[derive(Debug, Clone, PartialEq, Eq)]
struct St {
    group: String,
    status: String,
    ticket_file: String,
    proposed_week: String,
    task_id: String,
    task_session: String,
    resolved_pr: String,
    resolved_date: String,
    attempts: u32,
}

fn digits_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|c| c.is_ascii_digit())
}

fn pr_ref_ok(s: &str) -> bool {
    match s.split_once('#') {
        Some((repo, n)) => !repo.is_empty() && repo.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/')) && digits_ok(n, 15),
        None => false,
    }
}

fn st_encode(s: &St) -> String {
    let attempts = s.attempts.to_string();
    let vals = [&s.group, &s.status, &s.ticket_file, &s.proposed_week, &s.task_id, &s.task_session, &s.resolved_pr, &s.resolved_date, &attempts];
    let mut line = String::from("{");
    for (i, (k, v)) in ST_KEYS.iter().zip(vals).enumerate() {
        if i > 0 {
            line.push(',');
        }
        line.push_str(&format!("{}:{}", friction::jstr(k), friction::jstr(v)));
    }
    line.push('}');
    line
}

fn st_parse(line: &str) -> Result<St, String> {
    let mut p = friction::P { b: line.as_bytes(), i: 0 };
    let pairs = p.object()?;
    p.ws();
    if p.i != p.b.len() {
        return Err("trailing bytes after the object".to_string());
    }
    if pairs.len() != ST_KEYS.len() || pairs.iter().any(|(k, _)| !ST_KEYS.contains(&k.as_str())) {
        return Err("a state row has exactly the nine state keys".to_string());
    }
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let s = St {
        group: get("group"),
        status: get("status"),
        ticket_file: get("ticket_file"),
        proposed_week: get("proposed_week"),
        task_id: get("task_id"),
        task_session: get("task_session"),
        resolved_pr: get("resolved_pr"),
        resolved_date: get("resolved_date"),
        attempts: 0,
    };
    let attempts = get("attempts");
    if !digits_ok(&attempts, 3) {
        return Err("attempts must be digits".to_string());
    }
    let s = St { attempts: attempts.parse().unwrap_or(0), ..s };
    if !token_ok(&s.group) {
        return Err("group must be a plain token".to_string());
    }
    if !STATUSES.contains(&s.status.as_str()) {
        return Err("status must be one of proposed, resolved, regressed, escalated".to_string());
    }
    if !s.ticket_file.is_empty() && !token_ok(&s.ticket_file) {
        return Err("ticket_file must be a plain basename".to_string());
    }
    if !friction::week_ok(&s.proposed_week) {
        return Err("proposed_week must be <iso-year>-w<ww>".to_string());
    }
    if !s.task_id.is_empty() && !digits_ok(&s.task_id, 15) {
        return Err("task_id must be digits or empty".to_string());
    }
    if !s.task_session.is_empty() && !token_ok(&s.task_session) {
        return Err("task_session must be a plain token or empty".to_string());
    }
    if !s.resolved_pr.is_empty() && !pr_ref_ok(&s.resolved_pr) {
        return Err("resolved_pr must be <repo>#<digits> or empty".to_string());
    }
    if !s.resolved_date.is_empty() && !friction::date_ok(&s.resolved_date) {
        return Err("resolved_date must be a calendar date or empty".to_string());
    }
    if s.status == "resolved" && s.resolved_date.is_empty() {
        return Err("a resolved row needs resolved_date".to_string());
    }
    Ok(s)
}

/// The state file's raw bytes (kept for the write-time comparison) and its rows. A missing file is an empty
/// state (nothing is created by a read). Torn or bad = refuse (1); unreadable or over the cap = setup (2).
fn read_state(path: &Path) -> Result<(Vec<u8>, Vec<St>), (i32, String)> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((Vec::new(), Vec::new())),
        Err(_) => return Err((2, "the state file cannot be read".to_string())),
    };
    if !meta.is_file() {
        return Err((2, "the state path is not a file".to_string()));
    }
    let bytes = friction::read_capped(path, STATE_CAP).map_err(|_| (2, "the state file cannot be read".to_string()))?;
    if bytes.len() as u64 > STATE_CAP {
        return Err((2, format!("the state file is over the {STATE_CAP}-byte cap")));
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| (1, "the state file is not utf-8".to_string()))?;
    let mut rows = Vec::new();
    let mut n = 0;
    let mut rest = text;
    while !rest.is_empty() {
        n += 1;
        let Some(i) = rest.find('\n') else {
            return Err((1, format!("the state file has a torn tail at line {n}")));
        };
        rows.push(st_parse(&rest[..i]).map_err(|e| (1, format!("state line {n}: {e}")))?);
        rest = &rest[i + 1..];
    }
    Ok((bytes, rows))
}

/// The writer: take the state file's exclusive lock (creating it only now, after every check passed), re-read the
/// bytes through the locked descriptor and refuse if they differ from what the run decided on, then append.
struct Locked {
    f: fs::File,
}

fn lock_state(path: &Path, orig: &[u8]) -> Result<Locked, (i32, String)> {
    let f = friction::open_locked(path).map_err(|m| (2, m))?;
    let mut cur = Vec::new();
    (&f).take(STATE_CAP + 1).read_to_end(&mut cur).map_err(|_| (2, "the state file cannot be read".to_string()))?;
    if cur != orig {
        return Err((1, "the state file changed while this run was working; nothing was written".to_string()));
    }
    Ok(Locked { f })
}

impl Locked {
    fn append(&mut self, orig_len: usize, rows: &[St]) -> Result<(), (i32, String)> {
        let mut text = String::new();
        for r in rows {
            text.push_str(&st_encode(r));
            text.push('\n');
        }
        if (orig_len + text.len()) as u64 > STATE_CAP {
            return Err((1, format!("the state file would pass the {STATE_CAP}-byte cap")));
        }
        self.f.write_all(text.as_bytes()).map_err(|_| (2, "the state write failed".to_string()))?;
        self.f.sync_all().map_err(|_| (2, "the state sync failed".to_string()))?;
        Ok(())
    }
}

fn last_per_group(rows: &[St]) -> BTreeMap<String, St> {
    let mut m = BTreeMap::new();
    for r in rows {
        m.insert(r.group.clone(), r.clone());
    }
    m
}

// ---------------------------------------------------------------- the closed feed (a small JSON reader)

enum J {
    S(String),
    N(String),
    Other,
    A(Vec<J>),
    M(Vec<(String, J)>),
}

struct JP<'a> {
    b: &'a [u8],
    i: usize,
}

impl JP<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') {
            self.i += 1;
        }
    }

    fn hex4(&mut self) -> Result<u32, ()> {
        let h = self.b.get(self.i..self.i + 4).ok_or(())?;
        let s = std::str::from_utf8(h).map_err(|_| ())?;
        if !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(());
        }
        self.i += 4;
        u32::from_str_radix(s, 16).map_err(|_| ())
    }

    fn string(&mut self) -> Result<String, ()> {
        if self.b.get(self.i) != Some(&b'"') {
            return Err(());
        }
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or(())?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or(())?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            if (0xD800..0xDC00).contains(&hi) && self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                                self.i += 2;
                                let lo = self.hex4()?;
                                if (0xDC00..0xE000).contains(&lo) {
                                    char::from_u32(0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)).unwrap_or('\u{fffd}')
                                } else {
                                    '\u{fffd}'
                                }
                            } else {
                                char::from_u32(hi).unwrap_or('\u{fffd}')
                            }
                        }
                        _ => return Err(()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err(()),
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| ())
    }

    fn value(&mut self, depth: usize) -> Result<J, ()> {
        if depth > JSON_DEPTH {
            return Err(());
        }
        self.ws();
        match *self.b.get(self.i).ok_or(())? {
            b'"' => Ok(J::S(self.string()?)),
            b'{' => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(J::M(out));
                }
                loop {
                    self.ws();
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err(());
                    }
                    self.i += 1;
                    let v = self.value(depth + 1)?;
                    out.push((k, v));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(J::M(out));
                        }
                        _ => return Err(()),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(J::A(out));
                }
                loop {
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::A(out));
                        }
                        _ => return Err(()),
                    }
                }
            }
            b't' | b'f' | b'n' => {
                for lit in ["true", "false", "null"] {
                    if self.b[self.i..].starts_with(lit.as_bytes()) {
                        self.i += lit.len();
                        return Ok(J::Other);
                    }
                }
                Err(())
            }
            b'-' | b'0'..=b'9' => {
                let start = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    self.i += 1;
                }
                Ok(J::N(String::from_utf8_lossy(&self.b[start..self.i]).to_string()))
            }
            _ => Err(()),
        }
    }
}

/// One row of the closed feed (ticket-reconcile's `closed_with_evidence` shape plus an optional `session`).
struct Closed {
    id: String,
    repo: String,
    pr: String,
    at: String,
    session: String,
}

fn scalar(j: Option<&J>) -> Option<String> {
    match j {
        Some(J::S(s)) => Some(s.clone()),
        Some(J::N(n)) => Some(n.clone()),
        _ => None,
    }
}

fn read_closed(path: &Path) -> Result<Vec<Closed>, (i32, String)> {
    let bytes = friction::read_capped(path, CLOSED_CAP).map_err(|_| (2, "the closed feed cannot be read".to_string()))?;
    if bytes.len() as u64 > CLOSED_CAP {
        return Err((2, format!("the closed feed is over the {CLOSED_CAP}-byte cap")));
    }
    let bad = |m: &str| (1, format!("the closed feed is refused: {m}"));
    let mut p = JP { b: &bytes, i: 0 };
    let v = p.value(0).map_err(|()| bad("not valid JSON"))?;
    p.ws();
    if p.i != p.b.len() {
        return Err(bad("trailing bytes"));
    }
    let J::M(top) = v else { return Err(bad("not a JSON object")) };
    let Some(J::A(items)) = top.iter().find(|(k, _)| k == "closed_with_evidence").map(|(_, v)| v) else {
        return Err(bad("no closed_with_evidence array"));
    };
    let mut out = Vec::new();
    for it in items {
        let J::M(m) = it else { return Err(bad("a row is not an object")) };
        let g = |k: &str| m.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        let id = scalar(g("id")).filter(|s| !s.is_empty()).ok_or_else(|| bad("a row has no id"))?;
        let repo = scalar(g("repo")).filter(|s| !s.is_empty()).ok_or_else(|| bad("a row has no repo"))?;
        let pr = scalar(g("pr")).filter(|s| digits_ok(s, 15)).ok_or_else(|| bad("a row has no numeric pr"))?;
        let at = scalar(g("at")).map(|s| s.chars().take(10).collect::<String>()).filter(|s| friction::date_ok(s)).ok_or_else(|| bad("a row has no calendar at"))?;
        let session = match g("session") {
            None => String::new(),
            Some(J::S(s)) if s.is_empty() || token_ok(s) => s.clone(),
            Some(_) => return Err(bad("a row has a session that is not a plain token")),
        };
        if !pr_ref_ok(&format!("{repo}#{pr}")) {
            return Err(bad("a row repo is not a plain repo name"));
        }
        out.push(Closed { id, repo, pr, at, session });
    }
    Ok(out)
}

// ---------------------------------------------------------------- weeks

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The Monday (as a day number) of an ISO week `<yyyy>-w<ww>` (week 1 holds January 4th).
fn week_monday(w: &str) -> i64 {
    let y: i64 = w[0..4].parse().unwrap_or(1970);
    let n: i64 = w[6..8].parse().unwrap_or(1);
    let jan4 = days_from_civil(y, 1, 4);
    let weekday = (jan4 + 3).rem_euclid(7);
    jan4 - weekday + (n - 1) * 7
}

fn age_weeks(proposed: &str, now: &str) -> i64 {
    ((week_monday(now) - week_monday(proposed)) / 7).max(0)
}

// ---------------------------------------------------------------- triage

fn ticket_json(id: &str, g: &Group, rows: &[Row], week: &str) -> String {
    let blocker = g.impact == "blocker";
    let objective = format!("Fix the recurring friction group {} (score {}, occurrences {}); the evidence rows are listed in evidence_links.", g.id, g.score, g.occurrences);
    let ac1 = format!("the friction group {} has no new row for 2 ISO weeks after the merge", g.id);
    let ac2 = "a RED test reproduces the friction before the fix";
    let links: Vec<String> = g.members.iter().map(|&m| friction::jstr(&rows[m].id)).collect();
    let _ = week;
    format!(
        "{{\"id\":{},\"repo_id\":\"bridlework-harness\",\"title\":{},\"group\":\"friction\",\"size\":\"{}\",\"objective\":{},\"acceptance_criteria\":[{},{}],\"dependencies\":[],\"risk\":\"{}\",\"owner_role\":\"friction_learning\",\"evidence_links\":[{}]}}\n",
        friction::jstr(id),
        friction::jstr(&format!("friction {}", g.id)),
        if blocker { "L" } else { "M" },
        friction::jstr(&objective),
        friction::jstr(&ac1),
        friction::jstr(ac2),
        if blocker { "high" } else { "normal" },
        links.join(",")
    )
}

/// `harness friction triage --week <w> --state <file> --handoff-dir <dir> [--log <event-log>] [--closed <file>]
/// [--top K] [--config <file>]`. All checks come before any write; a refused run leaves the state and the
/// hand-off folder byte-identical (the optional `--log` sweep is the existing sweep, itself all-or-nothing).
pub fn triage_cmd(rest: &[String], testmode: bool) -> i32 {
    let flags = match parse_flags(rest, &["--week", "--state", "--handoff-dir", "--log", "--closed", "--top", "--config"], "triage") {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let Some(week) = flag(&flags, "--week") else { return fail("--week is required (never defaulted: no wall clock runs in this verb)") };
    if !friction::week_ok(week) {
        return fail("--week must be an ISO week <iso-year>-w<ww>: four digits, -w, two digits");
    }
    let Some(state_path) = flag(&flags, "--state") else { return fail("--state is required") };
    let Some(handoff) = flag(&flags, "--handoff-dir") else { return fail("--handoff-dir is required") };
    let cfg = match load_cfg(flag(&flags, "--config")) {
        Ok(c) => c,
        Err(c) => return c,
    };
    let top = match flag(&flags, "--top") {
        Some(v) => match parse_top(v) {
            Ok(k) => k,
            Err(c) => return c,
        },
        None => cfg.triage_top() as usize,
    };
    let list = match load_list("an unscanned inbox is never triaged") {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let handoff = Path::new(handoff);
    if !handoff.is_dir() {
        return fail("--handoff-dir must be an existing folder");
    }
    let state_path = Path::new(state_path);
    // Step 3 and 4 inputs are read BEFORE the sweep touches the inbox, so a corrupt state or feed refuses with
    // every file still byte-identical.
    let (orig, st_rows) = match read_state(state_path) {
        Ok(v) => v,
        Err((c, m)) => {
            return if c == 1 { refuse(&m) } else { fail(&m) };
        }
    };
    let closed = match flag(&flags, "--closed") {
        Some(p) => match read_closed(Path::new(p)) {
            Ok(c) => c,
            Err((c, m)) => {
                return if c == 1 { refuse(&m) } else { fail(&m) };
            }
        },
        None => Vec::new(),
    };
    // Step 1: the existing sweep path (the same function `harness friction sweep` runs).
    if let Some(log) = flag(&flags, "--log") {
        let mut a: Vec<String> = vec!["--log".into(), log.into(), "--week".into(), week.into()];
        if let Some(c) = flag(&flags, "--config") {
            a.push("--config".into());
            a.push(c.into());
        }
        let c = friction::sweep_cmd(&a, testmode);
        if c != 0 {
            return c;
        }
    }
    // Step 2: lint + rank.
    let rows = match lint_rows(&cfg, &ctx) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let all = match groups(&rows) {
        Ok(g) => g,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let order = ranked(&all);
    let by_id: HashMap<&str, &Group> = all.iter().map(|g| (g.id.as_str(), g)).collect();
    let mut cur = last_per_group(&st_rows);
    let mut new_rows: Vec<St> = Vec::new();

    // Step 4: resolve a proposed group ONLY by its bound task id (never the hand-off id).
    let mut resolved_now = 0usize;
    for g in cur.keys().cloned().collect::<Vec<_>>() {
        let s = cur[&g].clone();
        if s.status != "proposed" || s.task_id.is_empty() {
            continue;
        }
        let hit = closed.iter().find(|c| c.id == s.task_id && (s.task_session.is_empty() || c.session.is_empty() || c.session == s.task_session));
        if let Some(c) = hit {
            let n = St { status: "resolved".into(), resolved_pr: format!("{}#{}", c.repo, c.pr), resolved_date: c.at.clone(), ..s };
            cur.insert(g, n.clone());
            new_rows.push(n);
            resolved_now += 1;
        }
    }
    // Step 5: a resolved group with a member row dated after its resolved_date regresses; the second
    // regression (attempts >= 2) escalates and is never auto-proposed again.
    for g in cur.keys().cloned().collect::<Vec<_>>() {
        let s = cur[&g].clone();
        if s.status != "resolved" {
            continue;
        }
        let Some(gr) = by_id.get(g.as_str()) else { continue };
        if gr.members.iter().any(|&m| rows[m].date.as_str() > s.resolved_date.as_str()) {
            let attempts = s.attempts + 1;
            let n = St { status: if attempts >= 2 { "escalated".into() } else { "regressed".into() }, attempts, ..s };
            cur.insert(g, n.clone());
            new_rows.push(n);
        }
    }
    // Step 6: proposals. Regressed groups first, then every group broken twice (always), then the top K
    // (taken from the full ranked list BEFORE the already-filed groups are subtracted, so a re-run proposes 0).
    let topk: HashSet<&str> = order.iter().take(top).map(|g| g.id.as_str()).collect();
    let mut props: Vec<(&Group, &'static str)> = Vec::new();
    for g in &order {
        if cur.get(&g.id).is_some_and(|s| s.status == "regressed") {
            props.push((g, "regressed"));
        }
    }
    for g in &order {
        if cur.contains_key(&g.id) {
            continue;
        }
        if g.break_count >= 2 {
            props.push((g, "twice-broken"));
        } else if topk.contains(g.id.as_str()) {
            props.push((g, "rank"));
        }
    }
    // Step 7: the hand-off files, built and checked before any write.
    let first_n = st_rows
        .iter()
        .filter_map(|s| s.ticket_file.strip_prefix(&format!("{week}-")).and_then(|r| r.strip_suffix(".json")).and_then(|n| n.parse::<u32>().ok()))
        .max()
        .unwrap_or(0)
        + 1;
    let mut files: Vec<(std::path::PathBuf, String)> = Vec::new();
    for (k, (g, reason)) in props.iter().enumerate() {
        let next_n = first_n + k as u32;
        let id = format!("friction-{week}-{next_n}");
        let base = format!("{week}-{next_n}.json");
        let json = ticket_json(&id, g, &rows, week);
        if let Err(e) = crate::ticket::Ticket::parse(&json) {
            return refuse(&format!("a hand-off file would not parse as a ticket ({e})"));
        }
        let hits = crate::scan::scan_text(&ctx, "friction.triage", &json);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            return refuse(&format!("a hand-off file has a privacy hit ({}); nothing was written", classes.join(", ")));
        }
        let path = handoff.join(&base);
        if let Ok(existing) = fs::read(&path) {
            if existing != json.as_bytes() {
                return refuse("a hand-off file of that name already exists with other content; nothing was written");
            }
        }
        let prev_attempts = cur.get(&g.id).map(|s| s.attempts).unwrap_or(0);
        let n = St {
            group: g.id.clone(),
            status: "proposed".into(),
            ticket_file: base,
            proposed_week: week.to_string(),
            task_id: String::new(),
            task_session: String::new(),
            resolved_pr: String::new(),
            resolved_date: String::new(),
            attempts: prev_attempts,
        };
        let _ = reason;
        new_rows.push(n);
        files.push((path, json));
    }
    // Step 8: write. The state lock is taken first (and the state re-read through it), then the hand-off files,
    // then the state rows.
    if !new_rows.is_empty() {
        let mut lock = match lock_state(state_path, &orig) {
            Ok(l) => l,
            Err((c, m)) => return if c == 1 { refuse(&m) } else { fail(&m) },
        };
        for (path, json) in &files {
            if fs::write(path, json.as_bytes()).is_err() {
                return fail("a hand-off file cannot be written");
            }
        }
        if let Err((c, m)) = lock.append(orig.len(), &new_rows) {
            return if c == 1 { refuse(&m) } else { fail(&m) };
        }
        for (g, s) in new_rows.iter().map(|s| (s.group.clone(), s.clone())) {
            cur.insert(g, s);
        }
    }
    for ((g, reason), (_, _)) in props.iter().zip(files.iter()) {
        let s = &cur[&g.id];
        println!("propose group={} file={} reason={}", g.id, s.ticket_file, reason);
    }
    let mut open = 0usize;
    for (g, s) in &cur {
        if s.status == "proposed" {
            open += 1;
            println!("open group={} age_weeks={} bound={}", g, age_weeks(&s.proposed_week, week), if s.task_id.is_empty() { "no" } else { "yes" });
        }
    }
    for g in &all {
        if g.root == RootKind::Event && g.members.len() >= 2 {
            println!("signature group={} weeks={}", g.id, g.members.len());
        }
    }
    for (id, n) in friction::pattern_roots(&rows, cfg.pattern_threshold()) {
        println!("pattern id={id} repeats={n}");
    }
    let mut escalated = 0usize;
    for (g, s) in &cur {
        if s.status == "escalated" {
            escalated += 1;
            println!("escalate group={} attempts={}", g, s.attempts);
        }
    }
    println!("triage week={week} proposed={} open={open} resolved={resolved_now} escalated={escalated}", props.len());
    0
}

// ---------------------------------------------------------------- bind

/// `harness friction bind --state <file> --group <id> --task-id <digits> --task-session <token> [--config <file>]`:
/// the one writer of the task binding.
pub fn bind_cmd(rest: &[String], testmode: bool) -> i32 {
    let flags = match parse_flags(rest, &["--state", "--group", "--task-id", "--task-session", "--config"], "bind") {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let (Some(state), Some(group), Some(task_id), Some(task_session)) =
        (flag(&flags, "--state"), flag(&flags, "--group"), flag(&flags, "--task-id"), flag(&flags, "--task-session"))
    else {
        return fail("bind needs --state, --group, --task-id and --task-session");
    };
    if let Some(c) = flag(&flags, "--config") {
        if let Err(c) = load_cfg(Some(c)) {
            return c;
        }
    }
    let list = match load_list("an unscanned binding is never stored") {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    if !digits_ok(task_id, 15) {
        return refuse("--task-id must be all digits");
    }
    if !token_ok(task_session) {
        return refuse("--task-session must be a plain token");
    }
    for (name, v) in [("group", group), ("task-session", task_session)] {
        let hits = crate::scan::scan_text(&ctx, "friction.bind", v);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            return refuse(&format!("field {name} has a privacy hit ({})", classes.join(", ")));
        }
    }
    let state_path = Path::new(state);
    let (orig, rows) = match read_state(state_path) {
        Ok(v) => v,
        Err((c, m)) => return if c == 1 { refuse(&m) } else { fail(&m) },
    };
    let cur = last_per_group(&rows);
    let Some(s) = cur.get(group) else {
        return refuse("the group has no state row");
    };
    if s.status != "proposed" {
        return refuse("the group is not proposed (only a proposed group can be bound)");
    }
    if !s.task_id.is_empty() {
        if s.task_id == task_id && s.task_session == task_session {
            println!("bind already group={group}");
            return 0;
        }
        return refuse("the group is already bound to a different task");
    }
    let n = St { task_id: task_id.to_string(), task_session: task_session.to_string(), ..s.clone() };
    let mut lock = match lock_state(state_path, &orig) {
        Ok(l) => l,
        Err((c, m)) => return if c == 1 { refuse(&m) } else { fail(&m) },
    };
    if let Err((c, m)) = lock.append(orig.len(), &[n]) {
        return if c == 1 { refuse(&m) } else { fail(&m) };
    }
    println!("bind group={group} task={task_id}");
    0
}

// ---------------------------------------------------------------- page

/// One value for a markdown bullet: one line, no link grammar.
fn neutral(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\n' | '\r' | '\t' => ' ',
            '[' => '(',
            ']' => ')',
            '`' => '\'',
            c => c,
        })
        .collect()
}

/// `harness friction page --week <w> --state <file> --out <file> [--config <file>]`: the weekly report page. The
/// lesson text goes ONLY to `--out`; stdout is `page written groups=<n>`.
pub fn page_cmd(rest: &[String], testmode: bool) -> i32 {
    let flags = match parse_flags(rest, &["--week", "--state", "--out", "--config"], "page") {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let Some(week) = flag(&flags, "--week") else { return fail("--week is required") };
    if !friction::week_ok(week) {
        return fail("--week must be an ISO week <iso-year>-w<ww>: four digits, -w, two digits");
    }
    let Some(state) = flag(&flags, "--state") else { return fail("--state is required") };
    let Some(out) = flag(&flags, "--out") else { return fail("--out is required") };
    let cfg = match load_cfg(flag(&flags, "--config")) {
        Ok(c) => c,
        Err(c) => return c,
    };
    if friction::paths_name_one_file(out, cfg.friction_inbox()) || friction::paths_name_one_file(out, state) {
        return fail("--out must not name the friction inbox or the state file");
    }
    let list = match load_list("an unscanned inbox is never paged") {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let rows = match lint_rows(&cfg, &ctx) {
        Ok(r) => r,
        Err(c) => return c,
    };
    let all = match groups(&rows) {
        Ok(g) => g,
        Err(m) => {
            eprintln!("{m}");
            return 1;
        }
    };
    let (_, st_rows) = match read_state(Path::new(state)) {
        Ok(v) => v,
        Err((c, m)) => return if c == 1 { refuse(&m) } else { fail(&m) },
    };
    let cur = last_per_group(&st_rows);
    let mut page = format!("---\nkind: note\nupdated: {}\nsources: []\nverified: no\n---\n# Friction triage {week}\n", friction::today_utc());
    let mut n = 0usize;
    for g in &all {
        let Some(s) = cur.get(&g.id) else { continue };
        if s.status != "proposed" && s.status != "escalated" {
            continue;
        }
        n += 1;
        page.push_str(&format!(
            "\n## {}\n\n- status: {}\n- score: {}\n- occurrences: {}\n- bound: {}\n",
            g.id,
            s.status,
            g.score,
            g.occurrences,
            if s.task_id.is_empty() { "no" } else { "yes" }
        ));
        for &m in &g.members {
            let r = &rows[m];
            let at = if r.at.is_empty() { r.date.as_str() } else { r.at.as_str() };
            let or_dash = |v: &str| if v.is_empty() { "-".to_string() } else { neutral(v) };
            page.push_str(&format!(
                "- {} at: {} impact: {} text: {} why: {} proof: {}\n",
                r.id,
                at,
                if r.impact.is_empty() { "unrated" } else { r.impact.as_str() },
                neutral(&r.text),
                or_dash(&r.why),
                or_dash(&r.proof)
            ));
        }
    }
    if fs::write(Path::new(out), page.as_bytes()).is_err() {
        return fail("the page cannot be written");
    }
    println!("page written groups={n}");
    0
}
