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
/// The reason token of the per-ticket wall-time budget refusal.
pub const REASON_WALL_TIME_BUDGET: &str = "wall-time-budget";
/// The budget file schema version this reader accepts.
pub const BUDGET_VERSION: u64 = 1;

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

/// The wall-time meter of a ticket: written once, at the first paid run, and never rewritten. `size` is pinned at that moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub ticket: String,
    pub repo_id: String,
    /// `S`, `M` or `L`.
    pub size: String,
    /// Milliseconds since the epoch, 1..=u64::MAX.
    pub started_ms: u64,
}

impl Budget {
    pub fn size_ok(size: &str) -> bool {
        matches!(size, "S" | "M" | "L")
    }

    /// The canonical bytes, newline included.
    pub fn encode(&self) -> String {
        format!(
            "{{\"v\":{},\"ticket\":\"{}\",\"repo_id\":\"{}\",\"size\":\"{}\",\"started_ms\":{}}}\n",
            BUDGET_VERSION, self.ticket, self.repo_id, self.size, self.started_ms
        )
    }

    /// Strict decode: the bytes must equal the canonical encoding of a valid budget for exactly this ticket and repo. `started_ms` is canonical
    /// digits (no sign, no leading zero, not 0, at most 20 digits) that fit u64; a larger value is refused, never wrapped or saturated.
    pub fn decode(bytes: &[u8], ticket: &str, repo_id: &str) -> Result<Budget, String> {
        let refused = || "the budget file is not the canonical budget for this ticket and repo".to_string();
        let text = std::str::from_utf8(bytes).map_err(|_| refused())?;
        let lead = format!("{{\"v\":{},\"ticket\":\"{}\",\"repo_id\":\"{}\",\"size\":\"", BUDGET_VERSION, ticket, repo_id);
        let rest = text.strip_prefix(lead.as_str()).ok_or_else(refused)?;
        let (size, rest) = rest.split_at(rest.find('"').ok_or_else(refused)?);
        let digits = rest.strip_prefix("\",\"started_ms\":").and_then(|r| r.strip_suffix("}\n")).ok_or_else(refused)?;
        let canonical = !digits.is_empty() && digits.len() <= 20 && !digits.starts_with('0') && digits.bytes().all(|b| b.is_ascii_digit());
        if !canonical || !Budget::size_ok(size) {
            return Err(refused());
        }
        let started_ms = digits.parse::<u64>().map_err(|_| refused())?;
        let b = Budget { ticket: ticket.to_string(), repo_id: repo_id.to_string(), size: size.to_string(), started_ms };
        if b.encode().as_bytes() != bytes {
            return Err(refused());
        }
        Ok(b)
    }
}

/// One read of the wall clock in milliseconds since the epoch. A clock before the epoch is an error (never 0).
pub fn now_ms() -> Result<u64, String> {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "the clock is before the epoch".to_string())?;
    Ok(d.as_millis().min(u64::MAX as u128) as u64)
}

