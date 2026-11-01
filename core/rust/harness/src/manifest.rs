//! Resource manifest core (std only): the per-ticket append-only JSON Lines file that lists what a ticket owns.
//! One operation per line, compact, fixed key order, every key present, `v` (schema version) on every line.
//! Same line, reader and writer discipline as the event log (`events.rs`): a final fragment with no trailing newline is a
//! TORN TAIL, reported and never parsed; a complete bad line is CORRUPTION and stops the read; the writer appends under an
//! exclusive lock with a last-byte check, one write per line, then `sync_all`, and refuses to append onto a torn tail.
//! On top of the event log's rules the manifest adds a fence: the first line(s) name the ticket's roots (existing folders),
//! and every path entry must resolve inside one of them. The manifest file itself must live outside the roots, so removing
//! the roots never removes the record.

use crate::events::{token_ok, LineError, TornTail};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Schema version written on every line and the only one this reader accepts.
pub const SCHEMA_VERSION: u64 = 1;
/// Largest accepted line in bytes, newline included. Keeps each append one small write.
pub const LINE_CAP: usize = 4096;
/// Largest accepted manifest file in bytes. A bigger file is refused as corrupt: fail closed, never an unbounded read.
pub const FILE_CAP: usize = 1024 * 1024;

// ---------------------------------------------------------------- line model

/// The classes a manifest entry can have: four path classes, one process class, one port class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Class {
    Worktree,
    TempDir,
    Log,
    Evidence,
    Process,
    Port,
}

impl Class {
    pub const ALL: [Class; 6] =
        [Class::Worktree, Class::TempDir, Class::Log, Class::Evidence, Class::Process, Class::Port];

    pub fn as_str(&self) -> &'static str {
        match self {
            Class::Worktree => "worktree",
            Class::TempDir => "temp_dir",
            Class::Log => "log",
            Class::Evidence => "evidence",
            Class::Process => "process",
            Class::Port => "port",
        }
    }

    pub fn parse(s: &str) -> Option<Class> {
        Class::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// The path classes carry `path` as their target; process and port carry `pid` / `port`.
    pub fn is_path(&self) -> bool {
        matches!(self, Class::Worktree | Class::TempDir | Class::Log | Class::Evidence)
    }
}

/// One manifest line. Key order is fixed per shape (the schema file mirrors these sets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Line {
    /// Written only by init, only before any other line. `path` is a resolved, existing folder, never `/`.
    Root { ticket: String, path: String, ts_ms: u64 },
    AddPath { ticket: String, id: String, class: Class, path: String, ts_ms: u64 },
    AddProcess { ticket: String, id: String, pid: u64, pid_start_ms: u64, ts_ms: u64 },
    AddPort { ticket: String, id: String, port: u64, ts_ms: u64 },
    Remove { ticket: String, id: String, ts_ms: u64 },
}

const KEYS_ROOT: [&str; 5] = ["v", "op", "ticket", "path", "ts_ms"];
const KEYS_ADD_PATH: [&str; 7] = ["v", "op", "ticket", "id", "class", "path", "ts_ms"];
const KEYS_ADD_PROCESS: [&str; 8] = ["v", "op", "ticket", "id", "class", "pid", "pid_start_ms", "ts_ms"];
const KEYS_ADD_PORT: [&str; 7] = ["v", "op", "ticket", "id", "class", "port", "ts_ms"];
const KEYS_REMOVE: [&str; 5] = ["v", "op", "ticket", "id", "ts_ms"];

/// A path as it may be stored on a line: absolute, and holding no character that would need a JSON escape
/// (a path may legally contain spaces; it may not contain `"`, `\` or a control character).
pub fn path_ok(s: &str) -> bool {
    s.starts_with('/') && s.chars().all(|c| c >= ' ' && c != '"' && c != '\\')
}

impl Line {
    pub fn ticket(&self) -> &str {
        match self {
            Line::Root { ticket, .. }
            | Line::AddPath { ticket, .. }
            | Line::AddProcess { ticket, .. }
            | Line::AddPort { ticket, .. }
            | Line::Remove { ticket, .. } => ticket,
        }
    }

