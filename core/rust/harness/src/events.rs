//! Event log core (std only): the append-only JSON Lines file the caps' refusals are recorded in.
//! One event per line, compact, fixed key order, every key present, `v` (schema version) on every line. Since v2 every
//! event also carries its run identity: `run_id` (who issued the run this line belongs to), `role` and `model`; the first
//! line of a self-issued run is a `run_issued` event. An event missing any of these is refused, never guessed or defaulted.
//! Reader: every complete line must be a valid event (else CORRUPTION, the read stops there); a final fragment with no
//! trailing newline is a TORN TAIL, reported and never parsed. Writer: one `write()` of the whole line, then `sync_all`;
//! refuses to append onto a torn tail (it would fuse a new event onto garbage). Recovery of a torn tail is not here.
//! Kinds: thirteen today — the four base kinds (the PRE-B v2 shape with the run identity) and the nine P1b kinds
//! (payload kinds, no identity fields yet). Each kind's exact key set, value shapes and `v` are declared ONCE in the
//! kind table below; the reader, the writer and the schema test all consult that table and nothing else.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Schema version written on every line and the only one this reader accepts. A v1 line (no run identity) is refused
/// with the line and `v` named, never upgraded by guess.
pub const SCHEMA_VERSION: u64 = 2;
/// Largest accepted line in bytes, newline included. Keeps each append one small write.
pub const LINE_CAP: usize = 4096;
/// Keys of every v2 base kind, in canonical order (the PRE-B envelope, bytes as on master).
const KEYS_V2: [&str; 9] = ["v", "kind", "ts_ms", "run_id", "ticket", "repo", "reason", "role", "model"];

/// The single envelope declaration of the nine P1b kinds: the keys each of them carries before its payload. Today it is
/// the trio every line shares; the PRE-B run identity (`run_id`, `role`, `model`) joins these nine kinds by editing THIS
/// one list, once a run issuer exists — nowhere else.
const P1B_ENVELOPE: [&str; 3] = ["v", "kind", "ts_ms"];

/// The reason token of the `run_issued` line a self-issuing invocation appends before its first event.
pub const REASON_RUN_ISSUED: &str = "self-issue";

// ---------------------------------------------------------------- the kind table (the one source of every shape)

/// The value shape of one payload key. A value outside its declared shape is refused naming the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// A plain token (`token_ok`).
    Token,
    /// A full lowercase uuid, `8-4-4-4-12` hex; a short id is refused (every comparison is uuid to uuid).
    Uuid,
    /// 64 lowercase hex digits.
    Hex64,
    /// 40 or 64 lowercase hex digits (a git sha, either length a repository may use).
    Sha,
    /// Token segments joined by `/`: no leading `/`, no empty, `.` or `..` segment, no `~` — so neither an absolute
    /// nor a home path can be written.
    RelPath,
    /// An unsigned integer.
    Num,
    /// Exactly one of the listed tokens.
    OneOf(&'static [&'static str]),
}

impl Shape {
    /// Check a string value against this shape; the error names the key and the shape it broke.
    fn check_str(&self, key: &str, s: &str) -> Result<(), String> {
        match self {
            Shape::Token => {
                if token_ok(s) {
                    Ok(())
                } else {
                    Err(format!("{key} must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')"))
                }
            }
            Shape::Uuid => {
                if uuid_ok(s) {
                    Ok(())
                } else {
                    Err(format!("{key} must be a full lowercase uuid (8-4-4-4-12 hex; a short id is refused)"))
                }
            }
            Shape::Hex64 => {
                if s.len() == 64 && lower_hex(s) {
                    Ok(())
                } else {
                    Err(format!("{key} must be 64 lowercase hex digits"))
                }
            }
            Shape::Sha => {
                if (s.len() == 40 || s.len() == 64) && lower_hex(s) {
                    Ok(())
                } else {
                    Err(format!("{key} must be 40 or 64 lowercase hex digits"))
                }
            }
            Shape::RelPath => {
                if relpath_ok(s) {
                    Ok(())
                } else {
                    Err(format!("{key} must be a relative path: plain token segments joined by '/', no leading '/', no empty, '.' or '..' segment"))
                }
            }
            Shape::OneOf(vals) => {
                if vals.contains(&s) {
                    Ok(())
                } else {
                    Err(format!("{key} must be one of {vals:?}"))
                }
            }
            Shape::Num => Err(format!("{key} must be an unsigned integer")),
        }
    }
}

