//! `harness owed add | list | close` (std only, Unix; `docs/owed.md` is the contract). A promise in a
//! report to the operator is chat text no store holds, so a promised item can be lost (OW-1 of ask A15):
//! this module is the small store of owed items. The store file sits in the private overlay, named by
//! `--store <absolute-file>`, created once by hand as an empty file; no verb creates it, so a wrong path
//! cannot start a second store. No clock runs here: times arrive by `--at`. A refusal names a field and
//! a reason, no field value.
//! Exit codes: 0 ok; 1 a refused input; 2 usage, setup or a damaged store.

use crate::events::token_ok;
use crate::friction::{at_ok, jstr, proof_problem, P};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub const OWED_USAGE: &str = "usage: harness owed add --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --what <text> [--home <home>] | owed list --store <absolute-file> [--open] | owed close --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --status <done|dropped> (--ref <pointers> | --reason <text>)";

/// The store is read no further than this; one byte over is a damaged store (exit 2).
const STORE_CAP: u64 = 4 * 1024 * 1024;
/// `id` and `by` are at most this many bytes.
const TOKEN_CAP: usize = 64;
/// `what` and `reason` are at most this many bytes.
const LINE_CAP: usize = 500;
/// `home` is at most this many bytes.
const HOME_CAP: usize = 200;

// The open(2) flags std exposes no constants for (the quarantine.rs pattern): per-platform values read
// from each platform's own fcntl.h (Linux asm-generic/fcntl.h, macOS sys/fcntl.h). O_NOFOLLOW makes a
// symlink store fail the open itself (a swapped link cannot be followed past the stat check); O_NONBLOCK
// makes a fifo fail at once instead of blocking. A platform with no listed value fails the build here.
#[cfg(target_os = "linux")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0o400000 | 0o4000;
#[cfg(target_os = "macos")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0x0100 | 0x0004;

/// One add row: `{"op":"add","id":..,"at":..,"by":..,"what":..,"home":..}`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AddRow {
    pub id: String,
    pub at: String,
    pub by: String,
    pub what: String,
    pub home: String,
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

/// One owed item as the store holds it: added once, `open` until its close row sets `done` or `dropped`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: String,
    pub add_at: String,
    pub status: String,
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

fn home_problem(s: &str) -> Option<String> {
    const BAD: &str = "home must be empty, ticket:<id>, ledger:<row> or pr:<owner>/<repo>#<n>, at most 200 bytes";
    if s.is_empty() {
        return None;
    }
    if s.len() > HOME_CAP {
        return Some(BAD.to_string());
    }
    if let Some(id) = s.strip_prefix("ticket:") {
        return if token_ok(id) && id.len() <= TOKEN_CAP { None } else { Some(BAD.to_string()) };
    }
    if let Some(row) = s.strip_prefix("ledger:") {
        return if (1..=32).contains(&row.len()) && row.bytes().all(|b| b.is_ascii_alphanumeric()) { None } else { Some(BAD.to_string()) };
    }
    if let Some(rest) = s.strip_prefix("pr:") {
        let Some((slug, n)) = rest.split_once('#') else { return Some(BAD.to_string()) };
        let Some((owner, repo)) = slug.split_once('/') else { return Some(BAD.to_string()) };
        let name = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
        return if name(owner) && name(repo) && !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) { None } else { Some(BAD.to_string()) };
    }
    Some(BAD.to_string())
}

fn status_problem(s: &str) -> Option<String> {
    if s == "done" || s == "dropped" {
        None
    } else {
        Some("status must be done or dropped".to_string())
    }
}

/// `ref` holds `; `-separated proof pointers (the inbox proof grammar, checked by the one implementation).
fn ref_problem(s: &str) -> Option<String> {
    proof_problem(s).map(|_| "ref is not a valid pointer list".to_string())
}

/// Pairing, judged on values: an empty `ref` or `reason` counts as not given.
fn pairing_problem(status: &str, r: &str, reason: &str) -> Option<String> {
    match status {
        "done" if r.is_empty() || !reason.is_empty() => Some("status done needs --ref and takes no --reason".to_string()),
        "dropped" if reason.is_empty() || !r.is_empty() => Some("status dropped needs --reason and takes no --ref".to_string()),
        _ => None,
    }
}

