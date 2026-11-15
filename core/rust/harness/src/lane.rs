//! `harness lane claim | release | list` (std only; `docs/lanes.md` is the contract). A lane holds one live ticket. In this slice the cap
//! is a fixed 1 (no admission exists yet). One record file per held lane, `<lanes-dir>/lane-<n>`, written whole to a private temp file and
//! then published with a hard link, which is atomic and fails when the lane is taken: two `harness` processes racing for a lane can
//! never both win it, and a reader never sees a half-written record. Release is explicit: a lane is not freed by a dead process.
//! Exit 0 done; 1 policy refusal (one stderr line `lane: refused reason=<token>`); 2 usage error or unreadable or refused input.

use crate::config::Config;
use crate::ticket::Ticket;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

/// The cap on live tickets in this slice.
pub const CAP: u32 = 1;

const USAGE: &str = "usage: harness lane claim <ticket-file> --lanes-dir <absolute-dir> --config <file> --scope-file <file> | lane release <ticket-id> --lanes-dir <absolute-dir> | lane list --lanes-dir <absolute-dir> --config <file>";
const MAX_FILE: u64 = 1024 * 1024;
const HEADER: &str = "lane-record v1";
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// One held lane, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub lane: u32,
    pub ticket: String,
    pub repo_id: String,
    pub scope: Vec<String>,
}

enum Refusal {
    CapFull,
    AlreadyLive,
    NotLive,
}

impl Refusal {
    fn token(&self) -> &'static str {
        match self {
            Refusal::CapFull => "cap-full",
            Refusal::AlreadyLive => "already-live",
            Refusal::NotLive => "not-live",
        }
    }
}

/// A ticket id that may be stored: plain characters only, so it is one safe record line.
pub fn ticket_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && !id.starts_with('.') && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A scope entry: a relative path with no `..` part, no empty part, no control character.
pub fn scope_entry_ok(e: &str) -> Result<(), String> {
    if e.is_empty() || e.len() > 4096 {
        return Err("a scope entry is empty or too long".to_string());
    }
    if e.starts_with('/') || e.starts_with('\\') || e.as_bytes().get(1) == Some(&b':') {
        return Err("a scope entry is absolute".to_string());
    }
    if e.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
        return Err("a scope entry holds a control character".to_string());
    }
    if e.split(['/', '\\']).any(|p| p == "..") {
        return Err("a scope entry holds a `..` part".to_string());
    }
    Ok(())
}

/// The scope file: one entry per line, at least one entry. A blank line is skipped; an empty file is refused.
pub fn parse_scope(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        scope_entry_ok(line)?;
        if !out.iter().any(|x| x == line) {
            out.push(line.to_string());
        }
    }
    if out.is_empty() {
        return Err("the scope file holds no entry".to_string());
    }
    Ok(out)
}

fn encode(r: &Record) -> String {
    let mut s = format!("{HEADER}\nticket={}\nrepo_id={}\n", r.ticket, r.repo_id);
    for e in &r.scope {
        s.push_str(&format!("scope={e}\n"));
    }
    s
}

fn decode(lane: u32, text: &str) -> Result<Record, String> {
    let body = text.strip_suffix('\n').ok_or("a lane record is cut short")?;
    let mut lines = body.split('\n');
    if lines.next() != Some(HEADER) {
        return Err("a lane record has an unknown header".to_string());
    }
    let ticket = lines.next().and_then(|l| l.strip_prefix("ticket=")).filter(|t| ticket_id_ok(t)).ok_or("a lane record has no valid ticket line")?;
    let repo_id = lines.next().and_then(|l| l.strip_prefix("repo_id=")).ok_or("a lane record has no repo_id line")?;
    let mut scope = Vec::new();
    for l in lines {
        let e = l.strip_prefix("scope=").ok_or("a lane record has an unknown line")?;
        scope_entry_ok(e)?;
        scope.push(e.to_string());
    }
    if scope.is_empty() {
        return Err("a lane record has no scope entry".to_string());
    }
    Ok(Record { lane, ticket: ticket.to_string(), repo_id: repo_id.to_string(), scope })
}

fn lane_path(dir: &Path, n: u32) -> PathBuf {
    dir.join(format!("lane-{n}"))
}

fn read_small(p: &Path) -> std::io::Result<Vec<u8>> {
    let mut b = Vec::new();
    fs::File::open(p)?.take(MAX_FILE + 1).read_to_end(&mut b)?;
    Ok(b)
}

