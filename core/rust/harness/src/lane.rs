//! `harness lane claim | release | list` (std only; `docs/lanes.md` is the contract). A lane holds one live ticket. In this slice the cap
//! is one constant, `CAP`. One record file per held lane, `<lanes-dir>/lane-<n>` with n in 1..=CAP. Every claim, release and list runs as one
//! critical section under an exclusive `flock` on the lanes dir itself (no lock file is created), so the read-check-write of two `harness` processes never interleaves. A claim writes the
//! record to a temp file (`.claim.tmp`, default permissions, a stale one is removed under the lock) and renames it onto the lane name, so a
//! reader never sees a half-written record. The lanes dir is read strictly: a record that is not a regular file, cannot be read or parsed, has
//! a name other than `lane-<n>` (n canonical, 1..=CAP), or repeats a ticket, is an error and nothing is changed. No directory fsync is done, so a
//! claim or release is not guaranteed to survive a crash. Release is explicit: a lane is not freed by a dead process.
//! Exit 0 done; 1 policy refusal (one stderr line `lane: refused reason=<token>`); 2 usage error or unreadable or refused input.

use crate::config::Config;
use crate::ticket::Ticket;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// The cap on live tickets in this slice.
pub const CAP: u32 = 1;

const USAGE: &str = "usage: harness lane claim <ticket-file> --lanes-dir <absolute-dir> --config <file> --scope-file <file> | lane release <ticket-id> --lanes-dir <absolute-dir> | lane list --lanes-dir <absolute-dir> --config <file>";
const MAX_FILE: u64 = 1024 * 1024;
const HEADER: &str = "lane-record v1";

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

/// A scope entry: not empty (at most 4096 bytes), not starting with `/` or `\`, no control character, no `..` part (split on `/` and `\`), and
/// no empty part (`a//b` is refused) except one trailing `/`, which marks a folder (`a/` is allowed). Entries are stored as given.
pub fn scope_entry_ok(e: &str) -> Result<(), String> {
    if e.is_empty() || e.len() > 4096 {
        return Err("a scope entry is empty or too long".to_string());
    }
    if e.starts_with('/') || e.starts_with('\\') {
        return Err("a scope entry starts with a slash".to_string());
    }
    if e.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
        return Err("a scope entry holds a control character".to_string());
    }
    if e.split(['/', '\\']).any(|p| p == "..") {
        return Err("a scope entry holds a `..` part".to_string());
    }
    if e.strip_suffix('/').unwrap_or(e).split('/').any(|p| p.is_empty()) {
        return Err("a scope entry holds an empty part".to_string());
    }
    Ok(())
}

/// The scope file: one entry per line, at least one entry. A truly empty line is skipped; a whitespace-only line is refused; an empty file is
/// refused.
pub fn parse_scope(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            continue;
        }
        if line.trim().is_empty() {
            return Err("a scope line holds only whitespace".to_string());
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

/// Is `dir` a real directory? Ok(false) when it is missing and `create` is false. A link or a non-directory is refused. With `create` a missing
/// dir is created (the final component is then checked again).
fn check_dir(dir: &Path, create: bool) -> Result<bool, String> {
    let bad = || "the lanes dir must be a real directory, not a link".to_string();
    match fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() => Ok(true),
        Ok(_) => Err(bad()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !create {
                return Ok(false);
            }
            fs::create_dir_all(dir).map_err(|_| "the lanes dir cannot be created".to_string())?;
            match fs::symlink_metadata(dir) {
                Ok(m) if m.file_type().is_dir() => Ok(true),
                _ => Err(bad()),
            }
        }
        Err(_) => Err("the lanes dir cannot be inspected".to_string()),
    }
}

/// Every held lane, by lane number, read strictly (the caller holds the lock). A missing dir holds nothing. Everything under `lane-` must be a
/// regular file named `lane-<n>` (n canonical digits, 1..=CAP) that reads and parses, and no ticket may repeat; anything else is an error.
pub fn list_records(dir: &Path) -> Result<Vec<Record>, String> {
    list_records_cap(dir, CAP)
}

fn list_records_cap(dir: &Path, cap: u32) -> Result<Vec<Record>, String> {
    if !check_dir(dir, false)? {
        return Ok(Vec::new());
    }
    let rd = fs::read_dir(dir).map_err(|_| "the lanes dir cannot be read".to_string())?;
    let mut out: Vec<Record> = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|_| "the lanes dir cannot be read".to_string())?;
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(num) = name.strip_prefix("lane-") else { continue };
        let canonical = !num.is_empty() && !num.starts_with('0') && num.len() <= 9 && num.bytes().all(|b| b.is_ascii_digit());
        let lane: u32 = if canonical { num.parse().unwrap_or(0) } else { 0 };
        if !(1..=cap).contains(&lane) {
            return Err("a lane record has a name that is not lane-<n> with n from 1 to the cap".to_string());
        }
        let path = ent.path();
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("a lane record is not a regular file".to_string()),
            Err(_) => return Err("a lane record cannot be inspected".to_string()),
        }
        let mut f = fs::File::open(&path).map_err(|_| "a lane record cannot be read".to_string())?;
        if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
            return Err("a lane record is not a regular file".to_string());
        }
        let mut bytes = Vec::new();
        (&mut f).take(MAX_FILE + 1).read_to_end(&mut bytes).map_err(|_| "a lane record cannot be read".to_string())?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("a lane record is over 1 MiB".to_string());
        }
        let text = String::from_utf8(bytes).map_err(|_| "a lane record is not utf-8".to_string())?;
        let rec = decode(lane, &text)?;
        if out.iter().any(|r| r.ticket == rec.ticket) {
            return Err("two lane records hold the same ticket".to_string());
        }
        out.push(rec);
    }
    out.sort_by_key(|r| r.lane);
    Ok(out)
}

