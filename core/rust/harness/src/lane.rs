//! `harness lane claim | release | list | park | admission-check` (std only; `docs/lanes.md` is the contract). A lane holds one live ticket. The cap
//! on live tickets is 1 without a valid admission record and the smaller of the record's admitted lane count and the config `lanes.count` with
//! one, so no constant is edited to raise the cap. One record file per held lane, `<lanes-dir>/lane-<n>` with n in 1..=the config `lanes.count`
//! (the legal names follow the config, not the admission), and one parked file per reserved ticket, `<lanes-dir>/parked-<ticket-id>`: `park`
//! moves a live record there unchanged, so the lane is freed while the ticket's file list stays reserved. Parked records take no lane number and
//! do not count as live. Each claim, release, park and list runs as one critical section under an exclusive `flock`
//! on the lanes dir itself (no lock file is created), so the read-check-write of two `harness` processes never interleaves. A claim writes the
//! record to a temp file (`.claim.tmp`, default permissions, one left by a killed claim is removed under the lock by the next claim that takes a
//! lane) and renames it onto the lane name, so a reader never sees a half-written record. The lanes dir is read strictly: a record that is not a
//! regular file, cannot be read or parsed (a `repo_id` that is not a legal repo id does not parse: the clash is decided by the repo id, so a
//! record that spells another repo id is a bad record, not another repo), has a name other than `lane-<n>` (n canonical, 1..=the config
//! `lanes.count`) or `parked-<ticket-id>` (the id byte-equal to the `ticket=` line inside), or repeats a live ticket, is an error and nothing is
//! changed. The admission record is read fail closed too: one that exists and is not valid is a `bad-admission` refusal
//! checked before the lanes dir is touched, while a path where nothing exists means no admission (cap 1). A claim whose scope overlaps the scope
//! of a live ticket of the same repo is refused with `clash`: checked after `already-live` and the cap, before anything is written (`scope_entry_parts`
//! is the normal form of that comparison); a claim that overlaps a parked record of another ticket of the same repo is refused with `parked`
//! right after it. A `--lanes-dir` or `--records-dir` value that ends with `/` or holds a `.` or `..` part is exit 2 before anything is touched.
//! No directory fsync is done, so a claim, release or park is not guaranteed to survive a crash. Release is
//! explicit: a lane is freed by `release` and by `park` (which keeps the file list reserved), not by a dead process. A release with `--reason`, `--at` and `--records-dir` (the three go
//! together) that frees a lane appends one line to `<records-dir>/<ticket-id>.release`; the records dir and the record are checked before the lane
//! file is removed, and a write error after it is exit 2 with the lane freed. `park` takes the same record options and appends its line the same way.
//! Exit 0 done; 1 policy refusal (one stderr line `lane: refused reason=<token>`; `bad-admission` also names the field when the problem is a
//! field, `clash` also names the live ticket and the overlapping pair of entries, `parked` the parked ticket and the pair, and `low-disk` also names the free GB and the floor); 2 usage
//! error, unreadable or refused input, free disk space that cannot be read, or a lanes dir in a bad state.

use crate::config::Config;
use crate::repo_id::RepoId;
use crate::ticket::Ticket;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

const USAGE: &str = "usage: harness lane claim <ticket-file> --lanes-dir <absolute-dir> --config <file> --scope-file <file> [--admission <file>] | lane release <ticket-id> --lanes-dir <absolute-dir> --config <file> [--reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <absolute-dir>] | lane list --lanes-dir <absolute-dir> --config <file> [--admission <file>] | lane admission-check <file> --config <file> | lane park <ticket-id> --lanes-dir <absolute-dir> --config <file> [--reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <absolute-dir>]";
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
    /// A `park` for a ticket whose parked name the file system already resolves (its own parked file, or on a case-ignoring disk the parked
    /// file of a ticket whose id differs only in ASCII case).
    AlreadyParked,
    /// A claim for a second ticket with no valid admission (no `--admission`, or a path where nothing exists).
    NoAdmission,
    /// An admission file that exists and is not valid; the string names what is wrong (the field when it is a field).
    Admission(String),
    /// A claim whose scope overlaps the scope of a live ticket of the same repo; the string names the live ticket and the overlapping pair
    /// (`ticket=<id> live=<entry> new=<entry>`, the entries as stored).
    Clash(String),
    /// A claim whose scope overlaps a parked record of another ticket of the same repo; the string names the parked ticket and the pair
    /// (`ticket=<id> parked=<entry> new=<entry>`, the entries as stored).
    Parked(String),
    /// A claim under the config disk floor; the string gives `free_gb=<n> min_free_gb=<floor>` (the free bytes rounded down to GB).
    LowDisk(String),
}

