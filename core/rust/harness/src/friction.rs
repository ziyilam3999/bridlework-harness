//! The friction inbox (K-1 first slice, ticket 154): one row schema, one lint, one append, one close-note check.
//! The contract in plain words is `docs/friction-inbox.md`; this module is its strict reader and writer (std only).
//!
//! The inbox file itself is NEVER in this repo: it lives only in the private overlay (the config's `friction.inbox`).
//! Nothing here prints a lesson text, an inbox path or any other value a refusal saw: a refusal names the field,
//! the line number and the reason, so a private row can never leak through an error message.
//!
//! Exit codes (the whole verb family): 0 ok; 1 a refusal (a bad row, a privacy hit, a close note that does not
//! match the inbox — a refused write leaves the inbox byte-identical); 2 a usage or setup error (bad command line,
//! no config configured, an unreadable or oversized inbox or close note, no privacy list configured).

use crate::events::token_ok;
use std::fs;
use std::io::{Read, Write};
use std::path::Path;

/// The four capture-time routes (K-1 rule 2). The tag is the route chosen at capture, nothing more.
pub const TAGS: [&str; 4] = ["skill", "gate", "memory", "wiki"];
/// The row fields in canonical order (the JSON key order this module writes).
pub const FIELDS: [&str; 6] = ["id", "date", "ticket", "where-it-lives", "breaks", "text"];
/// The nine optional fields: `kind` and `repeats` (ticket 192) and `source` (ticket 193), then six added
/// after ticket 193 (`at`, `impact`, `why`, `proof`, `status`, `status_ref`). `kind` is the row's class
/// (default `friction`) and `repeats` the id of the pattern row this row repeats; `source` is the row's origin
/// (default empty = hand-written; the only other value is `event`, the T-9.3 event rows the `friction_event`
/// mapper produces). Each is omitted from the canonical line at its default, so every six-key row written
/// before this contract re-encodes byte-identical.
pub const OPTIONAL_FIELDS: [&str; 9] = ["kind", "repeats", "source", "at", "impact", "why", "proof", "status", "status_ref"];
/// The five non-default `status` words (the default, `open`, is an absent key). Each needs a `status_ref` pointer
/// of one kind (see `status_ref_problem`).
pub const STATUSES: [&str; 5] = ["fixed", "guarded", "ticket", "lesson", "no-action"];
/// The four evidence keys (ticket 3601) are the last four of `OPTIONAL_FIELDS`: `at` (an exact UTC timestamp),
/// `impact` (a rating), `why` (the cause) and `proof` (typed pointers). Each is omitted at its default, so every
/// row written before them re-encodes byte-identical. The legal `impact` values:
pub const IMPACTS: [&str; 4] = ["low", "medium", "high", "blocker"];
/// `why` is one line of at most this many bytes.
pub const WHY_CAP: usize = 1000;
/// `proof` is one line of at most this many bytes.
pub const PROOF_CAP: usize = 2000;
/// The two legal `kind` values (the default, `friction`, needs no key).
pub const KINDS: [&str; 2] = ["friction", "pattern"];
/// The legal non-default `source` values (the default, hand-written, needs no key).
pub const SOURCES: [&str; 1] = ["event"];
/// One encoded row, bytes, including the trailing newline this module always writes.
pub const LINE_CAP: usize = 16 * 1024;
/// The whole inbox is read no further than this; past it the verb fails closed (exit 2).
const INBOX_CAP: u64 = 4 * 1024 * 1024;
/// A close note is read no further than this; past it the check fails closed (exit 2).
const NOTE_CAP: u64 = 1024 * 1024;
/// The event log a sweep reads no further than; past it the sweep fails closed (exit 2). The same bound as
/// the inbox: the verb family reads both whole (ticket 200). The event log is append-only; the inbox is
/// appended to, and `set-status` rewrites a line of it in place.
const EVENT_LOG_CAP: u64 = 4 * 1024 * 1024;

/// One inbox row. `breaks` is empty or the id of the lesson that broke again (K-1 rule 3). `kind` is empty
/// (= `friction`), `friction` or `pattern`; `repeats` is empty or the id of the `kind: pattern` row this row
/// repeats (ticket 192); `source` is empty (hand-written) or `event` (the T-9.3 mapper, ticket 193). A
/// `pattern` row with no `repeats` is the root of its chain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub id: String,
    pub date: String,
    pub ticket: String,
    pub tag: String,
    pub breaks: String,
    pub text: String,
    pub kind: String,
    pub repeats: String,
    pub source: String,
    /// Optional (ticket 3601): `YYYY-MM-DDTHH:MM:SSZ`, its date part equal to `date`; empty = absent.
    pub at: String,
    /// Optional: `low`, `medium`, `high` or `blocker`; empty = unrated.
    pub impact: String,
    /// Optional: one line, the cause (suspected or proven), at most `WHY_CAP` bytes.
    pub why: String,
    /// Optional: `; `-separated typed pointers, at most `PROOF_CAP` bytes (see `proof_problem`).
    pub proof: String,
}

/// The two status keys of a row, kept beside the `Row` (a row built without them is open). `status` is empty
/// (= open; a canonical row omits the key), or one of `STATUSES`; `status_ref` is typed pointers (the proof
/// grammar and cap), required with a status and refused without one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub status: String,
    pub status_ref: String,
}

