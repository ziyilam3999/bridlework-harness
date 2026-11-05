//! Worker death record (std only, unix only): P1B-07, ticket #210.
//!
//! When a ticket process ends without being asked to — a time limit stopped it, the hard step line
//! stopped it, or it simply vanished — the supervisor records ONE death before it crosses the entry off
//! the manifest: a HANDOVER FILE (`<manifest-dir>/<ticket>.deaths/<id>.<pid>.<pid_start_ms>.json`, one
//! file per process life, outside the roots so teardown never touches it) and one existing `timed_out`
//! event whose `handover` field is that path RELATIVE TO THE MANIFEST DIR. This module owns the handover
//! format (encode/decode, fixed key order, 4096-byte cap), the deaths-folder checks (a real directory,
//! ours, mode 0700), the atomic no-replace write (a unique temp file, `sync_all`, then a hard link that
//! FAILS if the name exists — never a replacing rename), the idempotence lookup (an existing handover of
//! the same life is reused and ITS cause/limit win) and the pure clock rule.
//!
//! Crash safety (decision 6 of the plan): the caller stages (log pre-flight, then the handover on disk)
//! BEFORE any signal for a timeout, and commits (the `timed_out` append, skipped when the log already
//! holds this death's line) BEFORE the manifest removes. A crash between any two steps leaves the entry
//! on the sheet, so the next record-mode run finishes the job; a half-written note is never trusted
//! (the no-replace link plus the strict re-read).
//!
//! No program name, argv, cwd, path, ticket title or any free text is ever written: every field is a
//! token, a uuid, a hex sha or a number, so the record cannot carry a secret or a private folder.

use crate::events::{self, Field, Kind, Record};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

/// Byte cap of one handover line (decision 4). A file over it is garbage, never parsed.
pub const HANDOVER_CAP: usize = 4096;
/// Mode of the deaths folder (decision 8): no group or other bit at all.
const FOLDER_MODE: u32 = 0o700;
/// Mode of every handover file.
const FILE_MODE: u32 = 0o600;

/// Open flag for reading a handover at the final name (fix r1 F2, corrected r2 N5): NON-BLOCKING only,
/// so a FIFO or socket swapped in at the name between the metadata gate and the open cannot hang the
/// verb with the per-ticket lock held. NO no-follow is claimed for the FLAGS: the per-platform
/// `O_NOFOLLOW` values differ (macOS `0x100`, Linux `0o400000` on x86_64 only) and the r1 values were
/// wrong on macOS, so the door out of the folder is closed a stronger way — the no-follow metadata gate
/// BEFORE the open, then `opened_is_gate_file` proving the opened HANDLE is that same regular file.
#[cfg(target_os = "linux")]
const OPEN_NONBLOCK: i32 = 0o4000;
#[cfg(target_os = "macos")]
const OPEN_NONBLOCK: i32 = 0x0000_0004;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const OPEN_NONBLOCK: i32 = 0;

/// Why this process ended (decision 1): stopped by a limit (`timeout`), or provably gone before any
/// supervisor signal (`died` — whether it exited cleanly is P1B-08's claim, not this one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    Timeout,
    Died,
}

impl Cause {
    pub fn as_str(&self) -> &'static str {
        match self {
            Cause::Timeout => "timeout",
            Cause::Died => "died",
        }
    }
}

/// Which limit bound the process (decision 4's `limit` field): a time limit, the hard step line, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    Time,
    Step,
    None,
}

impl LimitKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LimitKind::Time => "time",
            LimitKind::Step => "step",
            LimitKind::None => "none",
        }
    }
}

/// Full lowercase uuid `8-4-4-4-12` hex — the event table's own rule (`Shape::Uuid`), restated here so
/// the parser can check a `--session` value with the table's rule before the lock.
pub fn uuid_ok(s: &str) -> bool {
    if s.len() != 36 || !s.is_ascii() {
        return false;
    }
    let hex = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    let mut at = 0usize;
    for (i, len) in [8usize, 4, 4, 4, 12].iter().enumerate() {
        if !hex(&s[at..at + len]) {
            return false;
        }
        at += len;
        if i < 4 {
            if s.as_bytes().get(at) != Some(&b'-') {
                return false;
            }
            at += 1;
        }
    }
    true
}

/// 40 or 64 lowercase hex digits — the event table's own rule (`Shape::Sha`), restated for `--checkpoint`.
pub fn sha_ok(s: &str) -> bool {
    let hex = |t: &str| !t.is_empty() && t.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (s.len() == 40 || s.len() == 64) && hex(s)
}

/// The pure clock rule (decision 8): the wall clock must not read before the entry's own `add` line was
/// written. A clock that does is a detectable backward jump, and the death is refused (kept) rather than
/// recorded with an impossible time.
pub fn clock_ok(now_ms: u64, add_ts_ms: u64) -> bool {
    now_ms >= add_ts_ms
}