fn lower_hex(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Full lowercase uuid `8-4-4-4-12` hex.
fn uuid_ok(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    let mut at = 0usize;
    for (i, len) in [8usize, 4, 4, 4, 12].iter().enumerate() {
        if !lower_hex(&s[at..at + len]) {
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

/// Token segments joined by `/`: refuses a leading or doubled `/`, an empty, `.` or `..` segment, and any character a
/// token forbids (so no `~`, no uppercase, no escapes).
fn relpath_ok(s: &str) -> bool {
    !s.is_empty() && s.split('/').all(|seg| token_ok(seg))
}

/// One declared payload field of a kind: its key and its value shape.
#[derive(Debug, Clone, Copy)]
struct FieldDef {
    key: &'static str,
    shape: Shape,
}

/// A declared cross-field honesty rule. `PassMeansManifestEmpty`: on `cleanup_result`, `result=pass` with
/// `manifest_left` above zero is refused — a pass that left entries behind is a lie the loader rejects cheaply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cross {
    None,
    PassMeansManifestEmpty,
}

/// The declared shape of one kind: its exact key set (envelope keys, then payload keys, canonical order), the value
/// shape of every payload key, the only `v` it exists at, and its cross-field rule. THIS TABLE IS THE ONE SOURCE: the
/// reader's accept/refuse logic, the doc's field tables and the schema test all consult it. No kind is described
/// anywhere else, and no literal test value is special-cased in the reader.
struct KindDef {
    /// Keys before the payload. The four base kinds use `KEYS_V2` (the PRE-B envelope, bytes as on master, v 2); the
    /// nine P1b kinds share `P1B_ENVELOPE` — the one declaration their envelope lives in.
    envelope: &'static [&'static str],
    /// Payload fields after the envelope, canonical order; empty for the base kinds (their whole shape is the envelope:
    /// the trio plus six plain-token identity keys).
    fields: &'static [FieldDef],
    /// The only schema version this kind exists at; a line naming any other `v` is refused naming `v`.
    v: u64,
    cross: Cross,
}

const BASE_DEF: KindDef = KindDef { envelope: &KEYS_V2, fields: &[], v: SCHEMA_VERSION, cross: Cross::None };

const SESSION_STARTED_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[FieldDef { key: "session_id", shape: Shape::Uuid }, FieldDef { key: "source", shape: Shape::OneOf(&["launch", "hook"]) }],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

const UNMANAGED_SESSION_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[
        FieldDef { key: "session_id", shape: Shape::Uuid },
        FieldDef { key: "ticket", shape: Shape::Token },
        FieldDef { key: "reason", shape: Shape::OneOf(&["not_supervisor_launched", "no_launch_record"]) },
    ],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

const LAUNCH_REFUSED_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[
        FieldDef { key: "launch_role", shape: Shape::Token },
        FieldDef { key: "brief_sha256", shape: Shape::Hex64 },
        FieldDef { key: "matched_entry", shape: Shape::Token },
    ],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

const TIMED_OUT_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[
        FieldDef { key: "ticket", shape: Shape::Token },
        FieldDef { key: "repo", shape: Shape::Token },
        FieldDef { key: "cause", shape: Shape::OneOf(&["timeout", "died"]) },
        FieldDef { key: "checkpoint", shape: Shape::Sha },
        FieldDef { key: "recipe", shape: Shape::OneOf(&["commit_push_then_resume"]) },
        FieldDef { key: "handover", shape: Shape::RelPath },
        FieldDef { key: "session_id", shape: Shape::Uuid },
    ],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

const CHECK_IN_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[
        FieldDef { key: "ticket", shape: Shape::Token },
        FieldDef { key: "repo", shape: Shape::Token },
        FieldDef { key: "steps", shape: Shape::Num },
        FieldDef { key: "soft_limit", shape: Shape::Num },
    ],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

const STEP_LIMIT_STOP_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[
        FieldDef { key: "ticket", shape: Shape::Token },
        FieldDef { key: "repo", shape: Shape::Token },
        FieldDef { key: "steps", shape: Shape::Num },
        FieldDef { key: "hard_limit", shape: Shape::Num },
    ],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

const CLEANUP_RESULT_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[
        FieldDef { key: "ticket", shape: Shape::Token },
        FieldDef { key: "repo", shape: Shape::Token },
        FieldDef { key: "result", shape: Shape::OneOf(&["pass", "fail"]) },
        FieldDef { key: "manifest_left", shape: Shape::Num },
    ],
    v: SCHEMA_VERSION,
    cross: Cross::PassMeansManifestEmpty,
};

const QUARANTINED_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[
        FieldDef { key: "ticket", shape: Shape::Token },
        FieldDef { key: "repo", shape: Shape::Token },
        FieldDef { key: "from", shape: Shape::RelPath },
        FieldDef { key: "to", shape: Shape::RelPath },
        FieldDef { key: "reason", shape: Shape::OneOf(&["unmanifested", "retention"]) },
    ],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

const HUMAN_ORIGIN_REFUSED_DEF: KindDef = KindDef {
    envelope: &P1B_ENVELOPE,
    fields: &[FieldDef { key: "target", shape: Shape::OneOf(&["config", "permission", "control_registry"]) }, FieldDef { key: "path", shape: Shape::RelPath }],
    v: SCHEMA_VERSION,
    cross: Cross::None,
};

impl KindDef {
    /// The full key set in canonical order: envelope keys, then payload keys.
    fn key_list(&self) -> Vec<&'static str> {
        self.envelope.iter().copied().chain(self.fields.iter().map(|f| f.key)).collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Escalation,
    ReplanRequired,
    BudgetExceeded,
    RunIssued,
    SessionStarted,
    UnmanagedSession,
    LaunchRefused,
    TimedOut,
    CheckIn,
    StepLimitStop,
    CleanupResult,
    Quarantined,
    HumanOriginRefused,
}

impl Kind {
    pub const ALL: [Kind; 13] = [
        Kind::Escalation,
        Kind::ReplanRequired,
        Kind::BudgetExceeded,
        Kind::RunIssued,
        Kind::SessionStarted,
        Kind::UnmanagedSession,
        Kind::LaunchRefused,
        Kind::TimedOut,
        Kind::CheckIn,
        Kind::StepLimitStop,
        Kind::CleanupResult,
        Kind::Quarantined,
        Kind::HumanOriginRefused,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Escalation => "escalation",
            Kind::ReplanRequired => "replan_required",
            Kind::BudgetExceeded => "budget_exceeded",
            Kind::RunIssued => "run_issued",
            Kind::SessionStarted => "session_started",
            Kind::UnmanagedSession => "unmanaged_session",
            Kind::LaunchRefused => "launch_refused",
            Kind::TimedOut => "timed_out",
            Kind::CheckIn => "check_in",
            Kind::StepLimitStop => "step_limit_stop",
            Kind::CleanupResult => "cleanup_result",
            Kind::Quarantined => "quarantined",
            Kind::HumanOriginRefused => "human_origin_refused",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// The table entry that declares this kind: its key set, value shapes and `v` (exposed as `keys()`/`version()`).
    fn def(&self) -> &'static KindDef {
        match self {
            Kind::Escalation | Kind::ReplanRequired | Kind::BudgetExceeded | Kind::RunIssued => &BASE_DEF,
            Kind::SessionStarted => &SESSION_STARTED_DEF,
            Kind::UnmanagedSession => &UNMANAGED_SESSION_DEF,
            Kind::LaunchRefused => &LAUNCH_REFUSED_DEF,
            Kind::TimedOut => &TIMED_OUT_DEF,
            Kind::CheckIn => &CHECK_IN_DEF,
            Kind::StepLimitStop => &STEP_LIMIT_STOP_DEF,
            Kind::CleanupResult => &CLEANUP_RESULT_DEF,
            Kind::Quarantined => &QUARANTINED_DEF,
            Kind::HumanOriginRefused => &HUMAN_ORIGIN_REFUSED_DEF,
        }
    }

    /// The exact key set of this kind at its declared version, in canonical order. Built from the table, so an
    /// envelope change is one edit in one declaration, never one per kind.
    pub fn keys(&self) -> Vec<&'static str> {
        self.def().key_list()
    }

    /// The only schema version this kind exists at (the version table's row for this kind).
    pub fn version(&self) -> u64 {
        self.def().v
    }

    /// The four base kinds (the PRE-B v2 shape, bytes as on master): the only kinds writable through the base `Event`
    /// struct. The nine P1b kinds carry a payload and are written through `Record`.
    pub fn is_base(&self) -> bool {
        matches!(self, Kind::Escalation | Kind::ReplanRequired | Kind::BudgetExceeded | Kind::RunIssued)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    /// Caller-supplied milliseconds since the epoch.
    pub ts_ms: u64,
    /// The run identity this event belongs to; never guessed, never defaulted.
    pub run_id: String,
    pub ticket: String,
    pub repo: String,
    pub reason: String,
    /// The role and the model of the run that writes the event; never defaulted.
    pub role: String,
    pub model: String,
}

/// An id or reason token: lowercase letters, digits, `-`, `_`, `.`; never empty, never `.` or `..`.
pub fn token_ok(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

impl Event {
    pub fn validate(&self) -> Result<(), String> {
        // The base `Event` struct expresses only the four base kinds (the PRE-B v2 shape). A P1b kind is refused here,
        // before any value is looked at, so the old CLI shape can never write one (its payload has no flags there).
        if !self.kind.is_base() {
            return Err(format!("kind {} is a P1b kind: the base event shape cannot express its payload (write it through the record writer)", self.kind.as_str()));
        }
        for (name, v) in [
            ("run_id", &self.run_id),
            ("ticket", &self.ticket),
            ("repo", &self.repo),
            ("reason", &self.reason),
            ("role", &self.role),
            ("model", &self.model),
        ] {
            if !token_ok(v) {
                return Err(format!("{name} must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')"));
            }
        }
        Ok(())
    }

    /// Canonical line including the trailing newline. Refuses an invalid event and a line over the cap.
    pub fn encode(&self) -> Result<String, AppendError> {
        self.validate().map_err(AppendError::Invalid)?;
        // Every string field is a validated token, so none needs escaping.
        let line = format!(
            "{{\"v\":{},\"kind\":\"{}\",\"ts_ms\":{},\"run_id\":\"{}\",\"ticket\":\"{}\",\"repo\":\"{}\",\"reason\":\"{}\",\"role\":\"{}\",\"model\":\"{}\"}}\n",
            SCHEMA_VERSION,
            self.kind.as_str(),
            self.ts_ms,
            self.run_id,
            self.ticket,
            self.repo,
            self.reason,
            self.role,
            self.model
        );
        if line.len() > LINE_CAP {
            return Err(AppendError::TooLong(line.len()));
        }
        Ok(line)
    }
}

// ---------------------------------------------------------------- the read record (every kind)

/// One payload value: a shape-validated string, or an unsigned integer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Str(String),
    Num(u64),
}

/// One read line of ANY kind. The eight base fields keep today's names and types (the four base kinds fill them from
/// their envelope); a P1b kind leaves them empty and carries its values in `payload`, looked up by key. `identity`
/// holds the envelope keys beyond `v`/`kind`/`ts_ms` (today: the base kinds' run identity), in canonical order; the
/// six base string fields are mirrors of it, filled by the reader for the existing callers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub kind: Kind,
    pub ts_ms: u64,
    pub run_id: String,
    pub ticket: String,
    pub repo: String,
    pub reason: String,
    pub role: String,
    pub model: String,
    pub identity: Vec<(&'static str, Field)>,
    /// The per-kind payload after the envelope, in the kind's canonical key order.
    pub payload: Vec<(&'static str, Field)>,
}

impl Record {
    /// The payload (or identity) value of one key.
    pub fn field(&self, key: &str) -> Option<&Field> {
        self.identity.iter().chain(self.payload.iter()).find(|(k, _)| *k == key).map(|(_, v)| v)
    }

    /// Fill the six base string fields from `identity` (they are its mirror for the existing callers).
    fn mirror_identity(&mut self) {
        let set = |name: &'static str, slot: &mut String| {
            if let Some(Field::Str(s)) = self.identity.iter().find(|(k, _)| *k == name).map(|(_, v)| v) {
                *slot = s.clone();
            }
        };
        set("run_id", &mut self.run_id);
        set("ticket", &mut self.ticket);
        set("repo", &mut self.repo);
        set("reason", &mut self.reason);
        set("role", &mut self.role);
        set("model", &mut self.model);
    }

    /// Check the cross-field rules this kind's table entry declares.
    fn check_cross(kind: Kind, get: impl Fn(&str) -> Option<Field>) -> Result<(), String> {
        if kind.def().cross == Cross::PassMeansManifestEmpty {
            let pass = matches!(get("result"), Some(Field::Str(s)) if s == "pass");
            let left = matches!(get("manifest_left"), Some(Field::Num(n)) if n > 0);
            if pass && left {
                return Err("manifest_left must be 0 when result is pass (a pass that left manifest entries behind is refused)".to_string());
            }
        }
        Ok(())
    }

    /// Sort caller-given key/value pairs into `identity` and `payload` against the kind's table entry: the exact key
    /// set (every declared key beyond the trio present, no key the kind does not declare, none twice), the declared
    /// value shape of every field, and the cross rules. Every error names the offending key. This is the single
    /// accept/refuse path: `build` (the writer's door) and `validate` (the re-check before encode) both come here.
    fn assemble(kind: Kind, pairs: &[(&str, Field)]) -> Result<(Vec<(&'static str, Field)>, Vec<(&'static str, Field)>), String> {
        let def = kind.def();
        let trio = ["v", "kind", "ts_ms"];
        let mut identity: Vec<(&'static str, Field)> = Vec::new();
        let mut payload: Vec<(&'static str, Field)> = Vec::new();
        for (k, v) in pairs {
            if trio.contains(k) {
                return Err(format!("{k} is written by the kind itself, not a caller value"));
            }
            let declared = def.fields.iter().find(|f| f.key == *k);
            let slot = def.envelope.iter().find(|e| **e == *k);
            if declared.is_none() && slot.is_none() {
                return Err(format!("unexpected key {k:?}"));
            }
            if identity.iter().chain(payload.iter()).any(|(known, _)| known == k) {
                return Err(format!("duplicate key {k:?}"));
            }
            match declared {
                Some(f) => {
                    let value = match (&v, f.shape) {
                        (Field::Str(s), shape) => {
                            shape.check_str(f.key, s)?;
                            v.clone()
                        }
                        (Field::Num(n), Shape::Num) => Field::Num(*n),
                        (Field::Num(_), _) => return Err(format!("{} has the wrong type (an unsigned integer where a string shape is declared)", f.key)),
                    };
                    payload.push((f.key, value));
                }
                None => {
                    // An envelope key beyond the trio (today: the base kinds' run identity): a plain token.
                    match v {
                        Field::Str(t) => {
                            Shape::Token.check_str(k, t)?;
                            identity.push((def.envelope.iter().find(|e| **e == *k).copied().unwrap(), Field::Str(t.clone())));
                        }
                        Field::Num(_) => return Err(format!("{k} has the wrong type (an unsigned integer where a plain token is declared)")),
                    }
                }
            }
        }
        for k in def.key_list() {
            if trio.contains(&k) {
                continue;
            }
            if !identity.iter().chain(payload.iter()).any(|(known, _)| *known == k) {
                return Err(format!("{k} missing"));
            }
        }
        let get = |name: &str| pairs.iter().find(|(k, _)| *k == name).map(|(_, v)| v.clone());
        Self::check_cross(kind, get)?;
        Ok((identity, payload))
    }

    /// Build a record of `kind` at `ts_ms` from key/value pairs (every declared key beyond `v`/`kind`/`ts_ms`, exact
    /// set, declared shapes). The library writer's door for the P1b kinds; later P1b tickets build their records here.
    pub fn build(kind: Kind, ts_ms: u64, pairs: &[(&str, Field)]) -> Result<Record, String> {
        let (identity, payload) = Self::assemble(kind, pairs)?;
        let mut rec = Record {
            kind,
            ts_ms,
            run_id: String::new(),
            ticket: String::new(),
            repo: String::new(),
            reason: String::new(),
            role: String::new(),
            model: String::new(),
            identity,
            payload,
        };
        rec.mirror_identity();
        Ok(rec)
    }

    /// Re-check this record against its kind's table entry (the same path `build` enforces), so a hand-mutated record
    /// is refused at encode, never written.
    pub fn validate(&self) -> Result<(), String> {
        let pairs: Vec<(&str, Field)> = self.identity.iter().chain(self.payload.iter()).map(|(k, v)| (*k, v.clone())).collect();
        let (identity, payload) = Self::assemble(self.kind, &pairs)?;
        if identity != self.identity || payload != self.payload {
            return Err("the key order does not follow the kind's declared canonical order".to_string());
        }
        Ok(())
    }

    /// Canonical line including the trailing newline: envelope keys then payload keys, in the kind's declared order.
    /// Refuses an invalid record and a line over the cap.
    pub fn encode(&self) -> Result<String, AppendError> {
        self.validate().map_err(AppendError::Invalid)?;
        let mut line = format!("{{\"v\":{},\"kind\":\"{}\",\"ts_ms\":{}", self.kind.version(), self.kind.as_str(), self.ts_ms);
        for (k, v) in self.identity.iter().chain(self.payload.iter()) {
            match v {
                Field::Str(t) => line.push_str(&format!(",\"{k}\":\"{t}\"")),
                Field::Num(n) => line.push_str(&format!(",\"{k}\":{n}")),
            }
        }
        line.push_str("}\n");
        if line.len() > LINE_CAP {
            return Err(AppendError::TooLong(line.len()));
        }
        Ok(line)
    }

    /// Strict decode of one line WITHOUT its newline: `v` first (any other value is refused naming `v`), then a known
    /// kind, then the kind's exact declared key set (an extra key, a missing key or a duplicate is refused naming the
    /// key), the declared value shape of every field, and the cross rules. Never guesses, never repairs.
    pub fn decode(line: &str) -> Result<Record, String> {
        if line.len() + 1 > LINE_CAP {
            return Err(format!("line exceeds the {LINE_CAP}-byte cap"));
        }
        let mut p = P { b: line.as_bytes(), i: 0 };
        let pairs = p.object()?;
        p.ws();
        if p.i != p.b.len() {
            return Err("trailing bytes".to_string());
        }
        let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v);
        match get("v") {
            None => return Err("v missing".to_string()),
            Some(Val::Num(n)) if *n == SCHEMA_VERSION => {}
            Some(Val::Num(n)) => return Err(format!("unsupported v {n}")),
            Some(_) => return Err("v has the wrong type".to_string()),
        }
        let kind = match get("kind") {
            None => return Err("kind missing".to_string()),
            Some(Val::Str(s)) => Kind::parse(s).ok_or_else(|| format!("unknown kind {s:?}"))?,
            Some(_) => return Err("kind has the wrong type".to_string()),
        };
        let def = kind.def();
        let keys = def.key_list();
        for (k, _) in &pairs {
            if !keys.contains(&k.as_str()) {
                return Err(format!("unexpected key {k:?}"));
            }
        }
        for k in &keys {
            if get(k).is_none() {
                return Err(format!("{k} missing"));
            }
        }
        let ts_ms = match get("ts_ms") {
            Some(Val::Num(n)) => *n,
            _ => return Err("ts_ms has the wrong type".to_string()),
        };
        let trio = ["v", "kind", "ts_ms"];
        let mut identity: Vec<(&'static str, Field)> = Vec::new();
        let mut payload: Vec<(&'static str, Field)> = Vec::new();
        for k in &keys {
            if trio.contains(k) {
                continue;
            }
            let v = get(k).unwrap();
            match def.fields.iter().find(|f| f.key == *k) {
                Some(f) => {
                    let value = match v {
                        Val::Str(t) => {
                            f.shape.check_str(f.key, t)?;
                            Field::Str(t.clone())
                        }
                        Val::Num(n) if f.shape == Shape::Num => Field::Num(*n),
                        _ => return Err(format!("{} has the wrong type", f.key)),
                    };
                    payload.push((f.key, value));
                }
                None => {
                    // An envelope key beyond the trio (today: the base kinds' run identity): a plain token.
                    let t = match v {
                        Val::Str(t) => t.clone(),
                        _ => return Err(format!("{k} has the wrong type")),
                    };
                    Shape::Token.check_str(*k, &t)?;
                    identity.push((*k, Field::Str(t)));
                }
            }
        }
        let getf = |name: &str| {
            identity
                .iter()
                .chain(payload.iter())
                .find(|(k, _)| *k == name)
                .map(|(_, v)| v.clone())
        };
        Self::check_cross(kind, getf)?;
        let mut rec = Record {
            kind,
            ts_ms,
            run_id: String::new(),
            ticket: String::new(),
            repo: String::new(),
            reason: String::new(),
            role: String::new(),
            model: String::new(),
            identity,
            payload,
        };
        rec.mirror_identity();
        Ok(rec)
    }
}

enum Val {
    Str(String),
    Num(u64),
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Result<(), String> {
        self.ws();
        if self.b.get(self.i) == Some(&c) {
            self.i += 1;
            Ok(())
        } else {
            Err(format!("expected '{}'", c as char))
        }
    }

    /// A string with no escapes: a validated token never needs one, so a backslash is refused outright.
    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let st = self.i;
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            match c {
                b'"' => break,
                b'\\' => return Err("escape sequences are not allowed".to_string()),
                c if c < 0x20 => return Err("raw control byte in string".to_string()),
                _ => self.i += 1,
            }
        }
        let s = std::str::from_utf8(&self.b[st..self.i]).map_err(|_| "invalid utf-8".to_string())?.to_string();
        self.i += 1;
        Ok(s)
    }

    fn value(&mut self) -> Result<Val, String> {
        self.ws();
        match self.b.get(self.i) {
            Some(b'"') => Ok(Val::Str(self.string()?)),
            Some(c) if c.is_ascii_digit() => {
                let st = self.i;
                while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.b[st..self.i]).map_err(|_| "bad number".to_string())?;
                if t.len() > 1 && t.starts_with('0') {
                    return Err("leading zero".to_string());
                }
                Ok(Val::Num(t.parse::<u64>().map_err(|_| "number out of range".to_string())?))
            }
            _ => Err("unexpected value".to_string()),
        }
    }

    /// A flat object of string and unsigned-integer values; a duplicate key is refused here.
    fn object(&mut self) -> Result<Vec<(String, Val)>, String> {
        self.eat(b'{')?;
        let mut out: Vec<(String, Val)> = Vec::new();
        loop {
            let k = self.string()?;
            self.eat(b':')?;
            let v = self.value()?;
            if out.iter().any(|(n, _)| *n == k) {
                return Err("duplicate key".to_string());
            }
            out.push((k, v));
            self.ws();
            match self.b.get(self.i) {
                Some(b',') => self.i += 1,
                Some(b'}') => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => return Err("expected ',' or '}'".to_string()),
            }
        }
    }
}

// ---------------------------------------------------------------- reader

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TornTail {
    /// 1-based line number the fragment would have had.
    pub line: usize,
    pub bytes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineError {
    /// 1-based.
    pub line: usize,
    pub msg: String,
}

/// What a read found. `events` are the intact events before any corruption; a corrupt line stops the read (nothing after it is
/// returned), so a caller can print `events` and still exit non-zero.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRead {
    pub events: Vec<Record>,
    pub torn_tail: Option<TornTail>,
    pub corrupt: Option<LineError>,
}

impl LogRead {
    pub fn is_clean(&self) -> bool {
        self.torn_tail.is_none() && self.corrupt.is_none()
    }
}

/// Parse log bytes. A final fragment with no newline is torn and never parsed, even when it is valid JSON.
pub fn parse_log(bytes: &[u8]) -> LogRead {
    let mut out = LogRead { events: Vec::new(), torn_tail: None, corrupt: None };
    let mut start = 0usize;
    let mut line_no = 0usize;
    while let Some(off) = bytes[start..].iter().position(|b| *b == b'\n') {
        line_no += 1;
        let raw = &bytes[start..start + off];
        start += off + 1;
        let parsed = match std::str::from_utf8(raw) {
            Ok(s) => Record::decode(s),
            Err(_) => Err("invalid utf-8".to_string()),
        };
        match parsed {
            Ok(ev) => out.events.push(ev),
            Err(msg) => {
                out.corrupt = Some(LineError { line: line_no, msg });
                return out;
            }
        }
    }
    if start < bytes.len() {
        out.torn_tail = Some(TornTail { line: line_no + 1, bytes: bytes.len() - start });
    }
    out
}

/// Read a log file. A missing or unreadable file is an `Err` (fail closed), not an empty log.
pub fn read_log(path: &Path) -> Result<LogRead, String> {
    let bytes = fs::read(path).map_err(|e| format!("cannot read log: {}", e.kind()))?;
    Ok(parse_log(&bytes))
}

// ---------------------------------------------------------------- writer

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendError {
    Invalid(String),
    TooLong(usize),
    TornTail,
    Path(String),
    Io(String),
}

impl fmt::Display for AppendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AppendError::Invalid(m) => write!(f, "event refused: {m}"),
            AppendError::TooLong(n) => write!(f, "event refused: line is {n} bytes, over the {LINE_CAP}-byte cap"),
            AppendError::TornTail => write!(f, "log refused: torn tail (the last line has no newline); nothing was appended"),
            AppendError::Path(m) => write!(f, "log path refused: {m}"),
            AppendError::Io(m) => write!(f, "log write failed: {m}"),
        }
    }
}

