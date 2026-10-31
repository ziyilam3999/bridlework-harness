//! Event log core (std only): the append-only JSON Lines file the caps' refusals are recorded in.
//! One event per line, compact, fixed key order, every key present, `v` (schema version) on every line.
//! Reader: every complete line must be a valid event (else CORRUPTION, the read stops there); a final fragment with no
//! trailing newline is a TORN TAIL, reported and never parsed. Writer: one `write()` of the whole line, then `sync_all`;
//! refuses to append onto a torn tail (it would fuse a new event onto garbage). Recovery of a torn tail is not here.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Schema version written on every line and the only one this reader accepts.
pub const SCHEMA_VERSION: u64 = 1;
/// Largest accepted line in bytes, newline included. Keeps each append one small write.
pub const LINE_CAP: usize = 4096;

/// Keys of every v1 kind, in canonical order.
const KEYS_V1: [&str; 6] = ["v", "kind", "ts_ms", "ticket", "repo", "reason"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Escalation,
    ReplanRequired,
    BudgetExceeded,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Escalation, Kind::ReplanRequired, Kind::BudgetExceeded];

    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Escalation => "escalation",
            Kind::ReplanRequired => "replan_required",
            Kind::BudgetExceeded => "budget_exceeded",
        }
    }

    pub fn parse(s: &str) -> Option<Kind> {
        Kind::ALL.iter().copied().find(|k| k.as_str() == s)
    }

    /// The exact key set of this kind at the current schema version, in canonical order.
    pub fn keys(&self) -> &'static [&'static str] {
        &KEYS_V1
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub kind: Kind,
    /// Caller-supplied milliseconds since the epoch.
    pub ts_ms: u64,
    pub ticket: String,
    pub repo: String,
    pub reason: String,
}

