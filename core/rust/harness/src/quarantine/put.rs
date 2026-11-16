//! `harness quarantine put` (ticket quarantine-put): move ONE item onto the shelf under `<quarantine-dir>/@shelf`,
//! with a label written before the move and completed after it, or refuse. Put removes nothing.
//! Steps 1 to 6 write nothing (usage, the two dirs, the source, placement, the live check, the device and the walk);
//! step 7 makes the shelf places and takes the lock; step 8 is entry, label, journal intent, move, second walk,
//! final label, journal line. The sweep helpers in the parent module are reused and stay unchanged.

use super::{
    cache_tag_ok, err2, err3, hex, num_ok, open_dir_no_follow, real_device, rename_verified, resolve_base_dir, usage_err,
    verified_parent, Stop, CACHEDIR_TAG, O_NONBLOCK, O_NOFOLLOW,
};
use crate::events::token_ok;
use crate::friction_event::civil_from_days;
use crate::launcher::sha256_hex;
use crate::manifest::{self, Target};
use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, FileExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const KINDS: [&str; 7] = ["worktree", "gate-clone", "ignored-leftover", "seat-old", "findings-rejected", "smoke-fail", "hand"];
/// Fixed cap on entries visited by the walk. Over it: exit 3, nothing written.
const MAX_VISITED: usize = 1_000_000;
/// Fixed cap on walk depth below the item. Over it: exit 3, nothing written.
const MAX_DEPTH: usize = 64;
/// Highest entry number tried for one kind and day.
const MAX_ENTRY_N: u32 = 10_000;
const MS_PER_DAY: u64 = 86_400_000;
const GIT_MSG: &str = "the item holds a git repository that put cannot audit yet; nothing was moved";

const FLAGS: [&str; 8] =
    ["--quarantine-dir", "--manifest-dir", "--kind", "--why", "--by", "--ticket", "--repo", "--ts-ms"];

/// One parsed `quarantine put` command line. The path is a `PathBuf` so a library caller can pass a name that is
/// not UTF-8. `ticket` and `repo` are empty when not given.
pub struct PutArgs {
    pub path: PathBuf,
    pub quarantine_dir: PathBuf,
    pub manifest_dir: PathBuf,
    pub kind: String,
    pub why: String,
    pub by: String,
    pub ticket: String,
    pub repo: String,
    pub ts_ms: u64,
}

/// The points where a test step may stop `put_with_step`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Point {
    /// After the intent line, right before the verified parent of the move.
    AfterIntent,
    /// Right after the rename, before the second walk.
    AfterMove,
}

/// Parse the argument list that follows `put` (no filesystem access).
pub fn parse_put(a: &[String]) -> Result<PutArgs, Stop> {
    let mut path: Option<&str> = None;
    let mut vals: [Option<&str>; 8] = [None; 8];
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if let Some(i) = FLAGS.iter().position(|f| *f == arg.as_str()) {
            let val = it.next().ok_or_else(|| usage_err("an option is missing its value"))?;
            if vals[i].replace(val.as_str()).is_some() {
                return Err(usage_err("an option was given twice"));
            }
        } else if arg.starts_with("--") {
            return Err(usage_err("unknown option"));
        } else if path.replace(arg.as_str()).is_some() {
            return Err(usage_err("more than one path given"));
        }
    }
    let need = |i: usize| vals[i].ok_or_else(|| usage_err(&format!("{} is required", FLAGS[i])));
    let path = path.ok_or_else(|| usage_err("no path given"))?;
    if vals[5] == Some("") || vals[6] == Some("") {
        return Err(usage_err("--ticket and --repo must be plain tokens"));
    }
    let ts_ms = match vals[7] {
        Some(s) => {
            if !num_ok(s) {
                return Err(usage_err("--ts-ms must be a non-negative integer"));
            }
            s.parse::<u64>().map_err(|_| usage_err("--ts-ms out of range"))?
        }
        None => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .map_err(|_| err2("the clock is before the epoch"))?,
    };
    let args = PutArgs {
        path: PathBuf::from(path),
        quarantine_dir: PathBuf::from(need(0)?),
        manifest_dir: PathBuf::from(need(1)?),
        kind: need(2)?.to_string(),
        why: need(3)?.to_string(),
        by: need(4)?.to_string(),
        ticket: vals[5].unwrap_or("").to_string(),
        repo: vals[6].unwrap_or("").to_string(),
        ts_ms,
    };
    check_values(&args)?;
    Ok(args)
}

/// Step 1: the value shapes (a library caller reaches this too).
fn check_values(a: &PutArgs) -> Result<(), Stop> {
    if !KINDS.contains(&a.kind.as_str()) {
        return Err(usage_err("--kind is not one of the listed kinds"));
    }
    let why_ok = !a.why.is_empty()
        && a.why.len() <= 200
        && a.why.bytes().all(|b| (0x20..=0x7e).contains(&b) && b != b'"' && b != b'\\');
    if !why_ok {
        return Err(usage_err("--why must be 1 to 200 printable ASCII bytes with no double quote and no backslash"));
    }
    if !token_ok(&a.by) {
        return Err(usage_err("--by must be a plain token (lowercase letters, digits, '-', '_', '.')"));
    }
    for (flag, v) in [("--ticket", &a.ticket), ("--repo", &a.repo)] {
        if !v.is_empty() && !token_ok(v) {
            return Err(usage_err(&format!("{flag} must be a plain token (lowercase letters, digits, '-', '_', '.')")));
        }
    }
    date_of(a.ts_ms)?;
    Ok(())
}