impl Status {
    /// The status word: `open` when the key is absent.
    pub fn word(&self) -> &str {
        if self.status.is_empty() {
            "open"
        } else {
            &self.status
        }
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` naming a real UTC instant (the date part a real calendar date).
pub fn at_ok(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 20 || b[10] != b'T' || b[13] != b':' || b[16] != b':' || b[19] != b'Z' {
        return false;
    }
    if !date_ok(&s[0..10]) {
        return false;
    }
    let two = |r: std::ops::Range<usize>| -> Option<u32> {
        if b[r.clone()].iter().all(|c| c.is_ascii_digit()) {
            s[r].parse().ok()
        } else {
            None
        }
    };
    matches!((two(11..13), two(14..16), two(17..19)), (Some(h), Some(m), Some(sec)) if h < 24 && m < 60 && sec < 60)
}

fn plain_path(p: &str) -> bool {
    !p.is_empty() && p.chars().all(|c| !c.is_whitespace() && !c.is_control())
}

/// One proof pointer, or the reason it is refused (the reason never echoes the pointer).
fn pointer_problem(p: &str) -> Option<&'static str> {
    const BAD: &str = "a pointer is not one of <repo>@<7-40 hex>:<path>[:<line>], session:<id>, pr:<owner>/<repo>#<n>, cmd:<text>, ~/<path>";
    if let Some(rest) = p.strip_prefix("session:") {
        return if token_ok(rest) { None } else { Some(BAD) };
    }
    if let Some(rest) = p.strip_prefix("pr:") {
        let Some((slug, n)) = rest.split_once('#') else { return Some(BAD) };
        let Some((owner, repo)) = slug.split_once('/') else { return Some(BAD) };
        let name = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        return if name(owner) && name(repo) && !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit()) { None } else { Some(BAD) };
    }
    if let Some(rest) = p.strip_prefix("cmd:") {
        return if !rest.trim().is_empty() && rest.chars().all(|c| !c.is_control()) { None } else { Some(BAD) };
    }
    if let Some(rest) = p.strip_prefix("~/") {
        return if plain_path(rest) { None } else { Some(BAD) };
    }
    if let Some((repo, rest)) = p.split_once('@') {
        let Some((sha, path)) = rest.split_once(':') else { return Some(BAD) };
        let sha_ok = (7..=40).contains(&sha.len()) && sha.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c));
        let path = match path.rsplit_once(':') {
            Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|c| c.is_ascii_digit()) => head,
            _ => path,
        };
        return if token_ok(repo) && sha_ok && plain_path(path) && !path.starts_with('/') && !path.starts_with('~') { None } else { Some(BAD) };
    }
    Some(BAD)
}

/// `proof` is `; `-separated pointers; the first refused one is the answer.
pub(crate) fn proof_problem(proof: &str) -> Option<&'static str> {
    if proof.len() > PROOF_CAP {
        return Some("proof is over its 2000-byte cap");
    }
    if proof.chars().any(|c| c.is_control()) {
        return Some("proof must be one line");
    }
    proof.split("; ").find_map(pointer_problem)
}

/// The kind of one pointer that already passed `pointer_problem`.
#[derive(PartialEq)]
enum PtrKind {
    Session,
    Pr,
    Cmd,
    Home,
    Repo,
}

fn ptr_kind(p: &str) -> PtrKind {
    if p.starts_with("session:") {
        PtrKind::Session
    } else if p.starts_with("pr:") {
        PtrKind::Pr
    } else if p.starts_with("cmd:") {
        PtrKind::Cmd
    } else if p.starts_with("~/") {
        PtrKind::Home
    } else {
        PtrKind::Repo
    }
}

/// For a repo pointer `<repo>@<sha>:<path>[:<line>]`: its path and whether it carries a line.
fn repo_path_line(p: &str) -> Option<(&str, bool)> {
    let (_, rest) = p.split_once('@')?;
    let (_, path) = rest.split_once(':')?;
    Some(match path.rsplit_once(':') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|c| c.is_ascii_digit()) => (head, true),
        _ => (path, false),
    })
}

/// True when the path holds a `..` component.
fn dotdot(path: &str) -> bool {
    path.split('/').any(|c| c == "..")
}

/// The pointer a status needs inside its `status_ref` (at least one pointer of the right kind).
fn status_ref_problem(status: &str, status_ref: &str) -> Option<&'static str> {
    if status_ref.len() > PROOF_CAP {
        return Some("status_ref is over its 2000-byte cap");
    }
    if status_ref.chars().any(|c| c.is_control()) {
        return Some("status_ref must be one line");
    }
    if proof_problem(status_ref).is_some() {
        return Some("status_ref has a pointer that is not one of <repo>@<7-40 hex>:<path>[:<line>], session:<id>, pr:<owner>/<repo>#<n>, cmd:<text>, ~/<path>");
    }
    let mut ptrs = status_ref.split("; ");
    match status {
        "fixed" => {
            if ptrs.any(|p| matches!(ptr_kind(p), PtrKind::Pr | PtrKind::Repo)) {
                None
            } else {
                Some("status_ref for status fixed needs a pr: or <repo>@<sha> pointer")
            }
        }
        "guarded" => {
            if ptrs.any(|p| ptr_kind(p) == PtrKind::Repo && matches!(repo_path_line(p), Some((path, true)) if path.ends_with("friction-controls.txt") && !dotdot(path))) {
                None
            } else {
                Some("status_ref for status guarded needs a <repo>@<sha>:<path>:<line> pointer whose path ends in friction-controls.txt")
            }
        }
        "ticket" => {
            if ptrs.any(|p| match ptr_kind(p) {
                PtrKind::Pr => true,
                PtrKind::Home => p.len() > "~/.bridlework/tickets/".len() && p.starts_with("~/.bridlework/tickets/") && !dotdot(p),
                _ => false,
            }) {
                None
            } else {
                Some("status_ref for status ticket needs a ~/.bridlework/tickets/<path> pointer or a pr: pointer")
            }
        }
        "lesson" => {
            if ptrs.any(|p| ptr_kind(p) == PtrKind::Repo && matches!(repo_path_line(p), Some((path, _)) if path.len() > ".sources/bridlework/lessons/".len() && path.starts_with(".sources/bridlework/lessons/") && !dotdot(path))) {
                None
            } else {
                Some("status_ref for status lesson needs a <repo>@<sha>:<path> pointer whose path is under .sources/bridlework/lessons/")
            }
        }
        _ => {
            if ptrs.any(|p| ptr_kind(p) == PtrKind::Cmd) {
                None
            } else {
                Some("status_ref for status no-action needs a cmd: pointer whose text is the reason")
            }
        }
    }
}

/// `YYYY-MM-DD` naming a real calendar date (month 1-12, the day inside that month, leap years included).
pub fn date_ok(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(|c| c.is_ascii_digit());
    if !(digits(0..4) && digits(5..7) && digits(8..10)) {
        return false;
    }
    let year: u32 = s[0..4].parse().unwrap();
    let month: u32 = s[5..7].parse().unwrap();
    let day: u32 = s[8..10].parse().unwrap();
    if !(1..=12).contains(&month) {
        return false;
    }
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let max = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    (1..=max).contains(&day)
}

/// The per-row rules (schema + field shapes). A reason names the field; it never echoes the text.
pub fn validate_row(row: &Row) -> Result<(), String> {
    if !token_ok(&row.id) {
        return Err("id must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')".to_string());
    }
    if !date_ok(&row.date) {
        return Err("date must be a real calendar date written YYYY-MM-DD".to_string());
    }
    if !token_ok(&row.ticket) {
        return Err("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')".to_string());
    }
    if !TAGS.contains(&row.tag.as_str()) {
        return Err("where-it-lives must be one of skill, gate, memory, wiki (K-1.1)".to_string());
    }
    if !row.breaks.is_empty() && !token_ok(&row.breaks) {
        return Err("breaks must be empty or a plain lesson id (lowercase letters, digits, '-', '_', '.')".to_string());
    }
    if row.text.trim().is_empty() {
        return Err("text is empty: a lesson row carries its lesson".to_string());
    }
    if !row.kind.is_empty() && !KINDS.contains(&row.kind.as_str()) {
        return Err("kind must be friction or pattern (empty means friction)".to_string());
    }
    if !row.repeats.is_empty() && !token_ok(&row.repeats) {
        return Err("repeats must be empty or a plain row id (lowercase letters, digits, '-', '_', '.')".to_string());
    }
    if !row.source.is_empty() && !SOURCES.contains(&row.source.as_str()) {
        return Err("source must be event (empty means a hand-written row)".to_string());
    }
    if !row.at.is_empty() {
        if !at_ok(&row.at) {
            return Err("at must be a real UTC instant written YYYY-MM-DDTHH:MM:SSZ".to_string());
        }
        if row.at[0..10] != row.date {
            return Err("at: its date part must equal the row date".to_string());
        }
    }
    if !row.impact.is_empty() && !IMPACTS.contains(&row.impact.as_str()) {
        return Err("impact must be low, medium, high or blocker (empty means unrated)".to_string());
    }
    if !row.why.is_empty() {
        if row.why.len() > WHY_CAP {
            return Err("why is over its 1000-byte cap".to_string());
        }
        if row.why.trim().is_empty() || row.why.chars().any(|c| c.is_control()) {
            return Err("why must be one non-blank line".to_string());
        }
    }
    if !row.proof.is_empty() {
        if let Some(m) = proof_problem(&row.proof) {
            return Err(m.to_string());
        }
    }
    Ok(())
}

/// The status rules: no status means no `status_ref`; a status is one of `STATUSES` and carries a `status_ref`
/// with the pointer that status needs. An empty status reads as open (so it takes no `status_ref`); an explicit
/// `open` is not in `STATUSES` and is refused here (a canonical row omits it). `parse_row_status` refuses an
/// explicit empty `status` key before this runs.
pub fn validate_status(s: &Status) -> Result<(), String> {
    if s.status.is_empty() {
        if !s.status_ref.is_empty() {
            return Err("status_ref is allowed only with a status other than open (an absent status reads as open)".to_string());
        }
        return Ok(());
    }
    if !STATUSES.contains(&s.status.as_str()) {
        return Err("status must be fixed, guarded, ticket, lesson or no-action (an open row omits status)".to_string());
    }
    if s.status_ref.is_empty() {
        return Err("status_ref is missing: a status other than open needs its pointer".to_string());
    }
    if let Some(m) = status_ref_problem(&s.status, &s.status_ref) {
        return Err(m.to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------- the cause tag (a rule of `add` only)

/// The three cause tags a hand-written friction row's `why` must hold at `add`: `Orchestrator`, `builder`,
/// `system`, byte for byte (letter case included).
const CAUSE_TAGS: [&str; 3] = ["Orchestrator", "builder", "system"];

/// A word byte: an ASCII letter, digit, underscore or hyphen (the bytes that may not follow a tag).
fn word_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-')
}

/// True when `why` holds a cause tag: the FIRST `cause:` of the why (the byte before it is not checked), then
/// one space, then one of `CAUSE_TAGS`, then the end of the why or one byte that is not a word byte. An empty
/// why holds no tag. A rule of `friction add` only: no other verb and no stored row is checked against it.
fn why_holds_cause_tag(why: &str) -> bool {
    let Some(rest) = why.split("cause:").nth(1) else { return false };
    CAUSE_TAGS.iter().any(|t| match rest.strip_prefix(' ').and_then(|r| r.strip_prefix(*t)) {
        Some(after) => after.is_empty() || !word_byte(after.as_bytes()[0]),
        None => false,
    })
}

// ---------------------------------------------------------------- strict JSON (the row grammar only)

/// A tiny strict reader for the one shape a row has: `{ "key": "value", ... }`, every value a string.
pub(crate) struct P<'a> {
    pub(crate) b: &'a [u8],
    pub(crate) i: usize,
}

impl<'a> P<'a> {
    pub(crate) fn ws(&mut self) {
        while self.i < self.b.len() && (self.b[self.i] == b' ' || self.b[self.i] == b'\t') {
            self.i += 1;
        }
    }

    /// One JSON string with the closed escape set this module writes (`\"` `\\` `\n` `\r` `\t` `\uXXXX`).
    /// A raw control byte inside a string is refused, so a row can never smuggle a second line.
    fn string(&mut self) -> Result<String, String> {
        if self.i >= self.b.len() || self.b[self.i] != b'"' {
            return Err("expected a string".to_string());
        }
        self.i += 1;
        let mut out = String::new();
        loop {
            if self.i >= self.b.len() {
                return Err("an unterminated string".to_string());
            }
            match self.b[self.i] {
                b'"' => {
                    self.i += 1;
                    return Ok(out);
                }
                b'\\' => {
                    self.i += 1;
                    if self.i >= self.b.len() {
                        return Err("an unterminated escape".to_string());
                    }
                    match self.b[self.i] {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            if self.i + 4 >= self.b.len() {
                                return Err("a short \\u escape".to_string());
                            }
                            // from_str_radix takes a leading sign; JSON takes hex digits only.
                            if !self.b[self.i + 1..self.i + 5].iter().all(u8::is_ascii_hexdigit) {
                                return Err("a bad \\u escape".to_string());
                            }
                            let hex = std::str::from_utf8(&self.b[self.i + 1..self.i + 5]).map_err(|_| "a bad \\u escape".to_string())?;
                            let v = u32::from_str_radix(hex, 16).map_err(|_| "a bad \\u escape".to_string())?;
                            out.push(char::from_u32(v).ok_or("a \\u escape that is not a scalar value".to_string())?);
                            self.i += 4;
                        }
                        _ => return Err("an escape outside the closed set".to_string()),
                    }
                    self.i += 1;
                }
                c if c < 0x20 => return Err("a control byte inside a string".to_string()),
                _ => {
                    // Copy the whole UTF-8 run at once; the line was decoded as UTF-8 before this, so it is valid.
                    let start = self.i;
                    while self.i < self.b.len() && self.b[self.i] != b'"' && self.b[self.i] != b'\\' && self.b[self.i] >= 0x20 {
                        self.i += 1;
                    }
                    out.push_str(std::str::from_utf8(&self.b[start..self.i]).map_err(|_| "invalid utf-8".to_string())?);
                    continue;
                }
            }
        }
    }

    /// One JSON object of string values. A duplicate key is refused (the key itself is never echoed: it is
    /// input that has passed no scan).
    pub(crate) fn object(&mut self) -> Result<Vec<(String, String)>, String> {
        self.ws();
        if self.i >= self.b.len() || self.b[self.i] != b'{' {
            return Err("expected a JSON object".to_string());
        }
        self.i += 1;
        let mut out = Vec::new();
        self.ws();
        if self.i < self.b.len() && self.b[self.i] == b'}' {
            self.i += 1;
            return Ok(out);
        }
        loop {
            self.ws();
            let k = self.string()?;
            if out.iter().any(|(n, _)| *n == k) {
                return Err("a duplicate key".to_string());
            }
            self.ws();
            if self.i >= self.b.len() || self.b[self.i] != b':' {
                return Err("expected ':' after a key".to_string());
            }
            self.i += 1;
            self.ws();
            let v = self.string()?;
            out.push((k, v));
            self.ws();
            if self.i >= self.b.len() {
                return Err("an unterminated object".to_string());
            }
            match self.b[self.i] {
                b',' => self.i += 1,
                b'}' => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => return Err("expected ',' or '}'".to_string()),
            }
        }
    }
}