/// An id or reason token: lowercase letters, digits, `-`, `_`, `.`; never empty, never `.` or `..`.
pub fn token_ok(s: &str) -> bool {
    !s.is_empty() && s != "." && s != ".." && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

impl Event {
    pub fn validate(&self) -> Result<(), String> {
        for (name, v) in [("ticket", &self.ticket), ("repo", &self.repo), ("reason", &self.reason)] {
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
            "{{\"v\":{},\"kind\":\"{}\",\"ts_ms\":{},\"ticket\":\"{}\",\"repo\":\"{}\",\"reason\":\"{}\"}}\n",
            SCHEMA_VERSION,
            self.kind.as_str(),
            self.ts_ms,
            self.ticket,
            self.repo,
            self.reason
        );
        if line.len() > LINE_CAP {
            return Err(AppendError::TooLong(line.len()));
        }
        Ok(line)
    }

    /// Strict decode of one line WITHOUT its newline: every key of the kind exactly once, no other key, right types, a known
    /// kind, `v` equal to `SCHEMA_VERSION` (checked first so a refusal names `v`), then `validate`.
    pub fn decode(line: &str) -> Result<Event, String> {
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
        for (k, _) in &pairs {
            if !kind.keys().contains(&k.as_str()) {
                return Err(format!("unexpected key {k:?}"));
            }
        }
        for k in kind.keys() {
            if get(k).is_none() {
                return Err(format!("{k} missing"));
            }
        }
        let text = |k: &str| match get(k) {
            Some(Val::Str(s)) => Ok(s.clone()),
            _ => Err(format!("{k} has the wrong type")),
        };
        let ts_ms = match get("ts_ms") {
            Some(Val::Num(n)) => *n,
            _ => return Err("ts_ms has the wrong type".to_string()),
        };
        let ev = Event { kind, ts_ms, ticket: text("ticket")?, repo: text("repo")?, reason: text("reason")? };
        ev.validate()?;
        Ok(ev)
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
    pub events: Vec<Event>,
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
            Ok(s) => Event::decode(s),
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

fn append_impl(root: &Path, log: &Path, ev: &Event, do_sync: bool) -> Result<(), AppendError> {
    let line = ev.encode()?;
    let path = check_log_path(root, log)?;
    let io = |e: std::io::Error| AppendError::Io(e.kind().to_string());
    // NOTE for 6b: `check_log_path` resolves the path and `open` resolves it again, so a symlink swapped in between could
    // point the open outside the root. Not closed here (out of scope for 6a); re-check the opened file's identity if needed.
    let mut f = OpenOptions::new().read(true).append(true).create(true).open(&path).map_err(io)?;
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
    let n = f.write(line.as_bytes()).map_err(io)?;
    if n != line.len() {
        return Err(AppendError::Io("short write".to_string()));
    }
    if do_sync {
        sync(&f).map_err(io)?;
    }
    Ok(())
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
        Event { kind, ts_ms: n, ticket: "t-1".to_string(), repo: "my-repo".to_string(), reason: "cap.hit".to_string() }
    }

    fn three(t: &Tmp) -> Vec<u8> {
        for (i, k) in Kind::ALL.iter().enumerate() {
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
        assert_eq!(r.events.iter().map(|e| e.kind).collect::<Vec<_>>(), Kind::ALL.to_vec());
        assert_eq!(
            ev(Kind::BudgetExceeded, 7).encode().unwrap(),
            "{\"v\":1,\"kind\":\"budget_exceeded\",\"ts_ms\":7,\"ticket\":\"t-1\",\"repo\":\"my-repo\",\"reason\":\"cap.hit\"}\n"
        );
    }

    #[test]
    fn ac1_planted_a_fourth_kind_is_refused_on_read_and_cannot_be_constructed() {
        assert_eq!(Kind::parse("surprise"), None);
        let l = "{\"v\":1,\"kind\":\"surprise\",\"ts_ms\":1,\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\"}";
        assert!(Event::decode(l).unwrap_err().contains("unknown kind"));
        // The twin: the same line with a real kind decodes.
        assert!(Event::decode(&l.replace("surprise", "escalation")).is_ok());
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
        assert!(Event::decode(good).is_ok(), "twin: v=1 passes");
        for (bad, want) in [
            (good.replace("\"v\":1", "\"v\":2"), "unsupported v 2"),
            (good.replace("\"v\":1", "\"v\":0"), "unsupported v 0"),
            (good.replace("\"v\":1,", ""), "v missing"),
            (good.replace("\"v\":1", "\"v\":\"1\""), "v has the wrong type"),
        ] {
            assert_eq!(Event::decode(&bad).unwrap_err(), want, "{bad}");
            let log = format!("{good}\n{bad}\n{good}\n");
            let r = parse_log(log.as_bytes());
            assert_eq!(r.events.len(), 1, "only the line before the bad one");
            let c = r.corrupt.unwrap();
            assert_eq!(c.line, 2);
            assert_eq!(c.msg, want);
        }
        // v is checked before the rest, so a v2 line with an unknown kind still names v.
        assert_eq!(Event::decode(&good.replace("\"v\":1", "\"v\":2").replace("escalation", "future")).unwrap_err(), "unsupported v 2");
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
        let long = format!("{{\"v\":1,\"kind\":\"escalation\",\"ts_ms\":1,\"ticket\":\"{}\",\"repo\":\"r\",\"reason\":\"x\"}}", "a".repeat(LINE_CAP));
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
                        let e = Event { kind: Kind::Escalation, ts_ms: i as u64, ticket: format!("w{w}"), repo: "r".to_string(), reason: "x".to_string() };
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
        assert_eq!(r.events, vec![ev(Kind::BudgetExceeded, 5)]);
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
            for field in 0..3 {
                let mut e = ev(Kind::Escalation, 1);
                match field {
                    0 => e.ticket = bad.to_string(),
                    1 => e.repo = bad.to_string(),
                    _ => e.reason = bad.to_string(),
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
    fn every_kind_has_the_exact_v1_key_set_and_encodes_exactly_those_keys() {
        for k in Kind::ALL {
            assert_eq!(k.keys(), &["v", "kind", "ts_ms", "ticket", "repo", "reason"]);
            let line = ev(k, 1).encode().unwrap();
            let mut p = P { b: line.trim_end().as_bytes(), i: 0 };
            let keys: Vec<String> = p.object().unwrap().into_iter().map(|(k, _)| k).collect();
            assert_eq!(keys, k.keys());
            assert_eq!(Kind::parse(k.as_str()), Some(k));
        }
    }
}
