//! Convergence cap state (std only, Unix). One small record per ticket in a caller-chosen "caps folder" counts the gate runs that started and
//! did not end in PASS. A run is PAID (recorded `in_flight`) before any check runs; only a decided PASS refunds it. When the count is already
//! `MAX_FIX_ROUNDS`, the next run is refused and one escalation event is appended to `<caps>/events.jsonl`.
//!
//! The record is `{"v":1,"ticket":"<id>","repo_id":"<repo>","fix_rounds":<0..4>,"in_flight":<bool>}` plus a newline, nothing else; the reader accepts
//! exactly the canonical bytes for the expected ticket and repo (so a copied, reordered, widened or out-of-range record is refused, never read as 0).
//! `rounds = fix_rounds + (1 if in_flight)`. This module owns the ONE parser (`Caps::read`), the lock, the record writer and the refusal writer, so
//! later per-ticket caps (re-plan threshold, budgets) extend it: a refusal carries an event kind and a reason token, and the writer takes the kind.
//! Every name comes from the validated ticket id or is fixed; a link is never followed or created through. No error repeats a folder.

use crate::events::{self, Event, Kind};
use crate::ticket_gate::ticket_id_ok;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// A run is refused when the ticket already has this many rounds: gate runs 1-4 execute, run 5 (the 4th fix round) is refused.
pub const MAX_FIX_ROUNDS: u8 = 4;
/// The record schema version this reader accepts.
pub const RECORD_VERSION: u64 = 1;
/// A record is read no further than this many bytes (4 KiB + 1), then refused as oversize.
pub const RECORD_READ_LIMIT: u64 = 4096 + 1;
/// The reason token of the fix-round cap refusal.
pub const REASON_FIX_ROUND_CAP: &str = "fix-round-cap";
/// The event log inside the caps folder.
pub const EVENTS_FILE: &str = "events.jsonl";
/// The reason token of the re-plan threshold refusal.
pub const REASON_REPLAN_THRESHOLD: &str = "replan-threshold";
/// This many identical FAILs in a row, on different heads, latch the ticket (the next run is refused with `replan-threshold`).
pub const REPLAN_AFTER: u8 = 2;
/// The streak schema version this reader accepts.
pub const STREAK_VERSION: u64 = 1;
/// The longest signature: a 250-byte gate id, `:`, and a class token (well under 64 bytes) are at most 265 bytes, so no legal gate reaches this.
pub const SIG_MAX: usize = 320;

/// What a ticket's record says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub ticket: String,
    pub repo_id: String,
    /// Runs that ended without PASS and are counted. 0-4.
    pub fix_rounds: u8,
    /// True from the moment a run is paid until it ends; a run that dies leaves it true.
    pub in_flight: bool,
}

impl Record {
    /// A state a writer may produce: `fix_rounds` 0-4, and never 4 with a run in flight (that would be 5 rounds).
    pub fn state_ok(fix_rounds: u8, in_flight: bool) -> bool {
        fix_rounds <= MAX_FIX_ROUNDS && !(fix_rounds == MAX_FIX_ROUNDS && in_flight)
    }

    /// The rounds a reader computes: runs that started and did not PASS, a run in progress included.
    pub fn rounds(&self) -> u8 {
        self.fix_rounds.saturating_add(u8::from(self.in_flight))
    }

    /// The canonical bytes, newline included.
    pub fn encode(&self) -> String {
        format!(
            "{{\"v\":{},\"ticket\":\"{}\",\"repo_id\":\"{}\",\"fix_rounds\":{},\"in_flight\":{}}}\n",
            RECORD_VERSION, self.ticket, self.repo_id, self.fix_rounds, self.in_flight
        )
    }

    /// Strict decode: the bytes must equal the canonical encoding of a valid state for exactly this ticket and repo. Nothing is parsed
    /// leniently, so no malformed value can wrap, saturate or read as 0.
    pub fn decode(bytes: &[u8], ticket: &str, repo_id: &str) -> Result<Record, String> {
        let refused = || "the cap record is not the canonical record for this ticket and repo".to_string();
        if std::str::from_utf8(bytes).is_err() {
            return Err(refused());
        }
        for fix_rounds in 0..=MAX_FIX_ROUNDS {
            for in_flight in [false, true] {
                if !Record::state_ok(fix_rounds, in_flight) {
                    continue;
                }
                let cand = Record { ticket: ticket.to_string(), repo_id: repo_id.to_string(), fix_rounds, in_flight };
                if cand.encode().as_bytes() == bytes {
                    return Ok(cand);
                }
            }
        }
        Err(refused())
    }
}