/// The UTC day of `ts_ms` as `YYYYMMDD`.
fn date_of(ts_ms: u64) -> Result<String, Stop> {
    let (y, m, d) = civil_from_days((ts_ms / MS_PER_DAY) as i64);
    if y > 9999 {
        return Err(usage_err("--ts-ms is past the year 9999"));
    }
    Ok(format!("{y:04}{m:02}{d:02}"))
}

/// `harness quarantine put` with the real device probe.
pub fn put(a: &PutArgs) -> Result<String, Stop> {
    put_with_device_probe(a, &real_device)
}

/// TEST SEAM, like `sweep_with_device_probe`: the device ids come from `device_of` instead of the filesystem, so the
/// cross-device refusal runs in CI without a second file system.
pub fn put_with_device_probe(a: &PutArgs, device_of: &dyn Fn(&Path) -> Option<u64>) -> Result<String, Stop> {
    put_with_step(a, device_of, &|_| Ok(()))
}

/// TEST SEAM: `step` runs at each `Point`; an `Err` ends put there with that stop and nothing more is written
/// (the shape of a crash). `put` calls it with a step that does nothing.
pub fn put_with_step(
    a: &PutArgs,
    device_of: &dyn Fn(&Path) -> Option<u64>,
    step: &dyn Fn(Point) -> Result<(), Stop>,
) -> Result<String, Stop> {
    // (1) value shapes, (2) the two dirs
    check_values(a)?;
    let date = date_of(a.ts_ms)?;
    let qd = check_dir("--quarantine-dir", &a.quarantine_dir)?;
    let md = check_dir("--manifest-dir", &a.manifest_dir)?;
    let q_up = ids_up(&qd)?;
    let m_up = ids_up(&md)?;
    if q_up.contains(&m_up[0]) || m_up.contains(&q_up[0]) || qd.starts_with(&md) || md.starts_with(&qd) {
        return Err(err2("the quarantine dir and the manifest dir must lie outside each other"));
    }
    // (3) the source, (4) placement on identities
    let src = check_source(&a.path)?;
    let src_id = (src.dev, src.ino);
    let src_up = ids_up(&src.parent)?;
    for (up, what) in [(&q_up, "quarantine dir"), (&m_up, "manifest dir")] {
        if up.contains(&src_id) {
            return Err(err2(&format!("the source equals or holds the {what}")));
        }
        if src_up.contains(&up[0]) {
            return Err(err2(&format!("the source lies inside the {what}")));
        }
    }
    // (5) the live check
    live_check(&md, &src, src_id, &src_up)?;
    // (6) the device, then the walk
    let q_dev = device_of(&qd).ok_or_else(|| err3("the quarantine dir cannot be examined; nothing was moved"))?;
    for p in [&src.full, &src.parent] {
        let dev = device_of(p).ok_or_else(|| err3("the source cannot be examined; nothing was moved"))?;
        if dev != q_dev {
            return Err(err3("cross-device: the item and the quarantine dir are on different file systems; nothing was moved"));
        }
    }
    let before = walk_item(&src.full, true)?;
    // (7) the shelf
    let shelf = open_shelf(&qd, &date)?;
    settle_stopped(&shelf.journal, &shelf.entries)?;
    // (8) entry, label, journal intent
    let (n, entry_path) = make_entry(&shelf.date_dir, &a.kind)?;
    let entry = format!("{date}/{}-{n}", a.kind);
    let entry_dir = open_dir_no_follow(&entry_path)
        .ok_or_else(|| err3("the entry folder cannot be opened; nothing was moved; the empty entry folder stays"))?;
    let mut from = src.parent.as_os_str().as_bytes().to_vec();
    from.push(b'/');
    from.extend_from_slice(src.name.as_bytes());
    let mut label = Label {
        entry: entry.clone(),
        kind: a.kind.clone(),
        from_hex: hex(&from),
        src_dev: src.dev,
        src_ino: src.ino,
        ticket: a.ticket.clone(),
        repo: a.repo.clone(),
        by: a.by.clone(),
        at_ms: a.ts_ms,
        why: a.why.clone(),
        files: before.files,
        bytes: before.bytes,
        skipped_build: before.skipped_build,
        before: before.fingerprint.clone(),
        after: String::new(),
        state: "intent".to_string(),
        judgeable: "yes".to_string(),
        reason: String::new(),
    };
    let intent = label.bytes();
    write_new(&entry_path.join("label.json"), &intent)
        .map_err(|_| err3("the label could not be written; nothing was moved; the entry folder stays"))?;
    let intent_line = format!(
        "{{\"v\":1,\"op\":\"intent\",\"entry\":\"{entry}\",\"src_dev\":{},\"src_ino\":{},\"label_sha\":\"{}\"}}\n",
        src.dev,
        src.ino,
        sha256_hex(&intent)
    );
    journal_append(&shelf.journal, &date, &intent_line)
        .map_err(|_| err3("the journal line could not be written; nothing was moved; the entry folder and its intent label stay"))?;
    step(Point::AfterIntent)?;
    // (d) the move: the verified parent, the same source, the name free in the entry, then the rename
    let Some(parent) = verified_parent(&src.chain) else {
        return Err(void_exit(&mut label, &entry_path, &shelf.journal, &date));
    };
    let same = fs::symlink_metadata(&src.full).map(|m| m.dev() == src.dev && m.ino() == src.ino).unwrap_or(false);
    if !same || fs::symlink_metadata(entry_path.join("item")).is_ok() {
        return Err(void_exit(&mut label, &entry_path, &shelf.journal, &date));
    }
    if rename_verified(&parent, &src.name, &entry_dir, "item").is_err() {
        return Err(void_exit(&mut label, &entry_path, &shelf.journal, &date));
    }
    step(Point::AfterMove)?;
    // (e) the second walk and the final label, (f) the moved line
    let moved_fail = || {
        err3("the item was moved but its label or journal line could not be finished; the label may still be in state intent")
    };
    let after = walk_item(&entry_path.join("item"), false).map_err(|_| moved_fail())?;
    let changed = after.fingerprint != before.fingerprint;
    label.after = after.fingerprint;
    label.state = "moved".to_string();
    if changed {
        label.judgeable = "no".to_string();
        label.reason = "changed-during-put".to_string();
    }
    let done = label.bytes();
    replace_label(&entry_path, &done).map_err(|_| moved_fail())?;
    let moved_line = format!("{{\"v\":1,\"op\":\"moved\",\"entry\":\"{entry}\",\"label_sha\":\"{}\"}}\n", sha256_hex(&done));
    journal_append(&shelf.journal, &date, &moved_line).map_err(|_| moved_fail())?;
    if changed {
        return Err(err3("the item changed while it was put; it is on the shelf with a label that says so"));
    }
    Ok(format!("put=done entry={entry} files={} bytes={}", before.files, before.bytes))
}