impl Refusal {
    fn token(&self) -> &'static str {
        match self {
            Refusal::CapFull => "cap-full",
            Refusal::AlreadyLive => "already-live",
            Refusal::NotLive => "not-live",
            Refusal::AlreadyParked => "already-parked",
            Refusal::NoAdmission => "no-admission",
            Refusal::Admission(_) => "bad-admission",
            Refusal::Clash(_) => "clash",
            Refusal::Parked(_) => "parked",
            Refusal::LowDisk(_) => "low-disk",
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

/// The normal form of a scope entry for the clash comparison ONLY (records keep storing entries as given, and what `scope_entry_ok` accepts
/// does not change): the entry is split on `/` after dropping one trailing `/`, every `.` part is dropped (`docs/./a.md` and `./docs/a.md`
/// are `docs/a.md`), and the parts are lowercased per ASCII (`Docs/A.md` is `docs/a.md`; the default macOS file system treats them as one
/// file, and refusing too much is the safe side). An entry that is nothing but `.` parts (`.` or `./`) has no parts: it names the whole repo.
/// The form does no Unicode normalization, resolves no symbolic link, and treats a `\` as an ordinary character (only `/` splits the parts).
pub fn scope_entry_parts(e: &str) -> Vec<String> {
    e.strip_suffix('/').unwrap_or(e).split('/').filter(|p| *p != ".").map(|p| p.to_ascii_lowercase()).collect()
}

/// Do two scope entries of one repo overlap, in normal form (`scope_entry_parts` of each)? The parts of one are a prefix of the parts of the
/// other, counted in whole parts: equal entries overlap; a folder entry (one that ends in `/`) overlaps every entry inside it; a file entry
/// overlaps an entry that lies inside it too (`docs` against `docs/x.md`: the same name cannot be both a file and a folder in one tree, so
/// refusing is the safe side); and an entry with no parts (the whole repo) overlaps every entry, including another whole-repo entry.
fn parts_overlap(pa: &[String], pb: &[String]) -> bool {
    pa.iter().zip(pb).all(|(x, y)| x == y)
}

/// The first clash of `rec` against the live records, in a repeatable order: the live records by lane number (lowest first; `list_records`
/// returns them sorted), then each live record's entries in stored order, then `rec`'s entries in stored order. Only records of the same
/// `repo_id` are compared, and every repo id in play is a legal one (`decode` refuses a record whose repo id is not, and the claim's comes
/// from the ticket reader), so a plain comparison cannot pass a pair whose two sides are spellings of one repo. Each entry is put into the
/// normal form once per call, not once per comparison, so that work grows with the two entry counts, not with their product. Returns the
/// live ticket, its entry and the new entry, as stored.
fn find_clash(live: &[Record], rec: &Record) -> Option<(String, String, String)> {
    let new_parts: Vec<Vec<String>> = rec.scope.iter().map(|e| scope_entry_parts(e)).collect();
    for r in live {
        if r.repo_id != rec.repo_id {
            continue;
        }
        let live_parts: Vec<Vec<String>> = r.scope.iter().map(|e| scope_entry_parts(e)).collect();
        for (le, lp) in r.scope.iter().zip(&live_parts) {
            for (ne, np) in rec.scope.iter().zip(&new_parts) {
                if parts_overlap(lp, np) {
                    return Some((r.ticket.clone(), le.clone(), ne.clone()));
                }
            }
        }
    }
    None
}

/// The first overlap of `rec` against the parked records of other tickets, in a repeatable order: the parked tickets by ticket id in byte
/// order (`read_parked` returns them sorted), then each parked record's entries in stored order, then `rec`'s entries in stored order. The
/// claim's own parked file (matched by the `ticket=` value, not by the file name) does not refuse the claim. Returns the parked ticket, its
/// entry and the new entry, as stored.
fn find_parked_clash(parked: &[Record], rec: &Record) -> Option<(String, String, String)> {
    let new_parts: Vec<Vec<String>> = rec.scope.iter().map(|e| scope_entry_parts(e)).collect();
    for p in parked.iter().filter(|p| p.ticket != rec.ticket) {
        if p.repo_id != rec.repo_id {
            continue;
        }
        let parked_parts: Vec<Vec<String>> = p.scope.iter().map(|e| scope_entry_parts(e)).collect();
        for (pe, pp) in p.scope.iter().zip(&parked_parts) {
            for (ne, np) in rec.scope.iter().zip(&new_parts) {
                if parts_overlap(pp, np) {
                    return Some((p.ticket.clone(), pe.clone(), ne.clone()));
                }
            }
        }
    }
    None
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
    // The clash is decided by the repo id, so a record that holds anything but a legal repo id (the one rule of `repo_id.rs`, the same rule a
    // ticket goes through) is a bad record, not another repo.
    let repo_id = RepoId::parse(repo_id).map_err(|_| "a lane record has a repo_id that is not a plain name".to_string())?;
    let mut scope = Vec::new();
    for l in lines {
        let e = l.strip_prefix("scope=").ok_or("a lane record has an unknown line")?;
        scope_entry_ok(e)?;
        scope.push(e.to_string());
    }
    if scope.is_empty() {
        return Err("a lane record has no scope entry".to_string());
    }
    Ok(Record { lane, ticket: ticket.to_string(), repo_id: repo_id.as_str().to_string(), scope })
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
                Err(field_bad(f, "is not one token of printable ASCII (at most 256 bytes)"))
            } else {
                Ok(())
            }
        }
        "step2" | "verdict" | "l3" => plain_text("is not plain text of at most 256 bytes (no control character, no U+2028 or U+2029)"),
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
/// valid record, both inside `Ok(Ok(..))`. A file that is there and not valid is the `bad-admission` refusal, `Ok(Err(..))`; the outer `Err` is an
/// exit-2 problem (the path cannot be inspected or read).
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

/// The parked records, by ticket id in byte order, read strictly (the caller holds the lock). A missing dir holds nothing. An entry named
/// `parked-<rest>` must be a regular file (a link is refused), at most 1 MiB, utf-8, parse as a lane record, and `<rest>` must be byte-equal
/// to its `ticket=` value; anything else is an error and nothing is changed. A ticket that is live and parked at once (a claim of the parked
/// ticket whose parked-file removal failed, or a file put there by hand) is accepted.
pub fn read_parked(dir: &Path) -> Result<Vec<Record>, String> {
    if !check_dir(dir, false)? {
        return Ok(Vec::new());
    }
    let rd = fs::read_dir(dir).map_err(|_| "the lanes dir cannot be read".to_string())?;
    let mut out: Vec<Record> = Vec::new();
    for ent in rd {
        let ent = ent.map_err(|_| "the lanes dir cannot be read".to_string())?;
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(rest) = name.strip_prefix("parked-") else { continue };
        if rest.is_empty() {
            return Err("a parked record name has no ticket id".to_string());
        }
        let path = ent.path();
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("a parked record is not a regular file".to_string()),
            Err(_) => return Err("a parked record cannot be inspected".to_string()),
        }
        // Opened like `open_record` (its `custom_flags`: no link follow, no blocking), so a path swapped for a link or a fifo between
        // the stat and this open fails the open or the handle check below and cannot block.
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new().read(true).custom_flags(OPEN_NOFOLLOW_NONBLOCK).open(&path).map_err(|_| "a parked record cannot be read".to_string())?;
        if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
            return Err("a parked record is not a regular file".to_string());
        }
        let mut bytes = Vec::new();
        (&mut f).take(MAX_FILE + 1).read_to_end(&mut bytes).map_err(|_| "a parked record cannot be read".to_string())?;
        if bytes.len() as u64 > MAX_FILE {
            return Err("a parked record is over 1 MiB".to_string());
        }
        if bytes.is_empty() {
            return Err("a parked record is empty".to_string());
        }
        let text = String::from_utf8(bytes).map_err(|_| "a parked record is not utf-8".to_string())?;
        let rec = decode(0, &text)?;
        if rest != rec.ticket {
            return Err("a parked record name does not match its ticket".to_string());
        }
        out.push(rec);
    }
    out.sort_by(|a, b| a.ticket.cmp(&b.ticket));
    Ok(out)
}