/// JSON-escape one string value. The escape set is closed: what `P::string` reads is exactly what this writes.
pub(crate) fn jstr(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Strict decode + validate of one row line WITHOUT its newline. Never guesses, never repairs: the exact field set,
/// then every row rule. The reason names the field.
pub fn parse_row(line: &str) -> Result<Row, String> {
    parse_row_status(line).map(|(row, _)| row)
}

/// `parse_row` that also returns the row's two status keys (default when the row has neither).
pub fn parse_row_status(line: &str) -> Result<(Row, Status), String> {
    if line.len() + 1 > LINE_CAP {
        return Err(format!("the row is over the {LINE_CAP}-byte cap"));
    }
    let mut p = P { b: line.as_bytes(), i: 0 };
    let pairs = p.object()?;
    p.ws();
    if p.i != p.b.len() {
        return Err("trailing bytes after the object".to_string());
    }
    if pairs.is_empty() {
        return Err("an empty row".to_string());
    }
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    for (k, _) in &pairs {
        if !FIELDS.contains(&k.as_str()) && !OPTIONAL_FIELDS.contains(&k.as_str()) {
            return Err("an unknown field: a row has exactly the six contract fields plus the nine optional ones (kind, repeats, source, at, impact, why, proof, status, status_ref)".to_string());
        }
    }
    // An explicit empty status reads as a hand-written default: refused, a canonical row omits the key.
    if get("status").is_some_and(|v| v.is_empty()) {
        return Err("status must not be empty: an open row omits the status key".to_string());
    }
    let mut row = Row::default();
    for (k, slot) in [("id", &mut row.id), ("date", &mut row.date), ("ticket", &mut row.ticket), ("where-it-lives", &mut row.tag), ("breaks", &mut row.breaks), ("text", &mut row.text)] {
        *slot = get(k).ok_or_else(|| format!("missing field {k:?}"))?;
    }
    // `kind`, `repeats` and `source` default to their defaults when absent; an explicit empty value reads the same.
    row.kind = get("kind").unwrap_or_default();
    row.repeats = get("repeats").unwrap_or_default();
    row.source = get("source").unwrap_or_default();
    row.at = get("at").unwrap_or_default();
    row.impact = get("impact").unwrap_or_default();
    row.why = get("why").unwrap_or_default();
    row.proof = get("proof").unwrap_or_default();
    let st = Status { status: get("status").unwrap_or_default(), status_ref: get("status_ref").unwrap_or_default() };
    validate_row(&row)?;
    validate_status(&st)?;
    Ok((row, st))
}

/// Canonical line WITHOUT its newline: the six fields in `FIELDS` order, then `kind`, `repeats` and `source`
/// when they are not at their defaults (`kind` empty or `friction`, `repeats` empty, `source` empty). Refuses
/// an invalid row and a line over the cap.
pub fn encode_row(row: &Row) -> Result<String, String> {
    encode_row_status(row, &Status::default())
}

/// `encode_row` with the two status keys, written last and only when set.
pub fn encode_row_status(row: &Row, st: &Status) -> Result<String, String> {
    validate_row(row)?;
    validate_status(st)?;
    let mut line = String::from("{");
    for (k, v) in [("id", &row.id), ("date", &row.date), ("ticket", &row.ticket), ("where-it-lives", &row.tag), ("breaks", &row.breaks), ("text", &row.text)] {
        if !line.ends_with('{') {
            line.push(',');
        }
        line.push_str(&format!("{}:{}", jstr(k), jstr(v)));
    }
    if !row.kind.is_empty() && row.kind != "friction" {
        line.push_str(&format!(",{}:{}", jstr("kind"), jstr(&row.kind)));
    }
    if !row.repeats.is_empty() {
        line.push_str(&format!(",{}:{}", jstr("repeats"), jstr(&row.repeats)));
    }
    if !row.source.is_empty() {
        line.push_str(&format!(",{}:{}", jstr("source"), jstr(&row.source)));
    }
    for (k, v) in [("at", &row.at), ("impact", &row.impact), ("why", &row.why), ("proof", &row.proof), ("status", &st.status), ("status_ref", &st.status_ref)] {
        if !v.is_empty() {
            line.push_str(&format!(",{}:{}", jstr(k), jstr(v)));
        }
    }
    line.push('}');
    if line.len() + 1 > LINE_CAP {
        return Err(format!("the row is {} bytes, over the {}-byte cap", line.len() + 1, LINE_CAP));
    }
    Ok(line)
}

// ---------------------------------------------------------------- the inbox file

/// One read inbox: the valid rows with their 1-based line numbers, the per-row problems, and a torn tail line if any.
#[derive(Debug, Default)]
pub struct Inbox {
    pub rows: Rows,
    pub problems: Vec<(usize, String)>,
    pub torn_tail: Option<usize>,
    /// True when a `set-status` recovery copy sits next to the inbox (a write did not finish). The inbox was not
    /// read then: the rows are empty. `read_inbox`, `read_inbox_locked` and close-check's missing-inbox branch set
    /// this flag, and the verbs built on them refuse on it, like they do for a torn tail.
    pub recovery_copy: bool,
}

/// The fixed stderr line of a verb that meets a recovery copy (no path, no row).
pub(crate) const RECOVERY_MSG: &str = "the inbox has a status-write recovery copy next to it, or the place for one cannot be checked (a set-status write may not have finished); compare the two files by hand and move the copy aside";

/// True when the inbox path names no file and a recovery copy sits beside it. `add` and `sweep` open the inbox
/// with create, so they check this first and refuse without creating the file.
fn missing_inbox_with_recovery_copy(inbox: &Path) -> bool {
    matches!(fs::metadata(inbox), Err(e) if e.kind() == std::io::ErrorKind::NotFound) && recovery_copy_present(inbox)
}

/// The recovery copy path: the canonical inbox path plus `.status-write`, so a symlinked-file or symlinked-folder
/// spelling of the inbox path names the same copy, in the real file's folder. A missing inbox resolves through its folder.
fn recovery_path(inbox: &Path) -> Option<std::path::PathBuf> {
    recovery_path_checked(inbox).ok().flatten()
}

/// Links followed by hand when the inbox path is a link to a file that does not exist yet.
const LINK_HOPS: usize = 40;

/// The recovery copy path with three answers. `Ok(Some(p))`: the path. `Ok(None)`: the folder of the inbox does not
/// exist, so no copy can sit there. `Err(())`: the path cannot be built (a link loop, a folder that cannot be
/// read), so the place for a copy cannot be checked.
///
/// A found inbox resolves with `canonicalize`. A link to a file that does not exist yet (a dangling link) is
/// followed by hand to its end, so the copy is looked for beside the file the first append would create.
fn recovery_path_checked(inbox: &Path) -> Result<Option<std::path::PathBuf>, ()> {
    let mut path = inbox.to_path_buf();
    if path.is_relative() {
        path = std::env::current_dir().map_err(|_| ())?.join(path);
    }
    for _ in 0..LINK_HOPS {
        match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => {
                // A link that resolves to a found file is `canonicalize`d below; a dangling one is followed one hop.
                if let Ok(real) = fs::canonicalize(&path) {
                    return Ok(with_copy_name(&real));
                }
                let to = fs::read_link(&path).map_err(|_| ())?;
                path = if to.is_absolute() { to } else { path.parent().ok_or(())?.join(to) };
            }
            Ok(_) => return Ok(with_copy_name(&fs::canonicalize(&path).map_err(|_| ())?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let name = path.file_name().ok_or(())?.to_os_string();
                return match fs::canonicalize(path.parent().ok_or(())?) {
                    Ok(dir) => Ok(with_copy_name(&dir.join(name))),
                    Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => Ok(None),
                    Err(_) => Err(()),
                };
            }
            Err(_) => return Err(()),
        }
    }
    Err(())
}

fn with_copy_name(real: &Path) -> Option<std::path::PathBuf> {
    let mut name = real.file_name()?.to_os_string();
    name.push(".status-write");
    Some(real.with_file_name(name))
}

/// True when a recovery copy (a regular file) sits at the recovery path. A symlink or a folder there is not a
/// copy this module made; `set-status` meets it at its exclusive create and refuses.
///
/// A path that cannot be built (a link loop, a folder that cannot be read) counts as a copy: the place for a copy
/// cannot be checked, so the verb refuses with the same line and the damaged-inbox code. A missing folder does
/// not: no copy can sit there.
pub(crate) fn recovery_copy_present(inbox: &Path) -> bool {
    match recovery_path_checked(inbox) {
        Ok(Some(p)) => fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_file()),
        Ok(None) => false,
        Err(()) => true,
    }
}

/// The valid rows of an inbox with their 1-based line numbers. It reads as the plain `Vec<(usize, Row)>` it
/// derefs to; the two status keys of each row (same order) ride beside it, so the one privacy pass sees them.
#[derive(Debug, Default, Clone)]
pub struct Rows {
    // Private on purpose: a `Rows` is built only through `push_with_status`, so the two lists keep the same
    // length and the privacy pass cannot skip the status keys of a row.
    list: Vec<(usize, Row)>,
    statuses: Vec<Status>,
}

impl Rows {
    /// Append one row with its status keys.
    pub fn push_with_status(&mut self, item: (usize, Row), st: Status) {
        self.list.push(item);
        self.statuses.push(st);
    }
}

impl std::ops::Deref for Rows {
    type Target = Vec<(usize, Row)>;
    fn deref(&self) -> &Self::Target {
        &self.list
    }
}

impl IntoIterator for Rows {
    type Item = (usize, Row);
    type IntoIter = std::vec::IntoIter<(usize, Row)>;
    fn into_iter(self) -> Self::IntoIter {
        self.list.into_iter()
    }
}

impl<'a> IntoIterator for &'a Rows {
    type Item = &'a (usize, Row);
    type IntoIter = std::slice::Iter<'a, (usize, Row)>;
    fn into_iter(self) -> Self::IntoIter {
        self.list.iter()
    }
}

/// Read at most the cap plus one byte, then lint every line. `Err` is a setup failure (unreadable, oversized, not
/// UTF-8); a bad row is a `problems` entry, not an `Err`, so one checker reports every line.
///
/// The read holds the SHARED lock on the inbox file (waiting up to `LOCK_WAIT`, then `Err`: busy) and releases
/// it when the function returns, so a writer does not change the file during the read and the caller holds no
/// lock afterwards. The recovery copy check runs after the lock is held, in the same run that reads.
///
/// A missing inbox has no file to lock. When a recovery copy sits beside the missing path the read still
/// reports it, so the verb gives the recovery line and not a read error.
pub fn read_inbox(path: &Path) -> Result<Inbox, String> {
    let meta = match fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && recovery_copy_present(path) => {
            return Ok(Inbox { recovery_copy: true, ..Inbox::default() });
        }
        Err(_) => return Err("the inbox cannot be read".to_string()),
    };
    if meta.is_dir() {
        return Err("the inbox path is a folder".to_string());
    }
    let f = fs::File::open(path).map_err(|_| "the inbox cannot be read".to_string())?;
    lock_wait(&f, true)?;
    read_inbox_locked(&f, path).map(|(inbox, _)| inbox)
}

/// The same read over an inbox descriptor the caller has already locked (shared in `read_inbox`, exclusive in
/// `add`, `sweep` and `set-status`). This function takes no lock itself. The rows a write will follow are read from
/// the very file the lock is held on. `path` only names the recovery copy.
/// It also returns the text read (empty when a recovery copy stopped the read), so a writer can keep the
/// bytes of the lines it does not change.
fn read_inbox_locked(f: &fs::File, path: &Path) -> Result<(Inbox, String), String> {
    if recovery_copy_present(path) {
        return Ok((Inbox { recovery_copy: true, ..Inbox::default() }, String::new()));
    }
    let mut buf = Vec::new();
    f.take(INBOX_CAP + 1).read_to_end(&mut buf).map_err(|_| "the inbox cannot be read".to_string())?;
    if buf.len() as u64 > INBOX_CAP {
        return Err(format!("the inbox is over the {}-byte cap", INBOX_CAP));
    }
    let text = String::from_utf8(buf).map_err(|_| "the inbox is not utf-8".to_string())?;
    Ok((lint_text(&text), text))
}

/// The whole-file lint over already-read inbox text: per-row rules, then the cross-row rules (duplicate `id`,
/// a `breaks` naming no lesson, a `breaks` naming its own row). Line numbers are 1-based.
pub fn lint_text(text: &str) -> Inbox {
    let mut out = Inbox::default();
    if text.is_empty() {
        return out;
    }
    let mut line_no = 0;
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            line_no += 1;
            let line = &text[start..i];
            match parse_row_status(line) {
                Ok((row, st)) => out.rows.push_with_status((line_no, row), st),
                Err(e) => out.problems.push((line_no, e)),
            }
            start = i + 1;
        }
    }
    if start < text.len() {
        // A tail with no newline is torn (a crashed append): reported, never read as a row.
        out.torn_tail = Some(line_no + 1);
    }
    out.problems.extend(cross_check(&out.rows));
    out
}