/// Take the exclusive `flock` on the lanes dir itself (opened read-only; no lock file is ever created, so a refused call leaves the dir
/// untouched), waiting for it. The caller has just checked that `dir` is a real directory. The lock is held while the File lives.
fn lock(dir: &Path) -> Result<fs::File, String> {
    let f = fs::File::open(dir).map_err(|_| "the lanes dir cannot be opened".to_string())?;
    if !f.metadata().map(|m| m.is_dir()).unwrap_or(false) {
        return Err("the lanes dir must be a real directory, not a link".to_string());
    }
    f.lock().map_err(|_| "the lock cannot be taken".to_string())?;
    Ok(f)
}

/// The live records, read under the lock.
fn list_locked(dir: &Path) -> Result<Vec<Record>, String> {
    if !check_dir(dir, false)? {
        return Ok(Vec::new());
    }
    let _guard = lock(dir)?;
    list_records(dir)
}

/// Take the lowest free lane for `rec` (its `lane` field is ignored), as one critical section. Ok(Err) is a policy refusal.
fn claim(dir: &Path, rec: Record) -> Result<Result<u32, Refusal>, String> {
    check_dir(dir, true)?;
    let _guard = lock(dir)?;
    let live = list_records(dir)?;
    if live.iter().any(|r| r.ticket == rec.ticket) {
        return Ok(Err(Refusal::AlreadyLive));
    }
    if live.len() >= CAP as usize {
        return Ok(Err(Refusal::CapFull));
    }
    let Some(n) = (1..=CAP).find(|n| !live.iter().any(|r| r.lane == *n)) else { return Ok(Err(Refusal::CapFull)) };
    let tmp = dir.join(".claim.tmp");
    match fs::symlink_metadata(&tmp) {
        Ok(_) => fs::remove_file(&tmp).map_err(|_| "a stale temp file cannot be removed".to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("the temp file cannot be inspected".to_string()),
    }
    let write = (|| -> std::io::Result<()> {
        let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        f.write_all(encode(&rec).as_bytes())?;
        f.sync_all()
    })();
    if write.is_err() {
        let _ = fs::remove_file(&tmp);
        return Err("the lane record cannot be written".to_string());
    }
    if fs::rename(&tmp, lane_path(dir, n)).is_err() {
        let _ = fs::remove_file(&tmp);
        return Err("the lane record cannot be published".to_string());
    }
    Ok(Ok(n))
}

/// Free the lane of `ticket`, as one critical section.
fn release(dir: &Path, ticket: &str) -> Result<Result<(), Refusal>, String> {
    if !check_dir(dir, false)? {
        return Ok(Err(Refusal::NotLive));
    }
    let _guard = lock(dir)?;
    let Some(r) = list_records(dir)?.into_iter().find(|r| r.ticket == ticket) else { return Ok(Err(Refusal::NotLive)) };
    fs::remove_file(lane_path(dir, r.lane)).map_err(|_| "the lane record cannot be removed".to_string())?;
    Ok(Ok(()))
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
            let recs = match list_locked(&dir) {
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
        for bad in ["", "\n", "/abs\n", "a/../b\n", "..\n", "a\0b\n", "a//b\n", " \n", "//\n"] {
            assert!(parse_scope(bad).is_err(), "{bad:?}");
        }
        assert_eq!(parse_scope("a/\n./b\nc:d\n\nx:y\n").unwrap().len(), 4);
    }

    #[test]
    fn a_ticket_may_not_hold_two_records() {
        let d = std::env::temp_dir().join(format!("lane-unit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let body = |t: &str| format!("{HEADER}\nticket={t}\nrepo_id=r\nscope=a\n");
        fs::write(d.join("lane-1"), body("t-x")).unwrap();
        fs::write(d.join("lane-2"), body("t-y")).unwrap();
        assert_eq!(list_records_cap(&d, 2).unwrap().len(), 2);
        assert!(list_records_cap(&d, 1).is_err());
        fs::write(d.join("lane-2"), body("t-x")).unwrap();
        assert!(list_records_cap(&d, 2).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_record_round_trips() {
        let r = Record { lane: 1, ticket: "t-1".into(), repo_id: "r".into(), scope: vec!["a".into(), "b/c".into()] };
        assert_eq!(decode(1, &encode(&r)).unwrap(), r);
        assert!(decode(1, "lane-record v1\nticket=t-1\n").is_err());
    }
}
