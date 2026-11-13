//! Session launcher core (P1B-18, S-1.5 parts 1-3). std only, unix only.
//!
//! `harness session launch` writes ONE `launch` record row first (fail closed: no row, no start), then
//! starts a named background session with the brief's exact bytes as one argv element, then maps the
//! host's SHORT id to the FULL uuid by reading the host's own listing and appends a `launch_ids` row
//! keyed by the same `run_id`. A record never carries a uuid the host did not print. If the id cannot
//! be mapped the session is stopped and a `launch_failed` row is appended.
//!
//! Exit codes: 0 launched; 1 the name is refused (P1B-16, `--new` mode); 2 usage, validation or the
//! launch record could not be written (nothing was started); 3 the host failed to start (non-zero,
//! no short id, E2BIG, timeout); 4 the id could not be mapped and the session was stopped; 5 the id
//! could not be mapped and the stop FAILED (stop it by hand); 6 a signer launch was refused because its
//! brief names a builder-state entry from the config (a `launch_refused` row is written, nothing started).
use crate::events::{self, Field, Kind, Record};
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Largest brief, in bytes. Linux allows one argv element of at most 131072 bytes including its NUL,
/// so this leaves a wide margin; checked before any write.
pub const BRIEF_MAX_BYTES: usize = 102400;
/// Default records file, relative to the harness process cwd (never `--cwd`).
pub const DEFAULT_RECORDS: &str = ".harness/launch-records.jsonl";
/// Roles whose launch is always checked against the config's `builder_state` list (S-1.5 part 4).
const ALWAYS_CHECKED_ROLES: [&str; 1] = ["gov"];
/// Roles a sign-off may run as (S-1.6); `--sign-off` is refused for any other role.
pub(crate) const SIGN_OFF_ROLES: [&str; 2] = ["gov", "e-rvw"];
const DEFAULT_START_TIMEOUT_MS: u64 = 30000;
const DEFAULT_MAP_TIMEOUT_MS: u64 = 10000;
const MAP_POLL_MS: u64 = 250;
const AUX_TIMEOUT_MS: u64 = 10000;
const STDOUT_CAP: usize = 1 << 20;
const RECORDS_READ_CAP: u64 = 16 << 20;
/// The host's output shape (Orchestrator input O-1): keys of one `agents --json --all` entry.
const AGENT_KEY_SHORT: &str = "id";
const AGENT_KEY_FULL: &str = "sessionId";
const AGENT_KEY_NAME: &str = "name";

pub const LAUNCH_USAGE: &str = "usage: harness session launch --name <n> --brief <file> --cwd <dir> --host <abs-path> [--records <file>] [--config <file>] [--sign-off] [--start-timeout-ms <n>] [--map-timeout-ms <n>] | session launches [--records <file>]";

// ------------------------------------------------------------------------------------ sha256

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01,
    0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc,
    0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08,
    0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// FIPS 180-4 sha256 of `data`, as 64 lowercase hex characters.
fn sha256_hex(data: &[u8]) -> String {
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, c) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7].wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            let t2 = s0.wrapping_add(maj);
            v[7] = v[6];
            v[6] = v[5];
            v[5] = v[4];
            v[4] = v[3].wrapping_add(t1);
            v[3] = v[2];
            v[2] = v[1];
            v[1] = v[0];
            v[0] = t1.wrapping_add(t2);
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