/// The fields of a row as (name, value) pairs in canonical order (the six required, then the two optional):
/// the one list every privacy scan walks.
fn row_fields(row: &Row) -> [(&'static str, &str); 13] {
    [
        ("id", row.id.as_str()),
        ("date", row.date.as_str()),
        ("ticket", row.ticket.as_str()),
        ("where-it-lives", row.tag.as_str()),
        ("breaks", row.breaks.as_str()),
        ("text", row.text.as_str()),
        ("kind", row.kind.as_str()),
        ("repeats", row.repeats.as_str()),
        ("source", row.source.as_str()),
        ("at", row.at.as_str()),
        ("impact", row.impact.as_str()),
        ("why", row.why.as_str()),
        ("proof", row.proof.as_str()),
    ]
}

/// The status keys of a row as (name, value) pairs: scanned with the row fields.
fn status_fields(st: &Status) -> [(&'static str, &str); 2] {
    [("status", st.status.as_str()), ("status_ref", st.status_ref.as_str())]
}

/// The privacy engine over the fields of the stored rows — the same rules `add` applies to a new row. The
/// verbs that run this pass are listed in docs/friction-inbox.md section 4; a reason names the line, the field
/// and the pattern class, never the bytes.
/// The two status keys of each row are scanned with its other fields (they travel in `Rows`).
pub fn privacy_problems(ctx: &crate::scan::Ctx, rows: &Rows) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (i, (n, row)) in rows.iter().enumerate() {
        scan_fields(ctx, *n, &row_fields(row), &mut out);
        if let Some(st) = rows.statuses.get(i) {
            scan_fields(ctx, *n, &status_fields(st), &mut out);
        }
    }
    out
}

fn scan_fields(ctx: &crate::scan::Ctx, n: usize, fields: &[(&'static str, &str)], out: &mut Vec<(usize, String)>) {
    for (name, value) in fields {
        let hits = crate::scan::scan_text(ctx, "friction", value);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            out.push((n, format!("field {name} has a privacy hit ({})", classes.join(", "))));
        }
    }
}

/// The cross-row pass a whole-file lint runs after every line parsed: duplicate ids across the file, `breaks`
/// references (forward and back, and a row naming itself), and the `repeats` pointer rules (ticket 192: a
/// pointer must name an existing row, never itself; the row carrying it must be `kind: pattern`; and the row it
/// names must be a `kind: pattern` chain ROOT — a pattern row with an empty `repeats` — so the repeat count of
/// a chain is one unambiguous number). `add` runs the same rules against the inbox plus its one
/// new row, so the verb and the lint can never disagree about what a good inbox is. The reasons name the field
/// only: a row value that has not passed the privacy scan is never echoed.
pub fn cross_check(rows: &[(usize, Row)]) -> Vec<(usize, String)> {
    // Linear by construction: the id sets and the id -> row map are built in single passes, so a full-cap
    // inbox (40001 rows) costs one pass, not a pass per row — the check runs INSIDE the writer lock, so a
    // quadratic scan would hold the lock long enough to turn concurrent losers into busy timeouts instead
    // of duplicate refusals.
    let mut problems = Vec::new();
    let mut by_id: std::collections::HashMap<&str, &Row> = std::collections::HashMap::with_capacity(rows.len());
    for (_, r) in rows {
        by_id.insert(r.id.as_str(), r);
    }
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::with_capacity(rows.len());
    for (line, row) in rows {
        if !seen.insert(row.id.as_str()) {
            problems.push((*line, "id: a duplicate of an earlier row".to_string()));
        }
        if !row.breaks.is_empty() {
            if row.breaks == row.id {
                problems.push((*line, "breaks names its own row: a lesson cannot break itself".to_string()));
            } else if !by_id.contains_key(row.breaks.as_str()) {
                problems.push((*line, "breaks names no lesson in the inbox".to_string()));
            }
        }
        if !row.repeats.is_empty() {
            if row.repeats == row.id {
                problems.push((*line, "repeats names its own row: a pattern cannot repeat itself".to_string()));
            } else if !by_id.contains_key(row.repeats.as_str()) {
                problems.push((*line, "repeats names no row in the inbox".to_string()));
            } else if row.kind != "pattern" {
                problems.push((*line, "repeats is allowed only on a kind: pattern row; a repeat of a friction row is filed with --breaks <row-id>".to_string()));
            } else if by_id[row.repeats.as_str()].kind != "pattern" {
                problems.push((*line, "repeats must name a kind: pattern row (its chain root); a repeat of a friction row is filed with --breaks <row-id>".to_string()));
            } else if !by_id[row.repeats.as_str()].repeats.is_empty() {
                problems.push((*line, "repeats must name the chain root: the row it names itself carries repeats".to_string()));
            }
        }
    }
    problems
}

// ---------------------------------------------------------------- the writer

/// Append one row as a single whole line plus newline, after the caller's checks have passed. The file is created when absent
/// (its folder is not). One write, then sync: success means the line is on disk.
pub fn append_row(path: &Path, row: &Row) -> Result<(), String> {
    let mut f = fs::OpenOptions::new().append(true).create(true).open(path).map_err(|_| "the inbox cannot be written".to_string())?;
    append_row_to(&mut f, row)
}

/// The same append through an open descriptor (the one `add` holds the writer lock on).
fn append_row_to(f: &mut fs::File, row: &Row) -> Result<(), String> {
    let mut line = encode_row(row)?;
    line.push('\n');
    f.write_all(line.as_bytes()).map_err(|_| "the inbox write failed".to_string())?;
    f.sync_all().map_err(|_| "the inbox sync failed".to_string())?;
    Ok(())
}

/// How long a writer waits for the lock before giving up (exit 2): far above any real critical section
/// (read, scan, check, append, sync of a capped inbox).
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Open the inbox FILE (creating it when absent, in its existing folder) and take the exclusive writer lock ON IT:
/// one kernel advisory lock (flock(2), std's file lock, Rust 1.89+) on the inbox's own inode. The spellings of
/// the path named here (a symlinked folder or file, a `.`/`..` detour, a hard link, a not-yet-created file
/// behind a symlink) open the same inode, so they take the one lock; there is no side lock file and no path is
/// built or compared. The kernel releases the lock when the descriptor closes (return, panic, kill -9), so
/// there is no stale-steal rule. The returned file is the locked descriptor: `add` and `sweep` read and append
/// through it (a second open of the inbox in the same process would wait for this very lock), and dropping it
/// releases the lock. `set-status` does not call this function: it opens the inbox without create, takes the
/// same lock on its own descriptor and reads through `read_inbox_locked`. `Err` is a setup failure (cannot open) or the lock stayed busy past LOCK_WAIT. Nothing
/// here prints any path.
pub(crate) fn open_locked(inbox: &Path) -> Result<fs::File, String> {
    let f = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(inbox)
        .map_err(|_| "the inbox cannot be written".to_string())?;
    lock_wait(&f, false)?;
    Ok(f)
}

/// Take the lock on an open inbox descriptor, shared or exclusive, waiting up to `LOCK_WAIT`.
fn lock_wait(f: &fs::File, shared: bool) -> Result<(), String> {
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        let got = if shared { f.try_lock_shared() } else { f.try_lock() };
        match got {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(_)) => {
                return Err(if shared { "the inbox cannot be read" } else { "the inbox cannot be written" }.to_string());
            }
        }
        if std::time::Instant::now() >= deadline {
            // A reader waits for a writer, and a writer waits for a writer or a reader: the reader text does not
            // name a writer.
            return Err(if shared { "the inbox is busy (it is locked); try again" } else { "the inbox is busy (another writer holds it); try again" }.to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub(crate) fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
    let f = fs::File::open(path)?;
    let mut buf = Vec::new();
    f.take(cap + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

// ---------------------------------------------------------------- the close-note check (K-1.5)

/// The lines of a close note whose trimmed form begins `lesson:` (the close-line grammar, `docs/friction-inbox.md`).
pub fn lesson_lines(note: &str) -> Vec<&str> {
    note.lines().filter(|l| l.trim_start().starts_with("lesson:")).collect()
}

/// K-1.5: the note must carry exactly one close line, and it must match the inbox rows for `ticket` exactly:
/// `lesson: none` passes only when the inbox holds no row for the ticket; `lesson: <id>[, <id> ...]` passes only
/// when the named ids are exactly the inbox rows whose ticket is this ticket. Ok carries the number of lessons named.
pub fn close_check(note: &str, ticket: &str, rows: &[(usize, Row)]) -> Result<usize, String> {
    let lines = lesson_lines(note);
    if lines.is_empty() {
        return Err("the close note has no `lesson:` line (K-1.5: name each lesson it captured, or `lesson: none`)".to_string());
    }
    if lines.len() > 1 {
        return Err(format!("the close note has {} `lesson:` lines; exactly one is allowed", lines.len()));
    }
    let value = lines[0].trim_start().strip_prefix("lesson:").unwrap_or("").trim();
    let captured: Vec<&Row> = rows.iter().filter(|(_, r)| r.ticket == ticket).map(|(_, r)| r).collect();
    if value == "none" {
        if captured.is_empty() {
            return Ok(0);
        }
        return Err(format!("lesson: none but the inbox holds {} row(s) with ticket {:?}; name them", captured.len(), ticket));
    }
    let mut named: Vec<&str> = Vec::new();
    for part in value.split(',') {
        let id = part.trim();
        if id.is_empty() {
            return Err("an empty lesson id in the close line".to_string());
        }
        if !token_ok(id) {
            return Err("a lesson id in the close line is not a plain token".to_string());
        }
        if named.contains(&id) {
            return Err("a lesson id in the close line is named twice".to_string());
        }
        named.push(id);
    }
    if named.is_empty() {
        return Err("the close line names no lesson id; write `lesson: none` or `lesson: <id>[, <id> ...]`".to_string());
    }
    for id in &named {
        match rows.iter().find(|(_, r)| &r.id == id) {
            None => return Err(format!("the lesson id {:?} is not in the inbox", id)),
            Some((_, r)) if r.ticket != ticket => {
                return Err(format!("the lesson id {:?} belongs to ticket {:?}, not this one", id, r.ticket));
            }
            Some(_) => {}
        }
    }
    for r in &captured {
        if !named.contains(&r.id.as_str()) {
            return Err(format!("the inbox row {:?} has ticket {:?} but is not named in the close line", r.id, ticket));
        }
    }
    Ok(named.len())
}

/// Read a close note under its cap. `Err` is a setup failure; the check itself is `close_check`.
pub fn read_note(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|_| "the close note cannot be read".to_string())?;
    if meta.is_dir() {
        return Err("the close note path is a folder".to_string());
    }
    let bytes = read_capped(path, NOTE_CAP).map_err(|_| "the close note cannot be read".to_string())?;
    if bytes.len() as u64 > NOTE_CAP {
        return Err(format!("the close note is over the {}-byte cap", NOTE_CAP));
    }
    String::from_utf8(bytes).map_err(|_| "the close note is not utf-8".to_string())
}

// ---------------------------------------------------------------- the verb family

pub const FRICTION_USAGE: &str = "usage: harness friction add --id <id> --date <YYYY-MM-DD> --ticket <t> --where-it-lives <skill|gate|memory|wiki> [--breaks <row-id>] [--kind <friction|pattern>] [--repeats <row-id>] [--source event] [--at <YYYY-MM-DDTHH:MM:SSZ>] [--impact <low|medium|high|blocker>] --why <cause> [--proof <pointers>] --text <lesson> [--config <file>] | friction lint [--config <file>] | friction open [--config <file>] | friction patterns [--config <file>] [--record <file>] | friction close-check <close-note> --ticket <t> [--config <file>] | friction sweep --log <event-log> --week <iso-year>-w<ww> [--config <file>] | friction rank [--top <k>] [--min-occurrences <n>] [--record <file>] [--config <file>] | friction suggest --text <t> [--why <w>] [--top <k>] [--before <row id>] [--record <file>] [--config <file>] [--jev-send <program> --jev-key-file <file> --jev-ledger-dir <dir> [--jev-deadline-ms <n>]] (suggest is unix only) | friction triage --week <iso-year>-w<ww> --state <file> --handoff-dir <dir> [--log <event-log>] [--closed <file>] [--controls <file> --lessons-dir <dir>] [--as-of <YYYY-MM-DDTHH:MM:SSZ>] [--top <k>] [--config <file>] | friction bind --state <file> --group <id> --task-id <id> --task-session <token> [--config <file>] | friction page --week <iso-year>-w<ww> --state <file> --out <file> [--config <file>] | friction controls-check --controls <file> --lessons-dir <dir> [--config <file>] | friction set-status --id <row id> --set <fixed|guarded|ticket|lesson|no-action|open> [--ref <pointers>] [--config <file>]";

/// The config source, resolved exactly as `harness config check` and `session check-name` resolve it: `--config`,
/// else `HARNESS_CONFIG`, no third source, an empty value refused and never defaulted.
pub(crate) fn config_path(file: Option<&str>) -> Result<std::path::PathBuf, String> {
    let path = match file {
        Some(f) => std::ffi::OsString::from(f),
        None => match std::env::var_os("HARNESS_CONFIG") {
            Some(v) => v,
            None => return Err("no config configured (pass --config <file> or set HARNESS_CONFIG)".to_string()),
        },
    };
    if path.is_empty() {
        return Err("the config source is empty".to_string());
    }
    Ok(std::path::PathBuf::from(path))
}

/// `harness friction add | lint | patterns | close-check | sweep | open | set-status` (K-1 slice 1 plus tickets
/// 192, 200; the contract is `docs/friction-inbox.md`). `set-status` is the one verb that rewrites a line in place.
/// Exit 0 ok; 1 a refusal (a refused write leaves the inbox byte-identical, stdout empty, stderr naming the field
/// and the reason — never the lesson text, never the inbox path); 2 a usage or setup error.
///
/// The order below is that of `add` (and `sweep` in the same shape): flags, then row rules, then the cause tag of a
/// hand-written friction row's `why` (below), then the privacy engine over the new row, then — under the exclusive
/// writer lock — the same privacy engine over every stored row and the inbox cross rules, and only then one append.
/// `set-status` has its own order: the flag checks, the config and privacy list, the scan of the new `--ref` and the
/// status rules run before the lock; under the lock come the hard-link check, the read, the stored-row checks, the
/// id lookup and the caps, then the recovery copy, then the rewrite in place (see `set_status_cmd`).
pub fn friction_cmd(a: &[String], testmode: bool) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{}", FRICTION_USAGE);
        return 2;
    };
    match sub.as_str() {
        "add" => add_cmd(rest, testmode),
        "lint" => lint_cmd(rest, testmode),
        "open" => open_cmd(rest, testmode),
        "set-status" => set_status_cmd(rest, testmode),
        "patterns" => patterns_cmd(rest, testmode),
        "close-check" => close_check_cmd(rest, testmode),
        "sweep" => sweep_cmd(rest, testmode),
        "rank" => crate::friction_loop::rank_cmd(rest, testmode),
        #[cfg(unix)]
        "suggest" => crate::friction_suggest::suggest_cmd(rest, testmode),
        "triage" => crate::friction_loop::triage_cmd(rest, testmode),
        "bind" => crate::friction_loop::bind_cmd(rest, testmode),
        "page" => crate::friction_loop::page_cmd(rest, testmode),
        "controls-check" => crate::friction_loop::controls_check_cmd(rest, testmode),
        _ => {
            eprintln!("{}", FRICTION_USAGE);
            2
        }
    }
}

/// The inbox rows, refusing a damaged inbox (fail closed): a torn tail, any bad row or any privacy hit in a
/// stored row blocks the append. Reads the locked descriptor, so the rows are the ones the append will follow.
fn rows_for_add(f: &fs::File, path: &Path, ctx: &crate::scan::Ctx) -> Result<Vec<(usize, Row)>, (i32, String)> {
    let (inbox, _) = read_inbox_locked(f, path).map_err(|m| (2, m))?;
    if inbox.recovery_copy {
        return Err((1, RECOVERY_MSG.to_string()));
    }
    if let Some(n) = inbox.torn_tail {
        return Err((1, format!("the inbox has a torn tail at line {n}; nothing was appended (run harness friction lint)")));
    }
    if let Some((n, m)) = inbox.problems.first() {
        return Err((1, format!("the inbox already has a problem at line {n}: {m}; nothing was appended (run harness friction lint)")));
    }
    if let Some((n, m)) = privacy_problems(ctx, &inbox.rows).first() {
        return Err((1, format!("the inbox already has a problem at line {n}: {m}; nothing was appended (run harness friction lint)")));
    }
    Ok(inbox.rows.list)
}

fn add_cmd(rest: &[String], testmode: bool) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let refuse = |m: &str| {
        eprintln!("harness: friction refused: {}", m);
        eprintln!("harness: ROW NOT RECORDED");
        1
    };
    let single = ["config", "id", "date", "ticket", "where-it-lives", "breaks", "text", "kind", "repeats", "source", "at", "impact", "why", "proof"];
    let mut o: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let Some(name) = arg.strip_prefix("--") else {
            return fail("an unexpected argument (only the flags of the usage line are allowed)");
        };
        if !single.contains(&name) {
            return fail("an unknown option (the usage line lists the flags)");
        }
        let Some(v) = it.next() else {
            return fail("a flag is missing its value");
        };
        if o.insert(name.to_string(), v.clone()).is_some() {
            return fail("a flag was given twice");
        }
    }
    // A missing required field is a row refusal naming the field (K-1.1: "no tag" is refused, not a usage error).
    for k in ["id", "date", "ticket", "where-it-lives", "text"] {
        if !o.contains_key(k) {
            eprintln!("harness: friction refused: {k} is missing: a row with a missing field is refused");
            eprintln!("harness: ROW NOT RECORDED");
            return 1;
        }
    }
    let cfg = match config_path(o.get("config").map(|s| s.as_str())) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let cfg = match crate::config::Config::load(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let inbox_path = std::path::PathBuf::from(cfg.friction_inbox());
    let row = Row {
        id: o["id"].clone(),
        date: o["date"].clone(),
        ticket: o["ticket"].clone(),
        tag: o["where-it-lives"].clone(),
        breaks: o.get("breaks").cloned().unwrap_or_default(),
        text: o["text"].clone(),
        kind: o.get("kind").cloned().unwrap_or_default(),
        repeats: o.get("repeats").cloned().unwrap_or_default(),
        source: o.get("source").cloned().unwrap_or_default(),
        at: o.get("at").cloned().unwrap_or_default(),
        impact: o.get("impact").cloned().unwrap_or_default(),
        why: o.get("why").cloned().unwrap_or_default(),
        proof: o.get("proof").cloned().unwrap_or_default(),
    };
    if let Err(e) = validate_row(&row) {
        return refuse(&e);
    }
    // The cause tag: a hand-written friction row (kind empty or `friction`, source empty) carries its cause in
    // `why`. Pattern rows and event rows are exempt. The check sits after the row rules and before the privacy
    // list is loaded, and reads only the why of the new row (a tag in the text or the proof does not count).
    if (row.kind.is_empty() || row.kind == "friction") && row.source.is_empty() && !why_holds_cause_tag(&row.why) {
        return refuse("why must hold a cause tag: cause: Orchestrator, cause: builder or cause: system");
    }
    // The privacy list loads before anything else is read: with no list configured the verb fails closed (2),
    // and the new row is scanned before any cross rule runs, so no unscanned byte of it is ever echoed.
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => {
            return fail("no privacy list configured: an unscanned row is never stored");
        }
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    for (name, value) in row_fields(&row) {
        let hits = crate::scan::scan_text(&ctx, "friction.add", value);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            return refuse(&format!("field {name} has a privacy hit ({}); the row is not stored", classes.join(", ")));
        }
    }
    // A row that cannot be encoded, or that can only be refused against an empty inbox (a `breaks` with no
    // earlier row to name), is refused BEFORE the inbox is opened, so a refused first row never creates the file.
    if let Err(e) = encode_row(&row) {
        return refuse(&e);
    }
    if !inbox_path.exists() {
        if let Some((_, m)) = cross_check(&[(1, row.clone())]).into_iter().next() {
            return refuse(&m);
        }
    }
    // A recovery copy beside a missing inbox: the copy is reported and the inbox is not created.
    if missing_inbox_with_recovery_copy(&inbox_path) {
        eprintln!("harness: friction refused: {RECOVERY_MSG}");
        return 1;
    }
    // One writer at a time: the exclusive lock is on the inbox file itself, held across read, check and append,
    // and released when the function returns, so the duplicate-id check does not race a concurrent add of the same id.
    let mut file = match open_locked(&inbox_path) {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let mut rows = match rows_for_add(&file, &inbox_path, &ctx) {
        Ok(r) => r,
        Err((code, m)) => {
            if m == RECOVERY_MSG {
                eprintln!("harness: friction refused: {m}");
                return code;
            }
            eprintln!("harness: friction refused: {}", m);
            eprintln!("harness: ROW NOT RECORDED");
            return code;
        }
    };
    let new_line = rows.len() + 1;
    rows.push((new_line, row.clone()));
    if let Some((_, m)) = cross_check(&rows).into_iter().find(|(n, _)| *n == new_line) {
        return refuse(&m);
    }
    match append_row_to(&mut file, &row) {
        Ok(()) => {
            println!("stored id={}", row.id);
            0
        }
        Err(e) => fail(&e),
    }
}

fn lint_cmd(rest: &[String], testmode: bool) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let mut file: Option<&str> = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => {
                let Some(v) = it.next() else {
                    return fail("--config is missing its value");
                };
                if file.replace(v.as_str()).is_some() {
                    return fail("--config was given twice");
                }
            }
            _ => {
                // A constant message: the argument bytes (an unknown flag, a stray positional — often the
                // inbox path) have passed no privacy scan, so they are never echoed.
                return fail("an unknown option (lint takes only --config <file>)");
            }
        }
    }
    let cfg = match config_path(file) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let cfg = match crate::config::Config::load(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    // The same privacy engine as `add` over every field of every stored row; with no list configured lint
    // fails closed (2) — an inbox is never linted clean unscanned.
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned inbox is never linted clean"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let inbox = match read_inbox(std::path::Path::new(cfg.friction_inbox())) {
        Ok(i) => i,
        Err(e) => return fail(&e),
    };
    if inbox.recovery_copy {
        eprintln!("harness: {RECOVERY_MSG}");
        return 1;
    }
    if let Some(n) = inbox.torn_tail {
        eprintln!("harness: inbox has a torn tail at line {n} (no trailing newline); not read as a row");
        return 1;
    }
    let mut problems = inbox.problems.clone();
    problems.extend(privacy_problems(&ctx, &inbox.rows));
    if !problems.is_empty() {
        for (n, m) in &problems {
            eprintln!("harness: inbox line {n}: {m}");
        }
        return 1;
    }
    println!("inbox ok rows={}", inbox.rows.len());
    0
}