/// The path of the parked file of `ticket` (a parked file's name is byte-equal to the `ticket=` line inside it, so this is also the file
/// whose `ticket=` value is `ticket`).
fn parked_path(dir: &Path, ticket: &str) -> PathBuf {
    dir.join(format!("parked-{ticket}"))
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

/// The live and parked records, read under the lock.
fn state_locked(dir: &Path, cap: u32) -> Result<(Vec<Record>, Vec<Record>), String> {
    if !check_dir(dir, false)? {
        return Ok((Vec::new(), Vec::new()));
    }
    let _guard = lock(dir)?;
    Ok((list_records(dir, cap)?, read_parked(dir)?))
}

/// Take the lowest free lane under `cap` for `rec` (its `lane` field is ignored), as one critical section. Refused when as many tickets are
/// live as the cap, or when the scope of `rec` overlaps the scope of a live ticket of the same repo (`clash`, checked after the cap: the cap
/// decides whether the claim may hold a lane at all, and that refusal does not look at the scope). The dir is read under `bound`, the
/// config name bound (a legal record name may sit above the claim cap, because the names follow the config, not the admission). `admitted`
/// tells whether the cap comes from a valid admission record, which picks the refusal token when no lane is free. Ok(Err) is a policy refusal.
fn claim(dir: &Path, rec: Record, cap: u32, bound: u32, admitted: bool) -> Result<Result<u32, Refusal>, String> {
    check_dir(dir, true)?;
    let _guard = lock(dir)?;
    let live = list_records(dir, bound)?;
    let parked = read_parked(dir)?;
    if live.iter().any(|r| r.ticket == rec.ticket) {
        return Ok(Err(Refusal::AlreadyLive));
    }
    // The cap bounds the number of live tickets, wherever their lanes sit (a record may hold a lane above the cap, e.g. after the cap was lowered).
    let free = if live.len() as u64 >= cap as u64 { None } else { (1..=cap).find(|n| !live.iter().any(|r| r.lane == *n)) };
    let Some(n) = free else {
        return Ok(Err(if admitted { Refusal::CapFull } else { Refusal::NoAdmission }));
    };
    // The clash is the last policy check before the parked overlap, so nothing is written before either; a claim with a valid admission and a
    // free lane is still refused here when its scope would break a live ticket of the same repo, and after it when it would break the
    // reservation of a parked ticket of the same repo.
    if let Some((ticket, live_entry, new_entry)) = find_clash(&live, &rec) {
        return Ok(Err(Refusal::Clash(format!("ticket={ticket} live={live_entry} new={new_entry}"))));
    }
    if let Some((ticket, parked_entry, new_entry)) = find_parked_clash(&parked, &rec) {
        return Ok(Err(Refusal::Parked(format!("ticket={ticket} parked={parked_entry} new={new_entry}"))));
    }
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
    // A claim of a ticket with its own parked file ends the reservation: the parked file is removed after the lane record is written, and a
    // failure of the removal keeps the parked file (the claim still succeeds; the wrapper arms its rollback only after exit 0).
    if parked.iter().any(|p| p.ticket == rec.ticket) {
        let _ = fs::remove_file(parked_path(dir, &rec.ticket));
    }
    Ok(Ok(n))
}

/// The record checks `release` and `park` run under the lock before they touch the lane file: with `spec` the records dir and the record are
/// checked (a refusal leaves the lane held), and the open record and its old length come back for the append after it. Without `spec` nothing
/// is created.
fn prep_record(dir: &Path, ticket: &str, spec: Option<&ReleaseSpec>) -> Result<Option<(fs::File, u64)>, String> {
    let Some(s) = spec else { return Ok(None) };
    let inside = || "--records-dir must not be the lanes dir or inside it".to_string();
    // Before anything is created: the nearest existing ancestor, resolved, must lie outside the lanes dir
    // (a ticket id such as `lane-1` would put its record among the lane records, which `list_records` refuses).
    if inside_lanes(&nearest_existing_ancestor(&s.records_dir), dir) {
        return Err(inside());
    }
    let created = prep_records_dir(&s.records_dir)?;
    if inside_lanes(&s.records_dir, dir) {
        if created {
            let _ = fs::remove_dir_all(&s.records_dir);
        }
        return Err(inside());
    }
    let path = s.records_dir.join(format!("{ticket}.release"));
    match fs::symlink_metadata(&path) {
        Ok(m) if m.file_type().is_file() => {}
        Ok(_) => return Err("the release record is not a regular file".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("the release record cannot be opened".to_string()),
    }
    let mut f = open_record(&path)?;
    // Re-check through the open handle, so a path swapped for a fifo or other non-regular file between the two calls is still caught
    // (a swapped-in link already fails the open).
    let meta = f.metadata().map_err(|_| "the release record cannot be opened".to_string())?;
    if !meta.file_type().is_file() {
        return Err("the release record is not a regular file".to_string());
    }
    let mut bytes = Vec::new();
    (&mut f).take(MAX_FILE + 1).read_to_end(&mut bytes).map_err(|_| "the release record cannot be opened".to_string())?;
    if bytes.len() as u64 > MAX_FILE {
        return Err("the release record is over 1 MiB".to_string());
    }
    let text = String::from_utf8(bytes).map_err(|_| "the release record is not utf-8".to_string())?;
    if !text.is_empty() && !text.ends_with('\n') {
        return Err("the release record does not end with a newline".to_string());
    }
    Ok(Some((f, meta.len())))
}

/// Append `<at> lane=<lane> reason=<reason>` to the record and sync it; on a write error the file is cut back to its old length (a failure of
/// the cut is ignored) and `lost` names what already happened and cannot be taken back (`reason=` runs to the line end; a later field goes
/// before it).
fn append_line(f: &mut fs::File, old_len: u64, s: &ReleaseSpec, lane: &str, lost: &str) -> Result<(), String> {
    let line = format!("{} lane={} reason={}\n", s.at, lane, s.reason);
    if f.write_all(line.as_bytes()).and_then(|_| f.sync_all()).is_err() {
        let _ = f.set_len(old_len);
        return Err(format!("{lost} and the release record cannot be written"));
    }
    Ok(())
}

/// Free the lane of `ticket` and end its reservation, as one critical section, reading the dir under the config name bound. A ticket with
/// neither a live record nor a parked file is `not-live`. With `spec` the records dir and the record are checked before anything is removed
/// (a refusal leaves the lane held), the lane file goes first, then the parked file, then one record line is appended (its lane value is
/// `parked` for a ticket that held only a parked file); a write error after the removals is exit 2 naming what was removed (`the lane was
/// freed`, or `the parked file was removed` for a parked-only ticket).
fn release(dir: &Path, ticket: &str, cap: u32, spec: Option<&ReleaseSpec>) -> Result<Result<(), Refusal>, String> {
    if !check_dir(dir, false)? {
        return Ok(Err(Refusal::NotLive));
    }
    let _guard = lock(dir)?;
    let live = list_records(dir, cap)?;
    let parked = read_parked(dir)?;
    let r = live.iter().find(|r| r.ticket == ticket);
    let parked_own = parked.iter().any(|p| p.ticket == ticket);
    if r.is_none() && !parked_own {
        return Ok(Err(Refusal::NotLive));
    }
    let rec = prep_record(dir, ticket, spec)?;
    if let Some(r) = r {
        fs::remove_file(lane_path(dir, r.lane)).map_err(|_| "the lane record cannot be removed".to_string())?;
    }
    if parked_own {
        let lost = if r.is_some() { "the lane was freed and the parked file cannot be removed" } else { "the parked file cannot be removed" };
        fs::remove_file(parked_path(dir, ticket)).map_err(|_| lost.to_string())?;
    }
    if let (Some(s), Some((mut f, old_len))) = (spec, rec) {
        let lost = if r.is_some() { "the lane was freed" } else { "the parked file was removed" };
        append_line(&mut f, old_len, s, &r.map(|r| r.lane.to_string()).unwrap_or_else(|| "parked".to_string()), lost)?;
    }
    Ok(Ok(()))
}

/// Move the live record of `ticket` onto `parked-<ticket-id>`, as one critical section, reading the dir under the config name bound: the lane
/// is freed and the ticket's file list stays reserved. The move does not replace a file the file system resolves as the parked name (its own
/// parked file, or on a case-ignoring disk the parked file of a ticket whose id differs only in ASCII case): `already-parked`. A ticket with
/// no live record (a parked-only ticket too) is `not-live`; a missing lanes dir is not created. With `spec` the record checks run before the
/// move and one line (`lane=<n>`, the lane the ticket held) is appended after it, as in `release`. Ok(Ok) carries the lane number.
fn park(dir: &Path, ticket: &str, cap: u32, spec: Option<&ReleaseSpec>) -> Result<Result<u32, Refusal>, String> {
    if !check_dir(dir, false)? {
        return Ok(Err(Refusal::NotLive));
    }
    let _guard = lock(dir)?;
    let live = list_records(dir, cap)?;
    read_parked(dir)?;
    let Some(r) = live.iter().find(|r| r.ticket == ticket) else { return Ok(Err(Refusal::NotLive)) };
    let target = parked_path(dir, ticket);
    if fs::symlink_metadata(&target).is_ok() {
        return Ok(Err(Refusal::AlreadyParked));
    }
    let rec = prep_record(dir, ticket, spec)?;
    fs::rename(lane_path(dir, r.lane), &target).map_err(|_| "the lane record cannot be parked".to_string())?;
    if let (Some(s), Some((mut f, old_len))) = (spec, rec) {
        append_line(&mut f, old_len, s, &r.lane.to_string(), "the ticket was parked")?;
    }
    Ok(Ok(r.lane))
}

/// What a release or park with the record options writes: the line parts `at` and `reason` (checked before the lanes dir is touched) and the
/// folder `records_dir` (the given path is checked before it, the resolved path under the lock).
struct ReleaseSpec<'a> {
    at: &'a str,
    reason: &'a str,
    records_dir: PathBuf,
}

// `open(2)` flags std exposes no constants for (the `quarantine.rs` pattern): the values differ per OS and per CPU
// family. Listed: macos, and linux with its x86_64 values (linux on arm64 has another O_NOFOLLOW and is not
// handled). An OS with no listed value fails the build here.
#[cfg(target_os = "linux")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0o400000 | 0o4000;
#[cfg(target_os = "macos")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0x0100 | 0x0004;

/// Open the release record for reading and appending, create allowed, without following a link and without
/// blocking (a fifo does not block the open; the regular-file checks before and after it refuse it).
fn open_record(p: &Path) -> Result<fs::File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().read(true).append(true).create(true).custom_flags(OPEN_NOFOLLOW_NONBLOCK).open(p).map_err(|_| "the release record cannot be opened".to_string())
}