    pub fn op(&self) -> &'static str {
        match self {
            Line::Root { .. } => "root",
            Line::AddPath { .. } | Line::AddProcess { .. } | Line::AddPort { .. } => "add",
            Line::Remove { .. } => "remove",
        }
    }

    /// The schema-file branch this shape mirrors, e.g. `add_temp_dir`.
    pub fn shape_id(&self) -> String {
        match self {
            Line::Root { .. } => "root".to_string(),
            Line::AddPath { class, .. } => format!("add_{}", class.as_str()),
            Line::AddProcess { .. } => "add_process".to_string(),
            Line::AddPort { .. } => "add_port".to_string(),
            Line::Remove { .. } => "remove".to_string(),
        }
    }

    /// The exact key set of this shape, in canonical order.
    pub fn keys(&self) -> &'static [&'static str] {
        match self {
            Line::Root { .. } => &KEYS_ROOT,
            Line::AddPath { .. } => &KEYS_ADD_PATH,
            Line::AddProcess { .. } => &KEYS_ADD_PROCESS,
            Line::AddPort { .. } => &KEYS_ADD_PORT,
            Line::Remove { .. } => &KEYS_REMOVE,
        }
    }

    fn validate(&self) -> Result<(), String> {
        match self {
            Line::Root { ticket, path, .. } => {
                if !token_ok(ticket) {
                    return Err("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')".to_string());
                }
                if !path_ok(path) {
                    return Err("root path must be absolute with no character that would need escaping".to_string());
                }
                if path == "/" {
                    return Err("root may not be /".to_string());
                }
            }
            Line::AddPath { ticket, id, class, path, .. } => {
                if !token_ok(ticket) || !token_ok(id) {
                    return Err("ticket and id must be plain tokens (lowercase letters, digits, '-', '_', '.')".to_string());
                }
                if !class.is_path() {
                    return Err(format!("class {} does not take a path", class.as_str()));
                }
                if !path_ok(path) {
                    return Err("path must be absolute with no character that would need escaping".to_string());
                }
            }
            Line::AddProcess { ticket, id, pid, .. } => {
                if !token_ok(ticket) || !token_ok(id) {
                    return Err("ticket and id must be plain tokens (lowercase letters, digits, '-', '_', '.')".to_string());
                }
                if *pid <= 1 {
                    return Err("pid must be an integer > 1 (signalling 0, 1 or a negative would hit a group or init)".to_string());
                }
            }
            Line::AddPort { ticket, id, port, .. } => {
                if !token_ok(ticket) || !token_ok(id) {
                    return Err("ticket and id must be plain tokens (lowercase letters, digits, '-', '_', '.')".to_string());
                }
                if *port < 1 || *port > 65535 {
                    return Err("port must be an integer in 1..65535".to_string());
                }
            }
            Line::Remove { ticket, id, .. } => {
                if !token_ok(ticket) || !token_ok(id) {
                    return Err("ticket and id must be plain tokens (lowercase letters, digits, '-', '_', '.')".to_string());
                }
            }
        }
        Ok(())
    }

    /// Canonical line including the trailing newline. Refuses an invalid line and a line over the cap.
    pub fn encode(&self) -> Result<String, ManifestError> {
        self.validate().map_err(ManifestError::Invalid)?;
        let line = match self {
            Line::Root { ticket, path, ts_ms } => {
                format!("{{\"v\":{},\"op\":\"root\",\"ticket\":\"{ticket}\",\"path\":\"{path}\",\"ts_ms\":{ts_ms}}}\n", SCHEMA_VERSION)
            }
            Line::AddPath { ticket, id, class, path, ts_ms } => {
                format!(
                    "{{\"v\":{},\"op\":\"add\",\"ticket\":\"{ticket}\",\"id\":\"{id}\",\"class\":\"{}\",\"path\":\"{path}\",\"ts_ms\":{ts_ms}}}\n",
                    SCHEMA_VERSION,
                    class.as_str()
                )
            }
            Line::AddProcess { ticket, id, pid, pid_start_ms, ts_ms } => {
                format!(
                    "{{\"v\":{},\"op\":\"add\",\"ticket\":\"{ticket}\",\"id\":\"{id}\",\"class\":\"process\",\"pid\":{pid},\"pid_start_ms\":{pid_start_ms},\"ts_ms\":{ts_ms}}}\n",
                    SCHEMA_VERSION
                )
            }
            Line::AddPort { ticket, id, port, ts_ms } => {
                format!(
                    "{{\"v\":{},\"op\":\"add\",\"ticket\":\"{ticket}\",\"id\":\"{id}\",\"class\":\"port\",\"port\":{port},\"ts_ms\":{ts_ms}}}\n",
                    SCHEMA_VERSION
                )
            }
            Line::Remove { ticket, id, ts_ms } => {
                format!("{{\"v\":{},\"op\":\"remove\",\"ticket\":\"{ticket}\",\"id\":\"{id}\",\"ts_ms\":{ts_ms}}}\n", SCHEMA_VERSION)
            }
        };
        if line.len() > LINE_CAP {
            return Err(ManifestError::TooLong(line.len()));
        }
        Ok(line)
    }

    /// Strict decode of one line WITHOUT its newline: `v` first (a refusal names `v`), then `op`, then the exact key set of
    /// the shape `op` and `class` pick, every key present, no other key, no duplicate, right types, then `validate`.
    pub fn decode(line: &str) -> Result<Line, String> {
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
        let op = match get("op") {
            None => return Err("op missing".to_string()),
            Some(Val::Str(s)) if s == "root" || s == "add" || s == "remove" => s.as_str(),
            Some(Val::Str(s)) => return Err(format!("unknown op {s:?}")),
            Some(_) => return Err("op has the wrong type".to_string()),
        };
        let keys: &[&str] = if op == "add" {
            match get("class") {
                None => return Err("class missing".to_string()),
                Some(Val::Str(s)) => match s.as_str() {
                    "process" => &KEYS_ADD_PROCESS,
                    "port" => &KEYS_ADD_PORT,
                    c if Class::parse(c).map(|k| k.is_path()).unwrap_or(false) => &KEYS_ADD_PATH,
                    c => return Err(format!("unknown class {c:?}")),
                },
                Some(_) => return Err("class has the wrong type".to_string()),
            }
        } else if op == "root" {
            &KEYS_ROOT
        } else {
            &KEYS_REMOVE
        };
        for (k, _) in &pairs {
            if !keys.contains(&k.as_str()) {
                return Err(format!("unexpected key {k:?}"));
            }
        }
        for k in keys {
            if get(k).is_none() {
                return Err(format!("{k} missing"));
            }
        }
        let text = |k: &str| match get(k) {
            Some(Val::Str(s)) => Ok(s.clone()),
            _ => Err(format!("{k} has the wrong type")),
        };
        let num = |k: &str| match get(k) {
            Some(Val::Num(n)) => Ok(*n),
            _ => Err(format!("{k} has the wrong type")),
        };
        let ticket = text("ticket")?;
        let ts_ms = num("ts_ms")?;
        let line = match op {
            "root" => Line::Root { ticket, path: text("path")?, ts_ms },
            "add" => {
                let id = text("id")?;
                match get("class").and_then(|v| match v {
                    Val::Str(s) => Class::parse(s),
                    _ => None,
                }) {
                    Some(c) if c.is_path() => Line::AddPath { ticket, id, class: c, path: text("path")?, ts_ms },
                    Some(Class::Process) => Line::AddProcess {
                        ticket,
                        id,
                        pid: num("pid")?,
                        pid_start_ms: num("pid_start_ms")?,
                        ts_ms,
                    },
                    Some(Class::Port) => Line::AddPort { ticket, id, port: num("port")?, ts_ms },
                    _ => unreachable!("class was checked above"),
                }
            }
            _ => Line::Remove { ticket, id: text("id")?, ts_ms },
        };
        line.validate().map_err(|e| format!("line refused: {e}"))?;
        Ok(line)
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

    /// A string with no escapes: a stored path or token never needs one, so a backslash is refused outright.
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

// ---------------------------------------------------------------- state

/// What an entry points at: a stored resolved path, a pid (with its OS start time, so a reused pid can be told apart), or a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Path(String),
    Process { pid: u64, pid_start_ms: u64 },
    Port(u64),
}