impl std::error::Error for AppendError {}

#[cfg(test)]
thread_local! {
    static SYNC_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn sync(f: &File) -> std::io::Result<()> {
    #[cfg(test)]
    SYNC_CALLS.with(|c| c.set(c.get() + 1));
    f.sync_all()
}

/// Resolve `log` and refuse a directory or anything that resolves outside `root` (a symlink out). The file may not exist yet:
/// then its parent must resolve inside `root`. A dangling symlink is refused.
pub fn check_log_path(root: &Path, log: &Path) -> Result<PathBuf, AppendError> {
    let bad = |m: &str| AppendError::Path(m.to_string());
    let root = root.canonicalize().map_err(|_| bad("root is not readable"))?;
    let resolved = match fs::symlink_metadata(log) {
        Ok(_) => log.canonicalize().map_err(|_| bad("path does not resolve"))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let name = log.file_name().ok_or_else(|| bad("path has no file name"))?;
            let parent = match log.parent() {
                Some(p) if !p.as_os_str().is_empty() => p,
                _ => Path::new("."),
            };
            parent.canonicalize().map_err(|_| bad("parent folder does not resolve"))?.join(name)
        }
        Err(_) => return Err(bad("path is not readable")),
    };
    if !resolved.starts_with(&root) {
        return Err(bad("path resolves outside the root"));
    }
    if resolved.is_dir() {
        return Err(bad("path is a directory"));
    }
    Ok(resolved)
}