/// `harness friction open [--config <file>]`: the rows whose status is open, one line each
/// (`open id=<id> date=<date> impact=<impact or -> ticket=<ticket>`; no text, why, proof or status_ref), then
/// `rows open=<n> fixed=<n> guarded=<n> ticket=<n> lesson=<n> no-action=<n> total=<n>`. Fail closed like `lint`:
/// a torn tail, any bad row or a privacy hit exits 1 with nothing on stdout.
fn open_cmd(rest: &[String], testmode: bool) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let mut file: Option<&str> = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => {
                let Some(v) = it.next() else {
                    return fail("--config is missing its value");
                };
                if file.replace(v.as_str()).is_some() {
                    return fail("--config was given twice");
                }
            }
            _ => return fail("an unknown option (open takes only --config <file>)"),
        }
    }
    let cfg = match config_path(file) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let cfg = match crate::config::Config::load(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned inbox is never listed"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let inbox = match read_inbox(std::path::Path::new(cfg.friction_inbox())) {
        Ok(i) => i,
        Err(e) => return fail(&e),
    };
    if inbox.recovery_copy {
        eprintln!("harness: {RECOVERY_MSG}");
        return 1;
    }
    if let Some(n) = inbox.torn_tail {
        eprintln!("harness: inbox has a torn tail at line {n} (no trailing newline); not read as a row");
        return 1;
    }
    let mut problems = inbox.problems.clone();
    problems.extend(privacy_problems(&ctx, &inbox.rows));
    if !problems.is_empty() {
        for (n, m) in &problems {
            eprintln!("harness: inbox line {n}: {m}");
        }
        return 1;
    }
    let mut counts = [0usize; 6];
    let mut text = String::new();
    for ((_, r), st) in inbox.rows.iter().zip(inbox.rows.statuses.iter()) {
        let word = st.word();
        if word == "open" {
            counts[0] += 1;
            let impact = if r.impact.is_empty() { "-" } else { r.impact.as_str() };
            text.push_str(&format!("open id={} date={} impact={} ticket={}\n", r.id, r.date, impact, r.ticket));
        } else if let Some(i) = STATUSES.iter().position(|s| *s == word) {
            counts[i + 1] += 1;
        }
    }
    print!("{text}");
    println!(
        "rows open={} fixed={} guarded={} ticket={} lesson={} no-action={} total={}",
        counts[0],
        counts[1],
        counts[2],
        counts[3],
        counts[4],
        counts[5],
        inbox.rows.len()
    );
    0
}

