//! `harness lane claim | release | list | admission-check` (std only; `docs/lanes.md` is the contract). A lane holds one live ticket. The cap
//! on live tickets is 1 without a valid admission record and the smaller of the record's admitted lane count and the config `lanes.count` with
//! one, so no constant is edited to raise the cap. One record file per held lane, `<lanes-dir>/lane-<n>` with n in 1..=the config `lanes.count`
//! (the legal names follow the config, not the admission). Every claim, release and list runs as one critical section under an exclusive `flock`
//! on the lanes dir itself (no lock file is created), so the read-check-write of two `harness` processes never interleaves. A claim writes the
//! record to a temp file (`.claim.tmp`, default permissions, one left by a killed claim is removed under the lock by the next claim that takes a
//! lane) and renames it onto the lane name, so a reader never sees a half-written record. The lanes dir is read strictly: a record that is not a
//! regular file, cannot be read or parsed, has a name other than `lane-<n>` (n canonical, 1..=the config `lanes.count`), or repeats a ticket, is
//! an error and nothing is changed. The admission record is read fail closed too: one that exists and is not valid is a `bad-admission` refusal
//! checked before the lanes dir is touched, while a path where nothing exists means no admission (cap 1). No directory fsync is done, so a claim
//! or release is not guaranteed to survive a crash. Release is explicit: a lane is not freed by a dead process.
//! Exit 0 done; 1 policy refusal (one stderr line `lane: refused reason=<token>`, and `bad-admission` also names the field when the problem is a
//! field); 2 usage error, unreadable or refused input, or a lanes dir in a bad state.

use crate::config::Config;
use crate::ticket::Ticket;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: harness lane claim <ticket-file> --lanes-dir <absolute-dir> --config <file> --scope-file <file> [--admission <file>] | lane release <ticket-id> --lanes-dir <absolute-dir> --config <file> | lane list --lanes-dir <absolute-dir> --config <file> [--admission <file>] | lane admission-check <file> --config <file>";
const MAX_FILE: u64 = 1024 * 1024;
const HEADER: &str = "lane-record v1";

/// The cap on live tickets without a valid admission record: the first lane needs no admission.
const NO_ADMISSION_CAP: u32 = 1;
/// The header line of an admission record.
const ADM_HEADER: &str = "lane-admission v1";
/// The size limit the code sets for an admission record.
const ADM_MAX_FILE: u64 = 64 * 1024;
/// Every field of an admission record after the header, in the order `docs/lanes.md` states them.
const ADM_FIELDS: [&str; 8] = ["lanes", "baseline", "step2", "reviewer", "verdict", "operator", "l3", "signed"];

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
    /// A claim for a second ticket with no valid admission (no `--admission`, or a path where nothing exists).
    NoAdmission,
    /// An admission file that exists and is not valid; the string names what is wrong (the field when it is a field).
    Admission(String),
}