/// Append one event: validate and encode (cap), check the path, open in append mode (create if absent), refuse a torn tail,
/// issue exactly ONE write of the whole line, then `sync_all`. Success means the line is on disk. The torn-tail check looks only
/// at the last byte; it does not validate the rest of the log.
pub fn append(root: &Path, log: &Path, ev: &Event) -> Result<(), AppendError> {
    append_impl(root, log, ev, true)
}

/// Append one record of ANY kind (the writer the P1b kinds are written through; later tickets call it, this ticket's
/// code does not). Same file protocol as `append`: validate and encode first (a refused record never touches the
/// file), path check, lock, torn-tail check, ONE write of the whole line, then `sync_all`.
pub fn append_record(root: &Path, log: &Path, rec: &Record) -> Result<(), AppendError> {
    let line = rec.encode()?;
    let mut f = open_locked(root, log, true)?;
    write_locked(&mut f, line.as_bytes(), true)
}

/// Open the log for appending under the module's exclusive advisory lock: path check, open (creating it only when `create`
/// — an inherited-id check must never conjure an empty log into existence), lock, then the torn-tail check on the last
/// byte. The handle is left readable at position 0, so a caller may read the settled file through the SAME lock before
/// writing — no concurrent writer of this module can be caught mid-append.
fn open_locked(root: &Path, log: &Path, create: bool) -> Result<File, AppendError> {
    let path = check_log_path(root, log)?;
    let io = |e: std::io::Error| AppendError::Io(e.kind().to_string());
    // NOTE for 6b: `check_log_path` resolves the path and `open` resolves it again, so a symlink swapped in between could
    // point the open outside the root. Not closed here (out of scope for 6a); re-check the opened file's identity if needed.
    let mut f = OpenOptions::new().read(true).append(true).create(create).open(&path).map_err(io)?;
    // Exclusive advisory lock across check + write + sync, so the torn-tail check only ever sees a settled file (another
    // writer of this module is never caught mid-append). Held until `f` drops. It does not bind a writer outside this module.
    f.lock().map_err(|e| AppendError::Io(e.to_string()))?;
    let len = f.metadata().map_err(io)?.len();
    if len > 0 {
        f.seek(SeekFrom::End(-1)).map_err(io)?;
        let mut last = [0u8; 1];
        f.read_exact(&mut last).map_err(io)?;
        if last[0] != b'\n' {
            return Err(AppendError::TornTail);
        }
    }
    f.seek(SeekFrom::Start(0)).map_err(io)?;
    Ok(f)
}

/// Write the already-encoded bytes and (optionally) sync. One `write` of the whole payload.
fn write_locked(f: &mut File, bytes: &[u8], do_sync: bool) -> Result<(), AppendError> {
    let io = |e: std::io::Error| AppendError::Io(e.kind().to_string());
    let n = f.write(bytes).map_err(io)?;
    if n != bytes.len() {
        return Err(AppendError::Io("short write".to_string()));
    }
    if do_sync {
        sync(f).map_err(io)?;
    }
    Ok(())
}

fn append_impl(root: &Path, log: &Path, ev: &Event, do_sync: bool) -> Result<(), AppendError> {
    let line = ev.encode()?;
    let mut f = open_locked(root, log, true)?;
    write_locked(&mut f, line.as_bytes(), do_sync)
}

