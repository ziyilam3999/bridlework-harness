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
/// The two optional fields (ticket 192): `kind` (the row's class, default `friction`) and `repeats` (the id of
/// the pattern row this row repeats). Both are omitted from the canonical line at their defaults, so every
/// six-key row written before this contract re-encodes byte-identical.
pub const OPTIONAL_FIELDS: [&str; 2] = ["kind", "repeats"];
/// The two legal `kind` values (the default, `friction`, needs no key).
pub const KINDS: [&str; 2] = ["friction", "pattern"];
/// One encoded row, bytes, including the trailing newline this module always writes.
pub const LINE_CAP: usize = 16 * 1024;
/// The whole inbox is read no further than this; past it the verb fails closed (exit 2).
const INBOX_CAP: u64 = 4 * 1024 * 1024;
/// A close note is read no further than this; past it the check fails closed (exit 2).
const NOTE_CAP: u64 = 1024 * 1024;

/// One inbox row. `breaks` is empty or the id of the lesson that broke again (K-1 rule 3). `kind` is empty
/// (= `friction`), `friction` or `pattern`; `repeats` is empty or the id of the `kind: pattern` row this row
/// repeats (ticket 192). A `pattern` row with no `repeats` is the root of its chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub id: String,
    pub date: String,
    pub ticket: String,
    pub tag: String,
    pub breaks: String,
    pub text: String,
    pub kind: String,
    pub repeats: String,
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
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
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
    Ok(())
}

// ---------------------------------------------------------------- strict JSON (the row grammar only)

/// A tiny strict reader for the one shape a row has: `{ "key": "value", ... }`, every value a string.
struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl<'a> P<'a> {
    fn ws(&mut self) {
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
    fn object(&mut self) -> Result<Vec<(String, String)>, String> {
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
fn jstr(s: &str) -> String {
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
            return Err("an unknown field: a row has exactly the six contract fields plus the two optional ones (kind, repeats)".to_string());
        }
    }
    let mut row = Row { id: String::new(), date: String::new(), ticket: String::new(), tag: String::new(), breaks: String::new(), text: String::new(), kind: String::new(), repeats: String::new() };
    for (k, slot) in [("id", &mut row.id), ("date", &mut row.date), ("ticket", &mut row.ticket), ("where-it-lives", &mut row.tag), ("breaks", &mut row.breaks), ("text", &mut row.text)] {
        *slot = get(k).ok_or_else(|| format!("missing field {k:?}"))?;
    }
    // The two optional fields default to their defaults when absent; an explicit empty value reads the same.
    row.kind = get("kind").unwrap_or_default();
    row.repeats = get("repeats").unwrap_or_default();
    validate_row(&row)?;
    Ok(row)
}

/// Canonical line WITHOUT its newline: the six fields in `FIELDS` order, then `kind` and `repeats` when they are
/// not at their defaults (`kind` empty or `friction`, `repeats` empty). Refuses an invalid row and a line over the cap.
pub fn encode_row(row: &Row) -> Result<String, String> {
    validate_row(row)?;
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
    pub rows: Vec<(usize, Row)>,
    pub problems: Vec<(usize, String)>,
    pub torn_tail: Option<usize>,
}

/// Read at most the cap plus one byte, then lint every line. `Err` is a setup failure (unreadable, oversized, not
/// UTF-8); a bad row is a `problems` entry, not an `Err`, so one checker reports every line.
pub fn read_inbox(path: &Path) -> Result<Inbox, String> {
    let meta = fs::metadata(path).map_err(|_| "the inbox cannot be read".to_string())?;
    if meta.is_dir() {
        return Err("the inbox path is a folder".to_string());
    }
    let bytes = read_capped(path, INBOX_CAP).map_err(|_| "the inbox cannot be read".to_string())?;
    inbox_from_bytes(bytes)
}

/// The same read over an already-open (and, for `add`, already-locked) inbox descriptor: the rows the append
/// will follow are read from the very file the lock is held on.
fn read_inbox_locked(f: &fs::File) -> Result<Inbox, String> {
    let mut buf = Vec::new();
    f.take(INBOX_CAP + 1).read_to_end(&mut buf).map_err(|_| "the inbox cannot be read".to_string())?;
    inbox_from_bytes(buf)
}

fn inbox_from_bytes(bytes: Vec<u8>) -> Result<Inbox, String> {
    if bytes.len() as u64 > INBOX_CAP {
        return Err(format!("the inbox is over the {}-byte cap", INBOX_CAP));
    }
    let text = String::from_utf8(bytes).map_err(|_| "the inbox is not utf-8".to_string())?;
    Ok(lint_text(&text))
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
            match parse_row(line) {
                Ok(row) => out.rows.push((line_no, row)),
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
fn row_fields(row: &Row) -> [(&'static str, &str); 8] {
    [
        ("id", row.id.as_str()),
        ("date", row.date.as_str()),
        ("ticket", row.ticket.as_str()),
        ("where-it-lives", row.tag.as_str()),
        ("breaks", row.breaks.as_str()),
        ("text", row.text.as_str()),
        ("kind", row.kind.as_str()),
        ("repeats", row.repeats.as_str()),
    ]
}

/// The privacy engine over every field of every stored row — the same rules `add` applies to a new row. Every
/// reader of the inbox runs this pass (add, lint, close-check); a reason names the line, the field and the
/// pattern class, never the bytes.
pub fn privacy_problems(ctx: &crate::scan::Ctx, rows: &[(usize, Row)]) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (n, row) in rows {
        for (name, value) in row_fields(row) {
            let hits = crate::scan::scan_text(ctx, "friction", value);
            if !hits.is_empty() {
                let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
                out.push((*n, format!("field {name} has a privacy hit ({})", classes.join(", "))));
            }
        }
    }
    out
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
                problems.push((*line, "repeats is allowed only on a kind: pattern row".to_string()));
            } else if by_id[row.repeats.as_str()].kind != "pattern" {
                problems.push((*line, "repeats must name a kind: pattern row (its chain root)".to_string()));
            } else if !by_id[row.repeats.as_str()].repeats.is_empty() {
                problems.push((*line, "repeats must name the chain root: the row it names itself carries repeats".to_string()));
            }
        }
    }
    problems
}