/// One live entry, in add order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub class: Class,
    pub target: Target,
}

/// The current state: the roots plus every `add` whose `id` has no later `remove`. Roots are not entries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct State {
    pub roots: Vec<String>,
    pub live: Vec<Entry>,
}

/// What a read found. `roots` and `live` are filled only when the file is clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestRead {
    pub lines: Vec<Line>,
    pub roots: Vec<String>,
    pub live: Vec<Entry>,
    pub torn_tail: Option<TornTail>,
    pub corrupt: Option<LineError>,
}

impl ManifestRead {
    pub fn is_clean(&self) -> bool {
        self.torn_tail.is_none() && self.corrupt.is_none()
    }
}

/// Walk the lines and fold them into a state, or name the line that breaks an invariant. A file that breaks one was not
/// written by this module's writer (which refuses all of these at write time), so a read reports it as corruption: fail closed.
fn build_state(numbered: &[(usize, Line)]) -> Result<State, (usize, String)> {
    let mut st = State::default();
    let mut seen_op = false;
    for (n, l) in numbered {
        match l {
            Line::Root { path, .. } => {
                if seen_op {
                    return Err((*n, "second init: a root line after another operation".to_string()));
                }
                st.roots.push(path.clone());
            }
            Line::AddPath { ticket: _, id, class, path, .. } => {
                if st.roots.is_empty() {
                    return Err((*n, "add before init".to_string()));
                }
                // Stored paths are resolved at write time and fenced; one outside every root is a fence break
                // by a writer this module did not produce: corruption on read, fail closed.
                if !inside_any_root(Path::new(path.as_str()), &st.roots) {
                    return Err((*n, format!("path {path} lies outside every root")));
                }
                if st.live.iter().any(|e| e.id == *id) {
                    return Err((*n, format!("duplicate live id {id:?}")));
                }
                if st.live.iter().any(|e| matches!(&e.target, Target::Path(p) if p == path)) {
                    return Err((*n, format!("the same path is listed twice live: {path}")));
                }
                st.live.push(Entry { id: id.clone(), class: *class, target: Target::Path(path.clone()) });
            }
            Line::AddProcess { ticket: _, id, pid, pid_start_ms, .. } => {
                if st.roots.is_empty() {
                    return Err((*n, "add before init".to_string()));
                }
                if st.live.iter().any(|e| e.id == *id) {
                    return Err((*n, format!("duplicate live id {id:?}")));
                }
                if st.live.iter().any(|e| matches!(&e.target, Target::Process { pid: p, .. } if p == pid)) {
                    return Err((*n, format!("the same pid is listed twice live: {pid}")));
                }
                st.live.push(Entry {
                    id: id.clone(),
                    class: Class::Process,
                    target: Target::Process { pid: *pid, pid_start_ms: *pid_start_ms },
                });
            }
            Line::AddPort { ticket: _, id, port, .. } => {
                if st.roots.is_empty() {
                    return Err((*n, "add before init".to_string()));
                }
                if st.live.iter().any(|e| e.id == *id) {
                    return Err((*n, format!("duplicate live id {id:?}")));
                }
                if st.live.iter().any(|e| matches!(&e.target, Target::Port(p) if p == port)) {
                    return Err((*n, format!("the same port is listed twice live: {port}")));
                }
                st.live.push(Entry { id: id.clone(), class: Class::Port, target: Target::Port(*port) });
            }
            Line::Remove { ticket: _, id, .. } => {
                if st.roots.is_empty() {
                    return Err((*n, "remove before init".to_string()));
                }
                let Some(pos) = st.live.iter().position(|e| e.id == *id) else {
                    return Err((*n, format!("remove of an id that is not live: {id}")));
                };
                st.live.remove(pos);
            }
        }
        if !matches!(l, Line::Root { .. }) {
            seen_op = true;
        }
    }
    Ok(st)
}