/// The add-row rules, in the order they are checked.
fn add_row_problem(r: &AddRow) -> Option<String> {
    token_problem(&r.id, "id")
        .or_else(|| at_problem(&r.at))
        .or_else(|| token_problem(&r.by, "by"))
        .or_else(|| line_problem(&r.what, "what"))
        .or_else(|| home_problem(&r.home))
}

/// The close-row rules, in the order they are checked: id, at, by, status, pairing, then ref (done) or
/// reason (dropped).
fn close_row_problem(r: &CloseRow) -> Option<String> {
    token_problem(&r.id, "id")
        .or_else(|| at_problem(&r.at))
        .or_else(|| token_problem(&r.by, "by"))
        .or_else(|| status_problem(&r.status))
        .or_else(|| pairing_problem(&r.status, &r.reference, &r.reason))
        .or_else(|| if r.status == "done" { ref_problem(&r.reference) } else { line_problem(&r.reason, "reason") })
}

// ---------------------------------------------------------------- encode and parse

/// The add row as one line without its newline, keys in the order this module writes.
fn encode_add(r: &AddRow) -> String {
    format!(
        "{{\"op\":\"add\",\"id\":{},\"at\":{},\"by\":{},\"what\":{},\"home\":{}}}",
        jstr(&r.id),
        jstr(&r.at),
        jstr(&r.by),
        jstr(&r.what),
        jstr(&r.home)
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
            if !same_set(&mut vec!["op", "id", "at", "by", "what", "home"]) {
                return Err("the keys are not the key set of its op".to_string());
            }
            let row = AddRow { id: get("id").unwrap(), at: get("at").unwrap(), by: get("by").unwrap(), what: get("what").unwrap(), home: get("home").unwrap() };
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

/// The fields of a stored row as (name, value) pairs in key order: the list the privacy scan walks.
fn row_fields(row: &Row) -> [(&'static str, &str); 6] {
    match row {
        Row::Add(r) => [("id", r.id.as_str()), ("at", r.at.as_str()), ("by", r.by.as_str()), ("what", r.what.as_str()), ("home", r.home.as_str()), ("", "")],
        Row::Close(r) => [("id", r.id.as_str()), ("at", r.at.as_str()), ("by", r.by.as_str()), ("status", r.status.as_str()), ("ref", r.reference.as_str()), ("reason", r.reason.as_str())],
    }
}

// ---------------------------------------------------------------- the store lint

/// The one store criterion for add, list and close: cap, utf-8, a torn last line, the first bad row,
/// then the first privacy hit, over the bytes of the locked handle. `Err` is the message after
/// `harness: error: `; `Ok` is the items in add-row order.
fn read_store_locked(f: &fs::File, ctx: &crate::scan::Ctx) -> Result<Vec<Item>, String> {
    let mut buf = Vec::new();
    f.take(STORE_CAP + 1).read_to_end(&mut buf).map_err(|_| "the owed store cannot be opened".to_string())?;
    if buf.len() as u64 > STORE_CAP {
        return Err(format!("the owed store is over {STORE_CAP} bytes"));
    }
    let text = String::from_utf8(buf).map_err(|_| "the owed store is not utf-8".to_string())?;
    if !text.is_empty() && !text.ends_with('\n') {
        let n = text.bytes().filter(|b| *b == b'\n').count() + 1;
        return Err(format!("the owed store has a torn last line at line {n}"));
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
    let mut items: Vec<Item> = Vec::new();
    for (line, row) in &rows {
        match row {
            Row::Add(a) => {
                if items.iter().any(|it| it.id == a.id) {
                    problems.push((*line, "id is added twice".to_string()));
                } else {
                    items.push(Item { id: a.id.clone(), add_at: a.at.clone(), status: "open".to_string() });
                }
            }
            Row::Close(c) => {
                let Some(pos) = items.iter().position(|it| it.id == c.id) else {
                    problems.push((*line, "close names no earlier added id".to_string()));
                    continue;
                };
                if items[pos].status != "open" {
                    problems.push((*line, "id is closed twice".to_string()));
                } else if c.at.as_str() < items[pos].add_at.as_str() {
                    problems.push((*line, "close is before the time the id was added".to_string()));
                } else {
                    items[pos].status = c.status.clone();
                }
            }
        }
    }
    // The first bad line wins; a field problem precedes a cross problem on the same line.
    problems.sort_by_key(|(n, _)| *n);
    if let Some((n, m)) = problems.first() {
        return Err(format!("the owed store has a bad row at line {n}: {m}"));
    }
    for (n, row) in &rows {
        for (name, value) in row_fields(row) {
            let hits = crate::scan::scan_text(ctx, "owed.store", value);
            if !hits.is_empty() {
                let classes: Vec<&str> = hits.iter().map(|h| h.class).collect();
                return Err(format!("the owed store has a privacy hit at line {n} in field {name} ({})", classes.join(", ")));
            }
        }
    }
    Ok(items)
}

// ---------------------------------------------------------------- the store file

/// Steps (6) and (7) of add: a no-follow stat of the store path (a linked parent folder is accepted,
/// only the last part must not be a link), then an open with O_NOFOLLOW and O_NONBLOCK, read-only for
/// list, read and append for add and close, no create. The handle is re-checked to be a regular file,
/// so a path swapped for a link between the two calls is still caught. `Err` is the message after
/// `harness: error: `.
fn open_store(path: &Path, write: bool) -> Result<fs::File, String> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err("the owed store does not exist".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotADirectory => return Err("the owed store does not exist".to_string()),
        Err(_) => return Err("the owed store cannot be opened".to_string()),
        Ok(m) if !m.file_type().is_file() => return Err("the owed store is not a regular file".to_string()),
        Ok(_) => {}
    }
    let mut o = fs::OpenOptions::new();
    o.read(true);
    if write {
        o.append(true);
    }
    let f = o.custom_flags(OPEN_NOFOLLOW_NONBLOCK).open(path).map_err(|_| "the owed store cannot be opened".to_string())?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err("the owed store is not a regular file".to_string());
    }
    Ok(f)
}

/// Step (11): one whole line plus newline, then sync; on a write or sync error the file is cut back to
/// its old length before the caller reports the failure.
fn append_line(f: &mut fs::File, line: &str) -> Result<(), ()> {
    let old_len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = line.as_bytes().to_vec();
    buf.push(b'\n');
    if f.write_all(&buf).and_then(|()| f.sync_all()).is_err() {
        let _ = f.set_len(old_len);
        return Err(());
    }
    Ok(())
}

// ---------------------------------------------------------------- flags

/// The `--flag value` parser of add and close: each listed flag at most once, one value (a value that
/// starts with `--` is taken as the value; `--testmode` and its word were taken by the global parser).
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

// ---------------------------------------------------------------- the verbs

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    2
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: owed refused: {m}");
    1
}

/// `harness owed add --store <absolute-file> --id <id> --at <t> --by <tag> --what <text> [--home <home>]`:
/// flags (2), store path (2), field rules (1), the privacy list (2), the scan of the new row (1), the
/// store stat and open (2), the exclusive lock (2), the store lint (2), the duplicate-id check (1), then
/// one appended, synced line. A refusal in any step before the write leaves the store byte-identical and
/// creates no file.
fn add_cmd(rest: &[String], testmode: bool) -> i32 {
    let o = match take_value_flags(rest, &["store", "id", "at", "by", "what", "home"]) {
        Ok(o) => o,
        Err(m) => return fail(&m),
    };
    for k in ["store", "id", "at", "by", "what"] {
        if !o.contains_key(k) {
            return fail(&format!("--{k} is required"));
        }
    }
    let store = match store_path(&o["store"]) {
        Ok(p) => p,
        Err(m) => return fail(&m),
    };
    let row = AddRow { id: o["id"].clone(), at: o["at"].clone(), by: o["by"].clone(), what: o["what"].clone(), home: o.get("home").cloned().unwrap_or_default() };
    if let Some(m) = add_row_problem(&row) {
        return refuse(&m);
    }
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned row is never stored"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    if let Some(m) = scan_hit(&ctx, "owed.add", &[("id", &row.id), ("at", &row.at), ("by", &row.by), ("what", &row.what), ("home", &row.home)]) {
        return refuse(&m);
    }
    let mut file = match open_store(&store, true) {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    if file.lock().is_err() {
        return fail("the owed store cannot be locked");
    }
    let items = match read_store_locked(&file, &ctx) {
        Ok(v) => v,
        Err(m) => return fail(&m),
    };
    if items.iter().any(|it| it.id == row.id) {
        return refuse("id is already in the store");
    }
    match append_line(&mut file, &encode_add(&row)) {
        Ok(()) => {
            println!("stored id={}", row.id);
            0
        }
        Err(()) => fail("the owed store cannot be written"),
    }
}

/// `harness owed list --store <absolute-file> [--open]`: flags and store path (2), the privacy list (2),
/// the stat, the read-only open and the shared lock (2), the store lint (2), then the listing: one line
/// per added id in the order of its add row, then one count line. Ids, status words and counts only: no
/// what, by, home, ref or reason text is printed. list writes nothing.
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
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned row is never stored"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let file = match open_store(&store, false) {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    if file.lock_shared().is_err() {
        return fail("the owed store cannot be locked");
    }
    let items = match read_store_locked(&file, &ctx) {
        Ok(v) => v,
        Err(m) => return fail(&m),
    };
    let count = |s: &str| items.iter().filter(|it| it.status == s).count();
    if open_only {
        for it in items.iter().filter(|it| it.status == "open") {
            println!("owed id={} status=open", it.id);
        }
        println!("owed open={}", count("open"));
    } else {
        for it in &items {
            println!("owed id={} status={}", it.id, it.status);
        }
        println!("owed open={} done={} dropped={}", count("open"), count("done"), count("dropped"));
    }
    0
}

