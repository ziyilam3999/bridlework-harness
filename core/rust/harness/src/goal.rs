//! `harness goal add | list | show | close | check` (std only, Unix; `docs/goal.md` is the contract). A goal is
//! the parent of tickets with its own finish line, one per focus group (WAITING-232 A): goals that span tickets
//! had no record and no finish line (WAITING-228), because the ticket schema and the plan index hold no goal or
//! parent field. This module is the small store of goals, modelled on the owed store (`owed.rs`). The store file
//! sits in the private overlay, named by `--store <absolute-file>`, created once by hand as an empty file; no
//! verb creates it, so a wrong path cannot start a second store. No clock runs here: times arrive by `--at`.
//! A refusal names a field and a reason, no field value.
//! Exit codes: 0 ok; 1 a refused input; 2 usage, setup or a damaged store.

use crate::events::token_ok;
use crate::friction::{at_ok, jstr, proof_problem, P};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const GOAL_USAGE: &str = "usage: harness goal add --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --title <text> --done-when <line> --source <pointers> | goal list --store <absolute-file> [--open] | goal show --store <absolute-file> --id <id> | goal close --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --status <met|dropped> (--ref <pointers> | --reason <text>) | goal check --store <absolute-file> --id <id>";

/// At most this many bytes plus one are read; a store over this is a damaged store (exit 2).
const STORE_CAP: u64 = 4 * 1024 * 1024;
/// `id` and `by` are at most this many bytes.
const TOKEN_CAP: usize = 64;
/// `title`, `done_when` and `reason` are at most this many bytes.
const LINE_CAP: usize = 500;

// The open(2) flags std exposes no constants for (the owed.rs pattern). O_NOFOLLOW makes a symlink store fail
// the open itself; O_NONBLOCK makes the open of a fifo return at once instead of blocking (the regular-file
// re-check then refuses it).
#[cfg(target_os = "linux")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0o400000 | 0o4000;
#[cfg(target_os = "macos")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0o0100 | 0x0004;

/// One add row: `{"op":"add","id":..,"at":..,"by":..,"title":..,"done_when":..,"source":..}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddRow {
    pub id: String,
    pub at: String,
    pub by: String,
    pub title: String,
    pub done_when: String,
    pub source: String,
}

/// One close row: `{"op":"close","id":..,"at":..,"by":..,"status":..,"ref":..,"reason":..}`; `ref` and
/// `reason` are both present, the one that does not apply is empty.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloseRow {
    pub id: String,
    pub at: String,
    pub by: String,
    pub status: String,
    pub reference: String,
    pub reason: String,
}

/// One goal as the store holds it: added once, `open` until its close row sets `met` or `dropped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Goal {
    pub id: String,
    pub title: String,
    pub done_when: String,
    pub added_by: String,
    pub add_at: String,
    pub source: String,
    pub status: String,
    /// The close row, once the goal is `met` or `dropped`.
    pub close: Option<CloseRow>,
}

enum Row {
    Add(AddRow),
    Close(CloseRow),
}

// ---------------------------------------------------------------- field rules (one message each)

fn token_problem(s: &str, field: &str) -> Option<String> {
    if token_ok(s) && s.len() <= TOKEN_CAP {
        None
    } else {
        Some(format!("{field} must be a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not . or .."))
    }
}

fn at_problem(s: &str) -> Option<String> {
    if at_ok(s) {
        None
    } else {
        Some("at must be a UTC time written YYYY-MM-DDTHH:MM:SSZ".to_string())
    }
}

fn line_problem(s: &str, field: &str) -> Option<String> {
    if s.trim().is_empty() || s.len() > LINE_CAP || s.chars().any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}')) {
        Some(format!("{field} must be one non-blank line of at most 500 bytes"))
    } else {
        None
    }
}

/// A finish line is a real line: blank, or only a dash, `tbd` or `?` (any case, after trimming), is none.
fn done_when_problem(s: &str) -> Option<String> {
    const BAD: &str = "done_when must be one non-blank line of at most 500 bytes, not only -, tbd or ?";
    if let Some(m) = line_problem(s, "done_when") {
        return Some(m);
    }
    let t = s.trim().to_ascii_lowercase();
    if matches!(t.as_str(), "-" | "tbd" | "?") { Some(BAD.to_string()) } else { None }
}