/// The move did not happen: label state void, one void line, and the stop.
fn void_exit(label: &mut Label, entry_path: &Path, journal: &Path, date: &str) -> Stop {
    label.state = "void".to_string();
    let bytes = label.bytes();
    let line = format!("{{\"v\":1,\"op\":\"void\",\"entry\":\"{}\",\"label_sha\":\"{}\"}}\n", label.entry, sha256_hex(&bytes));
    let done = replace_label(entry_path, &bytes).and_then(|()| journal_append(journal, date, &line));
    if done.is_ok() {
        err3("the move did not happen; nothing was moved; the entry is marked void")
    } else {
        err3("the move did not happen; nothing was moved; the entry folder stays and its label or journal line may not say void")
    }
}

// ---------------------------------------------------------------- steps 2 to 5

/// The raw bytes hold no `.` or `..` component and do not end in `/` (a trailing `/.` is a `.` component).
fn spelled_ok(raw: &[u8]) -> bool {
    !raw.ends_with(b"/") && !raw.split(|b| *b == b'/').any(|s| s == b"." || s == b"..")
}

fn check_dir(flag: &str, p: &Path) -> Result<PathBuf, Stop> {
    if !p.is_absolute() || !spelled_ok(p.as_os_str().as_bytes()) {
        return Err(err2(&format!("{flag} must be an absolute path with no `.` or `..` component and no trailing slash")));
    }
    resolve_base_dir(flag, p)
}

type Id = (u64, u64);

/// The (dev, ino) of a canonical path and of each ancestor, the path first (no-follow stats).
fn ids_up(p: &Path) -> Result<Vec<Id>, Stop> {
    p.ancestors()
        .map(|a| {
            fs::symlink_metadata(a)
                .map(|m| (m.dev(), m.ino()))
                .map_err(|_| err3("a folder above an argument cannot be examined; nothing was written"))
        })
        .collect()
}

/// The source: its canonical parent, its name, its identity and the chain for the later re-check.
struct Src {
    name: OsString,
    parent: PathBuf,
    full: PathBuf,
    dev: u64,
    ino: u64,
    chain: Vec<(PathBuf, u64, u64)>,
}

fn check_source(p: &Path) -> Result<Src, Stop> {
    if !p.is_absolute() || !spelled_ok(p.as_os_str().as_bytes()) {
        return Err(err2("the source must be an absolute path with no `.` or `..` component and no trailing slash"));
    }
    let m = match fs::symlink_metadata(p) {
        Ok(m) => m,
        Err(e) if e.kind() == ErrorKind::NotFound => return Err(err2("the source does not exist")),
        Err(_) => return Err(err2("the source cannot be examined")),
    };
    if m.file_type().is_symlink() {
        return Err(err2("the source is a link (a link is refused)"));
    }
    if !m.is_dir() && !m.is_file() {
        return Err(err2("the source is not a folder or a regular file"));
    }
    let name = p.file_name().ok_or_else(|| err2("the source must not be the filesystem root"))?.to_os_string();
    let parent_raw = p.parent().ok_or_else(|| err2("the source must not be the filesystem root"))?;
    let parent = parent_raw.canonicalize().map_err(|_| err2("the parent folder of the source cannot be resolved"))?;
    let mut chain: Vec<(PathBuf, u64, u64)> = parent
        .ancestors()
        .map(|a| {
            fs::symlink_metadata(a)
                .map(|m| (a.to_path_buf(), m.dev(), m.ino()))
                .map_err(|_| err3("a folder above the source cannot be examined; nothing was written"))
        })
        .collect::<Result<_, Stop>>()?;
    chain.reverse();
    let full = parent.join(&name);
    Ok(Src { name, parent, full, dev: m.dev(), ino: m.ino(), chain })
}