/// The nearest ancestor of `p` that exists on disk (`p` itself when it exists; the root when nothing does).
fn nearest_existing_ancestor(p: &Path) -> PathBuf {
    let mut a = p.to_path_buf();
    while fs::symlink_metadata(&a).is_err() {
        match a.parent() {
            Some(up) if up != a => a = up.to_path_buf(),
            _ => break,
        }
    }
    a
}

// `statvfs(2)` free-space reading (the `quarantine.rs` pattern): one `#[repr(C)]` struct per target, laid out as
// the platform header declares it. Unread fields carry a leading underscore. A target with no struct gets no
// `statvfs` reading: the same gate limits the real `free_bytes` to macOS and 64-bit Linux, and `free_bytes` is `None` elsewhere.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Default)]
struct Statvfs {
    _f_bsize: std::ffi::c_ulong,
    f_frsize: std::ffi::c_ulong,
    _f_blocks: u32,
    _f_bfree: u32,
    f_bavail: u32,
    _f_files: u32,
    _f_ffree: u32,
    _f_favail: u32,
    _f_fsid: std::ffi::c_ulong,
    _f_flag: std::ffi::c_ulong,
    _f_namemax: std::ffi::c_ulong,
}

#[cfg(all(target_os = "linux", target_pointer_width = "64"))]
#[repr(C)]
#[derive(Default)]
struct Statvfs {
    _f_bsize: std::ffi::c_ulong,
    f_frsize: std::ffi::c_ulong,
    _f_blocks: u64,
    _f_bfree: u64,
    f_bavail: u64,
    _f_files: u64,
    _f_ffree: u64,
    _f_favail: u64,
    _f_fsid: std::ffi::c_ulong,
    _f_flag: std::ffi::c_ulong,
    _f_namemax: std::ffi::c_ulong,
    _spare: [std::ffi::c_uint; 6],
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_pointer_width = "64")))]
impl Statvfs {
    /// `f_bavail * f_frsize` as bytes with a checked product. macOS: `f_bavail` is 32-bit and `f_frsize` an
    /// `unsigned long`.
    #[cfg(target_os = "macos")]
    fn avail_bytes(&self) -> Option<u64> {
        (self.f_bavail as u64).checked_mul(self.f_frsize)
    }