/// One death to record: the process life's identity plus everything the handover carries.
pub struct DeathSpec<'a> {
    pub ticket: &'a str,
    pub id: &'a str,
    pub pid: u64,
    pub pid_start_ms: u64,
    pub cause: Cause,
    pub limit: LimitKind,
    pub timeout_ms: u64,
    pub steps: u64,
    pub hard_limit: u64,
    pub repo: &'a str,
    pub checkpoint: &'a str,
    pub session_id: &'a str,
}

/// A staged death: the handover is on disk (or was already there), and the event may follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Staged {
    /// The EFFECTIVE cause: an existing handover of the same life wins over the fresh one (decision 7 —
    /// a timeout whose event write crashed is not later re-labelled `died`).
    pub cause: Cause,
    /// The handover path relative to the manifest dir (the event's `handover` field, a `RelPath`).
    pub relpath: String,
}

/// The deaths folder of `ticket` under manifest dir `dir`.
pub fn deaths_dir(dir: &Path, ticket: &str) -> PathBuf {
    dir.join(format!("{ticket}.deaths"))
}

/// The handover file name of one process life (decision 3's key: a re-used pid gets a different file).
pub fn handover_name(id: &str, pid: u64, pid_start_ms: u64) -> String {
    format!("{id}.{pid}.{pid_start_ms}.json")
}

/// The handover path RELATIVE TO THE MANIFEST DIR (every segment a token, so it passes `RelPath`).
pub fn handover_relpath(ticket: &str, id: &str, pid: u64, pid_start_ms: u64) -> String {
    format!("{ticket}.deaths/{}", handover_name(id, pid, pid_start_ms))
}

// ---------------------------------------------------------------- the handover line

/// The 15 handover keys in fixed order, with which are numbers (decision 4). The encode writes exactly
/// this order; the decode accepts nothing else.
const HANDOVER_KEYS: [(&str, bool); 15] = [
    ("v", true),
    ("ticket", false),
    ("repo", false),
    ("id", false),
    ("pid", true),
    ("pid_start_ms", true),
    ("cause", false),
    ("limit", false),
    ("timeout_ms", true),
    ("steps", true),
    ("hard_limit", true),
    ("detected_ms", true),
    ("checkpoint", false),
    ("recipe", false),
    ("session_id", false),
];

/// One parsed handover line. Every field is shape-constrained: there is no free text to carry a secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Handover {
    pub v: u64,
    pub ticket: String,
    pub repo: String,
    pub id: String,
    pub pid: u64,
    pub pid_start_ms: u64,
    pub cause: String,
    pub limit: String,
    pub timeout_ms: u64,
    pub steps: u64,
    pub hard_limit: u64,
    pub detected_ms: u64,
    pub checkpoint: String,
    pub recipe: String,
    pub session_id: String,
}

impl Handover {
    /// The compact one-line form: fixed key order, every key present, newline-terminated. No value ever
    /// needs a JSON escape (tokens, uuids, hex shas and numbers only), so none is attempted.
    pub fn encode(&self) -> String {
        format!(
            concat!(
                "{{\"v\":{},\"ticket\":\"{}\",\"repo\":\"{}\",\"id\":\"{}\",\"pid\":{},",
                "\"pid_start_ms\":{},\"cause\":\"{}\",\"limit\":\"{}\",\"timeout_ms\":{},\"steps\":{},",
                "\"hard_limit\":{},\"detected_ms\":{},\"checkpoint\":\"{}\",\"recipe\":\"{}\",\"session_id\":\"{}\"}}\n"
            ),
            self.v,
            self.ticket,
            self.repo,
            self.id,
            self.pid,
            self.pid_start_ms,
            self.cause,
            self.limit,
            self.timeout_ms,
            self.steps,
            self.hard_limit,
            self.detected_ms,
            self.checkpoint,
            self.recipe,
            self.session_id,
        )
    }