/// Sync a folder so a file made or removed in it is on disk.
fn sync_folder(dir: &Path) -> std::io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

/// Create the recovery copy: exclusive (an existing file, folder or planted symlink at the path refuses), with
/// the inbox's own permission bits. A setup failure leaves no copy: when the bits cannot be set, the partly made
/// copy is removed, and when that removal fails too the message names the path so the copy can be moved aside.
fn create_recovery(path: &Path, perms: &fs::Permissions) -> Result<fs::File, String> {
    const CANNOT: &str = "the recovery copy cannot be created; the status was not set";
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        opts.mode(perms.mode() & 0o7777);
    }
    let f = opts.open(path).map_err(|_| CANNOT.to_string())?;
    if f.set_permissions(perms.clone()).is_err() {
        drop(f);
        return Err(match fs::remove_file(path) {
            Ok(()) => CANNOT.to_string(),
            Err(_) => format!("{CANNOT}; a partly made recovery copy could not be removed: {}", path.display()),
        });
    }
    Ok(f)
}

/// `harness friction set-status --id <row id> --set <fixed|guarded|ticket|lesson|no-action|open> [--ref <pointers>]
/// [--config <file>]`: change the `status` and `status_ref` keys of the ONE row with that id, in place, and
/// nothing else. The flag checks, the scan of the new `--ref` and the status rules run before the lock is taken.
/// The hard-link check, the read, the stored-row checks, the id lookup and the cap checks run inside one
/// exclusive critical section on the inbox file (read through `read_inbox_locked` on the locked descriptor), and
/// they have passed before the recovery copy is made. Lines the verb does not change are written back from
/// their original bytes. The write keeps the inode: the whole old inbox goes to `<inbox>.status-write`
/// (exclusive create, synced), then the locked file is truncated and the new content written and synced, then
/// the copy is removed. A failure after the copy exists leaves it (exit 2). The readers that go through
/// `read_inbox` or `read_inbox_locked` refuse while it is there, until it is moved aside by hand. Exit 0 ok (a set equal to the current
/// values writes nothing); 1 a refusal (the inbox is byte-identical, stdout empty, one stderr line that names
/// at most a line number, a field and a privacy class); 2 a usage or setup error.
fn set_status_cmd(rest: &[String], testmode: bool) -> i32 {
    let usage = || {
        eprintln!("{}", FRICTION_USAGE);
        2
    };
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let refuse = |m: &str| {
        eprintln!("harness: friction refused: {m}; the status was not set");
        1
    };
    let mut o: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let name = match arg.as_str() {
            "--id" | "--set" | "--ref" | "--config" => arg.as_str(),
            _ => return usage(),
        };
        let Some(v) = it.next() else { return usage() };
        if o.insert(name, v.as_str()).is_some() {
            return usage();
        }
    }
    let (Some(&id), Some(&set)) = (o.get("--id"), o.get("--set")) else { return usage() };
    if !token_ok(id) {
        return refuse("--id is not a plain token");
    }
    if set != "open" && !STATUSES.contains(&set) {
        return refuse("--set must be fixed, guarded, ticket, lesson, no-action or open");
    }
    let ref_val = o.get("--ref").copied();
    let new = if set == "open" {
        if ref_val.is_some() {
            return refuse("--ref is not allowed with --set open");
        }
        Status::default()
    } else {
        Status { status: set.to_string(), status_ref: ref_val.unwrap_or("").to_string() }
    };
    let cfg = match config_path(o.get("--config").copied()) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let cfg = match crate::config::Config::load(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned status is not stored"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    // The new pointers are scanned before they are used anywhere (the status rules included).
    if let Some(r) = ref_val {
        let hits = crate::scan::scan_text(&ctx, "friction.set-status", r);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            return refuse(&format!("field status_ref has a privacy hit ({})", classes.join(", ")));
        }
    }
    if let Err(m) = validate_status(&new) {
        return refuse(&m);
    }
    let inbox_path = std::path::PathBuf::from(cfg.friction_inbox());
    // Opened without create: a missing inbox holds no such row, and the check has no time gap to the open.
    let mut f = match fs::OpenOptions::new().read(true).write(true).open(&inbox_path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A recovery copy beside the missing inbox is reported first (the same line and code as with the inbox).
            if recovery_copy_present(&inbox_path) {
                eprintln!("harness: {RECOVERY_MSG}");
                return 1;
            }
            return refuse("no row with that id");
        }
        Err(_) => return fail("the inbox cannot be written"),
    };
    if let Err(m) = lock_wait(&f, false) {
        return fail(&m);
    }
    // A hard link would keep the old bytes under the other name when the file is truncated and rewritten.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match f.metadata() {
            Ok(m) if m.nlink() > 1 => return refuse("the inbox has more than one hard link"),
            Ok(_) => {}
            Err(_) => return fail("the inbox cannot be read"),
        }
    }
    let perms = match f.metadata() {
        Ok(m) => m.permissions(),
        Err(_) => return fail("the inbox cannot be read"),
    };
    let (inbox, text) = match read_inbox_locked(&f, &inbox_path) {
        Ok(x) => x,
        Err(m) => return fail(&m),
    };
    if inbox.recovery_copy {
        eprintln!("harness: {RECOVERY_MSG}");
        return 1;
    }
    if let Some(n) = inbox.torn_tail {
        return refuse(&format!("the inbox has a torn tail at line {n}"));
    }
    if let Some((n, m)) = inbox.problems.first() {
        return refuse(&format!("the inbox has a problem at line {n}: {m}"));
    }
    if let Some((n, m)) = privacy_problems(&ctx, &inbox.rows).first() {
        return refuse(&format!("the inbox has a problem at line {n}: {m}"));
    }
    let Some(idx) = inbox.rows.iter().position(|(_, r)| r.id == id) else {
        return refuse("no row with that id");
    };
    let (line_no, row) = &inbox.rows[idx];
    if inbox.rows.statuses[idx] == new {
        println!("status set id={} status={}", row.id, new.word());
        return 0;
    }
    let new_line = match encode_row_status(row, &new) {
        Ok(l) => l,
        Err(m) => return refuse(&m),
    };
    // The lines other than the target are copied from their original bytes; the target is the canonical new line.
    let mut out = String::with_capacity(text.len() + new_line.len() + 1);
    for (i, l) in text.split_inclusive('\n').enumerate() {
        if i + 1 == *line_no {
            out.push_str(&new_line);
            out.push('\n');
        } else {
            out.push_str(l);
        }
    }
    if out.len() as u64 > INBOX_CAP {
        return refuse(&format!("the inbox would be over the {INBOX_CAP}-byte cap"));
    }
    let Some(rp) = recovery_path(&inbox_path) else {
        return fail("the recovery copy path cannot be built; the status was not set");
    };
    let Some(dir) = rp.parent().map(|d| d.to_path_buf()) else {
        return fail("the recovery copy path cannot be built; the status was not set");
    };
    // The checks above have passed. From the exclusive create on, a failure leaves the copy in place (fail closed).
    let mut copy = match create_recovery(&rp, &perms) {
        Ok(c) => c,
        Err(m) => return fail(&m),
    };
    const LEFT: &str = "the inbox write did not finish; a recovery copy was left next to the inbox";
    if copy.write_all(text.as_bytes()).and_then(|()| copy.sync_all()).and_then(|()| sync_folder(&dir)).is_err() {
        return fail(LEFT);
    }
    let rewrite = f
        .set_len(0)
        .and_then(|()| std::io::Seek::seek(&mut f, std::io::SeekFrom::Start(0)).map(|_| ()))
        .and_then(|()| f.write_all(out.as_bytes()))
        .and_then(|()| f.sync_all());
    if rewrite.is_err() {
        return fail(LEFT);
    }
    drop(copy);
    // The inbox is whole here. When the removal fails the copy is still there (LEFT is true); when only the
    // folder sync fails the copy is gone, and the message says so.
    if fs::remove_file(&rp).is_err() {
        return fail(LEFT);
    }
    if sync_folder(&dir).is_err() {
        return fail("the status was written and the recovery copy was removed, but the folder sync failed");
    }
    println!("status set id={} status={}", row.id, new.word());
    0
}