    /// The same product on 64-bit Linux: both fields are 64-bit, so no cast is needed.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    fn avail_bytes(&self) -> Option<u64> {
        self.f_bavail.checked_mul(self.f_frsize)
    }
}

#[cfg(any(target_os = "macos", all(target_os = "linux", target_pointer_width = "64")))]
extern "C" {
    fn statvfs(path: *const std::ffi::c_char, buf: *mut Statvfs) -> std::ffi::c_int;
}

/// The free bytes of the file system that holds `path` (the `f_bavail * f_frsize` of `statvfs(2)`, with a
/// checked product). `None` when the path holds a NUL byte, the call fails, `f_frsize` is 0, the product
/// overflows, or the target is not macOS or 64-bit Linux.
#[cfg(any(target_os = "macos", all(target_os = "linux", target_pointer_width = "64")))]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut v = Statvfs::default();
    // One `statvfs(2)` call on `c`: it only reads the path and fills `v`, a plain struct of integers.
    if unsafe { statvfs(c.as_ptr(), &mut v) } != 0 {
        return None;
    }
    if v.f_frsize == 0 {
        return None;
    }
    v.avail_bytes()
}

/// The same reading on a target the crate declares no `statvfs` struct for: fail closed.
#[cfg(not(any(target_os = "macos", all(target_os = "linux", target_pointer_width = "64"))))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// The outcome of the disk-floor check of a claim.
#[derive(Debug, PartialEq, Eq)]
enum FloorCheck {
    /// No floor is set, or the free space reaches the floor: the claim goes on.
    Ok,
    /// The free space is under the floor; the string is `free_gb=<n> min_free_gb=<floor>` (n is the free bytes
    /// rounded down to GB).
    LowDisk(String),
    /// A floor is set and the free space cannot be read (exit 2).
    Unreadable,
}