    /// Strict parse of one handover line: exactly the 15 declared keys, in the declared order, with the
    /// declared types and shapes. The line's own trailing newline is accepted (the file is a
    /// newline-terminated line); anything else — including any other trailing byte — is garbage.
    pub fn decode(text: &str) -> Result<Handover, String> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        let flat = parse_flat_object(text)?;
        if flat.len() != HANDOVER_KEYS.len() {
            return Err(format!("the handover holds {} keys, not {}", flat.len(), HANDOVER_KEYS.len()));
        }
        for (at, (key, is_num)) in HANDOVER_KEYS.iter().enumerate() {
            if flat[at].0 != *key {
                return Err(format!("key {} is {:?}, not {:?}", at + 1, flat[at].0, key));
            }
            let wrong = matches!((&flat[at].1, is_num), (HVal::N(_), false) | (HVal::S(_), true));
            if wrong {
                return Err(format!("key {key} has the wrong type"));
            }
        }
        let s = |k: &str| match flat.iter().find(|(n, _)| n == k) {
            Some((_, HVal::S(v))) => v.clone(),
            _ => unreachable!("the order check above placed every key"),
        };
        let n = |k: &str| match flat.iter().find(|(n, _)| n == k) {
            Some((_, HVal::N(v))) => *v,
            _ => unreachable!("the order check above placed every key"),
        };
        let h = Handover {
            v: n("v"),
            ticket: s("ticket"),
            repo: s("repo"),
            id: s("id"),
            pid: n("pid"),
            pid_start_ms: n("pid_start_ms"),
            cause: s("cause"),
            limit: s("limit"),
            timeout_ms: n("timeout_ms"),
            steps: n("steps"),
            hard_limit: n("hard_limit"),
            detected_ms: n("detected_ms"),
            checkpoint: s("checkpoint"),
            recipe: s("recipe"),
            session_id: s("session_id"),
        };
        if h.v != 1 {
            return Err(format!("v is {}, not 1", h.v));
        }
        for (name, val) in [("ticket", &h.ticket), ("repo", &h.repo), ("id", &h.id)] {
            if !events::token_ok(val) {
                return Err(format!("{name} is not a plain token"));
            }
        }
        if h.cause != "timeout" && h.cause != "died" {
            return Err("cause is neither timeout nor died".to_string());
        }
        if h.limit != "time" && h.limit != "step" && h.limit != "none" {
            return Err("limit is neither time, step nor none".to_string());
        }
        if h.recipe != "commit_push_then_resume" {
            return Err("recipe is not commit_push_then_resume".to_string());
        }
        if !sha_ok(&h.checkpoint) {
            return Err("checkpoint is not a 40 or 64 lowercase hex sha".to_string());
        }
        if !uuid_ok(&h.session_id) {
            return Err("session_id is not a full lowercase uuid".to_string());
        }
        Ok(h)
    }

    /// Is this handover the record of exactly the process life `spec` names?
    fn is_life_of(&self, spec: &DeathSpec<'_>) -> bool {
        self.ticket == spec.ticket && self.id == spec.id && self.pid == spec.pid && self.pid_start_ms == spec.pid_start_ms
    }
}

// ---------------------------------------------------------------- the flat JSON reader

/// One value of a flat object: a string with no escapes, or an unsigned integer.
enum HVal {
    S(String),
    N(u64),
}

/// Parse a flat compact JSON object of string and unsigned-integer values, refusing escapes, control
/// bytes, duplicate keys, leading zeros and any trailing bytes (the same rules the event log's own
/// reader enforces, restated for the handover line).
fn parse_flat_object(text: &str) -> Result<Vec<(String, HVal)>, String> {
    let b = text.as_bytes();
    let mut i = 0usize;
    let ws = |i: &mut usize| {
        while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\r') {
            *i += 1;
        }
    };
    let string = |i: &mut usize| -> Result<String, String> {
        ws(i);
        if b.get(*i) != Some(&b'"') {
            return Err("expected a string".to_string());
        }
        *i += 1;
        let st = *i;
        loop {
            match b.get(*i) {
                None => return Err("unterminated string".to_string()),
                Some(b'"') => break,
                Some(b'\\') => return Err("escape sequences are not allowed".to_string()),
                Some(c) if *c < 0x20 => return Err("raw control byte in string".to_string()),
                Some(_) => *i += 1,
            }
        }
        let s = std::str::from_utf8(&b[st..*i]).map_err(|_| "invalid utf-8".to_string())?.to_string();
        *i += 1;
        Ok(s)
    };
    let value = |i: &mut usize| -> Result<HVal, String> {
        ws(i);
        match b.get(*i) {
            Some(b'"') => Ok(HVal::S(string(i)?)),
            Some(c) if c.is_ascii_digit() => {
                let st = *i;
                while *i < b.len() && b[*i].is_ascii_digit() {
                    *i += 1;
                }
                let t = std::str::from_utf8(&b[st..*i]).map_err(|_| "bad number".to_string())?;
                if t.len() > 1 && t.starts_with('0') {
                    return Err("leading zero".to_string());
                }
                Ok(HVal::N(t.parse::<u64>().map_err(|_| "number out of range".to_string())?))
            }
            _ => Err("unexpected value".to_string()),
        }
    };
    ws(&mut i);
    if b.get(i) != Some(&b'{') {
        return Err("expected '{'".to_string());
    }
    i += 1;
    let mut out: Vec<(String, HVal)> = Vec::new();
    loop {
        let k = string(&mut i)?;
        ws(&mut i);
        if b.get(i) != Some(&b':') {
            return Err("expected ':'".to_string());
        }
        i += 1;
        let v = value(&mut i)?;
        if out.iter().any(|(n, _)| *n == k) {
            return Err("duplicate key".to_string());
        }
        out.push((k, v));
        ws(&mut i);
        match b.get(i) {
            Some(b',') => i += 1,
            Some(b'}') => {
                i += 1;
                break;
            }
            _ => return Err("expected ',' or '}'".to_string()),
        }
    }
    ws(&mut i);
    if i != b.len() {
        return Err("trailing bytes after the object".to_string());
    }
    Ok(out)
}