/// `source` holds `; `-separated proof pointers (the inbox proof grammar, checked by the one implementation).
fn source_problem(s: &str) -> Option<String> {
    proof_problem(s).map(|_| "source is not a valid pointer list".to_string())
}

fn status_problem(s: &str) -> Option<String> {
    if s == "met" || s == "dropped" {
        None
    } else {
        Some("status must be met or dropped".to_string())
    }
}

/// `ref` holds `; `-separated proof pointers (the inbox proof grammar, checked by the one implementation).
fn ref_problem(s: &str) -> Option<String> {
    proof_problem(s).map(|_| "ref is not a valid pointer list".to_string())
}

/// Pairing, judged on values: an empty `ref` or `reason` counts as not given.
fn pairing_problem(status: &str, r: &str, reason: &str) -> Option<String> {
    match status {
        "met" if r.is_empty() || !reason.is_empty() => Some("status met needs --ref and takes no --reason".to_string()),
        "dropped" if reason.is_empty() || !r.is_empty() => Some("status dropped needs --reason and takes no --ref".to_string()),
        _ => None,
    }
}

/// The add-row rules, in the order they are checked.
fn add_row_problem(r: &AddRow) -> Option<String> {
    token_problem(&r.id, "id")
        .or_else(|| at_problem(&r.at))
        .or_else(|| token_problem(&r.by, "by"))
        .or_else(|| line_problem(&r.title, "title"))
        .or_else(|| done_when_problem(&r.done_when))
        .or_else(|| source_problem(&r.source))
}

/// The close-row rules, in the order they are checked: id, at, by, status, pairing, then ref (met) or reason
/// (dropped).
fn close_row_problem(r: &CloseRow) -> Option<String> {
    token_problem(&r.id, "id")
        .or_else(|| at_problem(&r.at))
        .or_else(|| token_problem(&r.by, "by"))
        .or_else(|| status_problem(&r.status))
        .or_else(|| pairing_problem(&r.status, &r.reference, &r.reason))
        .or_else(|| if r.status == "met" { ref_problem(&r.reference) } else { line_problem(&r.reason, "reason") })
}

// ---------------------------------------------------------------- encode and parse

/// The add row as one line without its newline, keys in the order this module writes.
fn encode_add(r: &AddRow) -> String {
    format!(
        "{{\"op\":\"add\",\"id\":{},\"at\":{},\"by\":{},\"title\":{},\"done_when\":{},\"source\":{}}}",
        jstr(&r.id),
        jstr(&r.at),
        jstr(&r.by),
        jstr(&r.title),
        jstr(&r.done_when),
        jstr(&r.source)
    )
}

/// The close row as one line without its newline, keys in the order this module writes.
fn encode_close(r: &CloseRow) -> String {
    format!(
        "{{\"op\":\"close\",\"id\":{},\"at\":{},\"by\":{},\"status\":{},\"ref\":{},\"reason\":{}}}",
        jstr(&r.id),
        jstr(&r.at),
        jstr(&r.by),
        jstr(&r.status),
        jstr(&r.reference),
        jstr(&r.reason)
    )
}