fn refuse_manifest(stem: &str) -> Stop {
    err2(&format!(
        "the manifest of ticket {stem} cannot be read whole, so put cannot prove the source is not live work; fix the manifest by hand"
    ))
}

/// Step 5: each `*.jsonl` file in the manifest dir is a manifest; the source must not equal, lie inside or hold a
/// live path entry of any of them.
fn live_check(md: &Path, src: &Src, src_id: Id, src_up: &[Id]) -> Result<(), Stop> {
    let rd = fs::read_dir(md).map_err(|_| err3("the manifest dir cannot be read; nothing was written"))?;
    for ent in rd {
        let ent = ent.map_err(|_| err3("the manifest dir cannot be read; nothing was written"))?;
        let name = ent.file_name();
        let Some(stem_raw) = name.as_bytes().strip_suffix(b".jsonl") else { continue };
        let ft = ent.file_type().map_err(|_| err3("the manifest dir cannot be read; nothing was written"))?;
        if ft.is_dir() {
            continue;
        }
        let stem = std::str::from_utf8(stem_raw)
            .ok()
            .filter(|s| token_ok(s))
            .ok_or_else(|| err2("a file name in the manifest dir ends in .jsonl but is not a ticket id; fix the manifest dir by hand"))?;
        // A following stat, no open: a FIFO would block the read.
        match fs::metadata(ent.path()) {
            Ok(m) if m.is_file() => {}
            _ => return Err(refuse_manifest(stem)),
        }
        let read = manifest::read_manifest(md, stem).map_err(|_| refuse_manifest(stem))?;
        if !read.is_clean() {
            return Err(refuse_manifest(stem));
        }
        for e in &read.live {
            if let Target::Path(s) = &e.target {
                entry_conflict(Path::new(s), src, src_id, src_up)?;
            }
        }
    }
    Ok(())
}

/// One live path entry against the source: identities for an entry that exists, canonical text for one that does not.
fn entry_conflict(e: &Path, src: &Src, src_id: Id, src_up: &[Id]) -> Result<(), Stop> {
    let refused = || err2("the source equals, lies inside or holds a live manifest entry");
    match fs::symlink_metadata(e) {
        Ok(m) => {
            let eid = (m.dev(), m.ino());
            let parent = e.parent().ok_or_else(|| err2("a live manifest entry cannot be resolved"))?;
            let parent = parent.canonicalize().map_err(|_| err2("a live manifest entry cannot be resolved"))?;
            let e_up = ids_up(&parent)?;
            if eid == src_id || src_up.contains(&eid) || e_up.contains(&src_id) {
                return Err(refused());
            }
            Ok(())
        }
        Err(k) if matches!(k.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            let r = manifest::resolve_for_manifest(e).map_err(|_| err2("a live manifest entry cannot be resolved"))?;
            if src.full.starts_with(&r) || r.starts_with(&src.full) {
                return Err(refused());
            }
            Ok(())
        }
        Err(_) => Err(err2("a live manifest entry cannot be examined")),
    }
}

// ---------------------------------------------------------------- the walk and the fingerprint

struct Walk {
    files: u64,
    bytes: u64,
    skipped_build: u64,
    fingerprint: String,
}

struct W {
    dev: u64,
    refuse_git: bool,
    visited: usize,
    files: u64,
    bytes: u64,
    skipped: u64,
    lines: Vec<(Vec<u8>, String)>,
}

/// Walk the item read only, with no-follow stats, and fingerprint it. `refuse_git` is true for the first walk
/// (a `.git` entry is exit 2) and false for the walk after the move.
fn walk_item(item: &Path, refuse_git: bool) -> Result<Walk, Stop> {
    let m = fs::symlink_metadata(item).map_err(|_| err3("the item cannot be examined; nothing was written"))?;
    let name = item.file_name().map(|n| n.as_bytes().to_vec()).unwrap_or_default();
    let mut w = W { dev: m.dev(), refuse_git, visited: 0, files: 0, bytes: 0, skipped: 0, lines: Vec::new() };
    visit(&mut w, item, &name, b"", 0, &m)?;
    w.lines.sort_by(|a, b| a.0.cmp(&b.0));
    let mut text = String::new();
    for (_, l) in &w.lines {
        text.push_str(l);
        text.push('\n');
    }
    Ok(Walk { files: w.files, bytes: w.bytes, skipped_build: w.skipped, fingerprint: sha256_hex(text.as_bytes()) })
}