// ------------------------------------------------------------------------------------ mini JSON

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum J {
    Str(String),
    Num(u64),
    Bool(bool),
    Other,
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub(crate) fn get(&self, key: &str) -> Option<&J> {
        match self {
            J::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub(crate) fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(J::Str(s)) => Some(s),
            _ => None,
        }
    }
}

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }
    fn value(&mut self, depth: u32) -> Result<J, String> {
        if depth > 16 {
            return Err("nesting is too deep".into());
        }
        self.ws();
        match self.b.get(self.i).copied() {
            Some(b'"') => Ok(J::Str(self.string()?)),
            Some(b'[') => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b']') {
                    self.i += 1;
                    return Ok(J::Arr(out));
                }
                loop {
                    out.push(self.value(depth + 1)?);
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b']') => {
                            self.i += 1;
                            return Ok(J::Arr(out));
                        }
                        _ => return Err("expected `,` or `]`".into()),
                    }
                }
            }
            Some(b'{') => {
                self.i += 1;
                let mut out = Vec::new();
                self.ws();
                if self.b.get(self.i) == Some(&b'}') {
                    self.i += 1;
                    return Ok(J::Obj(out));
                }
                loop {
                    self.ws();
                    if self.b.get(self.i) != Some(&b'"') {
                        return Err("expected a key".into());
                    }
                    let k = self.string()?;
                    self.ws();
                    if self.b.get(self.i) != Some(&b':') {
                        return Err("expected `:`".into());
                    }
                    self.i += 1;
                    out.push((k, self.value(depth + 1)?));
                    self.ws();
                    match self.b.get(self.i) {
                        Some(b',') => self.i += 1,
                        Some(b'}') => {
                            self.i += 1;
                            return Ok(J::Obj(out));
                        }
                        _ => return Err("expected `,` or `}`".into()),
                    }
                }
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => {
                let s = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.b[s..self.i]).map_err(|_| "bad number")?;
                Ok(t.parse::<u64>().map(J::Num).unwrap_or(J::Other))
            }
            Some(_) => {
                for lit in ["true", "false", "null"] {
                    if self.b[self.i..].starts_with(lit.as_bytes()) {
                        self.i += lit.len();
                        return Ok(match lit {
                            "true" => J::Bool(true),
                            "false" => J::Bool(false),
                            _ => J::Other,
                        });
                    }
                }
                Err("unexpected character".into())
            }
            None => Err("unexpected end".into()),
        }
    }
    fn string(&mut self) -> Result<String, String> {
        self.i += 1;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).map_err(|_| "string is not utf-8".into()),
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("unterminated escape")?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'n' => '\n',
                        b't' => '\t',
                        b'r' => '\r',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'u' => {
                            let hex = self.b.get(self.i..self.i + 4).ok_or("short \\u escape")?;
                            let n = u32::from_str_radix(std::str::from_utf8(hex).map_err(|_| "bad \\u")?, 16)
                                .map_err(|_| "bad \\u")?;
                            self.i += 4;
                            char::from_u32(n).unwrap_or('\u{fffd}')
                        }
                        _ => return Err("bad escape".into()),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c if c < 0x20 => return Err("control byte in string".into()),
                c => out.push(c),
            }
        }
    }
}

pub(crate) fn parse_json(bytes: &[u8]) -> Result<J, String> {
    let mut p = P { b: bytes, i: 0 };
    let v = p.value(0)?;
    p.ws();
    if p.i != bytes.len() {
        return Err("trailing bytes after the value".into());
    }
    Ok(v)
}

fn jstr(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

// ------------------------------------------------------------------------------------ records file

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Append one compact row under an exclusive lock, refusing a torn tail; one `write`, then `sync_all`.
fn append_row(path: &Path, line: &str) -> Result<(), String> {
    let mut f = OpenOptions::new().read(true).append(true).create(true).open(path).map_err(|e| format!("open: {}", e.kind()))?;
    f.lock().map_err(|e| format!("lock: {e}"))?;
    let len = f.metadata().map_err(|e| format!("stat: {}", e.kind()))?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        f.seek(SeekFrom::Start(len - 1)).and_then(|_| f.read_exact(&mut last)).map_err(|e| format!("read: {}", e.kind()))?;
        if last[0] != b'\n' {
            return Err("the records file ends in a torn line".into());
        }
    }
    let mut data = line.as_bytes().to_vec();
    data.push(b'\n');
    f.write_all(&data).map_err(|e| format!("write: {}", e.kind()))?;
    f.sync_all().map_err(|e| format!("sync: {}", e.kind()))?;
    Ok(())
}

fn launch_row(ts: u64, run: &str, name: &str, role: &str, sha: &str, sign_off: bool) -> String {
    let mark = if sign_off { ",\"sign_off\":true" } else { "" };
    format!(
        "{{\"v\":1,\"kind\":\"launch\",\"ts_ms\":{ts},\"run_id\":{},\"name\":{},\"role\":{},\"brief_sha256\":{},\"owner\":\"orchestration\"{mark}}}",
        jstr(run),
        jstr(name),
        jstr(role),
        jstr(sha)
    )
}