impl Refusal {
    fn token(&self) -> &'static str {
        match self {
            Refusal::CapFull => "cap-full",
            Refusal::AlreadyLive => "already-live",
            Refusal::NotLive => "not-live",
            Refusal::NoAdmission => "no-admission",
            Refusal::Admission(_) => "bad-admission",
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

/// The upper bound of a legal lane number: the config `lanes.count`, clamped to the largest number a record name can spell (nine digits), so
/// the count itself is the bound for every real config.
fn name_cap(cfg: &Config) -> u32 {
    cfg.lane_count().min(999_999_999) as u32
}

/// The cap a claim runs under: 1 with no admitted count, else the smaller of the admitted count and the legal name bound.
fn claim_cap(admitted: Option<u32>, cfg: &Config) -> u32 {
    admitted.map_or(NO_ADMISSION_CAP, |n| n.min(name_cap(cfg)))
}

/// A `bad-admission` detail that names the field.
fn field_bad(field: &str, why: &str) -> String {
    format!("field={field} {why}")
}

/// A real calendar date spelled YYYY-MM-DD (four digits, a dash, two digits for the month, a dash, two digits for the day).
fn date_ok(v: &str) -> bool {
    let b = v.as_bytes();
    let shape = b.len() == 10
        && b[..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[5..7].iter().all(|c| c.is_ascii_digit())
        && b[7] == b'-'
        && b[8..10].iter().all(|c| c.is_ascii_digit());
    if !shape {
        return false;
    }
    let y: u64 = v[..4].parse().unwrap();
    let m: u64 = v[5..7].parse().unwrap();
    let d: u64 = v[8..10].parse().unwrap();
    let leap = y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400));
    let days = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    (1..=12).contains(&m) && d >= 1 && d <= days[(m - 1) as usize]
}

/// The shape of one admission field, as `docs/lanes.md` states it. `Err` names the field.
fn admission_field_ok(f: &str, v: &str) -> Result<(), String> {
    let plain_text = |why: &str| {
        if v.is_empty() {
            Err(field_bad(f, "is empty"))
        } else if v.len() > 256 || v.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
            Err(field_bad(f, why))
        } else {
            Ok(())
        }
    };
    match f {
        "lanes" => {
            let digits = !v.is_empty() && !v.starts_with('0') && v.len() <= 9 && v.bytes().all(|b| b.is_ascii_digit());
            match if digits { v.parse::<u32>().ok() } else { None } {
                Some(n) if n >= 2 => Ok(()),
                _ => Err(field_bad(f, "is not a whole number of at least 2")),
            }
        }
        "baseline" | "operator" => {
            if v.is_empty() {
                Err(field_bad(f, "is empty"))
            } else if v.len() > 256 || v.bytes().any(|b| !b.is_ascii_graphic()) {
                Err(field_bad(f, "is not one plain token of printable characters (no whitespace, no control character)"))
            } else {
                Ok(())
            }
        }
        "step2" | "verdict" | "l3" => plain_text("is not plain text of at most 256 characters"),
        "reviewer" => {
            if v.is_empty() {
                Err(field_bad(f, "is empty"))
            } else if v.len() > 128 || !v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')) {
                Err(field_bad(f, "is not a plain token (letters, digits, '.', '_', '-')"))
            } else {
                Ok(())
            }
        }
        "signed" => {
            if date_ok(v) {
                Ok(())
            } else {
                Err(field_bad(f, "is not a real date spelled YYYY-MM-DD"))
            }
        }
        _ => unreachable!("every field is matched"),
    }
}

/// Parse an admission record and return its admitted lane count. `Err` is the `bad-admission` detail: it names the field when the problem is a
/// field. Header first, then `field=value` lines in any order, each field exactly once.
fn parse_admission(text: &str) -> Result<u32, String> {
    let body = text.strip_suffix('\n').ok_or("the admission record is cut short")?;
    let mut lines = body.split('\n');
    if lines.next() != Some(ADM_HEADER) {
        return Err("the admission record has an unknown header".to_string());
    }
    let mut seen: Vec<(&str, &str)> = Vec::new();
    for l in lines {
        let (k, v) = l.split_once('=').ok_or("an unknown line".to_string())?;
        if !ADM_FIELDS.contains(&k) {
            return Err("an unknown line".to_string());
        }
        if seen.iter().any(|(k2, _)| *k2 == k) {
            return Err(field_bad(k, "is repeated"));
        }
        seen.push((k, v));
    }
    for f in ADM_FIELDS {
        let v = seen.iter().find(|(k, _)| *k == f).map(|(_, v)| *v).ok_or_else(|| field_bad(f, "is missing"))?;
        admission_field_ok(f, v)?;
    }
    let lanes: u32 = seen.iter().find(|(k, _)| *k == "lanes").map(|(_, v)| *v).unwrap().parse().unwrap();
    Ok(lanes)
}

/// What reading an admission path gave: nothing there (`Missing`, so the cap stays 1), a file that is there and not valid (`Bad` carries the
/// detail), or the admitted lane count. `Err` is an exit-2 problem (the path cannot be inspected or read).
enum AdmissionRead {
    Missing,
    Bad(String),
    Valid(u32),
}