/// One fingerprint line: `<relpath hex> <type> <mode octal> <size> <inode> <ctime s>.<ns> <link hex or ->`. A folder
/// writes `-` for size and ctime, the item itself `-` for ctime.
fn fp_line(rel: &[u8], ty: char, m: &fs::Metadata, item: bool, link: Option<&[u8]>) -> String {
    let dir = ty == 'd';
    let size = if dir { "-".to_string() } else { m.len().to_string() };
    let ctime = if dir || item { "-".to_string() } else { format!("{}.{}", m.ctime(), m.ctime_nsec()) };
    let link = link.map_or_else(|| "-".to_string(), hex);
    format!("{} {} {:o} {} {} {} {}", hex(rel), ty, m.mode() & 0o7777, size, m.ino(), ctime, link)
}

fn visit(w: &mut W, p: &Path, name: &[u8], rel: &[u8], depth: usize, m: &fs::Metadata) -> Result<(), Stop> {
    w.visited += 1;
    if w.visited > MAX_VISITED {
        return Err(err3("over the entries-visited cap (1000000); nothing was written"));
    }
    if depth > MAX_DEPTH {
        return Err(err3("over the walk depth cap (64); nothing was written"));
    }
    if m.dev() != w.dev {
        return Err(err3("an entry in the item is on another file system; nothing was written"));
    }
    if w.refuse_git && name.eq_ignore_ascii_case(b".git") {
        return Err(err2(GIT_MSG));
    }
    w.files += 1;
    let ft = m.file_type();
    if ft.is_symlink() {
        let l = fs::read_link(p).map_err(|_| err3("a link in the item cannot be read; nothing was written"))?;
        w.lines.push((rel.to_vec(), fp_line(rel, 'l', m, depth == 0, Some(l.as_os_str().as_bytes()))));
    } else if ft.is_dir() {
        if depth > 0 && name == b"target" && cache_tag_ok(&p.join(CACHEDIR_TAG)) {
            w.skipped += 1; // moves with the item; not entered, no line
            return Ok(());
        }
        w.lines.push((rel.to_vec(), fp_line(rel, 'd', m, depth == 0, None)));
        let rd = fs::read_dir(p).map_err(|_| err3("a folder in the item cannot be read; nothing was written"))?;
        for ent in rd {
            let ent = ent.map_err(|_| err3("an entry in the item cannot be read; nothing was written"))?;
            let n = ent.file_name();
            let cp = p.join(&n);
            let cm = fs::symlink_metadata(&cp).map_err(|_| err3("an entry in the item cannot be examined; nothing was written"))?;
            let mut crel = rel.to_vec();
            if depth > 0 {
                crel.push(b'/');
            }
            crel.extend_from_slice(n.as_bytes());
            visit(w, &cp, n.as_bytes(), &crel, depth + 1, &cm)?;
        }
    } else if ft.is_file() {
        w.bytes += m.len();
        w.lines.push((rel.to_vec(), fp_line(rel, 'f', m, depth == 0, None)));
    } else {
        w.lines.push((rel.to_vec(), fp_line(rel, 'o', m, depth == 0, None)));
    }
    Ok(())
}

// ---------------------------------------------------------------- step 7: the shelf

struct Shelf {
    _lock: File,
    journal: PathBuf,
    entries: PathBuf,
    date_dir: PathBuf,
}

fn make_dir(p: &Path) -> Result<(), Stop> {
    match fs::DirBuilder::new().mode(0o700).create(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(_) => Err(err3("a shelf folder could not be made; nothing was moved")),
    }
}

/// One shelf folder: made with mode 0700 when missing, then opened with no link follow. A link or a non-folder is exit 2.
fn shelf_dir(p: &Path) -> Result<File, Stop> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => return Err(err2("a shelf place is a link; nothing was moved")),
        Ok(m) if !m.is_dir() => return Err(err2("a shelf place is not a folder; nothing was moved")),
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => make_dir(p)?,
        Err(_) => return Err(err3("a shelf place cannot be examined; nothing was moved")),
    }
    open_dir_no_follow(p).ok_or_else(|| err2("a shelf place is a link or not a folder; nothing was moved"))
}

fn shelf_lock(p: &Path) -> Result<File, Stop> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_symlink() => return Err(err2("the shelf lock is a link; nothing was moved")),
        Ok(m) if !m.is_file() => return Err(err2("the shelf lock is not a regular file; nothing was moved")),
        Ok(_) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(_) => return Err(err3("the shelf lock cannot be examined; nothing was moved")),
    }
    let f = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW | O_NONBLOCK)
        .open(p)
        .map_err(|_| err2("the shelf lock cannot be opened (a link is refused); nothing was moved"))?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err(err2("the shelf lock is not a regular file; nothing was moved"));
    }
    match f.try_lock() {
        Ok(()) => Ok(f),
        Err(fs::TryLockError::WouldBlock) => Err(err3("a put is running (the shelf lock is held); nothing was moved")),
        Err(_) => Err(err3("the shelf lock cannot be taken; nothing was moved")),
    }
}

/// `@shelf`, its lock, then `entries`, `entries/<date>` and `journal`.
fn open_shelf(qd: &Path, date: &str) -> Result<Shelf, Stop> {
    let shelf = qd.join("@shelf");
    let _shelf_dir = shelf_dir(&shelf)?;
    let lock = shelf_lock(&shelf.join("lock"))?;
    let entries = shelf.join("entries");
    let _entries_dir = shelf_dir(&entries)?;
    let date_dir = entries.join(date);
    let _date_handle = shelf_dir(&date_dir)?;
    let journal = shelf.join("journal");
    let _journal_dir = shelf_dir(&journal)?;
    Ok(Shelf { _lock: lock, journal, entries, date_dir })
}