/// Today's UTC date as `YYYY-MM-DD` (std only: days since the epoch to the civil calendar). The trial
/// record's date, nothing more.
pub(crate) fn today_utc() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let z = (secs / 86400) as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// True when two path strings name one file: the same text, or both exist and are the same file by device and
/// inode (so a hard link counts), or both canonicalize (symlinks resolved) to the same path. A record file that
/// does not exist yet only matches its own text, so a fresh `--record` path is never mistaken for the inbox.
pub(crate) fn paths_name_one_file<A: AsRef<std::path::Path>, B: AsRef<std::path::Path>>(a: A, b: B) -> bool {
    let (pa, pb) = (a.as_ref(), b.as_ref());
    if pa == pb {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(ma), Ok(mb)) = (std::fs::metadata(pa), std::fs::metadata(pb)) {
            if ma.dev() == mb.dev() && ma.ino() == mb.ino() {
                return true;
            }
        }
    }
    match (std::fs::canonicalize(pa), std::fs::canonicalize(pb)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => false,
    }
}

/// The resolved form of a path (a relative one is taken against the working folder): a found path (a link counts,
/// also a dangling one) must canonicalize; a missing path needs a folder that canonicalizes and a file name.
fn resolved_form(p: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut path = p.to_path_buf();
    if path.is_relative() {
        path = std::env::current_dir().ok()?.join(path);
    }
    match fs::symlink_metadata(&path) {
        Ok(_) => fs::canonicalize(&path).ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let name = path.file_name()?.to_os_string();
            let dir = fs::canonicalize(path.parent()?).ok()?;
            Some(dir.join(name))
        }
        Err(_) => None,
    }
}

/// The one guard of `--record` for patterns, rank and suggest: an absolute path that resolves, names neither the
/// inbox nor a config or privacy list file, and is a new file, an empty regular file or a regular file that starts
/// with `{`. The file type is read before any open, so a FIFO cannot block the verb. Messages are constant.
pub(crate) fn record_guard(record: &str, config_flag: Option<&str>, inbox: &str) -> Result<(), &'static str> {
    if !record.starts_with('/') {
        return Err("--record must be an absolute path");
    }
    let Some(rec) = resolved_form(std::path::Path::new(record)) else {
        return Err("--record cannot be resolved");
    };
    let names = |other: &std::path::Path| paths_name_one_file(record, other) || resolved_form(other).is_some_and(|o| o == rec);
    if names(std::path::Path::new(inbox)) {
        return Err("--record must not name the friction inbox");
    }
    let mut protected: Vec<std::ffi::OsString> = Vec::new();
    if let Some(c) = config_flag {
        protected.push(c.into());
    }
    for var in ["HARNESS_CONFIG", "HARNESS_PRIVACY_LIST", "HARNESS_PRIVACY_OLD_LIST"] {
        if let Some(v) = std::env::var_os(var) {
            protected.push(v);
        }
    }
    for v in protected.iter().filter(|v| !v.is_empty()) {
        let v = std::path::Path::new(v);
        if names(v) {
            return Err("--record must not name a config file");
        }
        // A protected value that cannot be resolved (a dangling link) cannot be compared: refuse.
        if resolved_form(v).is_none() {
            return Err("--record cannot be resolved");
        }
    }
    const NOT_RECORD: &str = "--record names a file that is not a JSON-lines record";
    match fs::symlink_metadata(&rec) {
        Ok(md) => {
            if !md.file_type().is_file() {
                return Err(NOT_RECORD);
            }
            if md.len() > 0 {
                let mut first = [0u8; 1];
                let ok = fs::File::open(&rec).and_then(|mut f| f.read_exact(&mut first)).is_ok();
                if !ok || first[0] != b'{' {
                    return Err(NOT_RECORD);
                }
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err("--record cannot be resolved"),
    }
}

/// The mechanical pattern count (shared by `patterns` and `triage`): the chain roots (`kind: pattern`, empty
/// `repeats`) whose count of `repeats` rows is at least `threshold`, as (id, count), sorted.
pub(crate) fn pattern_roots(rows: &[Row], threshold: u64) -> Vec<(String, usize)> {
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for row in rows {
        if !row.repeats.is_empty() {
            *counts.entry(row.repeats.as_str()).or_default() += 1;
        }
    }
    let mut roots: Vec<(String, usize)> = rows
        .iter()
        .filter(|r| r.kind == "pattern" && r.repeats.is_empty())
        .map(|r| (r.id.clone(), counts.get(r.id.as_str()).copied().unwrap_or(0)))
        .filter(|(_, n)| *n >= threshold as usize)
        .collect();
    roots.sort();
    roots
}

/// `harness friction patterns [--config <file>] [--record <file>]` (ticket 192): the N-threshold listing.
/// Runs the FULL lint first (any lint failure = the same exit and stderr as `lint`, nothing on stdout); then
/// prints one line per chain-root pattern row whose count of `repeats` rows is >= N (`friction.pattern_threshold`,
/// default 3), sorted by id — `pattern id=<id> repeats=<n>` — and one closing line `patterns ok threshold=<N>
/// listed=<k>`. Ids and counts only: a row's text is never printed. Nothing is written except, when `--record`
/// is given, ONE appended JSONL trial line (`schemas/friction-trial.schema.json`). Being listed creates zero
/// files under any skill or ticket path. Exit codes as the family: 0 ok; 1 a refusal (the lint leg); 2 a usage
/// or setup error (a failed record append, so a half-written trial never reads as a clean run; and a `--record`
/// that the record guard refuses, for example one naming the inbox, because a trial line is not a row).
pub fn patterns_cmd(rest: &[String], testmode: bool) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let mut file: Option<&str> = None;
    let mut record: Option<&str> = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--config" => {
                let Some(v) = it.next() else {
                    return fail("--config is missing its value");
                };
                if file.replace(v.as_str()).is_some() {
                    return fail("--config was given twice");
                }
            }
            "--record" => {
                let Some(v) = it.next() else {
                    return fail("--record is missing its value");
                };
                if record.replace(v.as_str()).is_some() {
                    return fail("--record was given twice");
                }
            }
            _ => {
                // A constant message: the argument bytes have passed no privacy scan, so they are never echoed.
                return fail("an unknown option (patterns takes only --config <file> and --record <file>)");
            }
        }
    }
    let cfg = match config_path(file) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let cfg = match crate::config::Config::load(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    // A trial line is not a row: the record guard refuses a `--record` that is relative, unresolvable,
    // the inbox, a config or privacy list file, or not a JSON-lines record, before anything is read or
    // written. Exit 2, one line, nothing written.
    if let Some(path) = record {
        if let Err(m) = record_guard(path, file, cfg.friction_inbox()) {
            return fail(m);
        }
    }
    // The listing reads the inbox, so it runs the same privacy engine and fails closed without a list,
    // exactly like `add`, `lint` and `close-check`.
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned inbox is never listed"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let inbox = match read_inbox(std::path::Path::new(cfg.friction_inbox())) {
        Ok(i) => i,
        Err(e) => return fail(&e),
    };
    // The full lint, byte for byte the `lint` verb's refusal behaviour.
    if inbox.recovery_copy {
        eprintln!("harness: {RECOVERY_MSG}");
        return 1;
    }
    if let Some(n) = inbox.torn_tail {
        eprintln!("harness: inbox has a torn tail at line {n} (no trailing newline); not read as a row");
        return 1;
    }
    let mut problems = inbox.problems;
    problems.extend(privacy_problems(&ctx, &inbox.rows));
    if !problems.is_empty() {
        for (n, m) in &problems {
            eprintln!("harness: inbox line {n}: {m}");
        }
        return 1;
    }
    // The count is mechanical, never judged (the design rule): every row whose `repeats` names a root adds one
    // to that root's count; a root is a `kind: pattern` row with an empty `repeats`.
    let threshold = cfg.pattern_threshold();
    let plain: Vec<Row> = inbox.rows.iter().map(|(_, r)| r.clone()).collect();
    let roots = pattern_roots(&plain, threshold);
    // The trial record is appended BEFORE anything is printed, so a failed append is a clean exit 2 with an
    // empty stdout, never a printed listing the record disagrees with.
    if let Some(path) = record {
        let listed: Vec<String> = roots.iter().map(|(id, _)| jstr(id)).collect();
        let line = format!(
            "{{\"date\":{},\"threshold\":{},\"rows\":{},\"listed\":[{}]}}\n",
            jstr(&today_utc()),
            threshold,
            inbox.rows.len(),
            listed.join(",")
        );
        let mut f = match fs::OpenOptions::new().append(true).create(true).open(std::path::Path::new(path)) {
            Ok(f) => f,
            Err(_) => return fail("the trial record cannot be written"),
        };
        if f.write_all(line.as_bytes()).and_then(|()| f.sync_all()).is_err() {
            return fail("the trial record cannot be written");
        }
    }
    for (id, n) in &roots {
        println!("pattern id={id} repeats={n}");
    }
    println!("patterns ok threshold={} listed={}", threshold, roots.len());
    0
}

fn close_check_cmd(rest: &[String], testmode: bool) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let mut note: Option<&str> = None;
    let mut ticket: Option<&str> = None;
    let mut file: Option<&str> = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--ticket" => {
                let Some(v) = it.next() else {
                    return fail("--ticket is missing its value");
                };
                if ticket.replace(v.as_str()).is_some() {
                    return fail("--ticket was given twice");
                }
            }
            "--config" => {
                let Some(v) = it.next() else {
                    return fail("--config is missing its value");
                };
                if file.replace(v.as_str()).is_some() {
                    return fail("--config was given twice");
                }
            }
            s if s.starts_with("--") => return fail("unknown option"),
            s => {
                if note.replace(s).is_some() {
                    return fail("more than one close note given");
                }
            }
        }
    }
    let Some(note_path) = note else {
        eprintln!("{}", FRICTION_USAGE);
        return 2;
    };
    let Some(ticket) = ticket else {
        return fail("--ticket is required");
    };
    // The ticket names what the inbox is filtered by, so a non-token value is refused before anything is read.
    if !token_ok(ticket) {
        return fail("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')");
    }
    let note = match read_note(std::path::Path::new(note_path)) {
        Ok(t) => t,
        Err(e) => return fail(&e),
    };
    let cfg = match config_path(file) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let cfg = match crate::config::Config::load(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    // The close check reads the inbox, so it runs the same privacy scan; with no list configured it fails
    // closed (2), exactly like `add` and `lint`.
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned inbox is never checked"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    // The ticket is printed on the success line and named on refusals, so it is scanned first: a hit names the
    // field and the class only, never the bytes, and nothing reaches stdout.
    let ticket_hits = crate::scan::scan_text(&ctx, "friction.close-check", ticket);
    if !ticket_hits.is_empty() {
        let classes: Vec<&str> = ticket_hits.iter().map(|h| h.class).collect();
        eprintln!("harness: close-check refused: field ticket has a privacy hit ({}); the note is not checked", classes.join(", "));
        return 1;
    }
    // The close lines are scanned before any of their ids can be echoed by a refusal (a hit names the class
    // only, never the bytes; the ids the check names on later refusals have all passed this scan).
    for l in lesson_lines(&note) {
        let hits = crate::scan::scan_text(&ctx, "friction.close-check", l);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            eprintln!("harness: close-check refused: the `lesson:` line has a privacy hit ({}); the note is not checked", classes.join(", "));
            return 1;
        }
    }
    let inbox_path = std::path::Path::new(cfg.friction_inbox());
    // A missing inbox is a fresh install (no lessons captured yet): it holds no row, so only `lesson: none` can pass.
    // A DAMAGED inbox (unreadable, oversized, torn, bad rows, a privacy hit in a stored row) fails the check
    // closed: it cannot be compared against.
    let rows = if !inbox_path.exists() {
        // No lock to take on a file that is not there; a recovery copy beside the missing path still refuses.
        if recovery_copy_present(inbox_path) {
            return fail(RECOVERY_MSG);
        }
        Vec::new()
    } else {
        match read_inbox(inbox_path) {
            Ok(i) => {
                if i.recovery_copy {
                    return fail(RECOVERY_MSG);
                }
                if let Some(n) = i.torn_tail {
                    return fail(&format!("the inbox has a torn tail at line {n}; the close line cannot be checked"));
                }
                if let Some((n, m)) = i.problems.first() {
                    return fail(&format!("the inbox has a problem at line {n}: {m}; the close line cannot be checked"));
                }
                if let Some((n, m)) = privacy_problems(&ctx, &i.rows).first() {
                    return fail(&format!("the inbox has a problem at line {n}: {m}; the close line cannot be checked"));
                }
                i.rows.list
            }
            Err(e) => return fail(&e),
        }
    };
    match close_check(&note, ticket, &rows) {
        Ok(n) => {
            println!("close-check ok ticket={} lessons={}", ticket, n);
            0
        }
        Err(m) => {
            eprintln!("harness: close-check refused: {}", m);
            1
        }
    }
}