/// Read one admission path. A regular file only (a symbolic link too is `Bad`), at most 64 KiB, utf-8, parsed strictly.
fn read_admission(p: &Path) -> Result<AdmissionRead, String> {
    match fs::symlink_metadata(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AdmissionRead::Missing),
        Err(_) => return Err("the admission file cannot be inspected".to_string()),
        Ok(m) if !m.file_type().is_file() => return Ok(AdmissionRead::Bad("the admission file is not a regular file".to_string())),
        Ok(_) => {}
    }
    let mut f = fs::File::open(p).map_err(|_| "the admission file cannot be read".to_string())?;
    // Re-check through the open handle, so a path swapped for a link between the two calls is still caught.
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Ok(AdmissionRead::Bad("the admission file is not a regular file".to_string()));
    }
    let mut bytes = Vec::new();
    (&mut f).take(ADM_MAX_FILE + 1).read_to_end(&mut bytes).map_err(|_| "the admission file cannot be read".to_string())?;
    if bytes.len() as u64 > ADM_MAX_FILE {
        return Ok(AdmissionRead::Bad(format!("the admission file is over {ADM_MAX_FILE} bytes")));
    }
    let text = match String::from_utf8(bytes) {
        Ok(t) => t,
        Err(_) => return Ok(AdmissionRead::Bad("the admission file is not utf-8".to_string())),
    };
    match parse_admission(&text) {
        Ok(n) => Ok(AdmissionRead::Valid(n)),
        Err(d) => Ok(AdmissionRead::Bad(d)),
    }
}