/// Every held lane, by lane number. A missing dir holds nothing. A record that cannot be read or does not parse is an error (fail closed).
pub fn list_records(dir: &Path) -> Result<Vec<Record>, String> {
    let rd = match fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err("the lanes dir cannot be read".to_string()),
    };
    let mut out = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|_| "the lanes dir cannot be read".to_string())?;
        let name = ent.file_name();
        let Some(num) = name.to_str().and_then(|n| n.strip_prefix("lane-")) else { continue };
        if num.is_empty() || !num.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let lane: u32 = num.parse().map_err(|_| "a lane record has a bad lane number".to_string())?;
        let bytes = match read_small(&ent.path()) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue, // released while listing
            Err(_) => return Err("a lane record cannot be read".to_string()),
        };
        if bytes.len() as u64 > MAX_FILE {
            return Err("a lane record is over 1 MiB".to_string());
        }
        let text = String::from_utf8(bytes).map_err(|_| "a lane record is not utf-8".to_string())?;
        out.push(decode(lane, &text)?);
    }
    out.sort_by_key(|r| r.lane);
    Ok(out)
}

fn temp_name() -> String {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
    format!(".tmp-{}-{}-{}", std::process::id(), t, SEQ.fetch_add(1, Ordering::SeqCst))
}

/// Take the lowest free lane for `rec` (its `lane` field is ignored). Ok(Err) is a policy refusal.
fn claim(dir: &Path, mut rec: Record) -> Result<Result<u32, Refusal>, String> {
    fs::create_dir_all(dir).map_err(|_| "the lanes dir cannot be created".to_string())?;
    if list_records(dir)?.iter().any(|r| r.ticket == rec.ticket) {
        return Ok(Err(Refusal::AlreadyLive));
    }
    let tmp = dir.join(temp_name());
    let write = (|| -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(encode(&rec).as_bytes())?;
        f.sync_all()
    })();
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
        return Err("the lane record cannot be written".to_string());
    }
    let mut result = Ok(Err(Refusal::CapFull));
    for n in 1..=CAP {
        match fs::hard_link(&tmp, lane_path(dir, n)) {
            Ok(()) => {
                rec.lane = n;
                result = Ok(Ok(n));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => {
                result = Err("the lane record cannot be published".to_string());
                break;
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    result
}

fn release(dir: &Path, ticket: &str) -> Result<Result<(), Refusal>, String> {
    let Some(r) = list_records(dir)?.into_iter().find(|r| r.ticket == ticket) else { return Ok(Err(Refusal::NotLive)) };
    match fs::remove_file(lane_path(dir, r.lane)) {
        Ok(()) => Ok(Ok(())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Err(Refusal::NotLive)),
        Err(_) => Err("the lane record cannot be removed".to_string()),
    }
}

fn parse_args(rest: &[String], allowed: &[&str]) -> Result<(Option<String>, std::collections::HashMap<String, String>), String> {
    let mut pos: Option<String> = None;
    let mut m = std::collections::HashMap::new();
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        if let Some(k) = a.strip_prefix("--") {
            if !allowed.contains(&k) {
                return Err(format!("unknown option {a}"));
            }
            let v = it.next().ok_or_else(|| format!("{a} is missing its value"))?;
            if m.insert(k.to_string(), v.clone()).is_some() {
                return Err(format!("{a} was given twice"));
            }
        } else if pos.replace(a.clone()).is_some() {
            return Err("more than one positional argument given".to_string());
        }
    }
    Ok((pos, m))
}

fn lanes_dir(m: &std::collections::HashMap<String, String>) -> Result<PathBuf, String> {
    let d = m.get("lanes-dir").ok_or("--lanes-dir is required")?;
    let p = PathBuf::from(d);
    if !p.is_absolute() {
        return Err("--lanes-dir must be an absolute path".to_string());
    }
    Ok(p)
}

fn load_config(m: &std::collections::HashMap<String, String>) -> Result<Config, String> {
    let c = m.get("config").ok_or("--config is required")?;
    if c.is_empty() {
        return Err("the config source is empty".to_string());
    }
    Config::load(Path::new(c))
}

fn refused(r: Refusal) -> i32 {
    eprintln!("lane: refused reason={}", r.token());
    1
}

fn fail(m: &str) -> i32 {
    eprintln!("lane: error: {m}");
    2
}

/// Entry point for `harness lane ...`.
pub fn lane_cmd(a: &[String]) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{USAGE}");
        return 2;
    };
    let run = |allowed: &[&str]| parse_args(rest, allowed);
    match sub.as_str() {
        "claim" => {
            let (pos, m) = match run(&["lanes-dir", "config", "scope-file"]) {
                Ok(v) => v,
                Err(e) => return fail(&e),
            };
            let Some(file) = pos else {
                eprintln!("{USAGE}");
                return 2;
            };
            // Every input is checked before the lanes dir is touched, so a refused input leaves nothing behind.
            let dir = match lanes_dir(&m) {
                Ok(d) => d,
                Err(e) => return fail(&e),
            };
            let bytes = match read_small(Path::new(&file)) {
                Ok(b) => b,
                Err(_) => return fail("the ticket file cannot be read"),
            };
            let ticket = match Ticket::parse_bytes(&bytes) {
                Ok(t) => t,
                Err(e) => return fail(&format!("ticket refused: {e}")),
            };
            if !ticket_id_ok(&ticket.id) {
                return fail("ticket refused: `id` must be plain characters (letters, digits, '.', '_', '-'; at most 128; not starting with '.')");
            }
            if let Err(e) = load_config(&m) {
                return fail(&e);
            }
            let Some(sf) = m.get("scope-file") else { return fail("--scope-file is required") };
            let scope = match read_small(Path::new(sf)) {
                Ok(b) if b.len() as u64 > MAX_FILE => return fail("the scope file is over 1 MiB"),
                Ok(b) => match String::from_utf8(b) {
                    Ok(t) => match parse_scope(&t) {
                        Ok(s) => s,
                        Err(e) => return fail(&e),
                    },
                    Err(_) => return fail("the scope file is not utf-8"),
                },
                Err(_) => return fail("the scope file cannot be read"),
            };
            let rec = Record { lane: 0, ticket: ticket.id.clone(), repo_id: ticket.repo_id.as_str().to_string(), scope };
            match claim(&dir, rec) {
                Ok(Ok(n)) => {
                    println!("ticket={} lane={}", ticket.id, n);
                    0
                }
                Ok(Err(r)) => refused(r),
                Err(e) => fail(&e),
            }
        }
        "release" => {
            let (pos, m) = match run(&["lanes-dir"]) {
                Ok(v) => v,
                Err(e) => return fail(&e),
            };
            let Some(id) = pos else {
                eprintln!("{USAGE}");
                return 2;
            };
            if !ticket_id_ok(&id) {
                return fail("the ticket id must be plain characters (letters, digits, '.', '_', '-')");
            }
            let dir = match lanes_dir(&m) {
                Ok(d) => d,
                Err(e) => return fail(&e),
            };
            match release(&dir, &id) {
                Ok(Ok(())) => {
                    println!("ticket={id} released");
                    0
                }
                Ok(Err(r)) => refused(r),
                Err(e) => fail(&e),
            }
        }
        "list" => {
            let (pos, m) = match run(&["lanes-dir", "config"]) {
                Ok(v) => v,
                Err(e) => return fail(&e),
            };
            if pos.is_some() {
                return fail("lane list takes no positional argument");
            }
            let dir = match lanes_dir(&m) {
                Ok(d) => d,
                Err(e) => return fail(&e),
            };
            if let Err(e) = load_config(&m) {
                return fail(&e);
            }
            let recs = match list_records(&dir) {
                Ok(r) => r,
                Err(e) => return fail(&e),
            };
            for r in &recs {
                println!("ticket={} lane={} repo_id={} scope={}", r.ticket, r.lane, r.repo_id, r.scope.len());
            }
            println!("live={} cap={}", recs.len(), CAP);
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_entries_fail_closed() {
        assert!(parse_scope("src/a.rs\ndocs/b.md\n").is_ok());
        for bad in ["", "\n", "/abs\n", "a/../b\n", "..\n", "a\0b\n"] {
            assert!(parse_scope(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_record_round_trips() {
        let r = Record { lane: 1, ticket: "t-1".into(), repo_id: "r".into(), scope: vec!["a".into(), "b/c".into()] };
        assert_eq!(decode(1, &encode(&r)).unwrap(), r);
        assert!(decode(1, "lane-record v1\nticket=t-1\n").is_err());
    }
}