/// Parse manifest bytes for `ticket`: every complete line must be a valid line OF that ticket (a differing `ticket` is
/// corruption). A final fragment with no newline is torn and never parsed, even when valid JSON. Corruption wins over a
/// torn tail: only the corrupt line is reported. When the bytes are clean the state is folded in as well; a line that breaks
/// a state invariant (an add before init, a duplicate live id, a remove of a dead id, ...) is reported as corruption.
pub fn parse_manifest(bytes: &[u8], ticket: &str) -> ManifestRead {
    let mut out = ManifestRead { lines: Vec::new(), roots: Vec::new(), live: Vec::new(), torn_tail: None, corrupt: None };
    if bytes.len() > FILE_CAP {
        out.corrupt = Some(LineError { line: 1, msg: format!("the file is {} bytes, over the {}-byte (1 MiB) cap", bytes.len(), FILE_CAP) });
        return out;
    }
    let mut numbered: Vec<(usize, Line)> = Vec::new();
    let mut start = 0usize;
    let mut line_no = 0usize;
    while let Some(off) = bytes[start..].iter().position(|b| *b == b'\n') {
        line_no += 1;
        let raw = &bytes[start..start + off];
        start += off + 1;
        let parsed = match std::str::from_utf8(raw) {
            Ok(s) => Line::decode(s),
            Err(_) => Err("invalid utf-8".to_string()),
        };
        match parsed {
            Ok(l) if l.ticket() == ticket => numbered.push((line_no, l)),
            Ok(l) => {
                out.lines = numbered.iter().map(|(_, l)| l.clone()).collect();
                out.corrupt = Some(LineError { line: line_no, msg: format!("ticket {} differs from the file's {}", l.ticket(), ticket) });
                return out;
            }
            Err(msg) => {
                out.lines = numbered.iter().map(|(_, l)| l.clone()).collect();
                out.corrupt = Some(LineError { line: line_no, msg });
                return out;
            }
        }
    }
    if start < bytes.len() {
        out.torn_tail = Some(TornTail { line: line_no + 1, bytes: bytes.len() - start });
    }
    out.lines = numbered.iter().map(|(_, l)| l.clone()).collect();
    match build_state(&numbered) {
        Ok(st) => {
            // N-a: `roots` and `live` are filled only when the file is clean — a torn tail carries no state,
            // so a later listing cannot print entries from a torn file by mistake.
            if out.torn_tail.is_none() {
                out.roots = st.roots;
                out.live = st.live;
            }
        }
        Err((line, msg)) => {
            out.corrupt = Some(LineError { line, msg });
            out.torn_tail = None; // corruption wins over a torn tail
        }
    }
    out
}

/// The manifest file of `ticket` under `dir`: `<dir>/<ticket>.jsonl`.
pub fn manifest_file(dir: &Path, ticket: &str) -> PathBuf {
    dir.join(format!("{ticket}.jsonl"))
}

/// Read a ticket's manifest. A missing or unreadable file is an `Err` (fail closed), never an empty manifest.
pub fn read_manifest(dir: &Path, ticket: &str) -> Result<ManifestRead, String> {
    let file = manifest_file(dir, ticket);
    let bytes = fs::read(&file).map_err(|e| format!("cannot read manifest {}: {}", file.display(), e.kind()))?;
    Ok(parse_manifest(&bytes, ticket))
}

// ---------------------------------------------------------------- paths and the fence