// ---------------------------------------------------------------- the writer

/// Append one row as a single whole line plus newline, after every check has passed. The file is created when absent
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
/// one kernel advisory lock (flock(2), std's file lock, Rust 1.89+) on the inbox's own inode. Every spelling of
/// the path (a symlinked folder or file, a `.`/`..` detour, a hard link, a not-yet-created file behind a
/// symlink) opens the same inode, so they all take the ONE lock; there is no side lock file and no path is
/// built or compared. The kernel releases the lock when the descriptor closes (return, panic, kill -9), so
/// there is no stale-steal rule. The returned file is the locked descriptor: the caller reads and appends through it,
/// and dropping it releases the lock. `Err` is a setup failure (cannot open) or the lock stayed busy past
/// LOCK_WAIT. Nothing here prints any path.
fn open_locked(inbox: &Path) -> Result<fs::File, String> {
    let f = fs::OpenOptions::new()
        .read(true)
        .append(true)
        .create(true)
        .open(inbox)
        .map_err(|_| "the inbox cannot be written".to_string())?;
    let deadline = std::time::Instant::now() + LOCK_WAIT;
    loop {
        match f.try_lock() {
            Ok(()) => return Ok(f),
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(_)) => return Err("the inbox cannot be written".to_string()),
        }
        if std::time::Instant::now() >= deadline {
            return Err("the inbox is busy (another writer holds it); try again".to_string());
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn read_capped(path: &Path, cap: u64) -> std::io::Result<Vec<u8>> {
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

pub const FRICTION_USAGE: &str = "usage: harness friction add --id <id> --date <YYYY-MM-DD> --ticket <t> --where-it-lives <skill|gate|memory|wiki> [--breaks <lesson-id>] [--kind <friction|pattern>] [--repeats <row-id>] --text <lesson> [--config <file>] | friction lint [--config <file>] | friction patterns [--config <file>] [--record <file>] | friction close-check <close-note> --ticket <t> [--config <file>]";

/// The config source, resolved exactly as `harness config check` and `session check-name` resolve it: `--config`,
/// else `HARNESS_CONFIG`, no third source, an empty value refused and never defaulted.
fn config_path(file: Option<&str>) -> Result<std::path::PathBuf, String> {
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

/// `harness friction add | lint | close-check` (K-1 slice 1; the contract is `docs/friction-inbox.md`).
/// Exit 0 ok; 1 a refusal (a refused write leaves the inbox byte-identical, stdout empty, stderr naming the field
/// and the reason — never the lesson text, never the inbox path); 2 a usage or setup error. Nothing is written
/// before every check has passed: flags, then row rules, then the privacy engine over the new row, then — under
/// the exclusive writer lock — the same privacy engine over every stored row and the inbox cross rules, and only
/// then one append.
pub fn friction_cmd(a: &[String], testmode: bool) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{}", FRICTION_USAGE);
        return 2;
    };
    match sub.as_str() {
        "add" => add_cmd(rest, testmode),
        "lint" => lint_cmd(rest, testmode),
        "patterns" => patterns_cmd(rest, testmode),
        "close-check" => close_check_cmd(rest, testmode),
        _ => {
            eprintln!("{}", FRICTION_USAGE);
            2
        }
    }
}

/// The inbox rows, refusing a damaged inbox (fail closed): a torn tail, any bad row or any privacy hit in a
/// stored row blocks the append. Reads the locked descriptor, so the rows are the ones the append will follow.
fn rows_for_add(f: &fs::File, ctx: &crate::scan::Ctx) -> Result<Vec<(usize, Row)>, (i32, String)> {
    let inbox = read_inbox_locked(f).map_err(|m| (2, m))?;
    if let Some(n) = inbox.torn_tail {
        return Err((1, format!("the inbox has a torn tail at line {n}; nothing was appended (run harness friction lint)")));
    }
    if let Some((n, m)) = inbox.problems.first() {
        return Err((1, format!("the inbox already has a problem at line {n}: {m}; nothing was appended (run harness friction lint)")));
    }
    if let Some((n, m)) = privacy_problems(ctx, &inbox.rows).first() {
        return Err((1, format!("the inbox already has a problem at line {n}: {m}; nothing was appended (run harness friction lint)")));
    }
    Ok(inbox.rows)
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
    let single = ["config", "id", "date", "ticket", "where-it-lives", "breaks", "text", "kind", "repeats"];
    let mut o: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut it = rest.iter();
    while let Some(arg) = it.next() {
        let Some(name) = arg.strip_prefix("--") else {
            return fail("an unexpected argument (only the nine flags of the usage line are allowed)");
        };
        if !single.contains(&name) {
            return fail("an unknown option (the usage line lists the nine flags)");
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
    };
    if let Err(e) = validate_row(&row) {
        return refuse(&e);
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
    // One writer at a time: the exclusive lock is on the inbox file itself, held across read, check and append,
    // and released on every exit path, so the duplicate-id check can never race a concurrent add of the same id.
    let mut file = match open_locked(&inbox_path) {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    let mut rows = match rows_for_add(&file, &ctx) {
        Ok(r) => r,
        Err((code, m)) => {
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
    println!("inbox ok rows={}", inbox.rows.len());
    0
}

/// Today's UTC date as `YYYY-MM-DD` (std only: days since the epoch to the civil calendar). The trial
/// record's date, nothing more.
fn today_utc() -> String {
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

/// `harness friction patterns [--config <file>] [--record <file>]` (ticket 192): the N-threshold listing.
/// Runs the FULL lint first (any lint failure = the same exit and stderr as `lint`, nothing on stdout); then
/// prints one line per chain-root pattern row whose count of `repeats` rows is >= N (`friction.pattern_threshold`,
/// default 3), sorted by id — `pattern id=<id> repeats=<n>` — and one closing line `patterns ok threshold=<N>
/// listed=<k>`. Ids and counts only: a row's text is never printed. Nothing is written except, when `--record`
/// is given, ONE appended JSONL trial line (`schemas/friction-trial.schema.json`). Being listed creates zero
/// files under any skill or ticket path. Exit codes as the family: 0 ok; 1 a refusal (the lint leg); 2 a usage
/// or setup error (and a failed record append, so a half-written trial never reads as a clean run).
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
    let mut counts: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for (_, row) in &inbox.rows {
        if !row.repeats.is_empty() {
            *counts.entry(row.repeats.as_str()).or_default() += 1;
        }
    }
    let mut roots: Vec<(String, usize)> = inbox
        .rows
        .iter()
        .filter(|(_, r)| r.kind == "pattern" && r.repeats.is_empty())
        .map(|(_, r)| (r.id.clone(), counts.get(r.id.as_str()).copied().unwrap_or(0)))
        .filter(|(_, n)| *n >= threshold as usize)
        .collect();
    roots.sort();
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
        Vec::new()
    } else {
        match read_inbox(inbox_path) {
            Ok(i) => {
                if let Some(n) = i.torn_tail {
                    return fail(&format!("the inbox has a torn tail at line {n}; the close line cannot be checked"));
                }
                if let Some((n, m)) = i.problems.first() {
                    return fail(&format!("the inbox has a problem at line {n}: {m}; the close line cannot be checked"));
                }
                if let Some((n, m)) = privacy_problems(&ctx, &i.rows).first() {
                    return fail(&format!("the inbox has a problem at line {n}: {m}; the close line cannot be checked"));
                }
                i.rows
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