// ---------------------------------------------------------------- run identity (v2)

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// A fresh run id: `run-<ms>-<pid>-<seq>`, a plain token, unique per call. The wall clock is only the first part: the
/// process id separates concurrent invocations and the process-wide sequence separates two issues inside one process
/// inside one millisecond, so two appends back-to-back never share an id.
pub fn new_run_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    format!("run-{}-{}-{}", now_ms(), std::process::id(), SEQ.fetch_add(1, Ordering::Relaxed) + 1)
}

/// Write one event under one run identity (the v2 writer).
///
/// Without `inherited` (the caller had no `HARNESS_RUN_ID`) a fresh id is self-issued: a `run_issued` line is written
/// first, carrying the same `ts_ms`, `ticket`, `repo`, `role` and `model` as the event, then the event itself — two lines
/// in one write, one sync.
///
/// With `inherited` (the value of `HARNESS_RUN_ID`), that exact id is accepted only when this same log already holds a
/// `run_issued` line for it; then exactly ONE line (the event) is appended. The whole operation — open, lock, read the
/// settled log, check the issue, write — happens under the module's exclusive lock, so a concurrent writer can never make
/// the issue check read a half-written line as torn or corrupt. An unissued, malformed or unshowable id is refused and
/// nothing is written — an id from the environment is inherited, never trusted on faith.
///
/// Both lines are encoded (validated, capped) before any byte is written: a refused event leaves no orphaned `run_issued`
/// line. The event's own `run_id` field is ignored (overwritten by the resolved id).
pub fn append_under_run(root: &Path, log: &Path, ev: &Event, inherited: Option<&str>) -> Result<String, AppendError> {
    let bad = |m: String| AppendError::Invalid(m);
    if let Some(id) = inherited {
        if !token_ok(id) {
            return Err(bad(format!(
                "HARNESS_RUN_ID {id:?} must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')"
            )));
        }
    }
    // Self-issue: the id and BOTH lines are settled before the file is touched, so a refused event never creates the log
    // and never leaves an orphaned run_issued line behind.
    let prepared = if inherited.is_none() {
        let run_id = new_run_id();
        let under = Event { run_id: run_id.clone(), ..ev.clone() };
        let under_line = under.encode()?;
        let issue = Event {
            kind: Kind::RunIssued,
            ts_ms: under.ts_ms,
            run_id: run_id.clone(),
            ticket: under.ticket.clone(),
            repo: under.repo.clone(),
            reason: REASON_RUN_ISSUED.to_string(),
            role: under.role.clone(),
            model: under.model.clone(),
        };
        Some((run_id, format!("{}{}", issue.encode()?, under_line)))
    } else {
        None
    };
    // No creation for an inherited id: a log that does not exist cannot show an issue, and the refusal must not
    // conjure an empty log into being.
    let mut f = open_locked(root, log, inherited.is_none()).map_err(|e| match (&e, inherited.is_some()) {
        (AppendError::Io(m), true) => bad(format!("HARNESS_RUN_ID is set, so the log must be readable to show that id was issued: {m}")),
        _ => e,
    })?;
    let io = |e: std::io::Error| AppendError::Io(e.kind().to_string());
    let (run_id, payload) = match prepared {
        Some(done) => done,
        None => {
            let id = inherited.unwrap();
            // Read through the locked handle: the file is settled (no writer of this module is mid-append), so the issue
            // check can never see a phantom torn tail or a fused line from an in-flight write.
            let mut bytes = Vec::new();
            f.read_to_end(&mut bytes).map_err(io)?;
            let read = parse_log(&bytes);
            if let Some(c) = &read.corrupt {
                return Err(bad(format!("HARNESS_RUN_ID is set, but the log is corrupt at line {} ({})", c.line, c.msg)));
            }
            if let Some(t) = &read.torn_tail {
                return Err(bad(format!("HARNESS_RUN_ID is set, but the log has a torn tail at line {}", t.line)));
            }
            if !read.events.iter().any(|e| e.kind == Kind::RunIssued && e.run_id == id) {
                return Err(bad(format!("HARNESS_RUN_ID {id:?} was not issued in this log; an unissued id is never written under")));
            }
            let under = Event { run_id: id.to_string(), ..ev.clone() };
            (id.to_string(), under.encode()?)
        }
    };
    write_locked(&mut f, payload.as_bytes(), true)?;
    Ok(run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    /// A scratch folder outside the repo, removed on drop.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Tmp {
            let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
            let p = std::env::temp_dir().join(format!("events-core-{}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst), nanos));
            fs::create_dir_all(&p).unwrap();
            Tmp(p.canonicalize().unwrap())
        }

        fn log(&self) -> PathBuf {
            self.0.join("events.jsonl")
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn ev(kind: Kind, n: u64) -> Event {
        Event {
            kind,
            ts_ms: n,
            run_id: "run-1".to_string(),
            ticket: "t-1".to_string(),
            repo: "my-repo".to_string(),
            reason: "cap.hit".to_string(),
            role: "executor".to_string(),
            model: "m-1".to_string(),
        }
    }

    /// The read record of a base-kind event (the reader's view of `ev`).
    fn rec(e: &Event) -> Record {
        Record::decode(e.encode().unwrap().trim_end()).unwrap()
    }

    /// The three cap kinds (run_issued has its own tests below); three lines, so a torn tail is always the third.
    fn three(t: &Tmp) -> Vec<u8> {
        for (i, k) in Kind::ALL.iter().take(3).enumerate() {
            append(&t.0, &t.log(), &ev(*k, i as u64 + 1)).unwrap();
        }
        fs::read(t.log()).unwrap()
    }

    // AC1 round trip

    #[test]
    fn ac1_each_kind_round_trips_byte_identical_in_append_order() {
        let t = Tmp::new();
        let bytes = three(&t);
        let r = read_log(&t.log()).unwrap();
        assert!(r.is_clean());
        assert_eq!(r.events.len(), 3);
        let again: String = r.events.iter().map(|e| e.encode().unwrap()).collect();
        assert_eq!(again.as_bytes(), &bytes[..], "re-encoding the read events is byte-identical");
        assert_eq!(r.events.iter().map(|e| e.kind).collect::<Vec<_>>(), Kind::ALL[..3].to_vec());
        assert_eq!(
            ev(Kind::BudgetExceeded, 7).encode().unwrap(),
            "{\"v\":2,\"kind\":\"budget_exceeded\",\"ts_ms\":7,\"run_id\":\"run-1\",\"ticket\":\"t-1\",\"repo\":\"my-repo\",\"reason\":\"cap.hit\",\"role\":\"executor\",\"model\":\"m-1\"}\n"
        );
    }

    #[test]
    fn ac1_planted_a_fifth_kind_is_refused_on_read_and_cannot_be_constructed() {
        assert_eq!(Kind::parse("surprise"), None);
        let l = "{\"v\":2,\"kind\":\"surprise\",\"ts_ms\":1,\"run_id\":\"run-1\",\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\",\"role\":\"executor\",\"model\":\"m-1\"}";
        assert!(Record::decode(l).unwrap_err().contains("unknown kind"));
        // The twin: the same line with a real kind decodes.
        assert!(Record::decode(&l.replace("surprise", "escalation")).is_ok());
    }

    // AC2 torn tail

    #[test]
    fn ac2_torn_tail_is_reported_and_never_read_as_an_event() {
        let t = Tmp::new();
        let bytes = three(&t);
        let third_start = bytes.len() - bytes.iter().rev().skip(1).position(|b| *b == b'\n').unwrap() - 1;
        for cut in [third_start + 1, third_start + 20, bytes.len() - 1] {
            let r = parse_log(&bytes[..cut]);
            assert_eq!(r.events.len(), 2, "cut at {cut}: never 3, never the fragment");
            assert_eq!(r.torn_tail, Some(TornTail { line: 3, bytes: cut - third_start }));
            assert!(r.corrupt.is_none());
            assert!(!r.is_clean());
        }
    }

    #[test]
    fn ac2_planted_a_cut_exactly_at_a_line_boundary_is_clean() {
        let t = Tmp::new();
        let bytes = three(&t);
        let r = parse_log(&bytes);
        assert!(r.is_clean());
        assert_eq!(r.events.len(), 3);
        let two = parse_log(&bytes[..bytes.iter().position(|b| *b == b'\n').unwrap() + 1]);
        assert!(two.is_clean() && two.events.len() == 1);
    }

    #[test]
    fn ac2_planted_a_valid_json_fragment_without_a_newline_is_still_torn() {
        let one = ev(Kind::Escalation, 1).encode().unwrap();
        let no_nl = one.trim_end_matches('\n');
        let r = parse_log(no_nl.as_bytes());
        assert!(r.events.is_empty(), "a complete-looking event with no newline is not an event");
        assert_eq!(r.torn_tail, Some(TornTail { line: 1, bytes: no_nl.len() }));
        // The twin: add the newline and it is one event.
        assert_eq!(parse_log(one.as_bytes()).events.len(), 1);
    }

    #[test]
    fn ac2_a_torn_tail_that_is_not_utf8_is_torn_not_corrupt() {
        let mut b = ev(Kind::Escalation, 1).encode().unwrap().into_bytes();
        b.extend_from_slice(&[0xff, 0xfe, b'{']);
        let r = parse_log(&b);
        assert_eq!(r.events.len(), 1);
        assert!(r.torn_tail.is_some() && r.corrupt.is_none());
    }

    // AC3 no fusion

    #[test]
    fn ac3_append_onto_a_torn_tail_is_refused_and_the_file_is_unchanged() {
        let t = Tmp::new();
        let bytes = three(&t);
        let cut = &bytes[..bytes.len() - 7];
        fs::write(t.log(), cut).unwrap();
        let e = append(&t.0, &t.log(), &ev(Kind::Escalation, 9)).unwrap_err();
        assert_eq!(e, AppendError::TornTail);
        assert!(e.to_string().contains("torn tail"));
        assert_eq!(fs::read(t.log()).unwrap(), cut, "bytes unchanged");
        // The twin: the same log with its newline restored accepts the append.
        fs::write(t.log(), &bytes).unwrap();
        append(&t.0, &t.log(), &ev(Kind::Escalation, 9)).unwrap();
        assert_eq!(read_log(&t.log()).unwrap().events.len(), 4);
    }

    #[test]
    fn ac3_a_file_holding_only_a_fragment_is_a_torn_tail_too() {
        let t = Tmp::new();
        fs::write(t.log(), b"{\"v\":1").unwrap();
        assert_eq!(append(&t.0, &t.log(), &ev(Kind::Escalation, 1)).unwrap_err(), AppendError::TornTail);
        assert_eq!(fs::read(t.log()).unwrap(), b"{\"v\":1");
    }

    // AC4 unknown schema version

    #[test]
    fn ac4_unknown_or_missing_v_is_refused_naming_v_and_yields_no_event() {
        let good = ev(Kind::Escalation, 1).encode().unwrap();
        let good = good.trim_end();
        assert!(Record::decode(good).is_ok(), "twin: v=2 passes");
        for (bad, want) in [
            (good.replace("\"v\":2", "\"v\":3"), "unsupported v 3"),
            (good.replace("\"v\":2", "\"v\":1"), "unsupported v 1"),
            (good.replace("\"v\":2", "\"v\":0"), "unsupported v 0"),
            (good.replace("\"v\":2,", ""), "v missing"),
            (good.replace("\"v\":2", "\"v\":\"2\""), "v has the wrong type"),
        ] {
            assert_eq!(Record::decode(&bad).unwrap_err(), want, "{bad}");
            let log = format!("{good}\n{bad}\n{good}\n");
            let r = parse_log(log.as_bytes());
            assert_eq!(r.events.len(), 1, "only the line before the bad one");
            let c = r.corrupt.unwrap();
            assert_eq!(c.line, 2);
            assert_eq!(c.msg, want);
        }
        // v is checked before the rest, so a v3 line with an unknown kind still names v. A v1 line (no run identity) is
        // refused the same way: it is never upgraded by guess.
        assert_eq!(Record::decode(&good.replace("\"v\":2", "\"v\":3").replace("escalation", "future")).unwrap_err(), "unsupported v 3");
        let v1 = "{\"v\":1,\"kind\":\"escalation\",\"ts_ms\":1,\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\"}";
        assert_eq!(Record::decode(v1).unwrap_err(), "unsupported v 1");
    }

    // AC5 corruption is refused, not skipped

    #[test]
    fn ac5_each_failure_type_in_the_middle_stops_the_read_at_that_line() {
        let good = ev(Kind::ReplanRequired, 1).encode().unwrap();
        let g = good.trim_end();
        let bads = [
            ("bad json", "{not json}".to_string()),
            ("extra key", g.replace("}", ",\"extra\":\"x\"}")),
            ("missing key", g.replace(",\"reason\":\"cap.hit\"", "")),
            ("duplicate key", g.replace("\"repo\":\"my-repo\",", "\"repo\":\"a\",\"repo\":\"b\",")),
            ("unknown kind", g.replace("replan_required", "nope")),
            ("wrong type", g.replace("\"ts_ms\":1", "\"ts_ms\":\"1\"")),
            ("bad token", g.replace("my-repo", "My Repo")),
            ("escape", g.replace("cap.hit", "cap\\u002ehit")),
            ("negative ts", g.replace("\"ts_ms\":1", "\"ts_ms\":-1")),
            ("empty line", String::new()),
            ("trailing bytes", format!("{g} x")),
        ];
        for (name, bad) in bads {
            let log = format!("{g}\n{g}\n{bad}\n{g}\n");
            let r = parse_log(log.as_bytes());
            assert_eq!(r.events.len(), 2, "{name}: the intact events before the bad line");
            assert_eq!(r.corrupt.as_ref().map(|c| c.line), Some(3), "{name}");
            assert!(r.torn_tail.is_none(), "{name}");
        }
        // The twin: the same log with a good third line is clean with four events.
        let all = parse_log(format!("{g}\n{g}\n{g}\n{g}\n").as_bytes());
        assert!(all.is_clean() && all.events.len() == 4);
    }

    #[test]
    fn ac5_a_corrupt_first_line_returns_nothing() {
        let g = ev(Kind::Escalation, 1).encode().unwrap();
        let r = parse_log(format!("garbage\n{g}").as_bytes());
        assert!(r.events.is_empty());
        assert_eq!(r.corrupt.unwrap().line, 1);
        // The twin: a valid first line passes.
        assert!(parse_log(g.as_bytes()).is_clean());
    }

    #[test]
    fn ac5_corruption_wins_over_a_later_torn_tail() {
        let g = ev(Kind::Escalation, 1).encode().unwrap();
        let r = parse_log(format!("{g}bad\n{{\"v\"").as_bytes());
        assert_eq!(r.events.len(), 1);
        assert_eq!(r.corrupt.unwrap().line, 2);
        assert!(r.torn_tail.is_none());
    }

    #[test]
    fn ac5_an_over_cap_complete_line_is_corruption() {
        let long = format!(
            "{{\"v\":2,\"kind\":\"escalation\",\"ts_ms\":1,\"run_id\":\"run-1\",\"ticket\":\"{}\",\"repo\":\"r\",\"reason\":\"x\",\"role\":\"executor\",\"model\":\"m-1\"}}",
            "a".repeat(LINE_CAP)
        );
        let r = parse_log(format!("{long}\n").as_bytes());
        assert!(r.events.is_empty());
        assert!(r.corrupt.unwrap().msg.contains("cap"));
    }

    // AC6 concurrent writers

    const WRITERS: usize = 8;
    const PER: usize = 50;

    fn run_writers(t: &Tmp, write_one: fn(&Path, &Path, &Event)) {
        let barrier = Arc::new(std::sync::Barrier::new(WRITERS));
        let handles: Vec<_> = (0..WRITERS)
            .map(|w| {
                let (root, log, b) = (t.0.clone(), t.log(), barrier.clone());
                std::thread::spawn(move || {
                    b.wait();
                    for i in 0..PER {
                        let e = Event { kind: Kind::Escalation, ts_ms: i as u64, run_id: format!("run-{w}"), ticket: format!("w{w}"), repo: "r".to_string(), reason: "x".to_string(), role: "executor".to_string(), model: "m-1".to_string() };
                        write_one(&root, &log, &e);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
    }

    fn real_write(root: &Path, log: &Path, e: &Event) {
        append(root, log, e).unwrap();
    }

    /// Test-only: two writes per line. This is what a non-atomic writer looks like.
    fn split_write(_root: &Path, log: &Path, e: &Event) {
        let line = e.encode().unwrap();
        let (a, b) = line.as_bytes().split_at(line.len() / 2);
        let mut f = OpenOptions::new().append(true).create(true).open(log).unwrap();
        f.write_all(a).unwrap();
        std::thread::sleep(Duration::from_micros(300));
        f.write_all(b).unwrap();
    }

    #[test]
    fn ac6_eight_writers_fifty_events_each_no_fused_line() {
        let t = Tmp::new();
        run_writers(&t, real_write);
        let r = read_log(&t.log()).unwrap();
        assert!(r.is_clean(), "{:?}", r.corrupt);
        assert_eq!(r.events.len(), WRITERS * PER);
        for w in 0..WRITERS {
            let mine: Vec<u64> = r.events.iter().filter(|e| e.ticket == format!("w{w}")).map(|e| e.ts_ms).collect();
            assert_eq!(mine, (0..PER as u64).collect::<Vec<_>>(), "writer {w}: all present, own order kept");
        }
    }

    #[test]
    fn ac6_planted_a_non_atomic_writer_is_caught_by_the_same_check() {
        let t = Tmp::new();
        run_writers(&t, split_write);
        let r = read_log(&t.log()).unwrap();
        assert!(!r.is_clean() || r.events.len() != WRITERS * PER, "the check must see interleaving from a two-write writer");
    }

    /// Mutation proof for the lock: while another handle holds the lock with a half-written line on disk, `append` must wait
    /// (not refuse). Without the lock it would read the half line as a torn tail and return `Err(TornTail)` at once.
    #[test]
    fn ac6_append_waits_for_the_lock_instead_of_seeing_an_in_flight_line() {
        let t = Tmp::new();
        let line = ev(Kind::Escalation, 1).encode().unwrap();
        let (a, b) = line.as_bytes().split_at(line.len() / 2);
        let mut holder = OpenOptions::new().append(true).create(true).open(t.log()).unwrap();
        holder.lock().unwrap();
        holder.write_all(a).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let (root, log) = (t.0.clone(), t.log());
        let h = std::thread::spawn(move || tx.send(append(&root, &log, &ev(Kind::Escalation, 2))).unwrap());
        assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "append must still be waiting on the lock, not have refused");
        holder.write_all(b).unwrap();
        drop(holder);
        assert_eq!(rx.recv_timeout(Duration::from_secs(20)).unwrap(), Ok(()));
        h.join().unwrap();
        let r = read_log(&t.log()).unwrap();
        assert!(r.is_clean());
        assert_eq!(r.events.iter().map(|e| e.ts_ms).collect::<Vec<_>>(), vec![1, 2]);
    }

    #[test]
    fn ac6_planted_deterministic_interleave_is_seen() {
        let g = ev(Kind::Escalation, 1).encode().unwrap();
        let (a, b) = g.as_bytes().split_at(g.len() / 2);
        let other = ev(Kind::Escalation, 2).encode().unwrap();
        let mut bytes = a.to_vec();
        bytes.extend_from_slice(other.as_bytes());
        bytes.extend_from_slice(b);
        assert!(parse_log(&bytes).corrupt.is_some());
    }

    // AC7 durability and size

    #[test]
    fn ac7_an_over_cap_event_is_refused_and_the_file_is_unchanged() {
        let t = Tmp::new();
        append(&t.0, &t.log(), &ev(Kind::Escalation, 1)).unwrap();
        let before = fs::read(t.log()).unwrap();
        let mut big = ev(Kind::Escalation, 2);
        big.reason = "a".repeat(LINE_CAP);
        assert!(matches!(append(&t.0, &t.log(), &big).unwrap_err(), AppendError::TooLong(_)));
        assert_eq!(fs::read(t.log()).unwrap(), before);
        // The twin: just under the cap passes (line, newline included, is exactly LINE_CAP).
        let fixed = big.encode().map(|_| 0).unwrap_err();
        let AppendError::TooLong(n) = fixed else { panic!() };
        big.reason = "a".repeat(LINE_CAP - (n - LINE_CAP));
        assert_eq!(big.encode().unwrap().len(), LINE_CAP);
        append(&t.0, &t.log(), &big).unwrap();
        assert!(read_log(&t.log()).unwrap().is_clean());
    }

    fn syncs_during(f: impl FnOnce()) -> usize {
        SYNC_CALLS.with(|c| c.set(0));
        f();
        SYNC_CALLS.with(|c| c.get())
    }

    #[test]
    fn ac7_a_successful_append_calls_sync_all_once_and_a_writer_that_skips_it_is_seen() {
        let t = Tmp::new();
        assert_eq!(syncs_during(|| append(&t.0, &t.log(), &ev(Kind::Escalation, 1)).unwrap()), 1, "the real writer must fsync");
        // Planted: the no-fsync writer registers zero calls, which is exactly what the assertion above would fail on.
        assert_eq!(syncs_during(|| append_impl(&t.0, &t.log(), &ev(Kind::Escalation, 2), false).unwrap()), 0);
        // A refused append never reaches the sync.
        let mut bad = ev(Kind::Escalation, 0);
        bad.ticket = "BAD".to_string();
        assert_eq!(syncs_during(|| drop(append(&t.0, &t.log(), &bad))), 0);
    }

    #[test]
    fn ac7_the_line_is_on_disk_when_append_returns() {
        let t = Tmp::new();
        append(&t.0, &t.log(), &ev(Kind::BudgetExceeded, 5)).unwrap();
        // Read through a fresh handle: no in-process state is involved.
        let r = read_log(&t.log()).unwrap();
        assert_eq!(r.events, vec![rec(&ev(Kind::BudgetExceeded, 5))]);
    }

    #[test]
    fn rule18_fsync_cost_is_measured_not_assumed() {
        let t = Tmp::new();
        let st = std::time::Instant::now();
        for i in 0..20 {
            append(&t.0, &t.log(), &ev(Kind::Escalation, i)).unwrap();
        }
        eprintln!("events fsync smoke: 20 appends in {:?}", st.elapsed());
        assert_eq!(read_log(&t.log()).unwrap().events.len(), 20);
    }

    // AC8 restart

    #[test]
    fn ac8_a_second_writer_after_the_first_exits_appends_in_order() {
        let t = Tmp::new();
        let a = |n| append(&t.0, &t.log(), &ev(Kind::Escalation, n)).unwrap();
        a(1);
        a(2);
        a(3);
        assert_eq!(read_log(&t.log()).unwrap().events.iter().map(|e| e.ts_ms).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn ac8_planted_a_bad_first_line_is_refused_by_the_reader_but_not_by_the_tail_only_writer() {
        let t = Tmp::new();
        let g = ev(Kind::Escalation, 1).encode().unwrap();
        fs::write(t.log(), format!("garbage\n{g}")).unwrap();
        let r = read_log(&t.log()).unwrap();
        assert!(r.events.is_empty() && r.corrupt.is_some());
        append(&t.0, &t.log(), &ev(Kind::Escalation, 2)).unwrap();
        assert_eq!(read_log(&t.log()).unwrap().corrupt.unwrap().line, 1, "still refused on read");
        // Twin: a valid first line passes.
        fs::write(t.log(), &g).unwrap();
        assert!(read_log(&t.log()).unwrap().is_clean());
    }

    // AC9 fail-closed inputs

    #[test]
    fn ac9_bad_tokens_are_refused_and_nothing_is_created() {
        let t = Tmp::new();
        for bad in ["", "Upper", "has space", "nl\n", "q\"uote", "back\\slash", ".", "..", "a/b", "tab\t", "uni\u{e9}"] {
            for field in 0..6 {
                let mut e = ev(Kind::Escalation, 1);
                match field {
                    0 => e.run_id = bad.to_string(),
                    1 => e.ticket = bad.to_string(),
                    2 => e.repo = bad.to_string(),
                    3 => e.reason = bad.to_string(),
                    4 => e.role = bad.to_string(),
                    _ => e.model = bad.to_string(),
                }
                assert!(matches!(append(&t.0, &t.log(), &e).unwrap_err(), AppendError::Invalid(_)), "{bad:?} field {field}");
            }
        }
        assert!(!t.log().exists(), "a refused event leaves no file");
        // Twin: the valid token passes.
        assert!(token_ok("a-b_c.1"));
        append(&t.0, &t.log(), &ev(Kind::Escalation, 1)).unwrap();
    }

    #[test]
    fn ac9_a_directory_as_the_log_is_refused() {
        let t = Tmp::new();
        let d = t.0.join("adir");
        fs::create_dir(&d).unwrap();
        assert!(matches!(append(&t.0, &d, &ev(Kind::Escalation, 1)).unwrap_err(), AppendError::Path(_)));
    }

    #[cfg(unix)]
    #[test]
    fn ac9_a_symlink_resolving_outside_the_root_is_refused() {
        let root = Tmp::new();
        let outside = Tmp::new();
        std::os::unix::fs::symlink(outside.log(), root.log()).unwrap();
        assert!(matches!(append(&root.0, &root.log(), &ev(Kind::Escalation, 1)).unwrap_err(), AppendError::Path(_)));
        assert!(!outside.log().exists(), "nothing was written through the link");
        // A symlinked folder out of the root is refused for a new file too.
        let link = root.0.join("out");
        std::os::unix::fs::symlink(&outside.0, &link).unwrap();
        assert!(matches!(append(&root.0, &link.join("e.jsonl"), &ev(Kind::Escalation, 1)).unwrap_err(), AppendError::Path(_)));
        // A dangling link is refused.
        let dang = root.0.join("dangling.jsonl");
        std::os::unix::fs::symlink(root.0.join("nowhere"), &dang).unwrap();
        assert!(matches!(append(&root.0, &dang, &ev(Kind::Escalation, 1)).unwrap_err(), AppendError::Path(_)));
        // Twin: a symlink that stays inside the root is accepted.
        let real = root.0.join("real.jsonl");
        let inside = root.0.join("inside.jsonl");
        fs::write(&real, b"").unwrap();
        std::os::unix::fs::symlink(&real, &inside).unwrap();
        append(&root.0, &inside, &ev(Kind::Escalation, 1)).unwrap();
        assert_eq!(read_log(&real).unwrap().events.len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn ac9_an_unwritable_log_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let t = Tmp::new();
        append(&t.0, &t.log(), &ev(Kind::Escalation, 1)).unwrap();
        fs::set_permissions(t.log(), fs::Permissions::from_mode(0o400)).unwrap();
        let writable_anyway = OpenOptions::new().append(true).open(t.log()).is_ok(); // running as a user that ignores modes
        let r = append(&t.0, &t.log(), &ev(Kind::Escalation, 2));
        fs::set_permissions(t.log(), fs::Permissions::from_mode(0o600)).unwrap();
        if !writable_anyway {
            assert!(matches!(r.unwrap_err(), AppendError::Io(_)));
            assert_eq!(read_log(&t.log()).unwrap().events.len(), 1);
        }
    }

    #[test]
    fn a_missing_log_is_an_error_on_read_not_an_empty_log() {
        let t = Tmp::new();
        assert!(read_log(&t.log()).is_err());
    }

    #[test]
    fn every_kind_declares_the_trio_first_an_exact_unique_key_set_and_only_v2() {
        let trio = ["v", "kind", "ts_ms"];
        for k in Kind::ALL {
            let keys = k.keys();
            assert_eq!(&keys[..3], &trio, "{}: the shared trio comes first", k.as_str());
            assert_eq!(keys.len(), keys.iter().collect::<std::collections::HashSet<_>>().len(), "{}: no key twice", k.as_str());
            assert_eq!(k.version(), SCHEMA_VERSION, "{}: the table's v row", k.as_str());
            assert_eq!(Kind::parse(k.as_str()), Some(k));
            if k.is_base() {
                assert_eq!(keys, KEYS_V2, "{}: the PRE-B envelope, bytes as on master", k.as_str());
            } else {
                assert!(keys.starts_with(&P1B_ENVELOPE), "{}: the shared P1b envelope", k.as_str());
                assert!(keys.len() > 3, "{}: at least one payload field", k.as_str());
            }
        }
        assert_eq!(Kind::ALL.iter().filter(|k| k.is_base()).count(), 4);
        assert_eq!(Kind::ALL.iter().filter(|k| !k.is_base()).count(), 9);
    }

    #[test]
    fn a_base_kind_line_encodes_exactly_its_declared_keys_in_order() {
        for k in [Kind::Escalation, Kind::ReplanRequired, Kind::BudgetExceeded, Kind::RunIssued] {
            let mut e = ev(k, 1);
            if k == Kind::RunIssued {
                e.reason = REASON_RUN_ISSUED.to_string();
            }
            let line = e.encode().unwrap();
            let mut p = P { b: line.trim_end().as_bytes(), i: 0 };
            let keys: Vec<String> = p.object().unwrap().into_iter().map(|(k, _)| k).collect();
            assert_eq!(keys, k.keys());
            // The reader returns the same line back.
            let r = Record::decode(line.trim_end()).unwrap();
            assert_eq!(r.encode().unwrap(), line);
        }
    }

    #[test]
    fn the_base_event_writer_refuses_a_p1b_kind_before_any_value_is_looked_at() {
        for k in Kind::ALL {
            let mut e = ev(Kind::Escalation, 1);
            e.kind = k;
            let got = e.validate();
            if k.is_base() {
                assert!(got.is_ok(), "{}", k.as_str());
            } else {
                assert_eq!(got.unwrap_err(), format!("kind {} is a P1b kind: the base event shape cannot express its payload (write it through the record writer)", k.as_str()));
                // Refused at encode, so `append`/`append_under_run` write nothing.
                assert!(matches!(e.encode(), Err(AppendError::Invalid(_))));
            }
        }
    }

    // ---------------------------------------------------------------- run identity (v2)

    #[test]
    fn run_ids_are_unique_tokens_even_back_to_back() {
        let a = new_run_id();
        let b = new_run_id();
        let c = new_run_id();
        assert!(a != b && b != c && a != c, "three issues in one millisecond differ: {a} {b} {c}");
        for id in [a, b, c] {
            assert!(token_ok(&id), "{id} is a plain token");
            assert!(id.starts_with("run-"), "{id}");
        }
    }

    #[test]
    fn a_self_issued_write_appends_run_issued_then_the_event_under_one_id() {
        let t = Tmp::new();
        let id = append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 5), None).unwrap();
        assert!(token_ok(&id));
        let r = read_log(&t.log()).unwrap();
        assert!(r.is_clean());
        assert_eq!(r.events.len(), 2, "exactly two lines: run_issued, then the event");
        assert_eq!((r.events[0].kind, r.events[0].run_id.as_str(), r.events[0].reason.as_str()), (Kind::RunIssued, id.as_str(), REASON_RUN_ISSUED));
        assert_eq!(
            (r.events[1].kind, r.events[1].run_id.as_str(), r.events[1].ts_ms, r.events[1].ticket.as_str(), r.events[1].role.as_str(), r.events[1].model.as_str()),
            (Kind::Escalation, id.as_str(), 5, "t-1", "executor", "m-1")
        );
        // The issue line carries the event's own stamp and identity, not a second clock read.
        assert_eq!(r.events[0].ts_ms, r.events[1].ts_ms);
        assert_eq!((r.events[0].ticket.as_str(), r.events[0].repo.as_str(), r.events[0].role.as_str(), r.events[0].model.as_str()), ("t-1", "my-repo", "executor", "m-1"));
    }

    #[test]
    fn an_inherited_issued_id_adds_exactly_one_line_and_no_new_issue() {
        let t = Tmp::new();
        let id = append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 1), None).unwrap();
        append_under_run(&t.0, &t.log(), &ev(Kind::ReplanRequired, 2), Some(&id)).unwrap();
        let r = read_log(&t.log()).unwrap();
        assert_eq!(r.events.len(), 3, "issue + two events");
        assert_eq!(r.events.iter().filter(|e| e.kind == Kind::RunIssued).count(), 1, "no second run_issued");
        assert_eq!(r.events[2].run_id, id);
        // The same id issued in a DIFFERENT log is not inherited there: the issue is per log.
        let other = Tmp::new();
        assert!(append_under_run(&other.0, &other.log(), &ev(Kind::Escalation, 1), Some(&id)).is_err());
        assert!(!other.log().exists(), "the refusal left no file");
    }

    #[test]
    fn an_unissued_or_malformed_inherited_id_is_refused_and_writes_nothing() {
        let t = Tmp::new();
        append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 1), None).unwrap();
        let before = fs::read(t.log()).unwrap();
        for bad in ["never-issued", "Bad_ID", "", ".", "..", "run-1 "] {
            let e = append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 2), Some(bad)).unwrap_err();
            assert!(matches!(e, AppendError::Invalid(_)), "{bad:?}: {e}");
            assert_eq!(fs::read(t.log()).unwrap(), before, "{bad:?}: nothing was written");
        }
        // Twin: the issued id itself passes.
        let id = read_log(&t.log()).unwrap().events[0].run_id.clone();
        assert!(append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 3), Some(&id)).is_ok());
        // An inherited id with no log at all is refused (the issue cannot be shown), never treated as fresh.
        let empty = Tmp::new();
        assert!(append_under_run(&empty.0, &empty.log(), &ev(Kind::Escalation, 1), Some("run-x")).is_err());
        assert!(!empty.log().exists());
    }

    #[test]
    fn an_inherited_id_over_a_torn_or_corrupt_log_is_refused() {
        let t = Tmp::new();
        let id = append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 1), None).unwrap();
        let bytes = fs::read(t.log()).unwrap();
        fs::write(t.log(), &bytes[..bytes.len() - 1]).unwrap();
        assert!(append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 2), Some(&id)).is_err(), "torn tail");
        fs::write(t.log(), format!("garbage\n{}", String::from_utf8_lossy(&bytes))).unwrap();
        assert!(append_under_run(&t.0, &t.log(), &ev(Kind::Escalation, 2), Some(&id)).is_err(), "corruption");
    }

    #[test]
    fn a_self_issued_event_with_a_bad_role_or_model_is_refused_and_the_issue_is_not_written() {
        let t = Tmp::new();
        let mut bad = ev(Kind::Escalation, 1);
        bad.role = "Executor".to_string();
        assert!(append_under_run(&t.0, &t.log(), &bad, None).is_err());
        assert!(!t.log().exists(), "neither the run_issued line nor the event was written");
    }
}