/// Resolve `p` the way entries are stored: an existing path canonicalizes; a missing one resolves through its nearest
/// existing ancestor (symlinks and `..` resolved on the part that exists), keeping the missing tail as-is.
///
/// The missing tail may hold plain names only. A `..` or `.` there is REFUSED, never dropped or folded: dropping one
/// stores a different path from the one the caller will create (so the caller's folder could land outside every root),
/// and folding it lexically is wrong across symlinks. The raw text is scanned, because `Path` itself hides interior `.`.
pub fn resolve_for_manifest(p: &Path) -> Result<PathBuf, String> {
    if let Ok(c) = p.canonicalize() {
        return Ok(c);
    }
    let text = p.to_str().ok_or_else(|| format!("path {} is not utf-8", p.display()))?;
    let mut existing = PathBuf::new();
    let mut tail: Vec<&str> = Vec::new();
    for (i, seg) in text.split('/').enumerate() {
        if i == 0 && seg.is_empty() {
            existing.push("/"); // an absolute path starts at the root
            continue;
        }
        if seg.is_empty() {
            continue; // a doubled or trailing '/' names nothing
        }
        if !tail.is_empty() {
            if seg == "." || seg == ".." {
                return Err(format!(
                    "path {text} has a {seg:?} component after a part that does not exist yet; use a plain name (it could not be fenced faithfully)"
                ));
            }
            tail.push(seg);
            continue;
        }
        let next = existing.join(seg);
        if next.exists() {
            existing = next;
        } else if seg == "." || seg == ".." {
            // the OS could not resolve this prefix (an earlier part is missing or unreadable): fail closed
            return Err(format!("path {text} does not resolve"));
        } else {
            tail.push(seg);
        }
    }
    if existing.as_os_str().is_empty() {
        return Err(format!("path {text} does not resolve"));
    }
    let mut resolved = existing.canonicalize().map_err(|_| format!("path {text} does not resolve"))?;
    for name in &tail {
        resolved.push(name);
    }
    Ok(resolved)
}

/// The fence: `p` (already resolved) lies inside one of the roots (equal counts as inside).
fn inside_any_root(p: &Path, roots: &[String]) -> bool {
    roots.iter().any(|r| p.starts_with(Path::new(r.as_str())))
}

/// True when `dir` (which may not exist yet) would, once created, resolve inside one of `roots`: its nearest existing
/// ancestor already sits inside a root (a root always exists, so the nearest existing ancestor is at or below any root that
/// contains the final path). Unreadable means refuse: fail closed.
fn dir_inside_any_root(dir: &Path, roots: &[PathBuf]) -> bool {
    // A relative dir is answered for the current folder, so join it on before walking up; unreadable means refuse.
    let mut cur = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        match std::env::current_dir() {
            Ok(cwd) => cwd.join(dir),
            Err(_) => return true,
        }
    };
    while !cur.exists() {
        // A `..` in the missing part cannot be walked past honestly (`parent()` would drop it, so the answer would be
        // given for a different folder than the one that gets created): refuse, fail closed.
        if cur.file_name().is_none() {
            return true;
        }
        match cur.parent() {
            Some(p) => cur = p.to_path_buf(),
            None => break,
        }
    }
    match cur.canonicalize() {
        Ok(c) => roots.iter().any(|r| c.starts_with(r)),
        Err(_) => true,
    }
}

// ---------------------------------------------------------------- ownership

/// An ownership query. A path is owned when it equals, or lies inside, a live path entry (resolve it first). A pid or port is
/// owned when a live entry lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Query<'a> {
    Path(&'a Path),
    Pid(u64),
    Port(u64),
}

/// Answer an ownership query. Path comparison is component-wise on the stored resolved paths. A read that is not
/// clean (torn tail or corruption) owns NOTHING: the query itself fails closed, so a caller cannot forget to.
pub fn owns(read: &ManifestRead, q: Query<'_>) -> bool {
    if !read.is_clean() {
        return false;
    }
    match q {
        Query::Path(p) => read.live.iter().any(|e| match &e.target {
            Target::Path(s) => p.starts_with(Path::new(s.as_str())),
            _ => false,
        }),
        Query::Pid(pid) => read.live.iter().any(|e| matches!(&e.target, Target::Process { pid: p, .. } if *p == pid)),
        Query::Port(port) => read.live.iter().any(|e| matches!(&e.target, Target::Port(p) if *p == port)),
    }
}

// ---------------------------------------------------------------- operations