/// Strict decode of one stored line without its newline. The reason is one of the store lint's literal
/// strings; it names no value.
fn parse_row(line: &str) -> Result<Row, String> {
    let mut p = P { b: line.as_bytes(), i: 0 };
    let pairs = p.object().map_err(|_| "not one JSON object of strings".to_string())?;
    p.ws();
    if p.i != p.b.len() {
        return Err("not one JSON object of strings".to_string());
    }
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let mut keys: Vec<&str> = pairs.iter().map(|(n, _)| n.as_str()).collect();
    keys.sort_unstable();
    let same_set = |want: &mut Vec<&str>| {
        want.sort_unstable();
        keys == *want
    };
    match get("op").as_deref() {
        Some("add") => {
            if !same_set(&mut vec!["op", "id", "at", "by", "title", "done_when", "source"]) {
                return Err("the keys are not the key set of its op".to_string());
            }
            let row = AddRow {
                id: get("id").unwrap(),
                at: get("at").unwrap(),
                by: get("by").unwrap(),
                title: get("title").unwrap(),
                done_when: get("done_when").unwrap(),
                source: get("source").unwrap(),
            };
            if let Some(m) = add_row_problem(&row) {
                return Err(m);
            }
            Ok(Row::Add(row))
        }
        Some("close") => {
            if !same_set(&mut vec!["op", "id", "at", "by", "status", "ref", "reason"]) {
                return Err("the keys are not the key set of its op".to_string());
            }
            let row = CloseRow { id: get("id").unwrap(), at: get("at").unwrap(), by: get("by").unwrap(), status: get("status").unwrap(), reference: get("ref").unwrap(), reason: get("reason").unwrap() };
            if let Some(m) = close_row_problem(&row) {
                return Err(m);
            }
            Ok(Row::Close(row))
        }
        _ => Err("op must be add or close".to_string()),
    }
}

/// The first privacy hit of one stored row as (field, classes), in key order.
fn row_hit(ctx: &crate::scan::Ctx, row: &Row) -> Option<(&'static str, String)> {
    let fields: [(&'static str, &str); 6] = match row {
        Row::Add(r) => [("id", r.id.as_str()), ("at", r.at.as_str()), ("by", r.by.as_str()), ("title", r.title.as_str()), ("done_when", r.done_when.as_str()), ("source", r.source.as_str())],
        Row::Close(r) => [("id", r.id.as_str()), ("at", r.at.as_str()), ("by", r.by.as_str()), ("status", r.status.as_str()), ("ref", r.reference.as_str()), ("reason", r.reason.as_str())],
    };
    fields.iter().find_map(|(name, value)| {
        let hits = crate::scan::scan_text(ctx, "goal.store", value);
        if hits.is_empty() {
            None
        } else {
            Some((*name, hits.iter().map(|h| h.class).collect::<Vec<&str>>().join(", ")))
        }
    })
}

// ---------------------------------------------------------------- the store lint

/// The one store criterion for every verb: cap, utf-8, a torn last line, the first bad row, then the first
/// privacy hit, over the bytes of the locked handle. `Err` is the message after `harness: error: `; `Ok` is
/// the goals in add-row order.
fn read_store_locked(f: &fs::File, ctx: &crate::scan::Ctx) -> Result<Vec<Goal>, String> {
    let mut buf = Vec::new();
    f.take(STORE_CAP + 1).read_to_end(&mut buf).map_err(|_| "the goal store cannot be opened".to_string())?;
    if buf.len() as u64 > STORE_CAP {
        return Err(format!("the goal store is over {STORE_CAP} bytes"));
    }
    let text = String::from_utf8(buf).map_err(|_| "the goal store is not utf-8".to_string())?;
    if !text.is_empty() && !text.ends_with('\n') {
        let n = text.bytes().filter(|b| *b == b'\n').count() + 1;
        return Err(format!("the goal store has a torn last line at line {n}"));
    }
    let mut rows: Vec<(usize, Row)> = Vec::new();
    let mut problems: Vec<(usize, String)> = Vec::new();
    let mut line_no = 0;
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' {
            line_no += 1;
            match parse_row(&text[start..i]) {
                Ok(r) => rows.push((line_no, r)),
                Err(e) => problems.push((line_no, e)),
            }
            start = i + 1;
        }
    }
    // The cross rules, row by row in file order (a close row names an earlier add row).
    let mut goals: Vec<Goal> = Vec::new();
    for (line, row) in &rows {
        match row {
            Row::Add(a) => {
                if goals.iter().any(|g| g.id == a.id) {
                    problems.push((*line, "id is added twice".to_string()));
                } else {
                    goals.push(Goal {
                        id: a.id.clone(),
                        title: a.title.clone(),
                        done_when: a.done_when.clone(),
                        added_by: a.by.clone(),
                        add_at: a.at.clone(),
                        source: a.source.clone(),
                        status: "open".to_string(),
                        close: None,
                    });
                }
            }
            Row::Close(c) => {
                let Some(pos) = goals.iter().position(|g| g.id == c.id) else {
                    problems.push((*line, "close names no earlier added id".to_string()));
                    continue;
                };
                if goals[pos].status != "open" {
                    problems.push((*line, "id is closed twice".to_string()));
                } else if c.at.as_str() < goals[pos].add_at.as_str() {
                    problems.push((*line, "close is before the time the id was added".to_string()));
                } else {
                    goals[pos].status = c.status.clone();
                    goals[pos].close = Some(c.clone());
                }
            }
        }
    }
    // The first bad line wins; a line with a field problem holds no cross problem (it is not a row).
    problems.sort_by_key(|(n, _)| *n);
    if let Some((n, m)) = problems.first() {
        return Err(format!("the goal store has a bad row at line {n}: {m}"));
    }
    for (n, row) in &rows {
        if let Some((name, classes)) = row_hit(ctx, row) {
            return Err(format!("the goal store has a privacy hit at line {n} in field {name} ({classes})"));
        }
    }
    Ok(goals)
}