/// `harness owed close --store <absolute-file> --id <id> --at <t> --by <tag> --status <done|dropped>
/// (--ref <pointers> | --reason <text>)`: flags (2), store path (2), the close-row rules (1), the
/// privacy list (2), the scan of the new row (1), the stat, open, lock and store lint of add (2), then
/// the id checks (1) and one appended, synced close row.
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
    let list = match crate::overlay::load() {
        Ok(l) => l,
        Err(_) => return fail("no privacy list configured: an unscanned row is never stored"),
    };
    let ctx = crate::scan::Ctx { list: &list, testmode };
    if let Some(m) = scan_hit(&ctx, "owed.close", &[("id", &row.id), ("at", &row.at), ("by", &row.by), ("status", &row.status), ("ref", &row.reference), ("reason", &row.reason)]) {
        return refuse(&m);
    }
    let mut file = match open_store(&store, true) {
        Ok(f) => f,
        Err(m) => return fail(&m),
    };
    if file.lock().is_err() {
        return fail("the owed store cannot be locked");
    }
    let items = match read_store_locked(&file, &ctx) {
        Ok(v) => v,
        Err(m) => return fail(&m),
    };
    // Both times have one fixed format, so a string compare is a time compare.
    let Some(it) = items.iter().find(|it| it.id == row.id) else {
        return refuse("id is not in the store");
    };
    if it.status != "open" {
        return refuse("id is already closed");
    }
    if row.at.as_str() < it.add_at.as_str() {
        return refuse("at is before the time the id was added");
    }
    match append_line(&mut file, &encode_close(&row)) {
        Ok(()) => {
            println!("closed id={} status={}", row.id, row.status);
            0
        }
        Err(()) => fail("the owed store cannot be written"),
    }
}

/// `harness owed add | list | close` (OW-1): the entry point, after the global `--testmode` was taken.
pub fn owed_cmd(a: &[String], testmode: bool) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{OWED_USAGE}");
        return 2;
    };
    match sub.as_str() {
        "add" => add_cmd(rest, testmode),
        "list" => list_cmd(rest, testmode),
        "close" => close_cmd(rest, testmode),
        _ => {
            eprintln!("{OWED_USAGE}");
            2
        }
    }
}