// ---------------------------------------------------------------- the deaths folder

/// The folder decision table (decision 8), pure so the owner and mode rules are unit-testable. Ok, or
/// the fixed decision-9 reason. A symlink is refused BEFORE anything else: nothing in the deaths path is
/// ever followed up to this point, and nothing is written when this refuses.
pub fn folder_verdict(is_symlink: bool, is_dir: bool, owner_uid: u32, mode: u32, my_uid: u32) -> Result<(), &'static str> {
    if is_symlink {
        return Err("deaths folder is a symlink");
    }
    if !is_dir {
        return Err("deaths folder is not a directory");
    }
    if owner_uid != my_uid {
        return Err("deaths folder is owned by another user");
    }
    if mode & 0o077 != 0 {
        return Err("deaths folder is readable by group or others");
    }
    Ok(())
}

/// Our own uid, read through one exclusively-created temp file in the system temp dir (std has no
/// `getuid`; this is the same std-only route the crate's other ownership checks could use, and a temp
/// dir we cannot write into is itself a refusal-shaped answer). Never trusted for anything but the
/// owner comparison.
fn my_uid() -> Result<u32, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let p = std::env::temp_dir().join(format!("harness-uid-{}-{}", std::process::id(), nonce));
    let f = OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| e.kind().to_string())?;
    let uid = f.metadata().map_err(|e| e.kind().to_string())?.uid();
    drop(f);
    let _ = fs::remove_file(&p);
    Ok(uid)
}

/// Ensure the deaths folder exists and passes every check (decision 8): created 0700 when missing,
/// without following links; an existing folder must be a real directory, ours, with no group/other mode
/// bit. Err carries the fixed decision-9 reason (or the io kind wrapped by the caller).
pub fn ensure_folder(dir: &Path, ticket: &str) -> Result<PathBuf, String> {
    let folder = deaths_dir(dir, ticket);
    let uid = my_uid().map_err(|k| format!("cannot write the handover: {k}"))?;
    match fs::symlink_metadata(&folder) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(FOLDER_MODE).create(&folder).map_err(|e| format!("cannot write the handover: {}", e.kind()))?;
        }
        Err(e) => return Err(format!("cannot write the handover: {}", e.kind())),
        Ok(md) => {
            folder_verdict(md.file_type().is_symlink(), md.is_dir(), md.uid(), md.mode(), uid)?;
            return Ok(folder);
        }
    }
    // A folder this run just created is re-checked through the same door: a racer may have replaced it,
    // and the mode is proven, not assumed.
    let md = fs::symlink_metadata(&folder).map_err(|e| format!("cannot write the handover: {}", e.kind()))?;
    folder_verdict(md.file_type().is_symlink(), md.is_dir(), md.uid(), md.mode(), uid)?;
    Ok(folder)
}

// ---------------------------------------------------------------- staging and committing

/// The log pre-flight (decision 6a): the log must be missing or clean, and its path inside the root —
/// read BEFORE anything is signalled, so a torn log means "refuse", never "stop and discover after".
/// Fix r2 (N2): the read goes through `events::read_log` (the capped, regular-file-proving door of
/// #211) — a plain `fs::read` BLOCKS on a FIFO at the log path with the per-ticket lock held, and the
/// log lives inside the root, where the supervised program itself runs.
fn preflight_log(root: &Path, log: &Path) -> Result<(), String> {
    if let Err(e) = events::check_log_path(root, log) {
        return Err(format!("event log refused: {e}"));
    }
    read_log_for_death(log).map(|_| ())
}

/// The one log reader of the death record (fix r2 N2): a missing log is clean (nothing recorded yet);
/// every other read goes through `events::read_log`, which refuses — without opening — a symlink, a
/// FIFO, a directory or anything else that is not a regular file, proves the opened handle is that
/// same file, and caps the read at 64 MiB. Err carries the reason after `event log refused: `.
fn read_log_for_death(log: &Path) -> Result<events::LogRead, String> {
    if matches!(fs::symlink_metadata(log), Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
        return Ok(events::LogRead { events: Vec::new(), torn_tail: None, corrupt: None });
    }
    let lr = events::read_log(log).map_err(|e| format!("event log refused: {e}"))?;
    if let Some(c) = &lr.corrupt {
        return Err(format!("event log refused: line {} is corrupt: {}", c.line, c.msg));
    }
    if let Some(t) = &lr.torn_tail {
        return Err(format!("event log refused: torn tail at line {} ({} bytes, no newline)", t.line, t.bytes));
    }
    Ok(lr)
}

