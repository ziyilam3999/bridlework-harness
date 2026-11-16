//! `harness ticket merge-text` (std only, Unix; `docs/merge-text.md` is the contract). Reads a ticket file
//! and a body file, checks a squash subject and body, and writes two new files, or refuses. It runs no gh,
//! no git and no network call. Exit codes: 0 ok; 1 a refused input; 2 usage, setup or an unusable input.
//! No title, body or path text is printed (a reader refusal can name a ticket key).

use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

pub const USAGE: &str = "usage: harness ticket merge-text <ticket-file> --pr <n> --body-file <absolute-file> --out-dir <absolute-dir>";

/// The ticket is read up to 1 MiB plus one byte (memory cap; the strict reader refuses over 1 MiB).
const TICKET_READ_LIMIT: u64 = 1024 * 1024 + 1;
const BODY_CAP: usize = 4096;
const BODY_MAX_LINES: usize = 20;
const SUBJECT_CAP: usize = 256;

// The open(2) flags std has no constants for (the owed.rs pattern): no link follow, no fifo hang.
#[cfg(target_os = "linux")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0o400000 | 0o4000;
#[cfg(target_os = "macos")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0x0100 | 0x0004;

fn usage() -> i32 {
    eprintln!("{USAGE}");
    2
}

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    2
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: merge-text refused: {m}");
    1
}

struct Args {
    ticket: String,
    pr: String,
    body: PathBuf,
    out: PathBuf,
}

fn abs_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

fn pr_ok(s: &str) -> bool {
    (1..=9).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) && !s.starts_with('0')
}

fn parse_args(a: &[String]) -> Option<Args> {
    let (mut ticket, mut pr, mut body, mut out): (Option<&str>, Option<&str>, Option<&str>, Option<&str>) = (None, None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--pr" => &mut pr,
            "--body-file" => &mut body,
            "--out-dir" => &mut out,
            s if s.starts_with("--") => return None,
            s => {
                if ticket.replace(s).is_some() {
                    return None;
                }
                continue;
            }
        };
        let v = it.next()?;
        if slot.replace(v.as_str()).is_some() {
            return None;
        }
    }
    let (ticket, pr, body, out) = (ticket?, pr?, body?, out?);
    if !pr_ok(pr) || !abs_ok(body) || !abs_ok(out) {
        return None;
    }
    Some(Args { ticket: ticket.to_string(), pr: pr.to_string(), body: PathBuf::from(body), out: PathBuf::from(out) })
}

/// Why an input file could not be read: missing or unreadable, or not a regular file.
enum OpenErr {
    Unreadable,
    NotRegular,
}

/// A no-follow type check, then an open with no link follow and no block, then the handle is re-checked to
/// be a regular file (a path swapped for a fifo or a folder in between is still refused).
fn open_regular(path: &Path) -> Result<fs::File, OpenErr> {
    match fs::symlink_metadata(path) {
        Err(_) => return Err(OpenErr::Unreadable),
        Ok(m) if !m.file_type().is_file() => return Err(OpenErr::NotRegular),
        Ok(_) => {}
    }
    let f = fs::OpenOptions::new().read(true).custom_flags(OPEN_NOFOLLOW_NONBLOCK).open(path).map_err(|_| OpenErr::Unreadable)?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err(OpenErr::NotRegular);
    }
    Ok(f)
}

fn read_capped(path: &Path, limit: u64) -> Result<Vec<u8>, OpenErr> {
    let f = open_regular(path)?;
    let mut buf = Vec::new();
    f.take(limit).read_to_end(&mut buf).map_err(|_| OpenErr::Unreadable)?;
    Ok(buf)
}

/// The plain-text rule: a control character, U+2028 or U+2029.
fn has_control(s: &str) -> bool {
    s.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

/// Two or more runs of ASCII letters or digits joined by single `-`, then zero or more blanks, then a colon.
fn looks_like_trailer(line: &str) -> bool {
    let b = line.trim_start().as_bytes();
    let mut i = 0;
    let mut runs = 0;
    loop {
        let start = i;
        while i < b.len() && b[i].is_ascii_alphanumeric() {
            i += 1;
        }
        if i == start {
            return false;
        }
        runs += 1;
        if i < b.len() && b[i] == b'-' {
            i += 1;
        } else {
            break;
        }
    }
    if runs < 2 {
        return false;
    }
    while i < b.len() && (b[i] == b' ' || b[i] == b'\t') {
        i += 1;
    }
    b.get(i) == Some(&b':')
}

/// The output folder: a real folder (not a link), canonicalized once, with no `.git` entry in it or above it.
fn resolve_out(arg: &Path) -> Result<PathBuf, &'static str> {
    // Rebuilt from components: drops a trailing "/" or "/.", so the last name is not followed.
    let arg: PathBuf = arg.components().collect();
    let arg = arg.as_path();
    let meta = fs::symlink_metadata(arg).map_err(|_| "the output folder is missing or unreadable")?;
    if !meta.file_type().is_dir() {
        return Err("the output folder must be a real folder, not a link or a file");
    }
    let canon = arg.canonicalize().map_err(|_| "the output folder cannot be resolved")?;
    if canon.ancestors().any(|a| fs::symlink_metadata(a.join(".git")).is_ok()) {
        return Err("the output folder lies inside a work tree");
    }
    Ok(canon)
}