fn gate_part_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_' | b'.'))
}

fn class_part_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'_'))
}

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A well-formed signature: `<gate id>:<failure class>`, both from fixed alphabets (`:` is in neither), at most `SIG_MAX` bytes.
pub fn signature_ok(sig: &str) -> bool {
    match sig.split_once(':') {
        Some((gate, class)) => sig.len() <= SIG_MAX && gate_part_ok(gate) && class_part_ok(class),
        None => false,
    }
}

/// The signature of a decided FAIL: the failing gate id and its failure class, never the message, a line or a head. Fails closed when either
/// half is outside its alphabet or the whole is over `SIG_MAX`.
pub fn signature(gate: &str, class: &str) -> Result<String, String> {
    let sig = format!("{}:{}", gate, class);
    if !gate_part_ok(gate) || !class_part_ok(class) || sig.len() > SIG_MAX {
        return Err("the failure signature cannot be built".to_string());
    }
    Ok(sig)
}

/// The same-failure streak of a ticket: the last counted signature, the head it failed on, and how many consecutive fix attempts failed with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Streak {
    pub ticket: String,
    pub repo_id: String,
    pub sig: String,
    pub head: String,
    /// 1..=`REPLAN_AFTER`.
    pub count: u8,
}

impl Streak {
    pub fn state_ok(&self) -> bool {
        (1..=REPLAN_AFTER).contains(&self.count) && signature_ok(&self.sig) && sha_ok(&self.head)
    }

    /// Count has reached the threshold: the ticket needs a new plan.
    pub fn latched(&self) -> bool {
        self.count >= REPLAN_AFTER
    }

    /// The canonical bytes, newline included.
    pub fn encode(&self) -> String {
        format!(
            "{{\"v\":{},\"ticket\":\"{}\",\"repo_id\":\"{}\",\"sig\":\"{}\",\"head\":\"{}\",\"count\":{}}}\n",
            STREAK_VERSION, self.ticket, self.repo_id, self.sig, self.head, self.count
        )
    }

    /// Strict decode: the bytes must equal the canonical encoding of a valid streak for exactly this ticket and repo.
    pub fn decode(bytes: &[u8], ticket: &str, repo_id: &str) -> Result<Streak, String> {
        let refused = || "the streak file is not the canonical streak for this ticket and repo".to_string();
        let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
        let head_of_line = format!("{{\"v\":{},\"ticket\":\"{}\",\"repo_id\":\"{}\",\"sig\":\"", STREAK_VERSION, ticket, repo_id);
        let rest = text.strip_prefix(head_of_line.as_str()).ok_or_else(refused)?;
        let (sig, rest) = rest.split_at(rest.find('"').ok_or_else(refused)?);
        let rest = rest.strip_prefix("\",\"head\":\"").ok_or_else(refused)?;
        let (head, _) = rest.split_at(rest.find('"').ok_or_else(refused)?);
        for count in 1..=REPLAN_AFTER {
            let cand = Streak { ticket: ticket.to_string(), repo_id: repo_id.to_string(), sig: sig.to_string(), head: head.to_string(), count };
            if cand.state_ok() && cand.encode().as_bytes() == bytes {
                return Ok(cand);
            }
        }
        Err(refused())
    }
}

/// The streak after a decided verdict. `fail` is `Some((signature, head))` for a FAIL and `None` for a PASS (which clears it).
/// A FAIL on a different head with the same signature extends the streak (a fix attempt that did not help); a FAIL on the same head with the same
/// signature is a plain re-run and leaves it unchanged; every other FAIL starts a new streak at 1.
pub fn advance(prev: Option<&Streak>, ticket: &str, repo_id: &str, fail: Option<(&str, &str)>) -> Option<Streak> {
    let (sig, head) = fail?;
    let count = match prev {
        Some(p) if p.sig == sig && p.head == head => p.count,
        Some(p) if p.sig == sig => p.count.saturating_add(1).min(REPLAN_AFTER),
        _ => 1,
    };
    Some(Streak { ticket: ticket.to_string(), repo_id: repo_id.to_string(), sig: sig.to_string(), head: head.to_string(), count })
}