// ---------------------------------------------------------------- the store file

/// A no-follow stat of the store path (a linked parent folder is accepted, only the last part must not be a
/// link), then an open with O_NOFOLLOW and O_NONBLOCK, read-only for the reads, read and append for add and
/// close, no create. The handle is re-checked to be a regular file, so a path swapped for a fifo or a folder
/// between the two calls is still refused (a swapped link fails the open itself). `Err` is the message after
/// `harness: error: `.
fn open_store(path: &Path, write: bool) -> Result<fs::File, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err("the goal store does not exist".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => return Err("the goal store does not exist".to_string()),
        Err(_) => return Err("the goal store cannot be opened".to_string()),
        Ok(m) if !m.file_type().is_file() => return Err("the goal store is not a regular file".to_string()),
        Ok(_) => {}
    }
    let mut o = fs::OpenOptions::new();
    o.read(true);
    if write {
        o.append(true);
    }
    let f = o.custom_flags(OPEN_NOFOLLOW_NONBLOCK).open(path).map_err(|_| "the goal store cannot be opened".to_string())?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err("the goal store is not a regular file".to_string());
    }
    Ok(f)
}

/// One whole line plus newline, then sync; on a write or sync error the file is cut back to its old length
/// before the caller reports the failure.
fn append_line(f: &mut fs::File, line: &str) -> Result<(), ()> {
    let Ok(old_len) = f.metadata().map(|m| m.len()) else {
        return Err(());
    };
    let mut buf = line.as_bytes().to_vec();
    buf.push(b'\n');
    if f.write_all(&buf).and_then(|()| f.sync_all()).is_err() {
        let _ = f.set_len(old_len);
        return Err(());
    }
    Ok(())
}

// ---------------------------------------------------------------- flags

/// The `--flag value` parser of add, close, show and check: each listed flag at most once, one value (a value
/// that starts with `--` is taken as the value; `--testmode` and its word were taken by the global parser).
fn take_value_flags(a: &[String], allowed: &[&str]) -> Result<std::collections::HashMap<String, String>, String> {
    let mut m = std::collections::HashMap::new();
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let Some(name) = arg.strip_prefix("--") else {
            return Err("an unexpected argument".to_string());
        };
        if !allowed.contains(&name) {
            return Err("unknown option".to_string());
        }
        let Some(v) = it.next() else {
            return Err("a flag is missing its value".to_string());
        };
        if m.insert(name.to_string(), v.clone()).is_some() {
            return Err("a flag was given twice".to_string());
        }
    }
    Ok(m)
}