/// The disk floor of a claim: with a floor set, the free bytes of the file system of the lanes dir (its nearest
/// existing ancestor when it is missing) must reach `floor * 1000000000`, compared in `u128` so the product
/// cannot wrap. `read` reads the free bytes, a parameter so a test can supply its own; with no floor set the
/// reader is not called and the disk is not read. `release`, `park`, `list` and `admission-check` run no
/// check: a running job is not stopped, and its lane can be freed on a full disk.
fn check_disk_floor(dir: &Path, floor: Option<u64>, read: &dyn Fn(&Path) -> Option<u64>) -> FloorCheck {
    let Some(floor) = floor else { return FloorCheck::Ok };
    let Some(free) = read(&nearest_existing_ancestor(dir)) else { return FloorCheck::Unreadable };
    if (free as u128) < (floor as u128) * 1000000000 {
        return FloorCheck::LowDisk(format!("free_gb={} min_free_gb={floor}", free / 1000000000));
    }
    FloorCheck::Ok
}

/// Must `p` be read as the lanes dir or inside it, by `fs::canonicalize` of both? A canonicalize that fails is
/// the safe side.
fn inside_lanes(p: &Path, lanes: &Path) -> bool {
    match (fs::canonicalize(p), fs::canonicalize(lanes)) {
        (Ok(a), Ok(l)) => a.starts_with(&l),
        _ => true,
    }
}