// ---------------------------------------------------------------- step 8: entry, label, journal

/// The entry folder `<kind>-<n>`: the lowest n from 1 whose exclusive mkdir works (a taken name is skipped).
fn make_entry(date_dir: &Path, kind: &str) -> Result<(u32, PathBuf), Stop> {
    for n in 1..=MAX_ENTRY_N {
        let p = date_dir.join(format!("{kind}-{n}"));
        match fs::DirBuilder::new().mode(0o700).create(&p) {
            Ok(()) => return Ok((n, p)),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(_) => return Err(err3("the entry folder could not be made; nothing was moved")),
        }
    }
    Err(err3("no free entry number up to 10000; nothing was moved"))
}

struct Label {
    entry: String,
    kind: String,
    from_hex: String,
    src_dev: u64,
    src_ino: u64,
    ticket: String,
    repo: String,
    by: String,
    at_ms: u64,
    why: String,
    files: u64,
    bytes: u64,
    skipped_build: u64,
    before: String,
    after: String,
    state: String,
    judgeable: String,
    reason: String,
}

impl Label {
    /// Fixed field order; the text values are tokens, hex or printable ASCII with no quote or backslash.
    fn bytes(&self) -> Vec<u8> {
        format!(
            "{{\"v\":1,\"entry\":\"{}\",\"kind\":\"{}\",\"from_hex\":\"{}\",\"src_dev\":{},\"src_ino\":{},\"ticket\":\"{}\",\"repo\":\"{}\",\"by\":\"{}\",\"at_ms\":{},\"why\":\"{}\",\"files\":{},\"bytes\":{},\"skipped_build\":{},\"fingerprint_before\":\"{}\",\"fingerprint_after\":\"{}\",\"state\":\"{}\",\"judgeable\":\"{}\",\"reason\":\"{}\",\"git\":\"none\"}}\n",
            self.entry,
            self.kind,
            self.from_hex,
            self.src_dev,
            self.src_ino,
            self.ticket,
            self.repo,
            self.by,
            self.at_ms,
            self.why,
            self.files,
            self.bytes,
            self.skipped_build,
            self.before,
            self.after,
            self.state,
            self.judgeable,
            self.reason
        )
        .into_bytes()
    }
}

/// A new file (create_new), written whole, then `sync_all`.
fn write_new(p: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = OpenOptions::new().write(true).create_new(true).mode(0o600).open(p)?;
    f.write_all(bytes)?;
    f.sync_all()
}

/// Replace `label.json` through `label.json.<pid>-<k>.tmp` (the next k when the name is taken).
fn replace_label(entry_path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    for k in 1..=1000u32 {
        let tmp = entry_path.join(format!("label.json.{}-{}.tmp", std::process::id(), k));
        match write_new(&tmp, bytes) {
            Ok(()) => return fs::rename(&tmp, entry_path.join("label.json")),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free temporary name"))
}

fn open_journal(dir: &Path, date: &str, k: u64, create: bool) -> std::io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).append(true).mode(0o600).custom_flags(O_NOFOLLOW | O_NONBLOCK);
    if create {
        o.create_new(true);
    }
    let f = o.open(dir.join(format!("{date}-{k}.jsonl")))?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::other("a journal file is not a regular file"));
    }
    Ok(f)
}

/// Append one line to the journal file with the highest k of `date` (made when none exists) under an exclusive
/// lock, one write, `sync_all`. A file that does not end in a newline holds a fragment: the line goes to a new
/// file k+1 (create_new), and nothing is cut or removed.
fn journal_append(dir: &Path, date: &str, line: &str) -> std::io::Result<()> {
    let prefix = format!("{date}-");
    let mut top = 0u64;
    for ent in fs::read_dir(dir)? {
        let name = ent?.file_name();
        let Some(k) = name.to_str().and_then(|n| n.strip_prefix(prefix.as_str())).and_then(|r| r.strip_suffix(".jsonl")) else {
            continue;
        };
        if let Some(n) = k.parse::<u64>().ok().filter(|_| num_ok(k)) {
            top = top.max(n);
        }
    }
    let mut f = if top == 0 { open_journal(dir, date, 1, true)? } else { open_journal(dir, date, top, false)? };
    f.lock()?;
    let len = f.metadata()?.len();
    if len > 0 {
        let mut last = [0u8; 1];
        f.read_exact_at(&mut last, len - 1)?;
        if last[0] != b'\n' {
            f = open_journal(dir, date, top + 1, true)?;
            f.lock()?;
        }
    }
    let n = f.write(line.as_bytes())?;
    if n != line.len() {
        return Err(std::io::Error::other("a journal line was written in part"));
    }
    f.sync_all()
}

// ---------------------------------------------------------------- the settle of a stopped put

#[derive(PartialEq, Clone, Copy)]
enum JOp {
    Intent,
    Moved,
    Void,
}

/// One journal line as put writes it.
struct Rec {
    op: JOp,
    entry: String,
    dev: u64,
    ino: u64,
    sha: String,
}