/// Is the OPENED handle the very regular file the earlier no-follow metadata gate saw? (fix r2, N5 —
/// the same re-bind as `events::read_log`.) The open itself cannot be told no-follow portably (the
/// per-platform `O_NOFOLLOW` values differ and the r1 constants were wrong on macOS), so a same-user
/// swap of the final name between the gate and the open is refused HERE: the handle must itself be a
/// regular file and carry the gate's own device and inode, so a symlink, a FIFO or a different file
/// substituted at the name is never read, however briefly it sat there.
fn opened_is_gate_file(handle: &fs::Metadata, gate: &fs::Metadata) -> bool {
    handle.is_file() && handle.dev() == gate.dev() && handle.ino() == gate.ino()
}

/// Read the handover file at `final_path` if it is a plain, in-cap, strictly-parseable record of
/// `spec`'s life. Any other content — a symlink, an oversized file, garbage, a different life — is the
/// same refusal (decision 8: the planted thing is untouched).
fn read_existing(final_path: &Path, spec: &DeathSpec<'_>, relpath: &str) -> Result<Option<Handover>, String> {
    let mismatch = || format!("handover {relpath} exists and does not match this death");
    let md = fs::symlink_metadata(final_path).map_err(|e| format!("cannot write the handover: {}", e.kind()))?;
    // Fix r1 (F2/F3): a path at the final name is accepted only if the no-follow metadata says it is a
    // REGULAR file. A symlink — even one pointing at a VALID same-life handover outside the folder — a
    // FIFO, a socket, a device or a directory is the same planted-name refusal, and none of them is
    // ever opened: before this gate a FIFO hung the verb on a blocking open for read, with the
    // per-ticket lock held.
    if !md.is_file() {
        return Err(mismatch());
    }
    // The open is non-blocking (a swapped-in FIFO cannot hang the read), and the handle is then proven
    // to be the very regular file the gate above saw (fix r2 N5): a swap between the two is the same
    // planted-name refusal, and the thing now sitting at the name is never parsed.
    let mut f: File = OpenOptions::new().read(true).custom_flags(OPEN_NONBLOCK).open(final_path).map_err(|e| format!("cannot write the handover: {}", e.kind()))?;
    if !opened_is_gate_file(&f.metadata().map_err(|e| format!("cannot write the handover: {}", e.kind()))?, &md) {
        return Err(mismatch());
    }
    let mut buf = Vec::new();
    std::io::Read::take(std::io::Read::by_ref(&mut f), HANDOVER_CAP as u64 + 1).read_to_end(&mut buf).map_err(|e| format!("cannot write the handover: {}", e.kind()))?;
    if buf.len() > HANDOVER_CAP {
        return Err(mismatch());
    }
    let text = std::str::from_utf8(&buf).map_err(|_| mismatch())?;
    let h = Handover::decode(text).map_err(|_| mismatch())?;
    if !h.is_life_of(spec) {
        return Err(mismatch());
    }
    Ok(Some(h))
}

/// Write the handover atomically: a uniquely named temp file created EXCLUSIVELY (mode 0600), written,
/// `sync_all`, then hard-linked to the final name — a call that FAILS when the name exists, so a
/// pre-planted or racing file is never replaced — then the temp name is dropped. Ok(false) means the
/// link refused because the final name was already there: the caller re-reads it through the strict
/// door instead of touching it.
fn write_atomic(folder: &Path, spec: &DeathSpec<'_>, now: u64) -> Result<bool, String> {
    let io = |e: std::io::Error| format!("cannot write the handover: {}", e.kind());
    let final_path = folder.join(handover_name(spec.id, spec.pid, spec.pid_start_ms));
    let tmp = folder.join(format!(
        ".{}.{}.{}.{}.{}.tmp",
        spec.id,
        spec.pid,
        spec.pid_start_ms,
        std::process::id(),
        now
    ));
    {
        let mut f = OpenOptions::new().write(true).create_new(true).mode(FILE_MODE).open(&tmp).map_err(io)?;
        let hand = Handover {
            v: 1,
            ticket: spec.ticket.to_string(),
            repo: spec.repo.to_string(),
            id: spec.id.to_string(),
            pid: spec.pid,
            pid_start_ms: spec.pid_start_ms,
            cause: spec.cause.as_str().to_string(),
            limit: spec.limit.as_str().to_string(),
            timeout_ms: spec.timeout_ms,
            steps: spec.steps,
            hard_limit: spec.hard_limit,
            detected_ms: now,
            checkpoint: spec.checkpoint.to_string(),
            recipe: "commit_push_then_resume".to_string(),
            session_id: spec.session_id.to_string(),
        };
        let line = hand.encode();
        if line.len() > HANDOVER_CAP {
            return Err(format!("cannot write the handover: the line is {} bytes, over the cap", line.len()));
        }
        f.write_all(line.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
    }
    let linked = match fs::hard_link(&tmp, &final_path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(io(e));
        }
    };
    fs::remove_file(&tmp).map_err(io)?;
    Ok(linked)
}