/// The list parser: `--store <file>` and the valueless `--open`.
fn take_list_flags(a: &[String]) -> Result<(Option<String>, bool), String> {
    let mut store: Option<String> = None;
    let mut open = false;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--open" => {
                if open {
                    return Err("a flag was given twice".to_string());
                }
                open = true;
            }
            "--store" => {
                let Some(v) = it.next() else {
                    return Err("a flag is missing its value".to_string());
                };
                if store.replace(v.clone()).is_some() {
                    return Err("a flag was given twice".to_string());
                }
            }
            s if s.starts_with("--") => return Err("unknown option".to_string()),
            _ => return Err("an unexpected argument".to_string()),
        }
    }
    Ok((store, open))
}

fn store_path(v: &str) -> Result<PathBuf, String> {
    let p = PathBuf::from(v);
    if !p.is_absolute() {
        return Err("--store must be an absolute path".to_string());
    }
    Ok(p)
}

/// The privacy scan of a new row's fields, in the stated order: a hit names the field and the classes.
fn scan_hit(ctx: &crate::scan::Ctx, surface: &str, fields: &[(&str, &str)]) -> Option<String> {
    for (name, value) in fields {
        let hits = crate::scan::scan_text(ctx, surface, value);
        if !hits.is_empty() {
            let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
            return Some(format!("field {name} has a privacy hit ({})", classes.join(", ")));
        }
    }
    None
}

/// The privacy list, before any row is scanned or any store byte is read: without it the verb fails closed.
fn privacy_list(read: bool) -> Result<crate::overlay::List, i32> {
    crate::overlay::load().map_err(|_| {
        fail(if read { "no privacy list configured: an unscanned store is never read" } else { "no privacy list configured: an unscanned row is never stored" })
    })
}

/// The store opened and locked (exclusive for a write, shared for a read), then linted. `Err` is the exit
/// code already reported.
fn locked_goals(store: &Path, write: bool, ctx: &crate::scan::Ctx) -> Result<(fs::File, Vec<Goal>), i32> {
    let file = match open_store(store, write) {
        Ok(f) => f,
        Err(m) => return Err(fail(&m)),
    };
    // One critical section from the read to the write: the id checks below cannot race another writer.
    let locked = if write { file.lock() } else { file.lock_shared() };
    if locked.is_err() {
        return Err(fail("the goal store cannot be locked"));
    }
    match read_store_locked(&file, ctx) {
        Ok(v) => Ok((file, v)),
        Err(m) => Err(fail(&m)),
    }
}

// ---------------------------------------------------------------- the verbs

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    2
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: goal refused: {m}");
    1
}

const TOKEN_RULE: &str = "id must be a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not . or ..";

/// `harness goal add --store <absolute-file> --id <id> --at <t> --by <tag> --title <text> --done-when <line>
/// --source <pointers>`: flags (2), store path (2), the field rules (1), the privacy list (2), the scan of
/// the new row (1), the store stat and open (2), the exclusive lock (2), the store lint (2), the repeated-id
/// check (1), then one appended, synced line. A refusal in any step before the write leaves the store
/// byte-identical and creates no file.
fn add_cmd(rest: &[String], testmode: bool) -> i32 {
    let o = match take_value_flags(rest, &["store", "id", "at", "by", "title", "done-when", "source"]) {
        Ok(o) => o,
        Err(m) => return fail(&m),
    };
    for k in ["store", "id", "at", "by", "title", "done-when", "source"] {
        if !o.contains_key(k) {
            return fail(&format!("--{k} is required"));
        }
    }
    let store = match store_path(&o["store"]) {
        Ok(p) => p,
        Err(m) => return fail(&m),
    };
    let row = AddRow { id: o["id"].clone(), at: o["at"].clone(), by: o["by"].clone(), title: o["title"].clone(), done_when: o["done-when"].clone(), source: o["source"].clone() };
    if let Some(m) = add_row_problem(&row) {
        return refuse(&m);
    }
    let list = match privacy_list(false) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    if let Some(m) = scan_hit(&ctx, "goal.add", &[("id", &row.id), ("at", &row.at), ("by", &row.by), ("title", &row.title), ("done_when", &row.done_when), ("source", &row.source)]) {
        return refuse(&m);
    }
    let (mut file, goals) = match locked_goals(&store, true, &ctx) {
        Ok(v) => v,
        Err(c) => return c,
    };
    if goals.iter().any(|g| g.id == row.id) {
        return refuse("id is already in the store");
    }
    match append_line(&mut file, &encode_add(&row)) {
        Ok(()) => {
            println!("goal added id={} state=open", row.id);
            0
        }
        Err(()) => fail("the goal store cannot be written"),
    }
}