impl Rec {
    /// The exact bytes the writer makes for this record (no newline).
    fn text(&self) -> String {
        match self.op {
            JOp::Intent => format!(
                "{{\"v\":1,\"op\":\"intent\",\"entry\":\"{}\",\"src_dev\":{},\"src_ino\":{},\"label_sha\":\"{}\"}}",
                self.entry, self.dev, self.ino, self.sha
            ),
            JOp::Moved => format!("{{\"v\":1,\"op\":\"moved\",\"entry\":\"{}\",\"label_sha\":\"{}\"}}", self.entry, self.sha),
            JOp::Void => format!("{{\"v\":1,\"op\":\"void\",\"entry\":\"{}\",\"label_sha\":\"{}\"}}", self.entry, self.sha),
        }
    }
}

/// `<YYYYMMDD>/<kind>-<n>` as the writer makes it.
fn entry_ok(e: &str) -> bool {
    let Some((date, rest)) = e.split_once('/') else { return false };
    let Some((kind, n)) = rest.rsplit_once('-') else { return false };
    date.len() == 8
        && date.bytes().all(|b| b.is_ascii_digit())
        && KINDS.contains(&kind)
        && num_ok(n)
        && n.parse::<u32>().is_ok_and(|n| n >= 1)
        && !n.starts_with('0')
}

/// One complete line: one of the three shapes, exact bytes, or `None`.
fn parse_line(raw: &[u8]) -> Option<Rec> {
    let line = std::str::from_utf8(raw).ok()?;
    let rec = if let Some(r) = line.strip_prefix("{\"v\":1,\"op\":\"intent\",\"entry\":\"") {
        let (entry, r) = r.split_once("\",\"src_dev\":")?;
        let (dev, r) = r.split_once(",\"src_ino\":")?;
        let (ino, r) = r.split_once(",\"label_sha\":\"")?;
        let sha = r.strip_suffix("\"}")?;
        if !num_ok(dev) || !num_ok(ino) {
            return None;
        }
        Rec { op: JOp::Intent, entry: entry.to_string(), dev: dev.parse().ok()?, ino: ino.parse().ok()?, sha: sha.to_string() }
    } else {
        let (op, r) = if let Some(r) = line.strip_prefix("{\"v\":1,\"op\":\"moved\",\"entry\":\"") {
            (JOp::Moved, r)
        } else {
            (JOp::Void, line.strip_prefix("{\"v\":1,\"op\":\"void\",\"entry\":\"")?)
        };
        let (entry, r) = r.split_once("\",\"label_sha\":\"")?;
        let sha = r.strip_suffix("\"}")?;
        Rec { op, entry: entry.to_string(), dev: 0, ino: 0, sha: sha.to_string() }
    };
    let sha_ok = rec.sha.len() == 64 && rec.sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    (sha_ok && entry_ok(&rec.entry) && rec.text() == line).then_some(rec)
}

/// Read each journal file, in k order per date. A fragment at the end of a file (no newline) is skipped; the intents
/// with no later moved or void line come back in journal order.
fn unsettled_intents(journal: &Path) -> Result<Vec<Rec>, Stop> {
    let bad_name = || err3("the journal folder holds a name that is not a journal file; nothing was moved; fix it by hand");
    let unreadable = || err3("a journal file cannot be read; nothing was moved");
    let mut files: Vec<(String, u64)> = Vec::new();
    for ent in fs::read_dir(journal).map_err(|_| unreadable())? {
        let name = ent.map_err(|_| unreadable())?.file_name();
        let n = name.to_str().ok_or_else(bad_name)?;
        let (date, k) = n.strip_suffix(".jsonl").and_then(|r| r.split_once('-')).ok_or_else(bad_name)?;
        let kn = if num_ok(k) { k.parse::<u64>().ok() } else { None };
        match kn {
            Some(kn) if kn >= 1 && date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) && n == format!("{date}-{kn}.jsonl") => {
                files.push((date.to_string(), kn))
            }
            _ => return Err(bad_name()),
        }
    }
    files.sort();
    let mut pending: Vec<Rec> = Vec::new();
    for (date, k) in files {
        let mut f = OpenOptions::new()
            .read(true)
            .custom_flags(O_NOFOLLOW | O_NONBLOCK)
            .open(journal.join(format!("{date}-{k}.jsonl")))
            .map_err(|_| unreadable())?;
        if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
            return Err(unreadable());
        }
        let mut bytes = Vec::new();
        f.read_to_end(&mut bytes).map_err(|_| unreadable())?;
        for (i, chunk) in bytes.split_inclusive(|b| *b == b'\n').enumerate() {
            let Some(line) = chunk.strip_suffix(b"\n") else { continue };
            let Some(rec) = parse_line(line) else {
                return Err(err3(&format!(
                    "journal file {date} line {} is not a line put wrote; nothing was moved; fix it by hand",
                    i + 1
                )));
            };
            if rec.op == JOp::Intent {
                pending.push(rec);
            } else {
                pending.retain(|p| p.entry != rec.entry);
            }
        }
    }
    Ok(pending)
}