/// Read at most `RECORD_READ_LIMIT` bytes from `r`; more than 4 KiB is refused.
pub fn read_bounded<R: Read>(r: R) -> Result<Vec<u8>, String> {
    let mut buf = Vec::new();
    r.take(RECORD_READ_LIMIT).read_to_end(&mut buf).map_err(|_| "the cap record could not be read".to_string())?;
    if buf.len() as u64 >= RECORD_READ_LIMIT {
        return Err("the cap record is over the size cap".to_string());
    }
    Ok(buf)
}

/// A refusal of a run: the kind of event it is recorded as and its reason token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    pub kind: Kind,
    pub reason: &'static str,
}

/// May a run start, given the rounds already used? The fix-round cap is the only rule today.
pub fn may_start(rounds: u8) -> Result<(), Refusal> {
    if rounds >= MAX_FIX_ROUNDS {
        return Err(Refusal { kind: Kind::Escalation, reason: REASON_FIX_ROUND_CAP });
    }
    Ok(())
}

/// May a run start? A latched streak is refused first (`replan-threshold`, the more specific cause), then the fix-round cap.
pub fn start_check(rounds: u8, streak: Option<&Streak>) -> Result<(), Refusal> {
    if streak.map(|s| s.latched()).unwrap_or(false) {
        return Err(Refusal { kind: Kind::ReplanRequired, reason: REASON_REPLAN_THRESHOLD });
    }
    may_start(rounds)
}

/// The event that records a refusal. The `ticket` field is the lowercased id (an event ticket is a lowercase token).
pub fn event_for(r: &Refusal, ticket: &str, repo_id: &str, ts_ms: u64) -> Event {
    Event { kind: r.kind, ts_ms, ticket: ticket.to_ascii_lowercase(), repo: repo_id.to_string(), reason: r.reason.to_string() }
}