// ---------------------------------------------------------------- the weekly sweep (ticket 200)

/// The `--week` grammar: exactly the dedup-key suffix `<iso-year>-w<ww>` (four digits, `-w`, two digits).
/// Digits only, so nothing a caller could hide in the value can ever be echoed back by the summary line.
pub(crate) fn week_ok(w: &str) -> bool {
    let b = w.as_bytes();
    b.len() == 8
        && b[0..4].iter().all(|c| c.is_ascii_digit())
        && b[4] == b'-'
        && b[5] == b'w'
        && b[6..8].iter().all(|c| c.is_ascii_digit())
}

/// `harness friction sweep --log <event-log> --week <iso-year>-w<ww> [--config <file>]` (ticket 200): the
/// weekly sweep that feeds the T-9.3 mapper (`friction_event`) one week of the event log and files its rows
/// through the add path's own checks. The whole log is read under the family's 4 MiB cap and checked by the
/// event reader's own parser (a torn tail or a corrupt line refuses the whole sweep, exit 1, nothing filed);
/// the mapped rows for the given week then walk the same door `add` walks: every field of every row through
/// the privacy engine, then ONE `open_locked` critical section — `rows_for_add`, skip each row whose id is
/// already stored (the id carries the week, so this is what makes a re-run in the same week file nothing,
/// exit 0), `cross_check` the rest, append. All checks pass before the first append, so a refused sweep
/// leaves the inbox byte-identical and a crash mid-sweep leaves a prefix the next run completes. A sweep
/// that would file zero rows never creates a missing inbox. `--log` and `--week` are never defaulted: no
/// wall clock runs in this verb, so any past week can be re-swept and every run is deterministic. Exit
/// codes as the family: 0 ok; 1 a refusal; 2 a usage or setup error (a bad `--week`, no config, an
/// unreadable or oversized log, no privacy list). No path or flag value is ever echoed.
pub(crate) fn sweep_cmd(rest: &[String], testmode: bool) -> i32 {
    let fail = |m: &str| {
        eprintln!("harness: error: {}", m);
        2
    };
    let refuse = |m: &str| {
        eprintln!("harness: friction refused: {}", m);
        eprintln!("harness: ROWS NOT RECORDED");
        1
    };
    let mut log: Option<&str> = None;
    let mut week: Option<&str> = None;
    let mut file: Option<&str> = None;
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--log" => {
                let Some(v) = it.next() else {
                    return fail("--log is missing its value");
                };
                if log.replace(v.as_str()).is_some() {
                    return fail("--log was given twice");
                }
            }
            "--week" => {
                let Some(v) = it.next() else {
                    return fail("--week is missing its value");
                };
                if week.replace(v.as_str()).is_some() {
                    return fail("--week was given twice");
                }
            }
            "--config" => {
                let Some(v) = it.next() else {
                    return fail("--config is missing its value");
                };
                if file.replace(v.as_str()).is_some() {
                    return fail("--config was given twice");
                }
            }
            _ => {
                // A constant message: the argument bytes have passed no privacy scan, so they are never echoed.
                return fail("an unknown option (sweep takes only --log <file>, --week <iso-year>-w<ww> and --config <file>)");
            }
        }
    }
    let Some(log_path) = log else {
        return fail("--log is required (never defaulted: no wall clock runs in this verb)");
    };
    let Some(week) = week else {
        return fail("--week is required (never defaulted: no wall clock runs in this verb)");
    };
    // The week is grammar-checked before anything is read and before it can ever be echoed by the summary
    // line; the refusal names the grammar, never the value.
    if !week_ok(week) {
        return fail("--week must be an ISO week <iso-year>-w<ww>: four digits, -w, two digits (the dedup-key suffix)");
    }
    let cfg = match config_path(file) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let cfg = match crate::config::Config::load(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    // The privacy list loads before anything else is read: with no list configured the sweep fails closed
    // (2), exactly like `add` — an unscanned row is never filed.
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned row is never filed"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    // The log, read whole under the family cap (cap+1 bytes, so a log exactly at the cap is accepted and a
    // log one byte over is refused). A missing or unreadable log is a setup error, not an empty week.
    let log_path = std::path::Path::new(log_path);
    let log_bytes = match read_capped(log_path, EVENT_LOG_CAP) {
        Ok(b) => b,
        Err(_) => return fail("the event log cannot be read"),
    };
    if log_bytes.len() as u64 > EVENT_LOG_CAP {
        return fail(&format!("the event log is over the {}-byte cap; nothing was filed", EVENT_LOG_CAP));
    }
    // The event reader's own parser decides torn vs corrupt: a final fragment with no newline is a torn
    // tail, a complete-but-invalid line is corruption; both refuse the whole sweep (one bad line files no
    // rows), and the line number is the parser's, never re-counted here.
    let log_read = crate::events::parse_log(&log_bytes);
    if let Some(t) = &log_read.torn_tail {
        return refuse(&format!("the event log has a torn tail at line {}; nothing was filed", t.line));
    }
    if let Some(c) = &log_read.corrupt {
        return refuse(&format!("the event log is corrupt at line {}; nothing was filed", c.line));
    }
    // A clean parse means every line decoded and the file ends with a newline, so the whole read is UTF-8.
    let text = match String::from_utf8(log_bytes) {
        Ok(t) => t,
        Err(_) => return fail("the event log cannot be read"),
    };
    let lines: Vec<&str> = text.lines().collect();
    // The ONE dedup/earliest-ts implementation (the batch helper), not a re-implementation: fail closed on
    // the whole batch, one row per key, sorted by id.
    let rows = match crate::friction_event::map_lines(&lines) {
        Ok(r) => r,
        Err(e) => return refuse(&e),
    };
    // Only this week's rows: the id ends `.<iso-year>-w<ww>`, so the filter is a suffix match on the key.
    let suffix = format!(".{}", week);
    let week_rows: Vec<Row> = rows.into_iter().filter(|r| r.id.ends_with(&suffix)).collect();
    if week_rows.is_empty() {
        println!("sweep week={} filed=0 already=0", week);
        return 0;
    }
    // Every field of every row the sweep would file, through the same privacy engine as `add`; a hit names
    // the field and the class only, and the row — the whole batch — is not stored.
    for row in &week_rows {
        for (name, value) in row_fields(row) {
            let hits = crate::scan::scan_text(&ctx, "friction.sweep", value);
            if !hits.is_empty() {
                let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
                return refuse(&format!("field {name} has a privacy hit ({}); the row is not stored", classes.join(", ")));
            }
        }
    }
    // A row that cannot be encoded is refused BEFORE the inbox is opened, so a refused sweep never creates
    // the file (the mapper already validated every row, so this is the same backstop `add` keeps).
    for row in &week_rows {
        if let Err(e) = encode_row(row) {
            return refuse(&e);
        }
    }
    let inbox_path = std::path::PathBuf::from(cfg.friction_inbox());
    // A recovery copy beside a missing inbox: the copy is reported and the inbox is not created.
    if missing_inbox_with_recovery_copy(&inbox_path) {
        eprintln!("harness: friction refused: {RECOVERY_MSG}");
        return 1;
    }
    // One writer at a time, the same lock as `add`: read, skip, check and append through one descriptor.
    let mut file = match open_locked(&inbox_path) {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let mut rows = match rows_for_add(&file, &inbox_path, &ctx) {
        Ok(r) => r,
        Err((code, m)) => {
            if m == RECOVERY_MSG {
                eprintln!("harness: friction refused: {m}");
                return code;
            }
            eprintln!("harness: friction refused: {}", m);
            eprintln!("harness: ROWS NOT RECORDED");
            return code;
        }
    };
    // The idempotence step: a row already stored is SKIPPED, not refused (a re-run in the same week exits 0
    // with filed=0); the inbox's own duplicate-id refusal stays the second backstop.
    let stored: std::collections::HashSet<&str> = rows.iter().map(|(_, r)| r.id.as_str()).collect();
    let to_file: Vec<&Row> = week_rows.iter().filter(|r| !stored.contains(r.id.as_str())).collect();
    let already = week_rows.len() - to_file.len();
    let first_new = rows.len() + 1;
    for (i, row) in to_file.iter().enumerate() {
        rows.push((first_new + i, (*row).clone()));
    }
    // All checks before the first append (all-or-nothing against a half-checked inbox): the cross rules over
    // the stored rows plus every row about to be filed, refusing on any problem a NEW line would carry.
    if let Some((_, m)) = cross_check(&rows).into_iter().find(|(n, _)| *n >= first_new) {
        return refuse(&m);
    }
    for row in &to_file {
        if let Err(e) = append_row_to(&mut file, row) {
            return fail(&e);
        }
        println!("stored id={}", row.id);
    }
    println!("sweep week={} filed={} already={}", week, to_file.len(), already);
    0
}

#[cfg(test)]
mod uescape_tests {
    use super::P;

    fn object(body: &str) -> Result<Vec<(String, String)>, String> {
        P { b: body.as_bytes(), i: 0 }.object()
    }

    #[test]
    fn hex_escapes_decode() {
        let body = format!("{{\"k\":\"{b}u0043{b}u00e9{b}u00E9\"}}", b = '\\');
        assert_eq!(object(&body).unwrap(), vec![("k".to_string(), "C\u{e9}\u{e9}".to_string())]);
    }

    #[test]
    fn a_plus_escape_in_a_key_or_a_value_is_an_err() {
        let b = '\\';
        assert!(object(&format!("{{\"{b}u+043\":\"v\"}}")).is_err());
        assert!(object(&format!("{{\"k\":\"{b}u+043\"}}")).is_err());
    }
}