fn failed_row(run: &str, stage: &str, reason: &str) -> String {
    format!(
        "{{\"v\":1,\"kind\":\"launch_failed\",\"ts_ms\":{},\"run_id\":{},\"stage\":{},\"reason\":{}}}",
        now_ms(),
        jstr(run),
        jstr(stage),
        jstr(reason)
    )
}

// ------------------------------------------------------------------------------------ reader

/// One launch, joined by `run_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub run_id: String,
    pub name: String,
    pub role: String,
    pub brief_sha256: String,
    pub ts_ms: u64,
    pub short_id: Option<String>,
    pub session_id: Option<String>,
    /// `bound`, `failed`, `pending` or `refused`.
    pub state: &'static str,
    /// True only when the `launch` row carries `"sign_off":true` (a `--sign-off` launch, #3704).
    pub sign_off: bool,
}

pub(crate) fn uuid_ok(id: &str) -> bool {
    Record::build(Kind::SessionStarted, 0, &[("session_id", Field::Str(id.to_string())), ("source", Field::Str("launch".into()))])
        .is_ok()
}

/// Read and join the launch records. A corrupt line (or a torn tail) stops the read: `Err` names the line.
pub fn read_launches(path: &Path) -> Result<Vec<Launch>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|f| f.take(RECORDS_READ_CAP + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("cannot read {}: {}", path.display(), e.kind()))?;
    if bytes.len() as u64 > RECORDS_READ_CAP {
        return Err("the records file is over the read cap".into());
    }
    let mut out: Vec<Launch> = Vec::new();
    let mut lines: Vec<&[u8]> = bytes.split(|b| *b == b'\n').collect();
    if lines.last().map(|l| l.is_empty()) == Some(true) {
        lines.pop();
    } else if !lines.is_empty() {
        return Err(format!("line {}: torn tail (no final newline)", lines.len()));
    }
    for (n, raw) in lines.iter().enumerate() {
        let n = n + 1;
        let bad = |m: &str| format!("line {n}: {m}");
        let row = parse_json(raw).map_err(|e| bad(&e))?;
        if row.get("v") != Some(&J::Num(1)) {
            return Err(bad("`v` is not 1"));
        }
        let kind = row.str("kind").ok_or_else(|| bad("no `kind`"))?;
        let run = row.str("run_id").ok_or_else(|| bad("no `run_id`"))?.to_string();
        let ts = match row.get("ts_ms") {
            Some(J::Num(t)) => *t,
            _ => return Err(bad("no `ts_ms`")),
        };
        let find = |out: &mut Vec<Launch>| out.iter().position(|l| l.run_id == run);
        match kind {
            "launch" => {
                if find(&mut out).is_some() {
                    return Err(bad("a second launch row for the same run_id"));
                }
                let f = |k: &str| row.str(k).map(str::to_string).ok_or_else(|| bad(&format!("no `{k}`")));
                let sign_off = match row.get("sign_off") {
                    None => false,
                    Some(J::Bool(true)) => true,
                    Some(_) => return Err(bad("`sign_off` is not true")),
                };
                out.push(Launch {
                    run_id: run.clone(),
                    name: f("name")?,
                    role: f("role")?,
                    brief_sha256: f("brief_sha256")?,
                    ts_ms: ts,
                    short_id: None,
                    session_id: None,
                    state: "pending",
                    sign_off,
                });
            }
            "launch_ids" => {
                let i = find(&mut out).ok_or_else(|| bad("an id row with no launch row before it"))?;
                if out[i].state == "refused" {
                    return Err(bad("an id row for a refused launch"));
                }
                let sid = row.str("session_id").ok_or_else(|| bad("no `session_id`"))?;
                if !uuid_ok(sid) {
                    return Err(bad("`session_id` is not a uuid"));
                }
                let short = row.str("short_id").ok_or_else(|| bad("no `short_id`"))?;
                out[i].short_id = Some(short.to_string());
                out[i].session_id = Some(sid.to_string());
                if out[i].state != "failed" {
                    out[i].state = "bound";
                }
            }
            "launch_failed" => {
                let i = find(&mut out).ok_or_else(|| bad("a failure row with no launch row before it"))?;
                if out[i].state == "refused" {
                    return Err(bad("a failure row for a refused launch"));
                }
                out[i].state = "failed";
            }
            "launch_refused" => {
                if find(&mut out).is_some() {
                    return Err(bad("a second row for the same run_id as a refusal"));
                }
                let f = |k: &str| row.str(k).map(str::to_string).ok_or_else(|| bad(&format!("no `{k}`")));
                row.str("matched_entry").ok_or_else(|| bad("no `matched_entry`"))?;
                out.push(Launch {
                    run_id: run.clone(),
                    name: f("name")?,
                    role: f("role")?,
                    brief_sha256: f("brief_sha256")?,
                    ts_ms: ts,
                    short_id: None,
                    session_id: None,
                    state: "refused",
                    sign_off: false,
                });
            }
            _ => return Err(bad("unknown `kind`")),
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------ host calls

pub(crate) enum RunErr {
    Spawn(String),
    Timeout,
}

pub(crate) struct HostOut {
    pub(crate) ok: bool,
    pub(crate) stdout: Vec<u8>,
}

pub(crate) fn kill_group(pid: u32) {
    for k in ["/bin/kill", "/usr/bin/kill"] {
        if Path::new(k).is_file() {
            let _ = Command::new(k).args(["-KILL", "--", &format!("-{pid}")]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            break;
        }
    }
}

/// Run the host with a bounded wait, in its own process group. On the bound: SIGKILL the group, kill and reap the child.
pub(crate) fn run_host(host: &Path, args: &[&std::ffi::OsStr], cwd: Option<&Path>, timeout_ms: u64) -> Result<HostOut, RunErr> {
    let mut c = Command::new(host);
    c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).process_group(0);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let mut child = c.spawn().map_err(|e| RunErr::Spawn(e.kind().to_string()))?;
    let mut out = child.stdout.take().expect("stdout was piped");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut keep = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = out.read(&mut buf) {
            if n == 0 {
                break;
            }
            if keep.len() < STDOUT_CAP {
                keep.extend_from_slice(&buf[..n]);
            }
        }
        let _ = tx.send(keep);
    });
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    loop {
        match child.try_wait() {
            Ok(Some(st)) => {
                // a handed-off daemon may keep the pipe open: wait briefly for EOF, never for ever
                let stdout = rx.recv_timeout(Duration::from_secs(2)).unwrap_or_default();
                return Ok(HostOut { ok: st.success(), stdout });
            }
            Ok(None) => {}
            Err(e) => return Err(RunErr::Spawn(e.kind().to_string())),
        }
        if Instant::now() >= deadline {
            kill_group(child.id());
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunErr::Timeout);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// O-1 parser (default stub contract): the short id is the first whitespace token of the last non-empty stdout line.
fn parse_short_id(stdout: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(stdout);
    let line = s.lines().rev().find(|l| !l.trim().is_empty())?;
    let tok = line.split_whitespace().next()?;
    let ok = !tok.starts_with('-') && tok.len() <= 64 && tok.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    ok.then(|| tok.to_string())
}

enum MapErr {
    Fail(String),
}

/// One poll of `<host> agents --json --all`: `Ok(Some(uuid))` bound, `Ok(None)` not listed yet, `Err` final refusal.
fn map_once(host: &Path, short: &str, name: &str) -> Result<Option<String>, MapErr> {
    let o = match run_host(host, &["agents".as_ref(), "--json".as_ref(), "--all".as_ref()], None, AUX_TIMEOUT_MS) {
        Ok(o) if o.ok => o,
        _ => return Ok(None),
    };
    let Ok(J::Arr(items)) = parse_json(&o.stdout) else { return Ok(None) };
    let hits: Vec<&J> = items.iter().filter(|e| e.str(AGENT_KEY_SHORT) == Some(short) && e.str(AGENT_KEY_NAME) == Some(name)).collect();
    match hits.len() {
        0 => Ok(None),
        1 => match hits[0].str(AGENT_KEY_FULL) {
            Some(id) if uuid_ok(id) => Ok(Some(id.to_string())),
            _ => Err(MapErr::Fail("the host's full-id field is not a uuid".into())),
        },
        n => Err(MapErr::Fail(format!("{n} listing entries match the short id and name (ambiguous)"))),
    }
}

// ------------------------------------------------------------------------------------ launch

struct Args {
    name: String,
    brief: PathBuf,
    cwd: PathBuf,
    host: PathBuf,
    records: Option<PathBuf>,
    config: Option<std::ffi::OsString>,
    start_ms: u64,
    map_ms: u64,
    sign_off: bool,
}

fn refused_row(ts: u64, run: &str, name: &str, role: &str, sha: &str, entry: &str) -> String {
    format!(
        "{{\"v\":1,\"kind\":\"launch_refused\",\"ts_ms\":{ts},\"run_id\":{},\"name\":{},\"role\":{},\"brief_sha256\":{},\"matched_entry\":{}}}",
        jstr(run),
        jstr(name),
        jstr(role),
        jstr(sha),
        jstr(entry)
    )
}

fn parse_args(a: &[String]) -> Result<Args, String> {
    let (mut name, mut brief, mut cwd, mut host, mut records, mut config, mut start, mut map) =
        (None, None, None, None, None, None, None, None);
    let mut sign_off = false;
    let mut it = a.iter();
    while let Some(f) = it.next() {
        if f == "--sign-off" {
            if sign_off {
                return Err("--sign-off was given twice".into());
            }
            sign_off = true;
            continue;
        }
        let slot: &mut Option<String> = match f.as_str() {
            "--name" => &mut name,
            "--brief" => &mut brief,
            "--cwd" => &mut cwd,
            "--host" => &mut host,
            "--records" => &mut records,
            "--config" => &mut config,
            "--start-timeout-ms" => &mut start,
            "--map-timeout-ms" => &mut map,
            _ => return Err(format!("unknown argument `{f}`")),
        };
        let v = it.next().ok_or_else(|| format!("{f} is missing its value"))?;
        if slot.replace(v.clone()).is_some() {
            return Err(format!("{f} was given twice"));
        }
    }
    let need = |o: Option<String>, n: &str| o.ok_or_else(|| format!("{n} is required"));
    let ms = |o: Option<String>, d: u64, n: &str| match o {
        None => Ok(d),
        Some(s) => s.parse::<u64>().ok().filter(|v| *v > 0 && *v <= 3_600_000).ok_or_else(|| format!("{n} must be 1..=3600000")),
    };
    Ok(Args {
        name: need(name, "--name")?,
        brief: PathBuf::from(need(brief, "--brief")?),
        cwd: PathBuf::from(need(cwd, "--cwd")?),
        host: PathBuf::from(need(host, "--host")?),
        records: records.map(PathBuf::from),
        config: config.map(std::ffi::OsString::from),
        start_ms: ms(start, DEFAULT_START_TIMEOUT_MS, "--start-timeout-ms")?,
        map_ms: ms(map, DEFAULT_MAP_TIMEOUT_MS, "--map-timeout-ms")?,
        sign_off,
    })
}

/// `harness session launch ...`. See the module comment for the exit codes.
pub fn launch_cmd(a: &[String]) -> i32 {
    let usage_err = |m: &str| {
        eprintln!("harness: error: {m}");
        eprintln!("{LAUNCH_USAGE}");
        2
    };
    let args = match parse_args(a) {
        Ok(x) => x,
        Err(m) => return usage_err(&m),
    };
    // 1. validate everything before any write
    if !args.host.is_absolute() {
        return usage_err("--host must be an absolute path (no PATH lookup)");
    }
    if !std::fs::metadata(&args.host).map(|m| m.is_file()).unwrap_or(false) {
        return usage_err("--host is not an existing regular file");
    }
    let cwd = match args.cwd.canonicalize() {
        Ok(c) if c.is_dir() => c,
        _ => return usage_err("--cwd is not an existing directory"),
    };
    let mut brief = Vec::new();
    let read = File::open(&args.brief).and_then(|f| f.take(BRIEF_MAX_BYTES as u64 + 1).read_to_end(&mut brief));
    if read.is_err() {
        return usage_err("--brief cannot be read");
    }
    if brief.len() > BRIEF_MAX_BYTES {
        return usage_err(&format!("--brief is over the cap of {BRIEF_MAX_BYTES} bytes"));
    }
    if brief.is_empty() {
        return usage_err("--brief is empty");
    }
    let Ok(brief_str) = String::from_utf8(brief.clone()) else { return usage_err("--brief is not valid UTF-8") };
    if brief.contains(&0) {
        return usage_err("--brief contains a NUL byte");
    }
    if brief.starts_with(b"-") {
        return usage_err("--brief starts with `-` (the host could parse it as a flag)");
    }
    // 2. name check (the config source rule of `session check-name`)
    let cfg_path = match &args.config {
        Some(f) => f.clone(),
        None => match std::env::var_os("HARNESS_CONFIG") {
            Some(v) => v,
            None => return usage_err("no config configured (pass --config <file> or set HARNESS_CONFIG)"),
        },
    };
    if cfg_path.is_empty() {
        return usage_err("the config source is empty");
    }
    let cfg = match crate::config::Config::load(Path::new(&cfg_path)) {
        Ok(c) => c,
        Err(e) => return usage_err(&e),
    };
    let verdict = match crate::session_name::check(&args.name, cfg.lane_count(), true) {
        Ok(v) => v,
        Err(reason) => {
            eprintln!("harness: session name refused: {}: {}", args.name, reason);
            return 1;
        }
    };
    let role = verdict.role;
    if args.sign_off && !SIGN_OFF_ROLES.contains(&role) {
        return usage_err("--sign-off needs a gov or e-rvw name");
    }
    let checked = args.sign_off || ALWAYS_CHECKED_ROLES.contains(&role);
    // 3. hash and run id
    let sha = sha256_hex(&brief);
    let run = events::new_run_id();
    // 4. the launch row, first; fail closed
    let records = match &args.records {
        Some(p) => p.clone(),
        None => {
            let p = PathBuf::from(DEFAULT_RECORDS);
            if let Some(dir) = p.parent() {
                if let Err(e) = std::fs::create_dir_all(dir) {
                    eprintln!("launch refused: launch record not written: create folder: {}", e.kind());
                    return 2;
                }
            }
            p
        }
    };
    // 4b. signer check (S-1.5 part 4): after the records folder exists, before the `launch` row
    if checked {
        if cfg.builder_state().is_empty() {
            eprintln!("launch refused: a signer launch needs the config's builder_state list");
            return 2;
        }
        if let Some((entry, _)) = cfg.builder_state().iter().find(|(_, text)| brief_str.contains(text.as_str())) {
            let shape = Record::build(
                Kind::LaunchRefused,
                0,
                &[("launch_role", Field::Str(role.to_string())), ("brief_sha256", Field::Str(sha.clone())), ("matched_entry", Field::Str(entry.clone()))],
            );
            if let Err(e) = shape {
                eprintln!("launch refused: the signer brief names builder-state entry {entry}; refusal record not written: {e}");
                return 2;
            }
            if let Err(e) = append_row(&records, &refused_row(now_ms(), &run, &args.name, role, &sha, entry)) {
                eprintln!("launch refused: the signer brief names builder-state entry {entry}; refusal record not written: {e}");
                return 2;
            }
            eprintln!("launch refused: the signer brief names builder-state entry {entry}; nothing started");
            return 6;
        }
    }
    if let Err(e) = append_row(&records, &launch_row(now_ms(), &run, &args.name, role, &sha, args.sign_off)) {
        eprintln!("launch refused: launch record not written: {e}");
        return 2;
    }
    let fail_row = |stage: &str, reason: &str| {
        let _ = append_row(&records, &failed_row(&run, stage, reason));
    };
    // 5. start
    let argv: [&std::ffi::OsStr; 4] = ["--bg".as_ref(), "-n".as_ref(), args.name.as_ref(), brief_str.as_ref()];
    let started = match run_host(&args.host, &argv, Some(&cwd), args.start_ms) {
        Ok(o) => o,
        Err(RunErr::Timeout) => {
            fail_row("start", "timeout");
            eprintln!(
                "launch failed: host timed out after {} ms; host process group killed; a session named {} may exist: check '{} agents' and stop it by hand",
                args.start_ms,
                args.name,
                args.host.display()
            );
            return 3;
        }
        Err(RunErr::Spawn(k)) => {
            fail_row("start", &format!("spawn: {k}"));
            eprintln!("launch failed: the host could not be started: {k}");
            return 3;
        }
    };
    let short = match (started.ok, parse_short_id(&started.stdout)) {
        (true, Some(s)) => s,
        (false, _) => {
            fail_row("start", "host exited non-zero");
            eprintln!("launch failed: the host exited non-zero");
            return 3;
        }
        (true, None) => {
            fail_row("start", "no short id on host stdout");
            eprintln!("launch failed: the host printed no short id");
            return 3;
        }
    };
    // 6. map the short id to the full uuid
    let deadline = Instant::now() + Duration::from_millis(args.map_ms);
    let mapped: Result<String, String> = loop {
        match map_once(&args.host, &short, &args.name) {
            Ok(Some(id)) => break Ok(id),
            Ok(None) => {}
            Err(MapErr::Fail(m)) => break Err(m),
        }
        if Instant::now() >= deadline {
            break Err("the short id is not in the host's listing".into());
        }
        std::thread::sleep(Duration::from_millis(MAP_POLL_MS));
    };
    let bound = mapped.and_then(|id| {
        let row = format!(
            "{{\"v\":1,\"kind\":\"launch_ids\",\"ts_ms\":{},\"run_id\":{},\"short_id\":{},\"session_id\":{}}}",
            now_ms(),
            jstr(&run),
            jstr(&short),
            jstr(&id)
        );
        append_row(&records, &row).map(|_| id).map_err(|e| format!("id row not written: {e}"))
    });
    match bound {
        Ok(id) => {
            println!("launched run_id={run} name={} role={role} short_id={short} session_id={id}", args.name);
            0
        }
        Err(why) => {
            let stopped = matches!(
                run_host(&args.host, &["stop".as_ref(), short.as_ref()], None, AUX_TIMEOUT_MS),
                Ok(HostOut { ok: true, .. })
            );
            fail_row("map", &why);
            if stopped {
                eprintln!("launch failed: {why}; the session was stopped (short id {short})");
                4
            } else {
                eprintln!("launch failed: {why}; STOP FAILED: session {} is still running, stop short id {short} by hand", args.name);
                5
            }
        }
    }
}

/// `harness session launches [--records <file>]`.
pub fn launches_cmd(a: &[String]) -> i32 {
    let mut records: Option<&str> = None;
    let mut it = a.iter();
    while let Some(f) = it.next() {
        match (f.as_str(), it.next()) {
            ("--records", Some(v)) if records.is_none() => records = Some(v),
            _ => {
                eprintln!("{LAUNCH_USAGE}");
                return 2;
            }
        }
    }
    let path = PathBuf::from(records.unwrap_or(DEFAULT_RECORDS));
    match read_launches(&path) {
        Ok(ls) => {
            for l in ls {
                println!(
                    "run_id={} name={} role={} brief_sha256={} ts_ms={} short_id={} session_id={} state={}{}",
                    l.run_id,
                    l.name,
                    l.role,
                    l.brief_sha256,
                    l.ts_ms,
                    l.short_id.as_deref().unwrap_or("-"),
                    l.session_id.as_deref().unwrap_or("-"),
                    l.state,
                    if l.sign_off { " sign_off=yes" } else { "" }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("harness: error: {e}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sha256_hex;

    #[test]
    fn sha256_empty_vector() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
    }

    #[test]
    fn sha256_abc_vector() {
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    #[test]
    fn sha256_two_block_448_bit_vector() {
        assert_eq!(
            sha256_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }
}