/// One operation to apply. `init` writes one root line per root; the rest append one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op<'a> {
    Init { roots: &'a [PathBuf] },
    AddPath { id: &'a str, class: Class, path: &'a Path },
    AddProcess { id: &'a str, pid: u64, pid_start_ms: u64 },
    AddPort { id: &'a str, port: u64 },
    Remove { id: &'a str },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestError {
    /// The operation is refused; nothing was written.
    Invalid(String),
    TooLong(usize),
    /// The file is damaged (torn tail or corruption); a write refuses and changes nothing.
    Damaged { line: Option<usize>, msg: String },
    Io(String),
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ManifestError::Invalid(m) => write!(f, "manifest refused: {m}"),
            ManifestError::TooLong(n) => write!(f, "manifest refused: line is {n} bytes, over the {LINE_CAP}-byte cap"),
            ManifestError::Damaged { line: None, msg } => write!(f, "manifest is damaged (torn tail): {msg}; nothing was written"),
            ManifestError::Damaged { line: Some(n), msg } => write!(f, "manifest is corrupt at line {n}: {msg}; nothing was written"),
            ManifestError::Io(m) => write!(f, "manifest write failed: {m}"),
        }
    }
}

impl std::error::Error for ManifestError {}

#[cfg(test)]
thread_local! {
    static SYNC_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn sync(f: &File) -> std::io::Result<()> {
    #[cfg(test)]
    SYNC_CALLS.with(|c| c.set(c.get() + 1));
    f.sync_all()
}

/// Open the manifest file with the exclusive lock taken FIRST: read, validate and append are ONE critical
/// section, so two writers cannot both validate against the same stale read and both append (B5). `creating`
/// is set only by init, the one operation allowed to make the file; every other op opens an existing file,
/// so a missing manifest is refused having created nothing.
fn open_locked(file: &Path, ticket: &str, creating: bool) -> Result<File, ManifestError> {
    let io = |e: std::io::Error| ManifestError::Io(e.kind().to_string());
    let f = OpenOptions::new()
        .read(true)
        .append(true)
        .create(creating)
        .open(file)
        .map_err(|e| match (!creating, e.kind()) {
            (true, std::io::ErrorKind::NotFound) => {
                ManifestError::Invalid(format!("no manifest for ticket {ticket}; run init first"))
            }
            _ => io(e),
        })?;
    f.lock().map_err(|e| ManifestError::Io(e.to_string()))?;
    Ok(f)
}

/// Read the whole file through the locked handle. `parse_manifest`'s final-fragment rule subsumes the old
/// last-byte check: bytes that do not end in `\n` are a torn tail and refuse the write.
fn read_locked(f: &mut File) -> Result<Vec<u8>, ManifestError> {
    let io = |e: std::io::Error| ManifestError::Io(e.kind().to_string());
    let mut bytes = Vec::new();
    f.seek(SeekFrom::Start(0)).map_err(io)?;
    f.read_to_end(&mut bytes).map_err(io)?;
    Ok(bytes)
}

/// The write-side damage rules, applied to bytes read under the lock: a corrupt line or a torn tail refuses
/// the write unchanged, and a clean read with no roots is a file that exists but holds no init (an init that
/// crashed between create and write leaves a 0-byte file): appending would corrupt it for good, so refuse.
fn validate_for_write(bytes: &[u8], ticket: &str) -> Result<ManifestRead, ManifestError> {
    let r = parse_manifest(bytes, ticket);
    if let Some(c) = &r.corrupt {
        return Err(ManifestError::Damaged { line: Some(c.line), msg: c.msg.clone() });
    }
    if let Some(t) = &r.torn_tail {
        return Err(ManifestError::Damaged { line: None, msg: format!("torn tail at line {} ({} bytes)", t.line, t.bytes) });
    }
    if r.roots.is_empty() {
        return Err(ManifestError::Invalid(format!("no manifest for ticket {ticket}; run init first")));
    }
    Ok(r)
}

/// Append one encoded line through the already-locked handle: ONE write of the whole line, then `sync_all`.
/// Success means the line is on disk.
fn append_locked(f: &mut File, line: &Line) -> Result<(), ManifestError> {
    let text = line.encode()?;
    write_text_locked(f, &text)
}

/// Write one already-encoded line and sync it.
fn write_text_locked(f: &mut File, text: &str) -> Result<(), ManifestError> {
    let io = |e: std::io::Error| ManifestError::Io(e.kind().to_string());
    let n = f.write(text.as_bytes()).map_err(io)?;
    if n != text.len() {
        return Err(ManifestError::Io("short write".to_string()));
    }
    sync(f).map_err(io)?;
    Ok(())
}

/// The write half of init, for roots already validated and resolved. Every root line is encoded BEFORE anything is
/// created or written (N-f), so an over-long or invalid later root returns its error with nothing on disk.
fn write_init(dir: &Path, ticket: &str, resolved: &[PathBuf], ts_ms: u64) -> Result<(), ManifestError> {
    let lines: Vec<String> = resolved
        .iter()
        .map(|r| Line::Root { ticket: ticket.to_string(), path: r.to_string_lossy().into_owned(), ts_ms }.encode())
        .collect::<Result<_, _>>()?;
    fs::create_dir_all(dir).map_err(|e| ManifestError::Io(e.kind().to_string()))?;
    // ONE critical section (B5): the lock is taken first, then the empty-file check runs on bytes read
    // through the locked handle, so two concurrent inits of one fresh manifest leave exactly one.
    let mut f = open_locked(&manifest_file(dir, ticket), ticket, true)?;
    if !read_locked(&mut f)?.is_empty() {
        return Err(ManifestError::Invalid("second init: the manifest already holds lines".to_string()));
    }
    for text in &lines {
        write_text_locked(&mut f, text)?;
    }
    Ok(())
}

/// Validate one root for init: absolute, existing, a directory, not `/`; return it resolved.
fn resolve_root(root: &Path) -> Result<PathBuf, ManifestError> {
    let bad = |m: &str| ManifestError::Invalid(m.to_string());
    if !root.is_absolute() {
        return Err(bad(&format!("root {} is not absolute", root.display())));
    }
    let resolved = root.canonicalize().map_err(|_| bad(&format!("root {} does not resolve", root.display())))?;
    if resolved == Path::new("/") {
        return Err(bad("root may not be /"));
    }
    if !resolved.is_dir() {
        return Err(bad(&format!("root {} is not a directory", root.display())));
    }
    let s = resolved.to_str().ok_or_else(|| bad("root resolves to a path that is not utf-8"))?;
    if !path_ok(s) {
        return Err(bad("root resolves to a path holding a character that would need escaping"));
    }
    Ok(resolved)
}

/// Apply one operation. Every refusal leaves the file byte-identical; success means the line(s) are on disk.
pub fn apply(dir: &Path, ticket: &str, op: &Op, ts_ms: u64) -> Result<(), ManifestError> {
    if !token_ok(ticket) {
        return Err(ManifestError::Invalid("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')".to_string()));
    }
    match op {
        Op::Init { roots } => {
            if roots.is_empty() {
                return Err(ManifestError::Invalid("init needs at least one --root".to_string()));
            }
            let resolved: Vec<PathBuf> = roots.iter().map(|r| resolve_root(r)).collect::<Result<_, _>>()?;
            // The record must survive removal of the roots, so the manifest file may not live inside one of them.
            if dir_inside_any_root(dir, &resolved) {
                return Err(ManifestError::Invalid(
                    "the manifest dir resolves inside one of the ticket's roots; the record must survive removal of the roots".to_string(),
                ));
            }
            write_init(dir, ticket, &resolved, ts_ms)
        }
        Op::AddPath { id, class, path } => {
            if !token_ok(id) {
                return Err(ManifestError::Invalid("id must be a plain token (lowercase letters, digits, '-', '_', '.')".to_string()));
            }
            if !class.is_path() {
                return Err(ManifestError::Invalid(format!("class {} does not take a path", class.as_str())));
            }
            if !path.is_absolute() {
                return Err(ManifestError::Invalid(format!("path {} is not absolute", path.display())));
            }
            let orig = path.to_str().ok_or_else(|| ManifestError::Invalid("path is not utf-8".to_string()))?;
            if !path_ok(orig) {
                return Err(ManifestError::Invalid("path holds a character that would need escaping, or is not absolute".to_string()));
            }
            let resolved = resolve_for_manifest(path).map_err(ManifestError::Invalid)?;
            let stored = resolved.to_str().ok_or_else(|| ManifestError::Invalid("the resolved path is not utf-8".to_string()))?;
            if !path_ok(stored) {
                return Err(ManifestError::Invalid("the resolved path holds a character that would need escaping".to_string()));
            }
            // every file-state check runs inside the critical section (B5)
            let mut f = open_locked(&manifest_file(dir, ticket), ticket, false)?;
            let r = validate_for_write(&read_locked(&mut f)?, ticket)?;
            if r.live.iter().any(|e| e.id == *id) {
                return Err(ManifestError::Invalid(format!("duplicate live id {id:?}")));
            }
            if !inside_any_root(&resolved, &r.roots) {
                return Err(ManifestError::Invalid(format!("path {} resolves outside every root", stored)));
            }
            if r.live.iter().any(|e| matches!(&e.target, Target::Path(p) if p == stored)) {
                return Err(ManifestError::Invalid(format!("the same path is listed twice live: {stored}")));
            }
            append_locked(&mut f, &Line::AddPath {
                ticket: ticket.to_string(),
                id: id.to_string(),
                class: *class,
                path: stored.to_string(),
                ts_ms,
            })
        }
        Op::AddProcess { id, pid, pid_start_ms } => {
            if !token_ok(id) {
                return Err(ManifestError::Invalid("id must be a plain token (lowercase letters, digits, '-', '_', '.')".to_string()));
            }
            if *pid <= 1 {
                return Err(ManifestError::Invalid("pid must be an integer > 1 (signalling 0, 1 or a negative would hit a group or init)".to_string()));
            }
            let mut f = open_locked(&manifest_file(dir, ticket), ticket, false)?;
            let r = validate_for_write(&read_locked(&mut f)?, ticket)?;
            if r.live.iter().any(|e| e.id == *id) {
                return Err(ManifestError::Invalid(format!("duplicate live id {id:?}")));
            }
            if r.live.iter().any(|e| matches!(&e.target, Target::Process { pid: p, .. } if p == pid)) {
                return Err(ManifestError::Invalid(format!("the same pid is listed twice live: {pid}")));
            }
            append_locked(&mut f, &Line::AddProcess {
                ticket: ticket.to_string(),
                id: id.to_string(),
                pid: *pid,
                pid_start_ms: *pid_start_ms,
                ts_ms,
            })
        }
        Op::AddPort { id, port } => {
            if !token_ok(id) {
                return Err(ManifestError::Invalid("id must be a plain token (lowercase letters, digits, '-', '_', '.')".to_string()));
            }
            if *port < 1 || *port > 65535 {
                return Err(ManifestError::Invalid("port must be an integer in 1..65535".to_string()));
            }
            let mut f = open_locked(&manifest_file(dir, ticket), ticket, false)?;
            let r = validate_for_write(&read_locked(&mut f)?, ticket)?;
            if r.live.iter().any(|e| e.id == *id) {
                return Err(ManifestError::Invalid(format!("duplicate live id {id:?}")));
            }
            if r.live.iter().any(|e| matches!(&e.target, Target::Port(p) if p == port)) {
                return Err(ManifestError::Invalid(format!("the same port is listed twice live: {port}")));
            }
            append_locked(&mut f, &Line::AddPort {
                ticket: ticket.to_string(),
                id: id.to_string(),
                port: *port,
                ts_ms,
            })
        }
        Op::Remove { id } => {
            if !token_ok(id) {
                return Err(ManifestError::Invalid("id must be a plain token (lowercase letters, digits, '-', '_', '.')".to_string()));
            }
            let mut f = open_locked(&manifest_file(dir, ticket), ticket, false)?;
            let r = validate_for_write(&read_locked(&mut f)?, ticket)?;
            if !r.live.iter().any(|e| e.id == *id) {
                return Err(ManifestError::Invalid(format!("remove of an id that is not live: {id}")));
            }
            append_locked(&mut f, &Line::Remove { ticket: ticket.to_string(), id: id.to_string(), ts_ms })
        }
    }
}

// ---------------------------------------------------------------- pid identity

/// The OS start time of `pid` in ms since the epoch, read std-only: on Linux from `/proc` (`stat` field 22 plus the boot
/// time in `/proc/stat`; `CLK_TCK` is the userspace default 100); elsewhere from the system `ps` (`etime`, the elapsed
/// time, subtracted from the wall clock). A pid whose start time cannot be read is refused.
pub fn process_start_ms(pid: u64) -> Result<u64, String> {
    if pid <= 1 {
        return Err(format!("pid {pid} is not a process start time that can be read"));
    }

    #[cfg(target_os = "linux")]
    {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|_| format!("cannot read /proc/{pid}/stat"))?;
        let after_paren = stat.rsplit_once(')').map(|(_, r)| r).ok_or("stat has no command field")?;
        let fields: Vec<&str> = after_paren.split_whitespace().collect();
        // fields[0] is stat field 3 (state); starttime is field 22, so index 22 - 3 = 19.
        let ticks: u64 = fields.get(19).ok_or("stat has no starttime field")?.parse().map_err(|_| "starttime is not an integer")?;
        let btime = fs::read_to_string("/proc/stat")
            .map_err(|_| "cannot read /proc/stat".to_string())?
            .lines()
            .find_map(|l| l.strip_prefix("btime "))
            .ok_or("no btime in /proc/stat")?
            .trim()
            .parse::<u64>()
            .map_err(|_| "btime is not an integer")?;
        // CLK_TCK is 100 on every mainstream Linux userspace; the value is identity metadata, not a clock.
        Ok(btime * 1000 + ticks * 1000 / 100)
    }

    #[cfg(not(target_os = "linux"))]
    {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "clock is before the epoch".to_string())?
            .as_millis() as u64;
        let out = std::process::Command::new("ps")
            .arg("-o")
            .arg("etime=")
            .arg("-p")
            .arg(pid.to_string())
            .output()
            .map_err(|e| format!("cannot run ps: {e}"))?;
        if !out.status.success() {
            return Err(format!("ps does not know pid {pid}"));
        }
        let et = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if et.is_empty() {
            return Err(format!("ps returned no start time for pid {pid}"));
        }
        // `etime` prints [[dd-]hh:]mm:ss, sometimes with a fractional second; only whole seconds are knowable.
        let main = et.split('.').next().unwrap_or(&et);
        let (days, time) = match main.split_once('-') {
            Some((d, r)) => (d.trim().parse::<u64>().map_err(|_| format!("cannot parse ps etime {et:?}"))?, r),
            None => (0, main),
        };
        let parts: Vec<u64> = time
            .split(':')
            .map(|p| p.trim().parse::<u64>().map_err(|_| format!("cannot parse ps etime {et:?}")))
            .collect::<Result<_, _>>()?;
        let secs = match parts.as_slice() {
            [mm, ss] => days * 86400 + mm * 60 + ss,
            [hh, mm, ss] => days * 86400 + hh * 3600 + mm * 60 + ss,
            _ => return Err(format!("cannot parse ps etime {et:?}")),
        };
        now.checked_sub(secs * 1000).ok_or_else(|| "the clock is before the process start".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// N-f: a later root whose line is over the cap makes init refuse with `TooLong` and leaves NOTHING on disk (no
    /// manifest dir, no file, no earlier root line). The roots here are synthetic: `write_init` takes roots already
    /// validated, so no real folder 4 KiB deep is needed.
    #[test]
    fn an_over_long_later_root_line_refuses_init_with_nothing_written() {
        let base = std::env::temp_dir().join(format!("manifest-nf-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let dir = base.join("man");
        let long = PathBuf::from(format!("/{}", "r".repeat(LINE_CAP)));
        let roots = [PathBuf::from("/short-a"), long, PathBuf::from("/short-b")];
        let err = write_init(&dir, "t-1", &roots, 1).unwrap_err();
        assert!(matches!(err, ManifestError::TooLong(_)), "{err}");
        assert!(!dir.exists(), "nothing was created");
        // twin: three short roots write all three lines
        write_init(&dir, "t-1", &[PathBuf::from("/short-a"), PathBuf::from("/short-b"), PathBuf::from("/short-c")], 1).unwrap();
        let text = fs::read_to_string(manifest_file(&dir, "t-1")).unwrap();
        assert_eq!(text.lines().count(), 3);
        let _ = fs::remove_dir_all(&base);
    }
}