/// The records dir: a real folder (a link or a non-folder is refused), created with its parents when missing.
/// Ok(true) when it was missing at the first look.
fn prep_records_dir(dir: &Path) -> Result<bool, String> {
    match fs::symlink_metadata(dir) {
        Ok(m) if m.file_type().is_dir() => Ok(false),
        Ok(_) => Err("the records dir is not a folder or is a symbolic link".to_string()),
        Err(_) => {
            fs::create_dir_all(dir).map_err(|_| "the records dir cannot be created".to_string())?;
            Ok(true)
        }
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

/// Does this option value end with `/` or hold a `.` or `..` part (split on `/`)? Such a spelling can resolve through a symbolic link at the
/// end of the path (`<link>/` and `<link>/.` name the link target), so it is refused before anything is touched.
fn path_spelling_ok(v: &str) -> bool {
    !v.ends_with('/') && !v.split('/').any(|p| p == "." || p == "..")
}

fn lanes_dir(m: &std::collections::HashMap<String, String>) -> Result<PathBuf, String> {
    let d = m.get("lanes-dir").ok_or("--lanes-dir is required")?;
    let p = PathBuf::from(d);
    if !p.is_absolute() {
        return Err("--lanes-dir must be an absolute path".to_string());
    }
    if !path_spelling_ok(d) {
        return Err("--lanes-dir must not end with a slash or hold a . or .. part".to_string());
    }
    Ok(p)
}

/// The record options of `release` and `park`: all three or none, the value checks in the stated order, before the lanes dir is touched.
fn record_spec<'a>(m: &'a std::collections::HashMap<String, String>, dir: &Path) -> Result<Option<ReleaseSpec<'a>>, String> {
    let given = ["reason", "at", "records-dir"].iter().filter(|k| m.contains_key(**k)).count();
    if given == 0 {
        return Ok(None);
    }
    if given != 3 {
        return Err("--reason, --at and --records-dir go together".to_string());
    }
    let (reason, at, rd) = (&m["reason"], &m["at"], &m["records-dir"]);
    if reason.trim().is_empty() {
        return Err("--reason must not be empty or only white space".to_string());
    }
    if reason.contains('\n') {
        return Err("--reason must be one line".to_string());
    }
    if reason.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
        return Err("--reason must hold no control character".to_string());
    }
    if reason.len() > 1024 {
        return Err("--reason is over 1024 bytes".to_string());
    }
    if !crate::friction::at_ok(at) {
        return Err("--at must be a UTC time written YYYY-MM-DDTHH:MM:SSZ".to_string());
    }
    let rdp = PathBuf::from(rd.as_str());
    if !rdp.is_absolute() {
        return Err("--records-dir must be an absolute path".to_string());
    }
    if rd.ends_with('/') {
        return Err("--records-dir must not end with a slash".to_string());
    }
    // A `.` or `..` part keeps the message it has had since the check was added (the same message as the inside-lanes refusal).
    if !rd.split('/').all(|p| p != "." && p != "..") || rdp.starts_with(dir) {
        return Err("--records-dir must not be the lanes dir or inside it".to_string());
    }
    Ok(Some(ReleaseSpec { at, reason, records_dir: rdp }))
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
        Refusal::Clash(detail) => eprintln!("lane: refused reason=clash {detail}"),
        Refusal::Parked(detail) => eprintln!("lane: refused reason=parked {detail}"),
        Refusal::LowDisk(detail) => eprintln!("lane: refused reason=low-disk {detail}"),
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
            // The disk floor comes after every input check (bad-admission included, the match just below) and
            // before the lanes dir is touched: a refused or unreadable claim leaves nothing behind, and a missing
            // lanes dir stays missing.
            let (cap, from_admission) = match admitted {
                Err(r) => return refused(r),
                Ok(None) => (claim_cap(None, &cfg), false),
                Ok(Some(n)) => (claim_cap(Some(n), &cfg), true),
            };
            match check_disk_floor(&dir, cfg.min_free_gb(), &free_bytes) {
                FloorCheck::Ok => {}
                FloorCheck::LowDisk(detail) => return refused(Refusal::LowDisk(detail)),
                FloorCheck::Unreadable => return fail("the free disk space cannot be read"),
            }
            match claim(&dir, rec, cap, name_cap(&cfg), from_admission) {
                Ok(Ok(n)) => {
                    println!("ticket={} lane={}", ticket.id, n);
                    0
                }
                Ok(Err(r)) => refused(r),
                Err(e) => fail(&e),
            }
        }
        "release" => {
            let (pos, m) = match run(&["lanes-dir", "config", "reason", "at", "records-dir"]) {
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
            // The record options go together and are checked, in this order, before the lanes dir is touched.
            let spec = match record_spec(&m, &dir) {
                Ok(s) => s,
                Err(e) => return fail(&e),
            };
            match release(&dir, &id, name_cap(&cfg), spec.as_ref()) {
                Ok(Ok(())) => {
                    println!("ticket={id} released");
                    0
                }
                Ok(Err(r)) => refused(r),
                Err(e) => fail(&e),
            }
        }
        "park" => {
            let (pos, m) = match run(&["lanes-dir", "config", "reason", "at", "records-dir"]) {
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
            // The record options follow the release rules unchanged.
            let spec = match record_spec(&m, &dir) {
                Ok(s) => s,
                Err(e) => return fail(&e),
            };
            match park(&dir, &id, name_cap(&cfg), spec.as_ref()) {
                Ok(Ok(n)) => {
                    println!("ticket={id} parked lane={n}");
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
            let (recs, parked) = match state_locked(&dir, name_cap(&cfg)) {
                Ok(v) => v,
                Err(e) => return fail(&e),
            };
            for r in &recs {
                println!("ticket={} lane={} repo_id={} scope={}", r.ticket, r.lane, r.repo_id, r.scope.len());
            }
            // One line per parked record in ticket id byte order, after the live lines; `live=` counts live records only.
            for p in &parked {
                println!("parked ticket={} repo_id={} scope={}", p.ticket, p.repo_id, p.scope.len());
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

    #[test]
    fn a_record_whose_repo_id_is_not_a_plain_name_is_bad() {
        // the clash is decided by the repo id, so a record that spells another repo id must not read as another repo
        for bad in ["R", "r ", "", "my repo", ".."] {
            let text = format!("{HEADER}\nticket=t-1\nrepo_id={bad}\nscope=a\n");
            assert!(decode(1, &text).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_normal_form_of_the_clash_comparison() {
        // a `.` part does not change what an entry names; ASCII case does not either
        assert_eq!(scope_entry_parts("docs/./a.md"), vec!["docs", "a.md"]);
        assert_eq!(scope_entry_parts("./docs/a.md"), scope_entry_parts("docs/a.md"));
        assert_eq!(scope_entry_parts("Docs/A.md"), scope_entry_parts("docs/a.md"));
        assert_eq!(scope_entry_parts("docs/"), vec!["docs"]);
        // an entry that is nothing but `.` parts names the whole repo (no parts)
        assert!(scope_entry_parts(".").is_empty());
        assert!(scope_entry_parts("./").is_empty());
        // a backslash is an ordinary character: only `/` splits the parts
        assert_eq!(scope_entry_parts("src\\a.rs"), vec!["src\\a.rs"]);
    }

    #[test]
    fn entries_overlap_in_whole_path_parts() {
        let overlap = |a: &str, b: &str| parts_overlap(&scope_entry_parts(a), &scope_entry_parts(b));
        let both = |a: &str, b: &str| {
            assert!(overlap(a, b), "{a} vs {b}");
            assert!(overlap(b, a), "{a} vs {b}");
        };
        let none = |a: &str, b: &str| {
            assert!(!overlap(a, b), "{a} vs {b}");
            assert!(!overlap(b, a), "{a} vs {b}");
        };
        // equal after the normal form, in both orders
        both("docs/a.md", "docs/a.md");
        both("docs/a.md", "docs/./a.md");
        both("./docs/a.md", "docs/a.md");
        both("Docs/A.md", "docs/a.md");
        // a folder entry overlaps what lies inside it, in both orders
        both("docs/", "docs/x/y.md");
        both("docs/", "docs/sub/");
        // a file entry overlaps an entry of the same name or inside it (one name cannot be both a file and a folder)
        both("docs", "docs/");
        both("docs", "docs/x.md");
        // the whole repo clashes with every entry, and with another whole-repo entry
        both(".", "src/a.rs");
        both("./", ".");
        // near misses, in both orders
        none("docs/a.md", "docs/a.md.bak");
        none("docs/", "docs2/x.md");
        none("docs/a.md", "docs/b.md");
        none("docs/a", "docs/a-b");
        none("src\\a.rs", "src/a.rs");
    }

    #[test]
    fn find_clash_picks_the_first_pair_in_the_stated_order() {
        let mk = |lane: u32, ticket: &str, repo: &str, scope: &[&str]| Record {
            lane,
            ticket: ticket.into(),
            repo_id: repo.into(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
        };
        // lane order first, then the live record's stored entry order, then the new record's stored entry order
        let live = vec![mk(1, "t-a", "r", &["other/x", "docs/"]), mk(2, "t-b", "r", &["Docs/a.md"])];
        let new = mk(0, "t-c", "r", &["zz", "docs/a.md"]);
        let (t, l, n) = find_clash(&live, &new).unwrap();
        assert_eq!((t.as_str(), l.as_str(), n.as_str()), ("t-a", "docs/", "docs/a.md"));
        // a different repo never clashes, whatever the entries
        assert!(find_clash(&live, &mk(0, "t-c", "r2", &["docs/a.md"])).is_none());
        // no live record, no clash
        assert!(find_clash(&[], &new).is_none());
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

    #[test]
    fn the_disk_floor_check_reads_the_disk_only_with_a_floor_set() {
        let d = std::env::temp_dir().join(format!("lane-floor-unit-{}", std::process::id()));
        // no floor: the reader is not called (it panics) and the check passes
        assert_eq!(check_disk_floor(&d, None, &|_| panic!("the disk must not be read without a floor")), FloorCheck::Ok);
        // a floor and an unreadable reading: the exit-2 case
        assert_eq!(check_disk_floor(&d, Some(50), &|_| None), FloorCheck::Unreadable);
        // floor 50: exactly 50000000000 free claims, 49999999999 is refused with free_gb=49
        assert_eq!(check_disk_floor(&d, Some(50), &|_| Some(50000000000)), FloorCheck::Ok);
        assert_eq!(check_disk_floor(&d, Some(50), &|_| Some(49999999999)), FloorCheck::LowDisk("free_gb=49 min_free_gb=50".to_string()));
        // floor 999999999999999 with a reading of u64::MAX: refused (a u64 product would wrap to about 2.0e18 bytes, under u64::MAX, and pass)
        assert_eq!(
            check_disk_floor(&d, Some(999999999999999), &|_| Some(u64::MAX)),
            FloorCheck::LowDisk(format!("free_gb={} min_free_gb=999999999999999", u64::MAX / 1000000000))
        );
    }
}