/// A per-ticket lock, held while this value lives.
#[derive(Debug)]
pub struct Lock(#[allow(dead_code)] File);

#[derive(Debug, PartialEq, Eq)]
pub enum LockError {
    /// Another run for this ticket holds the lock.
    Busy,
    Other(String),
}

/// The caps folder: a real directory, canonicalized once, outside the repo. Every path below is built from it and a validated id or a fixed name.
#[derive(Debug, Clone)]
pub struct Caps {
    dir: PathBuf,
}

impl Caps {
    /// `arg` must be a real directory (not a link); it is canonicalized once and must lie outside `root` (the canonical repo folder).
    pub fn open(arg: &Path, root: &Path) -> Result<Caps, String> {
        let meta = fs::symlink_metadata(arg).map_err(|_| "the caps directory is missing or unreadable".to_string())?;
        if !meta.file_type().is_dir() {
            return Err("the caps directory must be a real directory, not a link".to_string());
        }
        let canon = arg.canonicalize().map_err(|_| "the caps directory cannot be resolved".to_string())?;
        if canon.starts_with(root) {
            return Err("the caps directory lies inside the repo".to_string());
        }
        Ok(Caps { dir: canon })
    }

    /// The canonical caps folder (the event log `events.jsonl` lives directly in it).
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn id_ok(id: &str) -> Result<(), String> {
        if ticket_id_ok(id) {
            Ok(())
        } else {
            Err("the ticket id cannot be used as a file name".to_string())
        }
    }

    fn record_name(id: &str) -> String {
        format!("{}.rounds", id)
    }

    fn streak_name(id: &str) -> String {
        format!("{}.replan", id)
    }

    /// The id is usable as a file name and no differently-cased twin of its record name exists. Nothing is created.
    pub fn check_names(&self, id: &str) -> Result<(), String> {
        Caps::id_ok(id)?;
        self.check_case_twin(id)?;
        self.check_streak_case_twin(id)
    }

    /// A differently-cased twin of the record name is a collision (by name, on every filesystem).
    fn check_case_twin(&self, id: &str) -> Result<(), String> {
        self.twin_of(&Caps::record_name(id), "a cap record whose name differs only by letter case already exists")
    }

    /// The same rule for the streak file name.
    fn check_streak_case_twin(&self, id: &str) -> Result<(), String> {
        self.twin_of(&Caps::streak_name(id), "a streak file whose name differs only by letter case already exists")
    }

    fn twin_of(&self, want: &str, msg: &str) -> Result<(), String> {
        let rd = fs::read_dir(&self.dir).map_err(|_| "the caps directory could not be listed".to_string())?;
        for e in rd {
            let name = e.map_err(|_| "the caps directory could not be listed".to_string())?.file_name().to_string_lossy().into_owned();
            if name != want && name.eq_ignore_ascii_case(want) {
                return Err(msg.to_string());
            }
        }
        Ok(())
    }

    /// The public read-only entry: `Ok(None)` when no record exists, `Ok(Some)` for a canonical record for this ticket and repo, `Err` for anything
    /// else (a link, a non-regular entry, an oversize or malformed record, a wrong identity, a case twin). A refusal is never read as 0.
    pub fn read(&self, id: &str, repo_id: &str) -> Result<Option<Record>, String> {
        Caps::id_ok(id)?;
        self.check_case_twin(id)?;
        let path = self.dir.join(Caps::record_name(id));
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("the cap record exists and is not a regular file".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("the cap record could not be inspected".to_string()),
        }
        let f = File::open(&path).map_err(|_| "the cap record could not be opened".to_string())?;
        if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
            return Err("the cap record exists and is not a regular file".to_string());
        }
        let bytes = read_bounded(f)?;
        Record::decode(&bytes, id, repo_id).map(Some)
    }

    /// Rounds used: 0 when there is no record. (A caller that also has the result folder must refuse "no record but a result file".)
    pub fn rounds(&self, id: &str, repo_id: &str) -> Result<u8, String> {
        Ok(self.read(id, repo_id)?.map(|r| r.rounds()).unwrap_or(0))
    }

    /// The streak reader: `Ok(None)` when no streak file exists (a ticket with no counted FAIL, or every ticket from before this rule), `Ok(Some)` for
    /// a canonical streak for this ticket and repo, `Err` for anything else (a link, a non-regular entry, an oversize or malformed file, a wrong
    /// identity, a case twin). A refusal is never read as "no streak".
    pub fn read_streak(&self, id: &str, repo_id: &str) -> Result<Option<Streak>, String> {
        Caps::id_ok(id)?;
        self.check_streak_case_twin(id)?;
        let path = self.dir.join(Caps::streak_name(id));
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("the streak file exists and is not a regular file".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("the streak file could not be inspected".to_string()),
        }
        let f = File::open(&path).map_err(|_| "the streak file could not be opened".to_string())?;
        if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
            return Err("the streak file exists and is not a regular file".to_string());
        }
        let bytes = read_bounded(f)?;
        Streak::decode(&bytes, id, repo_id).map(Some)
    }

    /// Replace the streak durably, with the lock held, exactly as `write` does for the record: a stale temp is removed (never opened for truncation),
    /// the temp is made with `create_new`, synced, renamed over the streak, and the folder is synced.
    pub fn write_streak(&self, s: &Streak, _held: &Lock) -> Result<(), String> {
        Caps::id_ok(&s.ticket)?;
        if !s.state_ok() {
            return Err("the streak state is out of range".to_string());
        }
        let target = self.dir.join(Caps::streak_name(&s.ticket));
        let tmp = self.dir.join(format!(".{}.replan.tmp", s.ticket));
        match fs::symlink_metadata(&target) {
            Ok(m) if !m.file_type().is_file() => return Err("the streak file exists and is not a regular file".to_string()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the streak file could not be inspected".to_string()),
        }
        match fs::symlink_metadata(&tmp) {
            Ok(_) => fs::remove_file(&tmp).map_err(|_| "a stale streak temp file could not be removed".to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the streak temp file could not be inspected".to_string()),
        }
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|_| "the streak file could not be created".to_string())?;
        if f.write_all(s.encode().as_bytes()).and_then(|_| f.sync_all()).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err("the streak file could not be written".to_string());
        }
        drop(f);
        if fs::rename(&tmp, &target).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err("the streak file could not be put in place".to_string());
        }
        File::open(&self.dir).and_then(|d| d.sync_all()).map_err(|_| "the caps directory could not be synced".to_string())
    }

    /// Clear the streak (a PASS): remove the file if it exists, then sync the folder. Absent is already clear. A link or other non-regular entry is
    /// refused, never removed or followed.
    pub fn clear_streak(&self, id: &str, _held: &Lock) -> Result<(), String> {
        Caps::id_ok(id)?;
        let target = self.dir.join(Caps::streak_name(id));
        match fs::symlink_metadata(&target) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("the streak file exists and is not a regular file".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err("the streak file could not be inspected".to_string()),
        }
        fs::remove_file(&target).map_err(|_| "the streak file could not be removed".to_string())?;
        File::open(&self.dir).and_then(|d| d.sync_all()).map_err(|_| "the caps directory could not be synced".to_string())
    }

    /// The per-ticket lock `.<id>.lock`, taken without waiting. The file is created once and never deleted. A link or non-regular entry is refused.
    pub fn lock(&self, id: &str) -> Result<Lock, LockError> {
        let other = |m: &str| LockError::Other(m.to_string());
        Caps::id_ok(id).map_err(LockError::Other)?;
        let path = self.dir.join(format!(".{}.lock", id));
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(_) => return Err(other("the lock file could not be created")),
            },
            Err(_) => return Err(other("the lock file could not be inspected")),
        }
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {}
            _ => return Err(other("the lock entry is not a regular file")),
        }
        let f = OpenOptions::new().read(true).open(&path).map_err(|_| other("the lock file could not be opened"))?;
        match f.try_lock() {
            Ok(()) => Ok(Lock(f)),
            Err(TryLockError::WouldBlock) => Err(LockError::Busy),
            Err(TryLockError::Error(_)) => Err(other("the lock could not be taken")),
        }
    }

    /// Replace the record durably, with the lock held: a stale temp is removed (never opened for truncation), the temp is made with `create_new`,
    /// synced, renamed over the record, and the folder is synced.
    pub fn write(&self, rec: &Record, _held: &Lock) -> Result<(), String> {
        Caps::id_ok(&rec.ticket)?;
        if !Record::state_ok(rec.fix_rounds, rec.in_flight) {
            return Err("the cap record state is out of range".to_string());
        }
        let target = self.dir.join(Caps::record_name(&rec.ticket));
        let tmp = self.dir.join(format!(".{}.rounds.tmp", rec.ticket));
        match fs::symlink_metadata(&target) {
            Ok(m) if !m.file_type().is_file() => return Err("the cap record exists and is not a regular file".to_string()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the cap record could not be inspected".to_string()),
        }
        match fs::symlink_metadata(&tmp) {
            Ok(_) => fs::remove_file(&tmp).map_err(|_| "a stale cap temp file could not be removed".to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the cap temp file could not be inspected".to_string()),
        }
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|_| "the cap record could not be created".to_string())?;
        if f.write_all(rec.encode().as_bytes()).and_then(|_| f.sync_all()).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err("the cap record could not be written".to_string());
        }
        drop(f);
        if fs::rename(&tmp, &target).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err("the cap record could not be put in place".to_string());
        }
        File::open(&self.dir).and_then(|d| d.sync_all()).map_err(|_| "the caps directory could not be synced".to_string())
    }

    /// Record a refusal in `<caps>/events.jsonl`. The log entry itself must be absent or a regular file: `events::append` would follow a link that
    /// resolves inside this folder (for example onto a record).
    pub fn escalate(&self, r: &Refusal, id: &str, repo_id: &str) -> Result<(), String> {
        let log = self.dir.join(EVENTS_FILE);
        match fs::symlink_metadata(&log) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("the event log entry is not a regular file".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the event log entry could not be inspected".to_string()),
        }
        let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        events::append(&self.dir, &log, &event_for(r, id, repo_id, ts_ms)).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("caps-unit-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d.canonicalize().unwrap()
    }

    fn rec(fix_rounds: u8, in_flight: bool) -> Record {
        Record { ticket: "EX-1".to_string(), repo_id: "r".to_string(), fix_rounds, in_flight }
    }

    // AC 22: the golden bytes and the formula, over every valid state.
    #[test]
    fn the_record_bytes_and_the_rounds_formula_are_pinned() {
        assert_eq!(rec(2, false).encode(), "{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"fix_rounds\":2,\"in_flight\":false}\n");
        assert_eq!(rec(3, true).encode(), "{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"fix_rounds\":3,\"in_flight\":true}\n");
        let mut seen = 0;
        for fr in 0..=MAX_FIX_ROUNDS {
            for fl in [false, true] {
                if !Record::state_ok(fr, fl) {
                    continue;
                }
                seen += 1;
                let r = rec(fr, fl);
                assert_eq!(r.rounds(), fr + u8::from(fl), "rounds = fix_rounds + in_flight");
                assert!(r.rounds() <= MAX_FIX_ROUNDS);
                assert_eq!(Record::decode(r.encode().as_bytes(), "EX-1", "r"), Ok(r), "round trip");
            }
        }
        assert_eq!(seen, 9, "5 x 2 states minus (4, in flight)");
        assert!(!Record::state_ok(4, true) && !Record::state_ok(5, false));
    }

    #[test]
    fn the_decoder_accepts_only_the_canonical_bytes_for_this_ticket_and_repo() {
        let good = rec(1, false).encode();
        assert!(Record::decode(good.as_bytes(), "EX-1", "r").is_ok(), "twin: the canonical record");
        let bad: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"nope\n".to_vec(),
            good.replace("\"v\":1", "\"v\":2").into_bytes(),
            good.replace("1,\"in", "5,\"in").into_bytes(),
            good.replace("1,\"in", "01,\"in").into_bytes(),
            good.replace("1,\"in", "18446744073709551615,\"in").into_bytes(),
            good.replace("false", "0").into_bytes(),
            format!("{good}{good}").into_bytes(),
            good.trim_end().as_bytes().to_vec(),
            [good.as_bytes(), b"\0"].concat(),
            vec![0xff, 0xfe],
            rec(4, true).encode().into_bytes(),
        ];
        for b in &bad {
            assert!(Record::decode(b, "EX-1", "r").is_err(), "{:?} must be refused", String::from_utf8_lossy(b));
        }
        assert!(Record::decode(good.as_bytes(), "EX-2", "r").is_err(), "another ticket's record");
        assert!(Record::decode(good.as_bytes(), "EX-1", "q").is_err(), "another repo's record");
    }

    // AC 13: the reader stops at 4 KiB + 1 bytes of a source that never ends.
    #[test]
    fn the_record_reader_stops_at_the_size_cap() {
        struct Endless(std::rc::Rc<std::cell::Cell<u64>>);
        impl Read for Endless {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                let left = (1u64 << 30).saturating_sub(self.0.get());
                let n = (buf.len() as u64).min(left) as usize;
                buf[..n].fill(b'x');
                self.0.set(self.0.get() + n as u64);
                Ok(n)
            }
        }
        let served = std::rc::Rc::new(std::cell::Cell::new(0));
        assert!(read_bounded(Endless(served.clone())).is_err());
        assert!(served.get() <= RECORD_READ_LIMIT, "served {} bytes", served.get());
        assert_eq!(read_bounded(&b"a short record"[..]), Ok(b"a short record".to_vec()), "twin: a small source");
        assert!(read_bounded(&vec![b'x'; 4096][..]).is_ok(), "exactly 4 KiB is read");
        assert!(read_bounded(&vec![b'x'; 4097][..]).is_err());
    }

    #[test]
    fn the_cap_refuses_at_four_rounds_and_not_before() {
        for n in 0..MAX_FIX_ROUNDS {
            assert_eq!(may_start(n), Ok(()), "{n} rounds: the run executes");
        }
        let r = may_start(MAX_FIX_ROUNDS).unwrap_err();
        assert_eq!((r.kind, r.reason), (Kind::Escalation, "fix-round-cap"));
    }

    // AC 21: the shared writer encodes a refusal of each existing kind.
    #[test]
    fn the_refusal_writer_is_not_tied_to_the_escalation_kind() {
        let d = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        for (kind, reason) in [(Kind::Escalation, "fix-round-cap"), (Kind::ReplanRequired, "replan-threshold"), (Kind::BudgetExceeded, "budget-spent")] {
            let r = Refusal { kind, reason };
            let e = event_for(&r, "EX-1", "r", 7);
            assert_eq!((e.kind, e.ticket.as_str(), e.repo.as_str(), e.reason.as_str(), e.ts_ms), (kind, "ex-1", "r", reason, 7));
            caps.escalate(&r, "EX-1", "r").unwrap();
        }
        let got = events::read_log(&d.join(EVENTS_FILE)).unwrap();
        assert!(got.is_clean());
        assert_eq!(got.events.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![Kind::Escalation, Kind::ReplanRequired, Kind::BudgetExceeded]);
        let _ = fs::remove_dir_all(&d);
    }

    // The public reader alone refuses a link (the gate's writer would also refuse it, so only the reader's own test can pin this guard).
    #[test]
    fn the_public_reader_never_follows_a_link() {
        let d = scratch();
        let outside = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        fs::write(outside.join("valid"), rec(2, false).encode()).unwrap();
        std::os::unix::fs::symlink(outside.join("valid"), d.join("EX-1.rounds")).unwrap();
        assert!(caps.read("EX-1", "r").is_err(), "a link to a valid record is refused");
        fs::remove_file(d.join("EX-1.rounds")).unwrap();
        fs::write(d.join("EX-1.rounds"), rec(2, false).encode()).unwrap();
        assert_eq!(caps.read("EX-1", "r"), Ok(Some(rec(2, false))), "twin: the same bytes as a regular file");
        let _ = fs::remove_dir_all(&d);
        let _ = fs::remove_dir_all(&outside);
    }

    const H1: &str = "1111111111111111111111111111111111111111";
    const H2: &str = "2222222222222222222222222222222222222222";

    fn streak(sig: &str, head: &str, count: u8) -> Streak {
        Streak { ticket: "EX-1".to_string(), repo_id: "r".to_string(), sig: sig.to_string(), head: head.to_string(), count }
    }

    // AC 18: the golden bytes of the streak file in each state, and the strict decoder.
    #[test]
    fn the_streak_bytes_are_pinned_and_the_decoder_is_strict() {
        let one = streak("t:check-failed", H1, 1);
        assert_eq!(
            one.encode(),
            "{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"sig\":\"t:check-failed\",\"head\":\"1111111111111111111111111111111111111111\",\"count\":1}\n"
        );
        assert!(streak("t:check-failed", H2, 2).encode().ends_with("\"count\":2}\n"));
        for c in 1..=REPLAN_AFTER {
            let s = streak("t:check-failed", H1, c);
            assert_eq!(Streak::decode(s.encode().as_bytes(), "EX-1", "r"), Ok(s), "round trip");
        }
        let good = one.encode();
        let bad: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"nope\n".to_vec(),
            good.replace("\"v\":1", "\"v\":2").into_bytes(),
            good.replace("\"count\":1", "\"count\":0").into_bytes(),
            good.replace("\"count\":1", "\"count\":3").into_bytes(),
            good.replace("\"count\":1", "\"count\":-1").into_bytes(),
            good.replace("\"count\":1", "\"count\":\"1\"").into_bytes(),
            good.replace("\"count\":1", "\"count\":01").into_bytes(),
            good.replace("t:check-failed", "tcheck-failed").into_bytes(),
            good.replace("t:check-failed", "T:check-failed").into_bytes(),
            good.replace("t:check-failed", "t:check failed").into_bytes(),
            good.replace("t:check-failed", &format!("{}:c", "a".repeat(SIG_MAX - 1))).into_bytes(),
            good.replace(H1, "abc").into_bytes(),
            good.replace(H1, &"A".repeat(40)).into_bytes(),
            format!("{good}{good}").into_bytes(),
            good.trim_end().as_bytes().to_vec(),
            [good.as_bytes(), b"\0"].concat(),
            vec![0xff, 0xfe],
            good.replace("\"head\":\"1111111111111111111111111111111111111111\",\"count\":1", "\"count\":1,\"head\":\"1111111111111111111111111111111111111111\"").into_bytes(),
        ];
        for b in &bad {
            assert!(Streak::decode(b, "EX-1", "r").is_err(), "{:?} must be refused", String::from_utf8_lossy(b));
        }
        assert!(Streak::decode(good.as_bytes(), "EX-2", "r").is_err(), "another ticket's streak");
        assert!(Streak::decode(good.as_bytes(), "EX-1", "q").is_err(), "another repo's streak");
    }

    // AC 18: every row of the transition table.
    #[test]
    fn the_streak_transition_table_is_pinned() {
        let a = "t:check-failed";
        let b = "u:check-failed";
        let none = advance(None, "EX-1", "r", Some((a, H1)));
        assert_eq!(none, Some(streak(a, H1, 1)), "FAIL, no streak");
        let one = streak(a, H1, 1);
        assert_eq!(advance(Some(&one), "EX-1", "r", Some((a, H2))), Some(streak(a, H2, 2)), "FAIL, different head, same signature");
        assert_eq!(advance(Some(&one), "EX-1", "r", Some((b, H2))), Some(streak(b, H2, 1)), "FAIL, different head, different signature");
        assert_eq!(advance(Some(&one), "EX-1", "r", Some((a, H1))), Some(one.clone()), "FAIL, same head, same signature: a plain re-run");
        assert_eq!(advance(Some(&one), "EX-1", "r", Some((b, H1))), Some(streak(b, H1, 1)), "FAIL, same head, different signature: a flaky check");
        assert_eq!(advance(Some(&one), "EX-1", "r", None), None, "PASS clears");
        assert_eq!(advance(None, "EX-1", "r", None), None, "PASS with no streak");
        let two = streak(a, H2, 2);
        assert_eq!(advance(Some(&two), "EX-1", "r", Some((a, H1))), Some(streak(a, H1, 2)), "the count never passes the threshold");
        assert_eq!(advance(Some(&two), "EX-1", "r", Some((a, H2))), Some(two.clone()), "same head and signature at the threshold");
        assert_eq!(advance(Some(&two), "EX-1", "r", None), None);
        assert!(!one.latched() && two.latched(), "latched iff the count reached the threshold");
    }

    // AC 18: the start rule over rounds 0-4 x latched or not.
    #[test]
    fn the_start_rule_refuses_a_latched_ticket_first_and_then_the_cap() {
        let open = streak("t:check-failed", H1, 1);
        let latched = streak("t:check-failed", H2, 2);
        for rounds in 0..=MAX_FIX_ROUNDS {
            let r = start_check(rounds, Some(&latched)).unwrap_err();
            assert_eq!((r.kind, r.reason), (Kind::ReplanRequired, "replan-threshold"), "latched at {rounds} rounds");
            for streak in [None, Some(&open)] {
                match start_check(rounds, streak) {
                    Ok(()) => assert!(rounds < MAX_FIX_ROUNDS, "{rounds} rounds, not latched"),
                    Err(r) => assert_eq!((rounds >= MAX_FIX_ROUNDS, r.kind, r.reason), (true, Kind::Escalation, "fix-round-cap")),
                }
            }
        }
    }

    // AC 18: the signature builder and its bound.
    #[test]
    fn the_signature_is_bounded_and_fixed_alphabet() {
        assert_eq!(signature("t", "check-failed"), Ok("t:check-failed".to_string()));
        let longest_class = "passes-at-base";
        let gate250 = "a".repeat(250);
        let sig = signature(&gate250, longest_class).unwrap();
        assert_eq!(sig.len(), 265);
        assert!(sig.len() <= SIG_MAX && signature_ok(&sig));
        assert!(signature(&"a".repeat(SIG_MAX), "c").is_err(), "321 bytes");
        assert!(signature(&"a".repeat(SIG_MAX - 2), "c").is_ok(), "exactly SIG_MAX");
        for (g, c) in [("", "c"), ("g", ""), ("G", "c"), ("g", "C"), ("g:h", "c"), ("g", "c:d"), ("g h", "c"), ("g", "c.d"), ("g", "c d")] {
            assert!(signature(g, c).is_err(), "{g:?}/{c:?}");
        }
        assert!(!signature_ok("no-colon") && !signature_ok("a:b:c"));
    }

    #[test]
    fn the_streak_file_is_written_read_and_cleared_under_the_lock() {
        let d = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        assert_eq!(caps.read_streak("EX-1", "r"), Ok(None));
        let held = caps.lock("EX-1").unwrap();
        let s = streak("t:check-failed", H1, 1);
        caps.write_streak(&s, &held).unwrap();
        assert_eq!(fs::read_to_string(d.join("EX-1.replan")).unwrap(), s.encode());
        assert_eq!(caps.read_streak("EX-1", "r"), Ok(Some(s.clone())));
        assert!(caps.read_streak("EX-1", "q").is_err(), "another repo");
        fs::write(d.join(".EX-1.replan.tmp"), "junk").unwrap();
        caps.write_streak(&streak("t:check-failed", H2, 2), &held).unwrap();
        assert!(!d.join(".EX-1.replan.tmp").exists(), "a stale temp is removed");
        caps.clear_streak("EX-1", &held).unwrap();
        assert!(!d.join("EX-1.replan").exists() && caps.read_streak("EX-1", "r") == Ok(None));
        caps.clear_streak("EX-1", &held).unwrap();
        fs::write(d.join("ex-1.replan"), "x").unwrap();
        assert!(caps.read_streak("EX-1", "r").is_err() && caps.check_names("EX-1").is_err(), "a case twin of the streak name");
        assert!(caps.read_streak("EX-2", "r").is_ok(), "twin: another ticket");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_write_then_a_read_round_trips_and_a_second_lock_is_busy() {
        let d = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        assert_eq!(caps.read("EX-1", "r"), Ok(None));
        let held = caps.lock("EX-1").unwrap();
        assert_eq!(caps.lock("EX-1").unwrap_err(), LockError::Busy, "the same ticket is busy");
        assert!(caps.lock("EX-2").is_ok(), "twin: another ticket is free");
        caps.write(&rec(2, true), &held).unwrap();
        assert_eq!(caps.read("EX-1", "r"), Ok(Some(rec(2, true))));
        assert_eq!(caps.rounds("EX-1", "r"), Ok(3));
        drop(held);
        assert!(caps.lock("EX-1").is_ok(), "released with the guard");
        let _ = fs::remove_dir_all(&d);
    }
}
