//! `harness session rotation-check --log <abs file> --session-id <uuid> --transcript <abs file> --utc-offset <+HH:MM|-HH:MM> [--now-ms <n>]`:
//! says whether the session is due for a handover. Two marks: the local day of now is after the local day of the start, or the
//! transcript holds one or more compaction lines. The start is the smallest ts_ms of the session_started rows with source hook
//! and this session id in the event log (the earliest hook row; a row from a resume, clear or compact before any startup row
//! cannot be told apart, an open path). The day is the day at a fixed offset. The compaction count is the one `session tokens`
//! reads; no byte of the transcript is printed. Exit 0 not due, 10 due (one stdout line), 2 usage, 3 refusal (stdout empty).
//! Read-only: opens two files for reading, writes nothing, starts no child process. std only.
use super::{civil_from_days, digits_to, parse_offset, MAX_NOW_MS};
use crate::events::{self, Field, Kind};
use std::path::Path;

const USAGE: &str = "usage: harness session rotation-check --log <abs file> --session-id <uuid> --transcript <abs file> --utc-offset <+HH:MM|-HH:MM> [--now-ms <n>]";

fn bad(msg: &str) -> i32 {
    eprintln!("harness: error: {msg}");
    eprintln!("{USAGE}");
    2
}

fn refuse(what: &str, why: &str) -> i32 {
    eprintln!("harness: rotation-check refused: {what}: {why}");
    3
}

/// The local day number of a moment: floor((ms / 1000 + offset seconds) / 86400).
fn day_of(ms: i128, offset_s: i128) -> i128 {
    (ms.div_euclid(1000) + offset_s).div_euclid(86400)
}

fn day_text(day: i128) -> String {
    let (y, m, d) = civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The earliest hook start of the session in the log, in ms.
fn read_start(log: &Path, id: &str) -> Result<u64, String> {
    let r = events::read_log(log)?;
    if !r.is_clean() {
        return Err("the log has a corrupt line or a torn tail".to_string());
    }
    r.events
        .iter()
        .filter(|e| e.kind == Kind::SessionStarted)
        .filter(|e| e.field("source") == Some(&Field::Str("hook".to_string())))
        .filter(|e| e.field("session_id") == Some(&Field::Str(id.to_string())))
        .map(|e| e.ts_ms)
        .min()
        .ok_or_else(|| "no start row for the session".to_string())
}

/// The compaction count of the transcript. The path kind is checked without following links, the opened file must be the
/// file that check saw, then the count is the one `session tokens` reads.
fn read_compactions(path: &Path) -> Result<u64, String> {
    let io = |e: std::io::Error| format!("the transcript cannot be opened: {}", e.kind());
    let path_md = std::fs::symlink_metadata(path).map_err(io)?;
    if path_md.file_type().is_symlink() {
        return Err("the transcript is a symbolic link".to_string());
    }
    if !path_md.file_type().is_file() {
        return Err("the transcript is not a regular file".to_string());
    }
    let file = std::fs::File::open(path).map_err(io)?;
    let handle_md = file.metadata().map_err(io)?;
    if !handle_md.is_file() {
        return Err("the transcript is not a regular file".to_string());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (handle_md.dev(), handle_md.ino()) != (path_md.dev(), path_md.ino()) {
            return Err("the transcript changed between check and open".to_string());
        }
    }
    crate::session_tokens::read_transcript(std::io::BufReader::new(file)).map(|t| t.compactions)
}

pub fn rotation_check_cmd(a: &[String]) -> i32 {
    let mut log: Option<&str> = None;
    let mut sid: Option<&str> = None;
    let mut transcript: Option<&str> = None;
    let mut offset: Option<&str> = None;
    let mut now: Option<&str> = None;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--log" => &mut log,
            "--session-id" => &mut sid,
            "--transcript" => &mut transcript,
            "--utc-offset" => &mut offset,
            "--now-ms" => &mut now,
            _ => return bad("bad arguments"),
        };
        match it.next() {
            Some(v) if slot.is_none() => *slot = Some(v.as_str()),
            _ => return bad("bad arguments"),
        }
    }
    let Some(log) = log else { return bad("--log is required") };
    let Some(sid) = sid else { return bad("--session-id is required") };
    let Some(transcript) = transcript else { return bad("--transcript is required") };
    let Some(offset) = offset else { return bad("--utc-offset is required") };
    if !Path::new(log).is_absolute() {
        return bad("--log must be an absolute path");
    }
    if !Path::new(transcript).is_absolute() {
        return bad("--transcript must be an absolute path");
    }
    if !crate::death::uuid_ok(sid) {
        return bad("--session-id must be a uuid");
    }
    let Some(offset_s) = parse_offset(offset) else {
        return bad("--utc-offset must be +HH:MM or -HH:MM (HH 00 to 14, MM 00 to 59)");
    };
    let now_ms: i128 = match now {
        Some(t) => match digits_to(t) {
            Some(n) if n <= MAX_NOW_MS => n,
            _ => return bad("--now-ms must be digits, at most 253401609599000"),
        },
        None => match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_millis() as i128,
            Err(_) => {
                eprintln!("harness: error: clock is before the epoch");
                return 2;
            }
        },
    };

    let start = match read_start(Path::new(log), sid) {
        Ok(s) => i128::from(s),
        Err(e) => return refuse("start record", &e),
    };
    if start > MAX_NOW_MS {
        return refuse("start record", "the start time is out of range");
    }
    if start > now_ms {
        return refuse("start record", "the start time is after now");
    }

    let name = format!("{sid}.jsonl");
    if Path::new(transcript).file_name().and_then(|n| n.to_str()) != Some(name.as_str()) {
        return refuse("transcript", "the file name does not match the session id");
    }
    // An end slash makes the operating system's answer for a link differ between runs, so the path is refused as written.
    if transcript.ends_with('/') {
        return refuse("transcript", "the path ends with a slash");
    }
    let compactions = match read_compactions(Path::new(transcript)) {
        Ok(n) => n,
        Err(e) => return refuse("transcript", &e),
    };

    let (start_day, now_day) = (day_of(start, offset_s), day_of(now_ms, offset_s));
    let by_day = now_day > start_day;
    let by_compaction = compactions >= 1;
    let reason = match (by_day, by_compaction) {
        (false, false) => "none",
        (true, false) => "day",
        (false, true) => "compaction",
        (true, true) => "day+compaction",
    };
    let due = by_day || by_compaction;
    println!(
        "rotation due={} reason={reason} compactions={compactions} start_day={} now_day={}",
        if due { "yes" } else { "no" },
        day_text(start_day),
        day_text(now_day)
    );
    if due {
        10
    } else {
        0
    }
}