/// Is the wall-time budget spent? `started_ms` is `None` when the clock has not started (not spent). A clock behind the start is an error (fail
/// closed, never elapsed 0). `now - start >= limit` is spent; `start + limit` is never computed, and `wall_seconds * 1000` saturates.
pub fn budget_spent(started_ms: Option<u64>, now_ms: u64, wall_seconds: u64) -> Result<bool, String> {
    let Some(start) = started_ms else { return Ok(false) };
    if now_ms < start {
        return Err("the clock is behind the ticket's budget start".to_string());
    }
    Ok(now_ms - start >= wall_seconds.saturating_mul(1000))
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

/// May a run start, with the wall-time budget known? Order: a latched streak (`replan-threshold`), then a spent budget (`wall-time-budget`), then
/// the fix-round cap.
pub fn start_check_budget(rounds: u8, streak: Option<&Streak>, spent: bool) -> Result<(), Refusal> {
    if streak.map(|s| s.latched()).unwrap_or(false) {
        return Err(Refusal { kind: Kind::ReplanRequired, reason: REASON_REPLAN_THRESHOLD });
    }
    if spent {
        return Err(Refusal { kind: Kind::BudgetExceeded, reason: REASON_WALL_TIME_BUDGET });
    }
    may_start(rounds)
}

/// The event that records a refusal. The `ticket` field is the lowercased id (an event ticket is a lowercase token), and
/// `role`/`model` come from the caller (never defaulted); the run id is filled by the v2 writer, never here.
pub fn event_for(r: &Refusal, ticket: &str, repo_id: &str, ts_ms: u64, role: &str, model: &str) -> Event {
    Event {
        kind: r.kind,
        ts_ms,
        run_id: String::new(),
        ticket: ticket.to_ascii_lowercase(),
        repo: repo_id.to_string(),
        reason: r.reason.to_string(),
        role: role.to_string(),
        model: model.to_string(),
    }
}

/// One event-identity field read from the environment: a missing value or a non-token value is refused (the event is not
/// recorded), because `role` and `model` are never guessed or defaulted.
fn env_field(name: &str) -> Result<String, String> {
    match std::env::var_os(name) {
        None => Err(format!("EVENT NOT RECORDED: {name} is not set; the role and model of the run are never guessed")),
        Some(v) => {
            let s = v.to_string_lossy().to_string();
            if events::token_ok(&s) {
                Ok(s)
            } else {
                Err(format!("EVENT NOT RECORDED: {name} must be a plain token (lowercase letters, digits, '-', '_', '.')"))
            }
        }
    }
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

    fn budget_name(id: &str) -> String {
        format!("{}.budget", id)
    }

    /// The same rule for the budget file name. Used only by the budget reader and writer (`check_names` is not widened).
    fn check_budget_case_twin(&self, id: &str) -> Result<(), String> {
        self.twin_of(&Caps::budget_name(id), "a budget file whose name differs only by letter case already exists")
    }

    /// The budget reader: `Ok(None)` when the clock has not started, `Ok(Some)` for a canonical budget for this ticket and repo, `Err` for anything
    /// else (a link, a non-regular entry, an oversize or malformed file, a wrong identity, a case twin). Never read as "not started".
    pub fn read_budget(&self, id: &str, repo_id: &str) -> Result<Option<Budget>, String> {
        Caps::id_ok(id)?;
        self.check_budget_case_twin(id)?;
        let path = self.dir.join(Caps::budget_name(id));
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("the budget file exists and is not a regular file".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err("the budget file could not be inspected".to_string()),
        }
        let f = File::open(&path).map_err(|_| "the budget file could not be opened".to_string())?;
        if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
            return Err("the budget file exists and is not a regular file".to_string());
        }
        let bytes = read_bounded(f)?;
        Budget::decode(&bytes, id, repo_id).map(Some)
    }

    /// Write the budget once, with the lock held: `Ok(true)` when written, `Ok(false)` when a regular file already exists (left untouched, never
    /// rewritten). A stale temp is removed by name (never opened for truncation); then `create_new`, sync, rename, folder sync.
    pub fn write_budget_once(&self, b: &Budget, _held: &Lock) -> Result<bool, String> {
        Caps::id_ok(&b.ticket)?;
        if !Budget::size_ok(&b.size) || b.started_ms == 0 {
            return Err("the budget state is out of range".to_string());
        }
        self.check_budget_case_twin(&b.ticket)?;
        let target = self.dir.join(Caps::budget_name(&b.ticket));
        let tmp = self.dir.join(format!(".{}.budget.tmp", b.ticket));
        match fs::symlink_metadata(&target) {
            Ok(m) if m.file_type().is_file() => return Ok(false),
            Ok(_) => return Err("the budget file exists and is not a regular file".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the budget file could not be inspected".to_string()),
        }
        match fs::symlink_metadata(&tmp) {
            Ok(_) => fs::remove_file(&tmp).map_err(|_| "a stale budget temp file could not be removed".to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the budget temp file could not be inspected".to_string()),
        }
        let mut f = OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|_| "the budget file could not be created".to_string())?;
        if f.write_all(b.encode().as_bytes()).and_then(|_| f.sync_all()).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err("the budget file could not be written".to_string());
        }
        drop(f);
        if fs::rename(&tmp, &target).is_err() {
            let _ = fs::remove_file(&tmp);
            return Err("the budget file could not be put in place".to_string());
        }
        File::open(&self.dir).and_then(|d| d.sync_all()).map_err(|_| "the caps directory could not be synced".to_string())?;
        Ok(true)
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
    /// resolves inside this folder (for example onto a record). The run identity comes from the environment: `HARNESS_RUN_ID` when set (accepted only
    /// if this log shows it was issued), else a self-issued id whose `run_issued` line is written first. `HARNESS_ROLE` and `HARNESS_MODEL` must be set
    /// and be plain tokens; a missing or bad value refuses the EVENT (which stays refused: a failed write never turns a refusal into a pass).
    pub fn escalate(&self, r: &Refusal, id: &str, repo_id: &str) -> Result<(), String> {
        let role = env_field("HARNESS_ROLE")?;
        let model = env_field("HARNESS_MODEL")?;
        let inherited = std::env::var_os("HARNESS_RUN_ID").map(|v| v.to_string_lossy().to_string());
        self.escalate_with(r, id, repo_id, &role, &model, inherited.as_deref()).map(|_| ())
    }

    /// `escalate` with the identity given explicitly (used by the env-reading wrapper above and by tests); returns the run id.
    pub fn escalate_with(&self, r: &Refusal, id: &str, repo_id: &str, role: &str, model: &str, inherited: Option<&str>) -> Result<String, String> {
        let log = self.dir.join(EVENTS_FILE);
        match fs::symlink_metadata(&log) {
            Ok(m) if m.file_type().is_file() => {}
            Ok(_) => return Err("the event log entry is not a regular file".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("the event log entry could not be inspected".to_string()),
        }
        let ts_ms = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        let ev = event_for(r, id, repo_id, ts_ms, role, model);
        events::append_under_run(&self.dir, &log, &ev, inherited).map_err(|e| e.to_string())
    }
}

/// Test-only gate between tests that start child processes (read side) and tests that lock a ticket and expect the lock to be free again after
/// the guard drops (write side). A child forked while a lock file is open holds a copy of it until it execs, so a re-lock in that window reads as
/// busy. Many readers may spawn at once; the write side runs alone, so no child can be mid-fork while it checks.
#[cfg(test)]
pub(crate) mod spawn_gate {
    use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
    static GATE: RwLock<()> = RwLock::new(());
    pub(crate) fn read() -> RwLockReadGuard<'static, ()> {
        GATE.read().unwrap_or_else(|e| e.into_inner())
    }
    pub(crate) fn write() -> RwLockWriteGuard<'static, ()> {
        GATE.write().unwrap_or_else(|e| e.into_inner())
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

    // AC 21: the shared writer encodes a refusal of each existing kind, each under its own self-issued run.
    #[test]
    fn the_refusal_writer_is_not_tied_to_the_escalation_kind() {
        let d = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        for (kind, reason) in [(Kind::Escalation, "fix-round-cap"), (Kind::ReplanRequired, "replan-threshold"), (Kind::BudgetExceeded, "budget-spent")] {
            let r = Refusal { kind, reason };
            let e = event_for(&r, "EX-1", "r", 7, "executor", "m-1");
            assert_eq!(
                (e.kind, e.ticket.as_str(), e.repo.as_str(), e.reason.as_str(), e.ts_ms, e.role.as_str(), e.model.as_str()),
                (kind, "ex-1", "r", reason, 7, "executor", "m-1")
            );
            caps.escalate_with(&r, "EX-1", "r", "executor", "m-1", None).unwrap();
        }
        let got = events::read_log(&d.join(EVENTS_FILE)).unwrap();
        assert!(got.is_clean());
        assert_eq!(
            got.events.iter().map(|e| e.kind).collect::<Vec<_>>(),
            vec![Kind::RunIssued, Kind::Escalation, Kind::RunIssued, Kind::ReplanRequired, Kind::RunIssued, Kind::BudgetExceeded],
            "each self-issued refusal writes its run_issued line first"
        );
        let ids: Vec<&str> = got.events.iter().map(|e| e.run_id.as_str()).collect();
        assert!(ids[0] != ids[2] && ids[2] != ids[4], "each invocation issues its own id");
        for e in &got.events {
            assert_eq!((e.role.as_str(), e.model.as_str(), e.ticket.as_str()), ("executor", "m-1", "ex-1"));
        }
        let _ = fs::remove_dir_all(&d);
    }

    // An inherited id is reused for the escalation only when this log shows it was issued.
    #[test]
    fn an_escalation_under_an_inherited_run_id_reuses_it_or_refuses() {
        let d = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        let r = Refusal { kind: Kind::Escalation, reason: REASON_FIX_ROUND_CAP };
        let id = caps.escalate_with(&r, "EX-1", "r", "executor", "m-1", None).unwrap();
        // Issued id: exactly one new line, no new run_issued.
        caps.escalate_with(&r, "EX-1", "r", "reviewer", "m-2", Some(&id)).unwrap();
        let got = events::read_log(&d.join(EVENTS_FILE)).unwrap();
        assert_eq!(got.events.len(), 3);
        assert_eq!(got.events.iter().filter(|e| e.kind == Kind::RunIssued).count(), 1);
        assert_eq!((got.events[2].run_id.as_str(), got.events[2].role.as_str(), got.events[2].model.as_str()), (id.as_str(), "reviewer", "m-2"));
        // Unissued id: refused, nothing written; the refusal itself is the caller's to keep.
        let before = fs::read(d.join(EVENTS_FILE)).unwrap();
        assert!(caps.escalate_with(&r, "EX-1", "r", "executor", "m-1", Some("never-issued")).is_err());
        assert_eq!(fs::read(d.join(EVENTS_FILE)).unwrap(), before);
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
        let _spawn_gate = spawn_gate::write();
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

    /// Load test for the cause of the flake: child processes started by other threads must never make a released lock look busy.
    /// Four threads start children (each holding the read side of the gate); the main thread locks, drops and re-locks under the write side.
    /// Without the write side this fails within a few rounds on a normal machine; with it, never.
    #[cfg(unix)]
    #[test]
    fn children_started_by_other_tests_never_make_a_released_lock_look_busy() {
        use std::process::{Command, Stdio};
        use std::sync::atomic::AtomicBool;
        let d = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let loaders: Vec<_> = (0..4)
            .map(|_| {
                let stop = stop.clone();
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _spawn_gate = spawn_gate::read();
                        let _ = Command::new("true").stdin(Stdio::null()).output();
                    }
                })
            })
            .collect();
        let mut busy = 0;
        for _ in 0..50 {
            let _spawn_gate = spawn_gate::write();
            drop(caps.lock("EX-1").unwrap());
            if caps.lock("EX-1").is_err() {
                busy += 1;
            }
        }
        stop.store(true, Ordering::Relaxed);
        for l in loaders {
            l.join().unwrap();
        }
        assert_eq!(busy, 0, "a re-lock after the guard dropped was refused {busy} times");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_write_then_a_read_round_trips_and_a_second_lock_is_busy() {
        let _spawn_gate = spawn_gate::write();
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

    fn bud(size: &str, started_ms: u64) -> Budget {
        Budget { ticket: "EX-1".to_string(), repo_id: "r".to_string(), size: size.to_string(), started_ms }
    }

    // A1: the golden bytes of the budget file and the round trip for each size.
    #[test]
    fn the_budget_bytes_are_pinned_and_round_trip() {
        assert_eq!(bud("M", 1761955200000).encode(), "{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"size\":\"M\",\"started_ms\":1761955200000}\n");
        assert_eq!(bud("S", 1).encode(), "{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"size\":\"S\",\"started_ms\":1}\n");
        for size in ["S", "M", "L"] {
            for ms in [1, 1761955200000, u64::MAX] {
                let b = bud(size, ms);
                assert_eq!(Budget::decode(b.encode().as_bytes(), "EX-1", "r"), Ok(b), "round trip {size} {ms}");
            }
        }
    }

    // A2: the strict decoder.
    #[test]
    fn the_budget_decoder_is_strict() {
        let good = bud("M", 1761955200000).encode();
        assert!(Budget::decode(good.as_bytes(), "EX-1", "r").is_ok(), "twin: the canonical bytes");
        let with = |from: &str, to: &str| good.replace(from, to).into_bytes();
        let bad: Vec<Vec<u8>> = vec![
            b"".to_vec(),
            b"nope\n".to_vec(),
            with("\"v\":1", "\"v\":2"),
            with("\"M\"", "\"s\""),
            with("\"M\"", "\"XL\""),
            with("\"M\"", "\"\""),
            with("1761955200000", "0"),
            with("1761955200000", "-1"),
            with("1761955200000", "01761955200000"),
            with("1761955200000", "\"5\""),
            with("1761955200000", "18446744073709551616"),
            with("1761955200000", "184467440737095516150"),
            with("1761955200000", ""),
            with("\"started_ms\":1761955200000}", "\"started_ms\":1761955200000,\"x\":1}"),
            with("\"size\":\"M\",\"started_ms\":1761955200000", "\"started_ms\":1761955200000,\"size\":\"M\""),
            with("\"v\":1,\"ticket\":\"EX-1\"", "\"ticket\":\"EX-1\",\"v\":1"),
            with(",\"started_ms\":1761955200000", ""),
            with("}\n", "}\r\n"),
            format!(" {good}").into_bytes(),
            format!("\u{feff}{good}").into_bytes(),
            format!("{good}{good}").into_bytes(),
            good.trim_end().as_bytes().to_vec(),
            [good.as_bytes(), b"\0"].concat(),
            vec![0xff, 0xfe],
            vec![b'x'; 4097],
        ];
        for b in &bad {
            assert!(Budget::decode(b, "EX-1", "r").is_err(), "{:?} must be refused", String::from_utf8_lossy(b));
        }
        assert!(Budget::decode(bud("M", u64::MAX).encode().as_bytes(), "EX-1", "r").is_ok(), "u64::MAX itself is legal");
        assert!(Budget::decode(good.as_bytes(), "EX-2", "r").is_err(), "another ticket's file");
        assert!(Budget::decode(good.as_bytes(), "EX-1", "q").is_err(), "another repo's file");
        assert!(read_bounded(&vec![b'x'; 4097][..]).is_err(), "the reader's cap is 4 KiB");
    }

    // A3: the spent rule over a table.
    #[test]
    fn the_spent_rule_is_pinned() {
        assert_eq!(budget_spent(None, 5, 60), Ok(false), "absent: the clock has not started");
        let (start, secs) = (1_000_000u64, 60u64);
        let limit = secs * 1000;
        assert_eq!(budget_spent(Some(start), start + limit - 1, secs), Ok(false), "one ms inside");
        assert_eq!(budget_spent(Some(start), start + limit, secs), Ok(true), "exactly at the limit is spent");
        assert_eq!(budget_spent(Some(start), start, secs), Ok(false), "elapsed 0");
        assert!(budget_spent(Some(start), start - 1, secs).is_err(), "clock behind the start");
        assert!(budget_spent(Some(u64::MAX), 5, secs).is_err(), "a start of u64::MAX is behind, no panic");
        assert_eq!(budget_spent(Some(u64::MAX), u64::MAX, secs), Ok(false), "equal to u64::MAX: elapsed 0, no overflow");
        let big = 999_999_999_999_999u64;
        assert_eq!(budget_spent(Some(1), 1 + big * 1000 - 1, big), Ok(false), "15-digit seconds, exact x 1000");
        assert_eq!(budget_spent(Some(1), 1 + big * 1000, big), Ok(true));
        assert_eq!(budget_spent(Some(1), u64::MAX, u64::MAX), Ok(false), "a huge limit saturates and never overflows");
        assert!(now_ms().unwrap() > 1_000_000_000_000, "the real clock reads");
    }

    // A4: the start order over rounds x latched x spent.
    #[test]
    fn the_start_order_is_replan_then_budget_then_cap() {
        let open = streak("t:check-failed", H1, 1);
        let latched = streak("t:check-failed", H2, 2);
        for rounds in 0..=MAX_FIX_ROUNDS {
            for (st, is_latched) in [(None, false), (Some(&open), false), (Some(&latched), true)] {
                for spent in [false, true] {
                    let got = start_check_budget(rounds, st, spent);
                    let want = if is_latched {
                        Err((Kind::ReplanRequired, "replan-threshold"))
                    } else if spent {
                        Err((Kind::BudgetExceeded, "wall-time-budget"))
                    } else if rounds >= MAX_FIX_ROUNDS {
                        Err((Kind::Escalation, "fix-round-cap"))
                    } else {
                        Ok(())
                    };
                    assert_eq!(got.map_err(|r| (r.kind, r.reason)), want, "rounds {rounds} latched {is_latched} spent {spent}");
                }
            }
        }
        assert_eq!(REASON_WALL_TIME_BUDGET, "wall-time-budget");
        assert_eq!(start_check(MAX_FIX_ROUNDS, None).unwrap_err().reason, "fix-round-cap", "the merged rule is unchanged");
    }

    // A5: the writer under the lock, the reader, and the case-twin scan.
    #[test]
    fn the_budget_file_is_written_once_and_never_through_a_link() {
        let d = scratch();
        let outside = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        assert_eq!(caps.read_budget("EX-1", "r"), Ok(None));
        let held = caps.lock("EX-1").unwrap();
        let b = bud("M", 1761955200000);
        assert_eq!(caps.write_budget_once(&b, &held), Ok(true), "twin: the plain write");
        assert_eq!(fs::read_to_string(d.join("EX-1.budget")).unwrap(), b.encode());
        assert_eq!(caps.read_budget("EX-1", "r"), Ok(Some(b.clone())));
        assert!(caps.read_budget("EX-1", "q").is_err(), "another repo");
        assert_eq!(caps.write_budget_once(&bud("L", 5), &held), Ok(false), "written once: a second write changes nothing");
        assert_eq!(fs::read_to_string(d.join("EX-1.budget")).unwrap(), b.encode());
        // a stale regular temp is removed by name, a temp link is removed and its target left alone
        let d2 = scratch();
        let caps2 = Caps::open(&d2, Path::new("/nonexistent-root")).unwrap();
        let held2 = caps2.lock("EX-1").unwrap();
        fs::write(d2.join(".EX-1.budget.tmp"), "junk").unwrap();
        assert_eq!(caps2.write_budget_once(&b, &held2), Ok(true));
        assert!(!d2.join(".EX-1.budget.tmp").exists(), "a stale temp is removed");
        fs::remove_file(d2.join("EX-1.budget")).unwrap();
        fs::write(outside.join("victim"), "victim bytes").unwrap();
        std::os::unix::fs::symlink(outside.join("victim"), d2.join(".EX-1.budget.tmp")).unwrap();
        assert_eq!(caps2.write_budget_once(&b, &held2), Ok(true));
        assert_eq!(fs::read_to_string(outside.join("victim")).unwrap(), "victim bytes", "the temp link's target is untouched");
        assert_eq!(fs::read_to_string(d2.join("EX-1.budget")).unwrap(), b.encode());
        // a link or a directory at the budget name is refused (reader and writer)
        let d3 = scratch();
        let caps3 = Caps::open(&d3, Path::new("/nonexistent-root")).unwrap();
        let held3 = caps3.lock("EX-1").unwrap();
        fs::write(outside.join("valid"), b.encode()).unwrap();
        std::os::unix::fs::symlink(outside.join("valid"), d3.join("EX-1.budget")).unwrap();
        assert!(caps3.read_budget("EX-1", "r").is_err(), "reader: a link to a valid file");
        assert!(caps3.write_budget_once(&b, &held3).is_err(), "writer: a link");
        fs::remove_file(d3.join("EX-1.budget")).unwrap();
        fs::create_dir(d3.join("EX-1.budget")).unwrap();
        assert!(caps3.read_budget("EX-1", "r").is_err() && caps3.write_budget_once(&b, &held3).is_err(), "a directory");
        // a directory at the temp name makes the write fail
        let d4 = scratch();
        let caps4 = Caps::open(&d4, Path::new("/nonexistent-root")).unwrap();
        let held4 = caps4.lock("EX-1").unwrap();
        fs::create_dir(d4.join(".EX-1.budget.tmp")).unwrap();
        assert!(caps4.write_budget_once(&b, &held4).is_err());
        assert!(!d4.join("EX-1.budget").exists());
        // a case twin is refused by name, for the reader and the writer; the merged check_names is not widened
        let d5 = scratch();
        let caps5 = Caps::open(&d5, Path::new("/nonexistent-root")).unwrap();
        let held5 = caps5.lock("EX-1").unwrap();
        fs::write(d5.join("ex-1.budget"), "x").unwrap();
        assert!(caps5.read_budget("EX-1", "r").is_err() && caps5.write_budget_once(&b, &held5).is_err(), "a case twin");
        assert!(caps5.check_names("EX-1").is_ok(), "check_names does not scan budget names");
        assert!(caps5.read_budget("EX-2", "r").is_ok(), "twin: another ticket");
        for p in [&d, &d2, &d3, &d4, &d5, &outside] {
            let _ = fs::remove_dir_all(p);
        }
    }

    fn names_in(d: &Path) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }

    // A5 / A2: the writer's own range guard, the reader's own 4 KiB cap, and a case twin that would otherwise decode.
    #[test]
    fn the_budget_writer_and_reader_guards_are_pinned() {
        let d = scratch();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        let held = caps.lock("EX-1").unwrap();
        let before = names_in(&d);
        for bad in [bud("M", 0), bud("XL", 5), bud("m", 5), bud("", 5)] {
            assert!(caps.write_budget_once(&bad, &held).is_err(), "{bad:?} must be refused");
            assert_eq!(names_in(&d), before, "nothing is left behind by {bad:?}");
        }
        assert_eq!(caps.write_budget_once(&bud("M", 1), &held), Ok(true), "twin: a legal budget is written");
        drop(held);

        // canonical for this ticket and repo, but over 4 KiB: the reader's own cap refuses it
        let d2 = scratch();
        let caps2 = Caps::open(&d2, Path::new("/nonexistent-root")).unwrap();
        let long = "r".repeat(4100);
        let big = Budget { ticket: "EX-1".to_string(), repo_id: long.clone(), size: "M".to_string(), started_ms: 7 };
        assert!(big.encode().len() > 4097);
        assert_eq!(Budget::decode(big.encode().as_bytes(), "EX-1", &long), Ok(big.clone()), "the bytes are canonical");
        fs::write(d2.join("EX-1.budget"), big.encode()).unwrap();
        assert!(caps2.read_budget("EX-1", &long).is_err(), "over 4 KiB is refused by the reader");

        // a case twin holding the canonical bytes for this ticket is still refused, on any filesystem
        let d3 = scratch();
        let caps3 = Caps::open(&d3, Path::new("/nonexistent-root")).unwrap();
        fs::write(d3.join("ex-1.budget"), bud("M", 7).encode()).unwrap();
        assert!(caps3.read_budget("EX-1", "r").is_err(), "a canonical case twin is refused");
        for p in [&d, &d2, &d3] {
            let _ = fs::remove_dir_all(p);
        }
    }
}