/// The shape checks of the body; `Err` is the text after `harness: merge-text refused: `.
fn body_problem(bytes: &[u8]) -> Result<(String, usize), String> {
    if bytes.len() > BODY_CAP {
        return Err(format!("body is over {BODY_CAP} bytes"));
    }
    if bytes.is_empty() {
        return Err("body is empty (0 bytes)".to_string());
    }
    let text = String::from_utf8(bytes.to_vec()).map_err(|_| "body is not UTF-8".to_string())?;
    let Some(inner) = text.strip_suffix('\n') else {
        return Err("body does not end with a newline".to_string());
    };
    let lines: Vec<&str> = inner.split('\n').collect();
    if lines.len() > BODY_MAX_LINES {
        return Err(format!("body has more than {BODY_MAX_LINES} lines"));
    }
    for (i, line) in lines.iter().enumerate() {
        let n = i + 1;
        if has_control(line) {
            return Err(format!("body line {n} has a control character"));
        }
        if (i == 0 || i + 1 == lines.len()) && line.trim().is_empty() {
            return Err(format!("body line {n} is blank at the start or the end"));
        }
        if looks_like_trailer(line) {
            return Err(format!("body line {n} looks like a trailer"));
        }
    }
    let count = lines.len();
    Ok((text, count))
}

fn classes(hits: &[crate::scan::Hit]) -> String {
    hits.iter().map(|h| h.class).collect::<Vec<&str>>().join(", ")
}

/// Create one new file (`create_new`): an entry made by someone else is neither replaced nor written through.
fn create_new(path: &Path, bytes: &[u8], made: &mut Vec<PathBuf>) -> Result<(), &'static str> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            "the output folder already holds a merge text file"
        } else {
            "the output folder cannot be written"
        }
    })?;
    made.push(path.to_path_buf());
    f.write_all(bytes).and_then(|()| f.sync_all()).map_err(|_| "the output folder cannot be written")
}

/// `harness ticket merge-text <ticket-file> --pr <n> --body-file <absolute-file> --out-dir <absolute-dir>`.
pub fn merge_text_cmd(a: &[String]) -> i32 {
    // (1) usage
    let Some(args) = parse_args(a) else { return usage() };
    // (2) the privacy list, before any input file is opened
    let list = match crate::overlay::load() {
        Ok(l) if !l.entries.is_empty() => l,
        _ => return fail("no privacy list configured: merge text is not written unscanned"),
    };
    // (3) the ticket file
    let bytes = match read_capped(Path::new(&args.ticket), TICKET_READ_LIMIT) {
        Ok(b) => b,
        Err(OpenErr::Unreadable) => return fail("the ticket file is missing or cannot be read"),
        Err(OpenErr::NotRegular) => return fail("the ticket file is not a regular file"),
    };
    // (4) the strict reader
    let ticket = match crate::ticket::Ticket::parse_bytes(&bytes) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("harness: ticket refused: {e}");
            return 1;
        }
    };
    // (5) the id becomes a file name
    if !crate::ticket_gate::ticket_id_ok(&ticket.id) {
        return fail("the ticket id is not a plain file name");
    }
    // (6) the output folder
    let out = match resolve_out(&args.out) {
        Ok(p) => p,
        Err(m) => return fail(m),
    };
    // (7) the body file, at most 4097 bytes
    let body = match read_capped(&args.body, BODY_CAP as u64 + 1) {
        Ok(b) => b,
        Err(OpenErr::Unreadable) => return fail("the body file is missing or cannot be read"),
        Err(OpenErr::NotRegular) => return fail("the body file is not a regular file"),
    };
    // (8) the subject
    let title = &ticket.title;
    if title.trim().is_empty() {
        return refuse("title is blank");
    }
    if has_control(title) {
        return refuse("title has a control character");
    }
    let subject = format!("{title} (#{})", args.pr);
    if subject.len() > SUBJECT_CAP {
        return refuse(&format!("subject is over {SUBJECT_CAP} bytes"));
    }
    // (9) the body
    let (text, count) = match body_problem(&body) {
        Ok(v) => v,
        Err(m) => return refuse(&m),
    };
    // (10) the privacy scan, testmode off
    let ctx = crate::scan::Ctx { list: &list, testmode: false };
    let hits = crate::scan::scan_text(&ctx, "merge.subject", &subject);
    if !hits.is_empty() {
        return refuse(&format!("subject has a privacy hit ({})", classes(&hits)));
    }
    for (i, line) in text.strip_suffix('\n').unwrap_or(&text).split('\n').enumerate() {
        let hits = crate::scan::scan_text(&ctx, "merge.body", line);
        if !hits.is_empty() {
            return refuse(&format!("body line {} has a privacy hit ({})", i + 1, classes(&hits)));
        }
    }
    // (11) the writes
    let subject_path = out.join(format!("{}.subject", ticket.id));
    let body_path = out.join(format!("{}.body", ticket.id));
    for p in [&subject_path, &body_path] {
        if fs::symlink_metadata(p).is_ok() {
            return fail("the output folder already holds a merge text file");
        }
    }
    let mut made: Vec<PathBuf> = Vec::new();
    let mut subject_line = subject.into_bytes();
    subject_line.push(b'\n');
    let written = create_new(&subject_path, &subject_line, &mut made).and_then(|()| create_new(&body_path, &body, &mut made));
    if let Err(m) = written {
        for p in &made {
            let _ = fs::remove_file(p);
        }
        return fail(m);
    }
    println!("merge-text ok ticket={} pr={} body_lines={count}", ticket.id, args.pr);
    0
}