/// Stage one death (decision 6 a+b): the clock rule, the log pre-flight, then the handover on disk —
/// all BEFORE any signal when the caller is about to stop the process. An existing handover of the same
/// life is REUSED and its cause wins (decision 7). Answers the effective cause and the relpath.
pub fn stage(dir: &Path, spec: &DeathSpec<'_>, log: &Path, root: &Path, now: u64, add_ts_ms: u64) -> Result<Staged, String> {
    if !clock_ok(now, add_ts_ms) {
        return Err("the clock reads before the process was recorded".to_string());
    }
    preflight_log(root, log)?;
    let folder = ensure_folder(dir, spec.ticket)?;
    let relpath = handover_relpath(spec.ticket, spec.id, spec.pid, spec.pid_start_ms);
    let final_path = folder.join(handover_name(spec.id, spec.pid, spec.pid_start_ms));
    match fs::symlink_metadata(&final_path) {
        // Not there yet: write it, then re-read through the strict door in case a racer linked first
        // (the no-replace link refuses and the re-read decides).
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => match write_atomic(&folder, spec, now)? {
            true => Ok(Staged { cause: spec.cause, relpath }),
            // A racer (or an earlier crashed run) put the final name there while this run wrote its
            // temp: the strict re-read decides reuse-or-refuse, and the planted file is untouched.
            false => {
                let h = read_existing(&final_path, spec, &relpath)?.ok_or_else(|| format!("handover {relpath} exists and does not match this death"))?;
                Ok(staged_of(&h, &relpath))
            }
        },
        Err(e) => Err(format!("cannot write the handover: {}", e.kind())),
        Ok(_) => {
            let h = read_existing(&final_path, spec, &relpath)?.ok_or_else(|| format!("handover {relpath} exists and does not match this death"))?;
            Ok(staged_of(&h, &relpath))
        }
    }
}

/// The staged view of an existing handover: ITS cause and relpath (decision 7).
fn staged_of(h: &Handover, relpath: &str) -> Staged {
    Staged {
        cause: if h.cause == "timeout" { Cause::Timeout } else { Cause::Died },
        relpath: relpath.to_string(),
    }
}

/// Commit one staged death (decision 6c): append the `timed_out` event UNLESS the log already holds a
/// `timed_out` line with this ticket and handover (decision 7 — the same death once). The append goes
/// through the library record writer, the only writer for the P1b kinds.
pub fn commit(spec: &DeathSpec<'_>, staged: &Staged, log: &Path, root: &Path, now: u64) -> Result<(), String> {
    // Fix r2 (N2): the idempotence lookup reads through the same capped, regular-file-proving door as
    // the pre-flight (`read_log_for_death`) — a plain `fs::read` blocks on a FIFO at the log path.
    let lr = read_log_for_death(log)?;
    let already = lr.events.iter().any(|rec| {
        rec.kind == Kind::TimedOut
            && matches!(
                (rec.field("ticket"), rec.field("handover")),
                (Some(Field::Str(t)), Some(Field::Str(h))) if t == spec.ticket && h == &staged.relpath
            )
    });
    if already {
        return Ok(());
    }
    let rec = Record::build(
        Kind::TimedOut,
        now,
        &[
            ("ticket", Field::Str(spec.ticket.to_string())),
            ("repo", Field::Str(spec.repo.to_string())),
            ("cause", Field::Str(staged.cause.as_str().to_string())),
            ("checkpoint", Field::Str(spec.checkpoint.to_string())),
            ("recipe", Field::Str("commit_push_then_resume".to_string())),
            ("handover", Field::Str(staged.relpath.clone())),
            ("session_id", Field::Str(spec.session_id.to_string())),
        ],
    )
    .map_err(|e| format!("event log refused: {e}"))?;
    events::append_record(root, log, &rec).map_err(|e| format!("event log refused: {e}"))
}