/// Resolve `--admission <path>` to the admitted lane count: `None` when the cap stays 1 (no option, or nothing at the path), `Some(n)` for a
/// valid record. A file that is there and not valid is the `bad-admission` refusal, returned as `Err`.
fn admitted_lanes(m: &std::collections::HashMap<String, String>) -> Result<Result<Option<u32>, Refusal>, String> {
    match m.get("admission") {
        None => Ok(Ok(None)),
        Some(p) => match read_admission(Path::new(p)) {
            Err(e) => Err(e),
            Ok(AdmissionRead::Missing) => Ok(Ok(None)),
            Ok(AdmissionRead::Bad(d)) => Ok(Err(Refusal::Admission(d))),
            Ok(AdmissionRead::Valid(n)) => Ok(Ok(Some(n))),
        },
    }
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
/// regular file named `lane-<n>` (n canonical digits, 1..=the config `lanes.count`) that reads and parses, and no ticket may repeat; anything
/// else is an error.
pub fn list_records(dir: &Path, cap: u32) -> Result<Vec<Record>, String> {
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
            return Err("a lane record has a name that is not lane-<n> with n from 1 to the config lanes.count".to_string());
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
fn list_locked(dir: &Path, cap: u32) -> Result<Vec<Record>, String> {
    if !check_dir(dir, false)? {
        return Ok(Vec::new());
    }
    let _guard = lock(dir)?;
    list_records(dir, cap)
}

/// Take the lowest free lane under `cap` for `rec` (its `lane` field is ignored), as one critical section, unless as many tickets are live as the cap. The dir is read under `bound`, the
/// config name bound (a legal record name may sit above the claim cap, because the names follow the config, not the admission). `admitted`
/// tells whether the cap comes from a valid admission record, which picks the refusal token when no lane is free. Ok(Err) is a policy refusal.
fn claim(dir: &Path, rec: Record, cap: u32, bound: u32, admitted: bool) -> Result<Result<u32, Refusal>, String> {
    check_dir(dir, true)?;
    let _guard = lock(dir)?;
    let live = list_records(dir, bound)?;
    if live.iter().any(|r| r.ticket == rec.ticket) {
        return Ok(Err(Refusal::AlreadyLive));
    }
    // The cap bounds the number of live tickets, wherever their lanes sit (a record may hold a lane above the cap, e.g. after the cap was lowered).
    let free = if live.len() as u64 >= cap as u64 { None } else { (1..=cap).find(|n| !live.iter().any(|r| r.lane == *n)) };
    let Some(n) = free else {
        return Ok(Err(if admitted { Refusal::CapFull } else { Refusal::NoAdmission }));
    };
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

/// Free the lane of `ticket`, as one critical section, reading the dir under the config name bound.
fn release(dir: &Path, ticket: &str, cap: u32) -> Result<Result<(), Refusal>, String> {
    if !check_dir(dir, false)? {
        return Ok(Err(Refusal::NotLive));
    }
    let _guard = lock(dir)?;
    let Some(r) = list_records(dir, cap)?.into_iter().find(|r| r.ticket == ticket) else { return Ok(Err(Refusal::NotLive)) };
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
    match r {
        Refusal::Admission(detail) => eprintln!("lane: refused reason=bad-admission {detail}"),
        other => eprintln!("lane: refused reason={}", other.token()),
    }
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
            let (pos, m) = match run(&["lanes-dir", "config", "scope-file", "admission"]) {
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
            let cfg = match load_config(&m) {
                Ok(c) => c,
                Err(e) => return fail(&e),
            };
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
            // The admission is an input like the ticket: a file that is there and not valid refuses the claim before the lanes dir is touched,
            // even when no ticket is live; a path where nothing exists means no admission (cap 1).
            let admitted = match admitted_lanes(&m) {
                Ok(v) => v,
                Err(e) => return fail(&e),
            };
            let rec = Record { lane: 0, ticket: ticket.id.clone(), repo_id: ticket.repo_id.as_str().to_string(), scope };
            match admitted {
                Err(r) => refused(r),
                Ok(None) => match claim(&dir, rec, claim_cap(None, &cfg), name_cap(&cfg), false) {
                    Ok(Ok(n)) => {
                        println!("ticket={} lane={}", ticket.id, n);
                        0
                    }
                    Ok(Err(r)) => refused(r),
                    Err(e) => fail(&e),
                },
                Ok(Some(n)) => match claim(&dir, rec, claim_cap(Some(n), &cfg), name_cap(&cfg), true) {
                    Ok(Ok(n2)) => {
                        println!("ticket={} lane={}", ticket.id, n2);
                        0
                    }
                    Ok(Err(r)) => refused(r),
                    Err(e) => fail(&e),
                },
            }
        }
        "release" => {
            let (pos, m) = match run(&["lanes-dir", "config"]) {
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
            let cfg = match load_config(&m) {
                Ok(c) => c,
                Err(e) => return fail(&e),
            };
            match release(&dir, &id, name_cap(&cfg)) {
                Ok(Ok(())) => {
                    println!("ticket={id} released");
                    0
                }
                Ok(Err(r)) => refused(r),
                Err(e) => fail(&e),
            }
        }
        "list" => {
            let (pos, m) = match run(&["lanes-dir", "config", "admission"]) {
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
            let cfg = match load_config(&m) {
                Ok(c) => c,
                Err(e) => return fail(&e),
            };
            let cap = match admitted_lanes(&m) {
                Ok(Ok(n)) => claim_cap(n, &cfg),
                Ok(Err(r)) => return refused(r),
                Err(e) => return fail(&e),
            };
            let recs = match list_locked(&dir, name_cap(&cfg)) {
                Ok(r) => r,
                Err(e) => return fail(&e),
            };
            for r in &recs {
                println!("ticket={} lane={} repo_id={} scope={}", r.ticket, r.lane, r.repo_id, r.scope.len());
            }
            println!("live={} cap={}", recs.len(), cap);
            0
        }
        "admission-check" => {
            let (pos, m) = match run(&["config"]) {
                Ok(v) => v,
                Err(e) => return fail(&e),
            };
            let Some(file) = pos else {
                eprintln!("{USAGE}");
                return 2;
            };
            let cfg = match load_config(&m) {
                Ok(c) => c,
                Err(e) => return fail(&e),
            };
            match read_admission(Path::new(&file)) {
                Err(e) => fail(&e),
                Ok(AdmissionRead::Missing) => fail("the admission file does not exist"),
                Ok(AdmissionRead::Bad(d)) => refused(Refusal::Admission(d)),
                Ok(AdmissionRead::Valid(n)) => {
                    println!("admission ok lanes={n} cap={}", claim_cap(Some(n), &cfg));
                    0
                }
            }
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
        assert_eq!(list_records(&d, 2).unwrap().len(), 2);
        assert!(list_records(&d, 1).is_err());
        fs::write(d.join("lane-2"), body("t-x")).unwrap();
        assert!(list_records(&d, 2).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_record_round_trips() {
        let r = Record { lane: 1, ticket: "t-1".into(), repo_id: "r".into(), scope: vec!["a".into(), "b/c".into()] };
        assert_eq!(decode(1, &encode(&r)).unwrap(), r);
        assert!(decode(1, "lane-record v1\nticket=t-1\n").is_err());
    }

    /// A valid twin of the admission record, for the field-by-field refusals below.
    fn good_admission() -> String {
        "lane-admission v1\nlanes=2\nbaseline=main@0f1e2d3\nstep2=p1a 5 p4a 10 d2a 20\nreviewer=rev-one\nverdict=approve-two-lanes\noperator=go-2026-11-14b\nl3=pass\nsigned=2026-11-14\n".to_string()
    }

    #[test]
    fn an_admission_round_trips_and_fields_in_any_order() {
        assert_eq!(parse_admission(&good_admission()).unwrap(), 2);
        // the fields may come in any order
        let mut fields = good_lines();
        fields.remove(0);
        fields.reverse();
        let text = format!("lane-admission v1\n{}\n", fields.join("\n"));
        assert_eq!(parse_admission(&text).unwrap(), 2);
    }

    /// The lines of the valid twin, owned, so a test may edit them freely.
    fn good_lines() -> Vec<String> {
        good_admission().lines().map(|l| l.to_string()).collect()
    }

    fn join_lines(v: Vec<String>) -> String {
        format!("{}\n", v.join("\n"))
    }

    #[test]
    fn every_admission_field_problem_names_the_field() {
        // one field, one broken line: the whole `field=...` line is replaced
        let rewrite = |f: &str, line: &str| -> String {
            let prefix = format!("{f}=");
            join_lines(good_lines().into_iter().map(|l| if l.starts_with(&prefix) { line.to_string() } else { l }).collect())
        };
        for f in ADM_FIELDS {
            let prefix = format!("{f}=");
            // missing
            let dropped: Vec<String> = good_lines().into_iter().filter(|l| !l.starts_with(&prefix)).collect();
            let e = parse_admission(&join_lines(dropped)).unwrap_err();
            assert!(e.contains(f), "{f}: {e}");
            // empty
            let e = parse_admission(&rewrite(f, &prefix)).unwrap_err();
            assert!(e.contains(f), "{f}: {e}");
            // repeated
            let line = good_lines().into_iter().find(|l| l.starts_with(&prefix)).unwrap();
            let mut twice = good_lines();
            twice.push(line);
            let e = parse_admission(&join_lines(twice)).unwrap_err();
            assert!(e.contains(f) && e.contains("repeated"), "{f}: {e}");
        }
        // out of range: 1 lane, a leading zero, a non-number, a bad date, a reviewer with a space, a baseline with a space
        for (f, bad) in [("lanes", "lanes=1"), ("lanes", "lanes=03"), ("lanes", "lanes=two"), ("signed", "signed=2026-02-30"), ("signed", "signed=2026-13-01"), ("signed", "signed=not-a-date"), ("reviewer", "reviewer=two words"), ("baseline", "baseline=has space"), ("operator", "operator=")] {
            let e = parse_admission(&rewrite(f, bad)).unwrap_err();
            assert!(e.contains(f), "{bad}: {e}");
        }
        // a wrong header and an unknown line are bad too
        assert!(parse_admission(&good_admission().replacen(ADM_HEADER, "lane-admission v2", 1)).is_err());
        assert!(parse_admission(&format!("{}\nextra=1\n", good_admission().trim_end_matches('\n'))).is_err());
    }

    #[test]
    fn the_cap_is_the_smaller_of_the_admission_and_the_config() {
        let cfg = |count: u64| {
            let t = format!("repos:\nbudgets:\n  s_wall_seconds: 1\n  s_tokens: 1\n  m_wall_seconds: 1\n  m_tokens: 1\n  l_wall_seconds: 1\n  l_tokens: 1\nshadow:\n  p1a_cap: 1\n  p4a_cap: 1\n  d2a_cap: 1\n  p7_cap: 1\n  p1a_token_budget: 1\n  p4a_token_budget: 1\n  d2a_token_budget: 1\n  p7_token_budget: 1\nlanes:\n  count: {count}\nfriction:\n  inbox: /placeholder/friction-inbox.jsonl\nfactory:\n  design_tier: T3\n");
            Config::parse(&t).unwrap()
        };
        assert_eq!(claim_cap(None, &cfg(4)), 1);
        assert_eq!(claim_cap(Some(2), &cfg(4)), 2);
        assert_eq!(claim_cap(Some(4), &cfg(4)), 4);
        assert_eq!(claim_cap(Some(4), &cfg(3)), 3);
    }
}