/// `harness goal list --store <absolute-file> [--open]`: flags and store path (2), the privacy list (2), the
/// stat, the read-only open and the shared lock (2), the store lint (2), then the listing: one line per goal
/// in the order of its add row, then one count line. Ids, state words and counts only: no title, done_when,
/// source, ref or reason text is printed. list writes nothing.
fn list_cmd(rest: &[String], testmode: bool) -> i32 {
    let (store_flag, open_only) = match take_list_flags(rest) {
        Ok(v) => v,
        Err(m) => return fail(&m),
    };
    let Some(store_s) = store_flag else {
        return fail("--store is required");
    };
    let store = match store_path(&store_s) {
        Ok(p) => p,
        Err(m) => return fail(&m),
    };
    let list = match privacy_list(true) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let (_, goals) = match locked_goals(&store, false, &ctx) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let count = |s: &str| goals.iter().filter(|g| g.status == s).count();
    if open_only {
        for g in goals.iter().filter(|g| g.status == "open") {
            println!("goal id={} state=open", g.id);
        }
        println!("goal open={}", count("open"));
    } else {
        for g in &goals {
            println!("goal id={} state={}", g.id, g.status);
        }
        println!("goal open={} met={} dropped={}", count("open"), count("met"), count("dropped"));
    }
    0
}

/// `harness goal show --store <absolute-file> --id <id>`: flags (2), store path (2), the id rule (1), the
/// privacy list (2), the scan of the id (1: it is printed), the stat, the read-only open and the shared lock
/// (2), the store lint (2), then the goal's lines. Unlike `owed list`, `show` prints the stored text; the
/// rows were scanned on write and are scanned again on this read.
fn show_cmd(rest: &[String], testmode: bool) -> i32 {
    let o = match take_value_flags(rest, &["store", "id"]) {
        Ok(o) => o,
        Err(m) => return fail(&m),
    };
    for k in ["store", "id"] {
        if !o.contains_key(k) {
            return fail(&format!("--{k} is required"));
        }
    }
    let store = match store_path(&o["store"]) {
        Ok(p) => p,
        Err(m) => return fail(&m),
    };
    let id = o["id"].clone();
    if token_problem(&id, "id").is_some() {
        return refuse(TOKEN_RULE);
    }
    let list = match privacy_list(true) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    if let Some(m) = scan_hit(&ctx, "goal.show", &[("id", &id)]) {
        return refuse(&m);
    }
    let (_, goals) = match locked_goals(&store, false, &ctx) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let Some(g) = goals.iter().find(|g| g.id == id) else {
        println!("goal refused id={id} reason=unknown");
        return 1;
    };
    println!("goal id={} state={}", g.id, g.status);
    println!("title: {}", g.title);
    println!("done_when: {}", g.done_when);
    println!("added {} by {} source: {}", g.add_at, g.added_by, g.source);
    if let Some(c) = &g.close {
        if c.status == "met" {
            println!("closed {} by {} ref: {}", c.at, c.by, c.reference);
        } else {
            println!("closed {} by {} reason: {}", c.at, c.by, c.reason);
        }
    }
    0
}