// ---------------------------------------------------------------- #210 (tests)

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn spec<'a>(ticket: &'a str, id: &'a str, pid: u64, start: u64, cause: Cause) -> DeathSpec<'a> {
        DeathSpec {
            ticket,
            id,
            pid,
            pid_start_ms: start,
            cause,
            limit: LimitKind::None,
            timeout_ms: 0,
            steps: 0,
            hard_limit: 0,
            repo: "r",
            checkpoint: "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c",
            session_id: "0b6e3c1a-2f4d-4c8e-9a7b-1d2e3f405162",
        }
    }

    fn tmp() -> PathBuf {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("death-unit-{}-{}", std::process::id(), n));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// AC 12: the pure clock rule — `now < add ts_ms` refuses, `now >= add ts_ms` proceeds.
    #[test]
    fn clock_rule_table() {
        assert!(!clock_ok(99, 100), "a clock before the add line refuses");
        assert!(clock_ok(100, 100), "a clock AT the add line proceeds");
        assert!(clock_ok(101, 100), "a clock after the add line proceeds");
    }

    /// AC 10 (the unit half): the folder decision table — symlink, not a directory, a foreign owner, a
    /// group/other mode bit, and the two clean shapes.
    #[test]
    fn folder_verdict_table() {
        let me: u32 = fs::metadata(std::env::temp_dir()).unwrap().uid();
        assert_eq!(folder_verdict(true, true, me, 0o700, me), Err("deaths folder is a symlink"));
        assert_eq!(folder_verdict(false, false, me, 0o700, me), Err("deaths folder is not a directory"));
        assert_eq!(folder_verdict(false, true, me + 1, 0o700, me), Err("deaths folder is owned by another user"));
        assert_eq!(folder_verdict(false, true, me, 0o755, me), Err("deaths folder is readable by group or others"));
        assert_eq!(folder_verdict(false, true, me, 0o700, me), Ok(()));
        assert_eq!(folder_verdict(false, true, me, 0o500, me), Ok(()), "a stricter mode is still fine");
    }

    /// Decision 4: the encoded line carries exactly the 15 keys in order, newline-terminated and within
    /// the cap; the decode round-trips; anything else — a reordered key, a missing key, an extra key, a
    /// wrong type, garbage — is refused.
    #[test]
    fn handover_line_roundtrip_and_strictness() {
        let h = Handover {
            v: 1,
            ticket: "t".into(),
            repo: "r".into(),
            id: "a".into(),
            pid: 123,
            pid_start_ms: 456,
            cause: "timeout".into(),
            limit: "time".into(),
            timeout_ms: 1000,
            steps: 0,
            hard_limit: 0,
            detected_ms: 789,
            checkpoint: "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c".into(),
            recipe: "commit_push_then_resume".into(),
            session_id: "0b6e3c1a-2f4d-4c8e-9a7b-1d2e3f405162".into(),
        };
        let line = h.encode();
        assert!(line.ends_with('\n') && line.len() <= HANDOVER_CAP);
        assert_eq!(Handover::decode(&line).unwrap(), h);
        // order
        let reordered = line.replace("\"ticket\":\"t\",\"repo\":\"r\"", "\"repo\":\"r\",\"ticket\":\"t\"");
        assert!(Handover::decode(&reordered).is_err(), "a reordered key set is refused");
        // missing key
        let k = "\"steps\":0,";
        let missing = line.replace(k, "");
        assert!(Handover::decode(&missing).is_err(), "a missing key is refused");
        // wrong type
        let typed = line.replace("\"pid\":123", "\"pid\":\"123\"");
        assert!(Handover::decode(&typed).is_err(), "a string where a number belongs is refused");
        // garbage
        assert!(Handover::decode("not json at all").is_err());
        assert!(Handover::decode("").is_err());
        // a bad shape value
        let bad_cause = line.replace("\"cause\":\"timeout\"", "\"cause\":\"exploded\"");
        assert!(Handover::decode(&bad_cause).is_err(), "an unknown cause value is refused");
    }

    /// AC 8c: idempotence — a full record (handover + event) stages and commits WITHOUT a second append;
    /// a handover whose event write crashed (cause timeout, no event) commits ONE line carrying
    /// `timeout`, never the fresh `died` (the existing handover's cause wins).
    #[test]
    fn same_death_once_idempotence() {
        let dir = tmp();
        let log = dir.join("log.jsonl");
        let s = spec("t", "a", 123, 456, Cause::Died);

        // Fresh: stage + commit writes the handover and one event line.
        let st = stage(&dir, &s, &log, &dir, 1_000, 900).unwrap();
        assert_eq!(st.cause, Cause::Died);
        commit(&s, &st, &log, &dir, 1_100).unwrap();
        let bytes = fs::read(&log).unwrap();
        assert_eq!(String::from_utf8_lossy(&bytes).lines().count(), 1);

        // Full record present: a second stage+commit appends nothing.
        let st2 = stage(&dir, &s, &log, &dir, 2_000, 900).unwrap();
        commit(&s, &st2, &log, &dir, 2_100).unwrap();
        assert_eq!(fs::read(&log).unwrap(), bytes, "the same death is not appended twice");

        // A crashed TIMEOUT run: its handover (cause `timeout`) is on disk, its event line lost. A
        // fresh `died` staging of the SAME life must REUSE the handover and its cause, and the commit
        // carries `timeout` (decision 7: a timeout is never re-labelled `died`).
        fs::remove_file(dir.join("t.deaths").join("a.123.456.json")).unwrap();
        fs::remove_file(&log).unwrap();
        let mut crashed = spec("t", "a", 123, 456, Cause::Timeout);
        crashed.limit = LimitKind::Time;
        crashed.timeout_ms = 1000;
        let first = stage(&dir, &crashed, &log, &dir, 3_000, 900).unwrap();
        assert_eq!(first.cause, Cause::Timeout, "the fresh timeout staging writes a timeout handover");
        let hand_before = fs::read(dir.join("t.deaths").join("a.123.456.json")).unwrap();
        let again = stage(&dir, &s, &log, &dir, 4_000, 900).unwrap();
        assert_eq!(again.cause, Cause::Timeout, "the existing handover's cause wins over the fresh died");
        assert_eq!(fs::read(dir.join("t.deaths").join("a.123.456.json")).unwrap(), hand_before, "the existing handover was rewritten");
        commit(&s, &again, &log, &dir, 4_100).unwrap();
        let text = fs::read_to_string(&log).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("\"cause\":\"timeout\""), "the appended line carries timeout, not died: {text}");
        let _ = first;
        let _ = fs::remove_dir_all(&dir);
    }

    /// AC 9: the atomic write — success leaves NO temp name behind; a stray earlier temp file in the
    /// folder does not change the outcome; a final name that already exists is NEVER replaced (its bytes
    /// are unchanged after the refused write).
    #[test]
    fn atomic_write_never_replaces_and_leaves_no_temp() {
        let dir = tmp();
        let folder = dir.join("t.deaths");
        fs::create_dir_all(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        // A stray temp file from an earlier crashed run.
        fs::write(folder.join(".stray.tmp"), b"junk").unwrap();
        let log = dir.join("log.jsonl");
        let s = spec("t", "a", 123, 456, Cause::Died);
        let st = stage(&dir, &s, &log, &dir, 1_000, 900).unwrap();
        assert_eq!(st.cause, Cause::Died);
        let names: Vec<String> = fs::read_dir(&folder).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.iter().filter(|n| *n != ".stray.tmp").count(), 1, "exactly one handover file: {names:?}");
        assert!(!names.iter().any(|n| n.ends_with(".tmp") && n != ".stray.tmp"), "no temp name of THIS write survives: {names:?}");

        // A second LIFE (different pid_start_ms) whose final name is pre-planted with foreign bytes: the
        // write refuses and the planted bytes are untouched.
        let other = folder.join("b.789.999.json");
        fs::write(&other, b"planted bytes").unwrap();
        let s2 = spec("t", "b", 789, 999, Cause::Died);
        let refused = stage(&dir, &s2, &log, &dir, 2_000, 900);
        assert!(refused.is_err(), "a pre-planted foreign handover refuses");
        assert_eq!(fs::read(&other).unwrap(), b"planted bytes", "the planted file was replaced");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Fix r2 (N3, the AC 9 mutant): `write_atomic` called with the final name ALREADY planted answers
    /// `Ok(false)` and leaves the planted bytes untouched — the no-replace `hard_link` is the only
    /// route to the final name, so replacing it with a create+truncate write goes red here (the stage
    /// path never exercises the race: it only calls `write_atomic` after seeing the name absent).
    #[test]
    fn write_atomic_refuses_a_preplanted_final_name() {
        let dir = tmp();
        let folder = dir.join("t.deaths");
        fs::create_dir_all(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
        let s = spec("t", "c", 321, 654, Cause::Timeout);
        let final_path = folder.join(handover_name(s.id, s.pid, s.pid_start_ms));
        fs::write(&final_path, b"preplanted bytes").unwrap();
        let linked = write_atomic(&folder, &s, 5_000).unwrap();
        assert!(!linked, "the no-replace link must answer false for an existing final name");
        assert_eq!(fs::read(&final_path).unwrap(), b"preplanted bytes", "the planted final name was replaced");
        let names: Vec<String> = fs::read_dir(&folder).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names, vec![final_path.file_name().unwrap().to_string_lossy().into_owned()], "no temp name survives: {names:?}");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Fix r2 (N5): the handle-identity rule — the OPENED handle is the very regular file the gate saw.
    /// The same file read twice passes; a different file, and any non-regular thing at the name, is
    /// refused. (The gate-then-open SWAP itself cannot be planted deterministically without a seam, so
    /// the rule is unit-tested at its own table; which is recorded in the close file.)
    #[test]
    fn opened_handle_must_be_the_gated_regular_file() {
        let dir = tmp();
        let a = dir.join("a.json");
        let b = dir.join("b.json");
        fs::write(&a, b"one").unwrap();
        fs::write(&b, b"two").unwrap();
        let gate = fs::symlink_metadata(&a).unwrap();
        let same = fs::File::open(&a).unwrap().metadata().unwrap();
        let other = fs::File::open(&b).unwrap().metadata().unwrap();
        assert!(opened_is_gate_file(&same, &gate), "the handle of the gated file itself passes");
        assert!(!opened_is_gate_file(&other, &gate), "a handle of a DIFFERENT file is refused");
        let fifo = dir.join("fifo");
        let _ = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
        let fifo_gate = fs::symlink_metadata(&fifo).unwrap();
        let fifo_open = fs::OpenOptions::new().read(true).custom_flags(OPEN_NONBLOCK).open(&fifo).unwrap();
        assert!(!opened_is_gate_file(&fifo_open.metadata().unwrap(), &fifo_gate), "a FIFO at the name is refused on the handle itself");
        let _ = fs::remove_dir_all(&dir);
    }
}
