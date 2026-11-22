//! The seat log read of `harness session watch --seat-log <abs file>` (std only, unix). On a blocked verdict the newest fitting record of
//! the log the seat hooks write becomes one detail line. This module opens the one path it is given and nothing else; the log is the hook
//! record file, never the host session file. The record format is in docs/session-watch.md.
//!
//! The read is bounded: the last 65536 bytes of the file, the first line of that window skipped when the window does not start at the file
//! start. The file is opened without following a link and without blocking, and must be a regular file by fstat on the opened handle. No
//! failure of the log is ever a refusal: a missing file is `none`, any other problem is `unreadable`. The text is cut to one line and 200
//! bytes and run through the privacy scan; a hit, a scan error or no list prints `withheld`.

use crate::launcher::{parse_json, J};
use crate::overlay;
use crate::scan::{scan_text, Ctx};
use std::fs::OpenOptions;
use std::io::{ErrorKind, Read, Seek, SeekFrom};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

#[cfg(target_os = "linux")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0o400000 | 0o4000;
#[cfg(target_os = "macos")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0x0100 | 0x0004;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0;

/// The read window, in bytes from the end of the file.
const WINDOW: u64 = 65536;
/// The text of the detail line, in bytes.
const TEXT_MAX: usize = 200;
/// The printed tool name, in characters.
const TOOL_MAX: usize = 40;

/// What the blocked entry is, which picks the record event.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Block {
    Dialog,
    Asked,
    Other,
}

struct Rec {
    event: String,
    tool: Option<String>,
    command: Option<String>,
    message: Option<String>,
}

enum Read_ {
    None,
    Unreadable,
    Found(Rec),
}

fn wanted(b: Block, event: &str) -> bool {
    match b {
        Block::Dialog => event == "PermissionRequest",
        Block::Asked => event == "Stop",
        Block::Other => event == "PermissionRequest" || event == "Stop",
    }
}

fn read_newest(path: &Path, floor_ms: u64, b: Block) -> Read_ {
    let mut f = match OpenOptions::new().read(true).custom_flags(OPEN_NOFOLLOW_NONBLOCK).open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == ErrorKind::NotFound => return Read_::None,
        Err(_) => return Read_::Unreadable,
    };
    let Ok(m) = f.metadata() else { return Read_::Unreadable };
    if !m.file_type().is_file() {
        return Read_::Unreadable;
    }
    let start = m.len().saturating_sub(WINDOW);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return Read_::Unreadable;
    }
    let mut buf = Vec::new();
    if f.take(WINDOW).read_to_end(&mut buf).is_err() {
        return Read_::Unreadable;
    }
    let mut lines = buf.split(|c| *c == b'\n');
    if start > 0 {
        lines.next();
    }
    let mut best: Option<(u64, Rec)> = None;
    for raw in lines {
        let Ok(text) = std::str::from_utf8(raw) else { continue };
        let Ok(j @ J::Obj(_)) = parse_json(text.as_bytes()) else { continue };
        let Some(J::Num(ts)) = j.get("ts_ms") else { continue };
        let Some(event) = j.str("event") else { continue };
        if *ts < floor_ms || !wanted(b, event) {
            continue;
        }
        if best.as_ref().is_some_and(|(t, _)| *ts < *t) {
            continue;
        }
        let rec = Rec {
            event: event.to_string(),
            tool: j.str("tool").map(str::to_string),
            command: j.str("command").map(str::to_string),
            message: j.str("message").map(str::to_string),
        };
        best = Some((*ts, rec));
    }
    match best {
        Some((_, r)) => Read_::Found(r),
        None => Read_::None,
    }
}

/// One line, at most 200 bytes cut on a character boundary.
fn one_line(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let c = if c.is_control() { ' ' } else { c };
        if out.len() + c.len_utf8() > TEXT_MAX {
            break;
        }
        out.push(c);
    }
    out
}

/// The scanned text of a free-text field: `-` when absent, `withheld` on a hit, a scan error or no list.
fn text_field(v: Option<&str>) -> String {
    let Some(v) = v else { return "-".into() };
    let t = one_line(v);
    match overlay::load() {
        Ok(list) => {
            if scan_text(&Ctx { list: &list, testmode: false }, "seat-log", &t).is_empty() {
                t
            } else {
                "withheld".into()
            }
        }
        Err(_) => "withheld".into(),
    }
}

fn tool_field(v: Option<&str>) -> String {
    match v {
        None => "-".into(),
        Some(t) => t.chars().take(TOOL_MAX).map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '-' }).collect(),
    }
}

/// The detail line (without a newline) for a blocked verdict: `detail none`, `detail unreadable`, `detail tool=<t> command=<c>` or
/// `detail message=<m>`. `floor_ms` is the launch record time.
pub fn detail_line(path: &Path, floor_ms: u64, b: Block) -> String {
    match read_newest(path, floor_ms, b) {
        Read_::None => "detail none".into(),
        Read_::Unreadable => "detail unreadable".into(),
        Read_::Found(r) if r.event == "PermissionRequest" => {
            format!("detail tool={} command={}", tool_field(r.tool.as_deref()), text_field(r.command.as_deref()))
        }
        Read_::Found(r) => format!("detail message={}", text_field(r.message.as_deref())),
    }
}