/// `harness goal close --store <absolute-file> --id <id> --at <t> --by <tag> --status <met|dropped>
/// (--ref <pointers> | --reason <text>)`: flags (2), store path (2), the close-row rules (1), the privacy
/// list (2), the scan of the new row (1), the stat, open, exclusive lock and store lint of add (2), then the
/// id checks (1) and one appended, synced close row. `met` carries proof pointers: a close of `met` without a
/// valid `--ref` is refused (WAITING-229).
fn close_cmd(rest: &[String], testmode: bool) -> i32 {
    let o = match take_value_flags(rest, &["store", "id", "at", "by", "status", "ref", "reason"]) {
        Ok(o) => o,
        Err(m) => return fail(&m),
    };
    for k in ["store", "id", "at", "by", "status"] {
        if !o.contains_key(k) {
            return fail(&format!("--{k} is required"));
        }
    }
    let store = match store_path(&o["store"]) {
        Ok(p) => p,
        Err(m) => return fail(&m),
    };
    let row = CloseRow { id: o["id"].clone(), at: o["at"].clone(), by: o["by"].clone(), status: o["status"].clone(), reference: o.get("ref").cloned().unwrap_or_default(), reason: o.get("reason").cloned().unwrap_or_default() };
    if let Some(m) = close_row_problem(&row) {
        return refuse(&m);
    }
    let list = match privacy_list(false) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    if let Some(m) = scan_hit(&ctx, "goal.close", &[("id", &row.id), ("at", &row.at), ("by", &row.by), ("status", &row.status), ("ref", &row.reference), ("reason", &row.reason)]) {
        return refuse(&m);
    }
    let (mut file, goals) = match locked_goals(&store, true, &ctx) {
        Ok(v) => v,
        Err(c) => return c,
    };
    let Some(g) = goals.iter().find(|g| g.id == row.id) else {
        return refuse("id is not in the store");
    };
    if g.status != "open" {
        return refuse("id is already closed");
    }
    // Both times have one fixed format, so a string compare is a time compare. An open goal has one earlier
    // row, its add row: the last row of the id.
    if row.at.as_str() < g.add_at.as_str() {
        return refuse("at is before the time the id was added");
    }
    match append_line(&mut file, &encode_close(&row)) {
        Ok(()) => {
            println!("goal closed id={} state={}", row.id, row.status);
            0
        }
        Err(()) => fail("the goal store cannot be written"),
    }
}

/// `harness goal check --store <absolute-file> --id <id>`: flags (2), store path (2), the id rule (1, on
/// stderr, before the store is read), the privacy list (2), the scan of the id (1), the stat, the read-only
/// open and the shared lock (2), the store lint (2), then the verdict. A damaged store is exit 2: `check`
/// never prints `goal ok` for a store it could not read whole.
fn check_cmd(rest: &[String], testmode: bool) -> i32 {
    let o = match take_value_flags(rest, &["store", "id"]) {
        Ok(o) => o,
        Err(m) => return fail(&m),
    };
    for k in ["store", "id"] {
        if !o.contains_key(k) {
            return fail(&format!("--{k} is required"));
        }
    }
    let store = match store_path(&o["store"]) {
        Ok(p) => p,
        Err(m) => return fail(&m),
    };
    let id = o["id"].clone();
    if token_problem(&id, "id").is_some() {
        return refuse(TOKEN_RULE);
    }
    let list = match privacy_list(true) {
        Ok(l) => l,
        Err(c) => return c,
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    if let Some(m) = scan_hit(&ctx, "goal.check", &[("id", &id)]) {
        return refuse(&m);
    }
    let (_, goals) = match locked_goals(&store, false, &ctx) {
        Ok(v) => v,
        Err(c) => return c,
    };
    match goals.iter().find(|g| g.id == id) {
        Some(g) if g.status == "open" => {
            println!("goal ok id={id} state=open");
            0
        }
        Some(_) => {
            println!("goal refused id={id} reason=closed");
            1
        }
        None => {
            println!("goal refused id={id} reason=unknown");
            1
        }
    }
}

/// `harness goal add | list | show | close | check` (WAITING-228): the entry point, after the global
/// `--testmode` was taken. There is no `--ask` and no `--group` flag: the goal is the focus group, and ask
/// rows name it in the ledger (WAITING-232 A).
pub fn goal_cmd(a: &[String], testmode: bool) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{GOAL_USAGE}");
        return 2;
    };
    match sub.as_str() {
        "add" => add_cmd(rest, testmode),
        "list" => list_cmd(rest, testmode),
        "show" => show_cmd(rest, testmode),
        "close" => close_cmd(rest, testmode),
        "check" => check_cmd(rest, testmode),
        _ => {
            eprintln!("{GOAL_USAGE}");
            2
        }
    }
}