/// `label.json` of an entry: no link follow, a regular file, at most 64 KiB.
fn read_label(entry_path: &Path) -> Option<Vec<u8>> {
    let f = OpenOptions::new().read(true).custom_flags(O_NOFOLLOW | O_NONBLOCK).open(entry_path.join("label.json")).ok()?;
    if !f.metadata().ok()?.is_file() {
        return None;
    }
    let mut v = Vec::new();
    f.take(65_537).read_to_end(&mut v).ok()?;
    (v.len() <= 65_536).then_some(v)
}

/// Under the shelf lock: settle each intent with no later moved or void line from what is on disk, or refuse.
/// Moves nothing, removes nothing, does not look at the old source path.
fn settle_stopped(journal: &Path, entries: &Path) -> Result<(), Stop> {
    for p in unsettled_intents(journal)? {
        settle_one(&p, journal, entries)?;
    }
    Ok(())
}

fn settle_one(p: &Rec, journal: &Path, entries: &Path) -> Result<(), Stop> {
    let undecided = || err3("a stopped put cannot be decided; nothing was moved; fix the entry by hand");
    let (date, name) = p.entry.split_once('/').ok_or_else(undecided)?;
    let entry_path = entries.join(date).join(name);
    let text = read_label(&entry_path).filter(|b| sha256_hex(b) == p.sha).ok_or_else(undecided)?;
    let text = String::from_utf8(text).map_err(|_| undecided())?;
    let marks = ["\"state\":\"intent\"", "\"fingerprint_after\":\"\"", "\"judgeable\":\"yes\",\"reason\":\"\""];
    if !text.starts_with(&format!("{{\"v\":1,\"entry\":\"{}\",", p.entry)) || marks.iter().any(|m| !text.contains(m)) {
        return Err(undecided());
    }
    let item = entry_path.join("item");
    let (op, done) = match fs::symlink_metadata(&item) {
        Ok(m) if m.dev() == p.dev && m.ino() == p.ino => {
            let before = text
                .split_once("\"fingerprint_before\":\"")
                .and_then(|(_, r)| r.split_once('"'))
                .map(|(f, _)| f.to_string())
                .ok_or_else(undecided)?;
            let w = walk_item(&item, true).map_err(|_| err3("the item of a stopped put cannot be walked; nothing was moved"))?;
            let mut done = text
                .replacen("\"fingerprint_after\":\"\"", &format!("\"fingerprint_after\":\"{}\"", w.fingerprint), 1)
                .replacen("\"state\":\"intent\"", "\"state\":\"moved\"", 1);
            if w.fingerprint != before {
                done = done.replacen("\"judgeable\":\"yes\",\"reason\":\"\"", "\"judgeable\":\"no\",\"reason\":\"changed-during-put\"", 1);
            }
            ("moved", done)
        }
        Ok(_) => return Err(undecided()),
        Err(e) if e.kind() == ErrorKind::NotFound => ("void", text.replacen("\"state\":\"intent\"", "\"state\":\"void\"", 1)),
        Err(_) => return Err(undecided()),
    };
    let done = done.into_bytes();
    let line = format!("{{\"v\":1,\"op\":\"{op}\",\"entry\":\"{}\",\"label_sha\":\"{}\"}}\n", p.entry, sha256_hex(&done));
    replace_label(&entry_path, &done)
        .and_then(|()| journal_append(journal, date, &line))
        .map_err(|_| err3("a stopped put could not be settled; nothing was moved; fix the entry by hand"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);
    const TAG: &str = "Signature: 8a477f597d28d172789f06886806bc55\n";

    struct Dir(PathBuf);

    impl Dir {
        fn new() -> Dir {
            let p = std::env::temp_dir().join(format!("put-unit-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fp(p: &Path) -> String {
        match walk_item(p, true) {
            Ok(w) => w.fingerprint,
            Err(_) => panic!("the walk works"),
        }
    }

    #[test]
    fn the_fingerprint_sees_a_same_size_edit_with_mtime_set_back() {
        let d = Dir::new();
        let item = d.0.join("item");
        fs::create_dir_all(&item).unwrap();
        let file = item.join("a.txt");
        fs::write(&file, b"abcdef").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let before = fp(&item);
        let m0 = fs::metadata(&file).unwrap();
        let mtime = m0.modified().unwrap();
        let f = OpenOptions::new().write(true).open(&file).unwrap();
        f.write_all_at(b"X", 2).unwrap();
        f.set_modified(mtime).unwrap();
        drop(f);
        let m1 = fs::metadata(&file).unwrap();
        assert_eq!(m1.len(), m0.len(), "same size");
        assert_eq!(m1.modified().unwrap(), mtime, "mtime set back");
        assert!((m1.ctime(), m1.ctime_nsec()) != (m0.ctime(), m0.ctime_nsec()), "the ctime moved");
        assert_ne!(fp(&item), before, "the fingerprint sees the edit");
    }

    #[test]
    fn removing_a_tagged_target_leaves_the_fingerprint_as_it_was() {
        let d = Dir::new();
        let item = d.0.join("item");
        let target = item.join("proj/target");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("CACHEDIR.TAG"), TAG).unwrap();
        fs::write(target.join("big.bin"), b"payload").unwrap();
        fs::write(item.join("proj/src.rs"), b"fn main() {}").unwrap();
        let before = fp(&item);
        fs::remove_dir_all(&target).unwrap();
        assert_eq!(fp(&item), before, "a dropped tagged target does not change the fingerprint");
    }
}
