//! `harness quarantine sweep`, `harness quarantine delete-listed` and `harness quarantine drop-build` (std only, Unix): P1B-13, ticket #156; drop-build: build-output-drop.
//! The sweep NEVER deletes: anything left in the ticket's roots that the manifest does not own is carried, whole and
//! untouched, to the quarantine shelf — `<quarantine-dir>/<ticket>/<batch>/<item>`, generated token names only, an
//! original name is never reused as a quarantine name — with one journal line written BEFORE each move (the crash
//! trace) and one `quarantined` event written AFTER it (through the library record writer, like every P1b kind).
//! Deleting a listed `temp_dir` is a SEPARATE, explicit verb that names exactly one id, and its library function
//! `delete_listed` is the ONE delete path P1B-14 teardown must call, never a second one.
//! `drop-build` deletes only cache-tagged build output (a `target` folder holding the cache tag) of a ticket whose
//! manifest shows it closed, reached by a component walk of the same kind.
//! Symlinks are never followed anywhere in this module: a link found in a root is a leftover like any other and is
//! moved as the link itself (its target is never read or touched), a listed link is unlinked as the link only, and
//! the recursive delete unlinks a link inside a listed folder without following it. The walk is read-only and
//! completes before the first move; in the sweep and delete-listed each cap and each refusal fires before
//! anything is written, and drop-build holds that order from the caps lock file on (a ticket refused after the
//! lock leaves an empty lock file). No folder path is ever printed on stdout or stderr (the workspace privacy
//! rule): fixed lines of fixed fields only.

use crate::caps::{Caps, LockError};
use crate::events::{self, token_ok, Field, Kind, Record};
use crate::manifest::{self, Class, Line, Op, Target};
use std::ffi::{c_char, c_int, CString, OsStr};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::os::unix::io::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness quarantine sweep <ticket> --manifest-dir <abs> --workspace-root <abs> --quarantine-dir <abs> --log <abs> --repo <token> --role <token> --model <token> [--max-items <n>] [--ts-ms <n>] | quarantine delete-listed <ticket> --id <id> --manifest-dir <abs> [--ts-ms <n>] | quarantine drop-build <ticket> --root <abs> [--root <abs> ...] --manifest-dir <abs> --caps-dir <abs> [--dry-run]";

/// Default cap on leftovers moved by one sweep. Over it the sweep fails closed: exit 3, nothing moved.
pub const DEFAULT_MAX_ITEMS: u64 = 1000;
/// Fixed cap on entries visited by the walk (all roots together). Over it: exit 3, nothing moved.
pub const MAX_VISITED: usize = 100_000;
/// Fixed cap on walk depth below a root. Over it: exit 3, nothing moved.
pub const MAX_DEPTH: usize = 64;
/// Highest `seq` tried when minting a batch name (`b-<ts_ms>-<pid>-<seq>`); over it the sweep fails closed.
const MAX_BATCH_SEQ: u64 = 10_000;

/// A stop: the exit code and the full stderr line (already prefixed, folder-free).
pub struct Stop(pub i32, pub String);

fn err2(m: &str) -> Stop {
    Stop(2, format!("harness: error: {}", m))
}

fn err3(m: &str) -> Stop {
    Stop(3, format!("harness: error: {}", m))
}

fn usage_err(m: &str) -> Stop {
    Stop(2, format!("harness: error: {}\n{}", m, USAGE))
}

/// Absolute, and no `..` component (the same shape `workspace create` requires of its dirs).
fn dir_arg_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// A numeric flag value: digits only, no sign, no empty string (parsed to u64 by the caller).
fn num_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// A required single-valued flag: `None` is a usage error naming the flag.
fn req<'a>(k: &str, v: Option<&'a str>) -> Result<&'a str, Stop> {
    v.ok_or_else(|| usage_err(&format!("--{k} is required")))
}

// ---------------------------------------------------------------- sweep arguments

/// One parsed `quarantine sweep` command line. Every value is validated BEFORE anything is written.
pub struct SweepArgs {
    pub ticket: String,
    pub manifest_dir: PathBuf,
    pub workspace_root: PathBuf,
    pub quarantine_dir: PathBuf,
    pub log: PathBuf,
    pub repo: String,
    pub role: String,
    pub model: String,
    pub max_items: u64,
    pub ts_ms: u64,
}

fn parse_sweep(a: &[String]) -> Result<SweepArgs, Stop> {
    let mut ticket: Option<String> = None;
    type SweepOpt<'a> = (Option<&'a str>, Option<&'a str>, Option<&'a str>, Option<&'a str>, Option<&'a str>, Option<&'a str>, Option<&'a str>, Option<&'a str>, Option<&'a str>);
    let (mut md, mut ws, mut qd, mut log, mut repo, mut role, mut model, mut max, mut ts): SweepOpt =
        (None, None, None, None, None, None, None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manifest-dir" | "--workspace-root" | "--quarantine-dir" | "--log" | "--repo" | "--role" | "--model"
            | "--max-items" | "--ts-ms" => {
                let val = it.next().ok_or_else(|| usage_err("an option is missing its value"))?;
                let slot = match arg.as_str() {
                    "--manifest-dir" => &mut md,
                    "--workspace-root" => &mut ws,
                    "--quarantine-dir" => &mut qd,
                    "--log" => &mut log,
                    "--repo" => &mut repo,
                    "--role" => &mut role,
                    "--model" => &mut model,
                    "--max-items" => &mut max,
                    _ => &mut ts,
                };
                if slot.replace(val.as_str()).is_some() {
                    return Err(usage_err("an option was given twice"));
                }
            }
            s if s.starts_with("--") => return Err(usage_err("unknown option")),
            s => {
                if ticket.replace(s.to_string()).is_some() {
                    return Err(usage_err("more than one ticket given"));
                }
            }
        }
    }
    let ticket = ticket.ok_or_else(|| usage_err("no ticket given"))?;
    if !token_ok(&ticket) {
        return Err(usage_err("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')"));
    }
    for (flag, val) in [
        ("--manifest-dir", req("manifest-dir", md)?),
        ("--workspace-root", req("workspace-root", ws)?),
        ("--quarantine-dir", req("quarantine-dir", qd)?),
        ("--log", req("log", log)?),
    ] {
        if !dir_arg_ok(val) {
            return Err(usage_err(&format!("{} must be an absolute path with no `..` component", flag)));
        }
    }
    for (flag, val) in [("--repo", req("repo", repo)?), ("--role", req("role", role)?), ("--model", req("model", model)?)] {
        if !token_ok(val) {
            return Err(usage_err(&format!("{} must be a plain token (lowercase letters, digits, '-', '_', '.')", flag)));
        }
    }
    // Every user-supplied number is parsed HERE, inside parse_sweep, so a bad value is refused before a single
    // filesystem write (a usage error must never leave a half-swept workspace behind).
    let max_items = match max {
        None => DEFAULT_MAX_ITEMS,
        Some(s) => {
            if !num_ok(s) {
                return Err(usage_err("--max-items must be a non-negative integer"));
            }
            s.parse::<u64>().map_err(|_| usage_err("--max-items out of range"))?
        }
    };
    let ts_ms = match ts {
        Some(s) => {
            if !num_ok(s) {
                return Err(usage_err("--ts-ms must be a non-negative integer"));
            }
            s.parse::<u64>().map_err(|_| usage_err("--ts-ms out of range"))?
        }
        None => SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).map_err(|_| err2("the clock is before the epoch"))?,
    };
    Ok(SweepArgs {
        ticket,
        manifest_dir: PathBuf::from(req("manifest-dir", md)?),
        workspace_root: PathBuf::from(req("workspace-root", ws)?),
        quarantine_dir: PathBuf::from(req("quarantine-dir", qd)?),
        log: PathBuf::from(req("log", log)?),
        repo: req("repo", repo)?.to_string(),
        role: req("role", role)?.to_string(),
        model: req("model", model)?.to_string(),
        max_items,
        ts_ms,
    })
}

/// Resolve one of the caller's base folders: an existing REAL directory (a file or a link is refused — the check is
/// a no-follow stat), canonical, never `/`. Only the resolved value is used afterwards.
fn resolve_base_dir(flag: &str, arg: &Path) -> Result<PathBuf, Stop> {
    let meta = fs::symlink_metadata(arg).map_err(|_| err2(&format!("{} is missing or unreadable", flag)))?;
    if meta.file_type().is_symlink() {
        return Err(err2(&format!("{} must be a real directory (a link is refused)", flag)));
    }
    if !meta.is_dir() {
        return Err(err2(&format!("{} must be a real directory (a file is refused)", flag)));
    }
    let canon = arg.canonicalize().map_err(|_| err2(&format!("{} cannot be resolved", flag)))?;
    if canon == Path::new("/") {
        return Err(err2(&format!("{} may not be the filesystem root", flag)));
    }
    Ok(canon)
}

// ---------------------------------------------------------------- shared helpers

/// One leftover found by the walk: its source path and that path's RAW BYTES relative to the workspace root
/// (raw, because a name need not be UTF-8; the journal stores these bytes hex-encoded), plus the ANCESTOR
/// CHAIN the walk examined — the root and every real directory between it and the leftover, each with the
/// dev/ino it had at walk time. The chain is re-checked immediately before the item's rename (review r2, F2 /
/// RACE-S, closed by #187): every ancestor is opened with O_DIRECTORY|O_NOFOLLOW — a swapped-in link, FIFO
/// or other non-directory fails the open itself, never followed and never blocked (review r1, F1) — and its
/// handle's stat matched against the recorded dev/ino, and the immediate parent's handle is HELD into the rename, which is dirfd-relative — an ancestor
/// that is gone, is now a symlink, or is a different directory makes the sweep SKIP the item (nothing moved
/// for it, no journal line, no event, exit 3 at the end naming the item token) instead of following a swapped
/// component out of the roots, and a swap landing after the check can no longer redirect the move.
struct Leftover {
    src: PathBuf,
    rel: Vec<u8>,
    chain: Vec<(PathBuf, u64, u64)>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Decode lowercase hex of even length, else `None`.
fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect()
}

/// The `from` relpath of a quarantined event: the source relpath when every segment is a plain token, else the
/// `untokenable` form (the journal holds the exact bytes either way).
fn from_rel(ticket: &str, item: &str, rel: &[u8]) -> String {
    match std::str::from_utf8(rel) {
        Ok(s) if !s.is_empty() && s.split('/').all(token_ok) => s.to_string(),
        _ => format!("{}/untokenable/{}", ticket, item),
    }
}

/// Write one `quarantined` event through the library record writer (the only writer for P1b kinds). Written AFTER
/// the move it describes; a failure here is a stop (exit 3) with the item already safely on the shelf and journalled.
fn write_quarantined_event(a: &SweepArgs, from: &str, to: &str, reason: &str) -> Result<(), Stop> {
    let rec = Record::build(
        Kind::Quarantined,
        a.ts_ms,
        &[
            ("ticket", Field::Str(a.ticket.clone())),
            ("repo", Field::Str(a.repo.clone())),
            ("from", Field::Str(from.to_string())),
            ("to", Field::Str(to.to_string())),
            ("reason", Field::Str(reason.to_string())),
        ],
    )
    .map_err(|_| err3("the quarantined event could not be built"))?;
    let root = a.log.parent().unwrap_or(Path::new("."));
    events::append_record(root, &a.log, &rec)
        .map_err(|_| err3("the quarantined event could not be written; the journal holds the move"))?;
    Ok(())
}

// ---------------------------------------------------------------- the journal

/// One journal intent line: `{"item":"i-1","src_hex":"<hex>","reason":"unmanifested"}` — exact bytes, no spaces, no
/// escapes (item and reason are tokens, the source is hex), so the reader can stay strict and total.
fn intent_line(item: &str, rel: &[u8], reason: &str) -> String {
    format!("{{\"item\":\"{}\",\"src_hex\":\"{}\",\"reason\":\"{}\"}}\n", item, hex(rel), reason)
}

struct Intent {
    item: String,
    src_hex: String,
    reason: String,
}

/// Strict decode of one COMPLETE journal line. Anything else — wrong shape, non-token item, non-hex or odd-length
/// hex, a reason outside the two declared — is undecidable, and the caller fails closed.
fn parse_intent(line: &str) -> Result<Intent, ()> {
    let rest = line.strip_prefix("{\"item\":\"").ok_or(())?;
    let (item, rest) = rest.split_once("\",\"src_hex\":\"").ok_or(())?;
    let (src_hex, rest) = rest.split_once("\",\"reason\":\"").ok_or(())?;
    let reason = rest.strip_suffix("\"}").ok_or(())?;
    if !token_ok(item)
        || src_hex.is_empty()
        || !src_hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || src_hex.len() % 2 != 0
        || (reason != "unmanifested" && reason != "retention")
    {
        return Err(());
    }
    Ok(Intent { item: item.to_string(), src_hex: src_hex.to_string(), reason: reason.to_string() })
}

/// Append one intent line to `<batch>/journal.jsonl` under an exclusive lock, ONE write, then `sync_all`. This is
/// the write-ahead: it happens BEFORE the rename, so a crash mid-carry leaves a trace that says what was being moved.
fn append_intent(batch: &Path, item: &str, rel: &[u8], reason: &str) -> Result<(), Stop> {
    let line = intent_line(item, rel, reason);
    let io = || err3("a batch journal could not be written");
    let mut f = OpenOptions::new().read(true).append(true).create(true).open(batch.join("journal.jsonl")).map_err(|_| io())?;
    f.lock().map_err(|_| io())?;
    let n = f.write(line.as_bytes()).map_err(|_| io())?;
    if n != line.len() {
        return Err(io());
    }
    f.sync_all().map_err(|_| io())?;
    Ok(())
}

/// A batch directory name: `b-<digits>-<digits>-<digits>` (ts_ms, pid, seq).
fn batch_name_ok(name: &str) -> bool {
    let Some(rest) = name.strip_prefix('b') else { return false };
    let Some(rest) = rest.strip_prefix('-') else { return false };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

// ---------------------------------------------------------------- reconcile

/// The `to` values of every `quarantined` event already in the log, or a refusal the sweep cannot proceed past.
/// An ABSENT log (decided here by this reader's own no-follow stat, so a dangling link is NOT absent) holds no
/// matching event, exactly as before anything was ever written. Any other log the checked reader refuses — over
/// the read cap, a symbolic link, not a regular file, changed between check and open, any io error — is a
/// REFUSAL: a reconcile that cannot see the whole record must not write it again (the duplicate-event bug). A
/// CORRUPT line is the same refusal naming the line number only (the read stops at the first bad line, so every
/// event after it would be silently missing). A TORN TAIL keeps today's behaviour: every complete line before
/// the fragment is returned, so the list is complete, and the fragment was never a written event.
fn quarantined_tos(log: &Path) -> Result<Vec<String>, Stop> {
    if let Err(e) = fs::symlink_metadata(log) {
        if e.kind() == std::io::ErrorKind::NotFound {
            return Ok(Vec::new()); // nothing was ever written; a later write creates the log
        }
        // any other stat error falls through: the checked reader re-stats and reports its own fixed reason
    }
    let read = events::read_log(log).map_err(|reason| unreadable_log(&reason))?;
    if let Some(c) = &read.corrupt {
        return Err(unreadable_log(&format!("corrupt at line {}", c.line)));
    }
    Ok(read
        .events
        .iter()
        .filter(|e| e.kind == Kind::Quarantined)
        .filter_map(|e| match e.field("to") {
            Some(Field::Str(s)) => Some(s.clone()),
            _ => None,
        })
        .collect())
}

/// The one-line refusal for an event log the sweep cannot read whole. `<reason>` is the reader's own fixed
/// text (names the cap, the link, the file type or the corrupt line number; never a path, never a log byte).
fn unreadable_log(reason: &str) -> Stop {
    err3(&format!("reconcile refused: the event log cannot be read whole ({}); nothing was moved", reason))
}

/// Reconcile every journal under `<quarantine-dir>/<ticket>/` BEFORE walking: an intent whose item sits in its batch
/// and whose source is gone but which has no matching event gets its event written now; an intent whose source is
/// still there and whose item is absent is simply found again by the walk (a fresh batch); an intent whose source AND
/// item both exist, or neither does, cannot be decided — exit 3, nothing moved. A journal line that is torn (a final
/// fragment with no newline) is skipped: the mover renames only after the journal line was synced whole, so a torn
/// line never preceded a move. A COMPLETE line that cannot be parsed is undecidable: exit 3. The event log is read
/// once, lazily, before any reconcile write: a log that cannot be read WHOLE (over the read cap, a link, not a
/// regular file, corrupt) refuses the sweep here — exit 3, nothing moved, nothing written — while an absent log
/// still reads as no events (see `quarantined_tos`).
fn reconcile(a: &SweepArgs, shelf: &Path, ws: &Path) -> Result<(), Stop> {
    let entries = match fs::read_dir(shelf) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(err3("the quarantine shelf cannot be read; nothing was moved")),
    };
    let mut tos: Option<Vec<String>> = None;
    for ent in entries.flatten() {
        let name = ent.file_name();
        let Some(bname) = name.to_str() else { continue };
        if !batch_name_ok(bname) {
            continue; // not a batch of ours; nothing already on the shelf is ever touched
        }
        let batch = shelf.join(bname);
        let bytes = match fs::read(batch.join("journal.jsonl")) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(err3("a batch journal cannot be read; nothing was moved")),
        };
        let mut start = 0usize;
        while let Some(off) = bytes[start..].iter().position(|b| *b == b'\n') {
            let raw = &bytes[start..start + off];
            start += off + 1;
            let s = std::str::from_utf8(raw).map_err(|_| err3("a batch journal holds a line this reader cannot decide; nothing was moved"))?;
            let intent = parse_intent(s).map_err(|_| err3("a batch journal holds a line this reader cannot decide; nothing was moved"))?;
            let rel = unhex(&intent.src_hex).ok_or_else(|| err3("a batch journal holds a line this reader cannot decide; nothing was moved"))?;
            let item_path = batch.join(&intent.item);
            let src = ws.join(std::ffi::OsString::from_vec(rel.clone()));
            let to = format!("{}/{}/{}", a.ticket, bname, intent.item);
            // An intent that already has its matching event is SETTLED: the move it describes happened and was
            // recorded, so whatever sits at the source path NOW is simply a later leftover of the same name — the
            // walk's business, in a fresh batch. Only an intent with no event is decided here, from what exists.
            if tos.is_none() {
                tos = Some(quarantined_tos(&a.log)?);
            }
            let tos = tos.as_ref().unwrap();
            if tos.contains(&to) {
                continue;
            }
            let src_there = fs::symlink_metadata(&src).is_ok();
            let item_there = fs::symlink_metadata(&item_path).is_ok();
            match (src_there, item_there) {
                (false, true) => {
                    // moved, and the event is missing (a crash between rename and event): write it now
                    let from = from_rel(&a.ticket, &intent.item, &rel);
                    write_quarantined_event(a, &from, &to, &intent.reason)?;
                }
                (true, false) => continue, // found again by the walk, in a fresh batch
                _ => return Err(err3("a batch journal intent cannot be decided (source and item both present, or neither); nothing was moved")),
            }
        }
        // a final fragment with no newline never preceded a move: skipped, never parsed
    }
    Ok(())
}

// ---------------------------------------------------------------- the walk

/// Top-down walk of every root with no-follow stats ONLY, complete before the first move. An entry a live path entry
/// owns (equals or inside) is skipped whole; an entry a live path entry lies inside is descended into (real
/// directories only — a symlink is never descended); anything else is a leftover, moved later as one unit. The caps
/// (leftovers over `max_items`, entries visited over 100,000, depth over 64) fail closed: exit 3, nothing moved.
fn walk(ws: &Path, roots: &[PathBuf], live: &[PathBuf], max_items: u64) -> Result<Vec<Leftover>, Stop> {
    let mut out: Vec<Leftover> = Vec::new();
    let mut visited = 0usize;
    for root in roots {
        let meta = fs::symlink_metadata(root).map_err(|_| err3("a root cannot be walked; nothing was moved"))?;
        let chain = vec![(root.clone(), meta.dev(), meta.ino())];
        walk_dir(ws, root, live, &chain, &mut out, &mut visited, 1, max_items)?;
    }
    Ok(out)
}

/// One directory of the walk. Recursive (depth is capped at `MAX_DEPTH`, so the stack is bounded); the current
/// ancestor chain — each directory the walk has descended into, with the dev/ino it had when examined — is
/// carried down and CLONED into every leftover found below it, so the move phase can re-check it.
// reason: bundling the eight walk parameters into a context struct is a refactor beyond this cleanup (deferred follow-up)
#[allow(clippy::too_many_arguments)]
fn walk_dir(
    ws: &Path,
    dir: &Path,
    live: &[PathBuf],
    chain: &[(PathBuf, u64, u64)],
    out: &mut Vec<Leftover>,
    visited: &mut usize,
    depth: usize,
    max_items: u64,
) -> Result<(), Stop> {
    let rd = fs::read_dir(dir).map_err(|_| err3("a root cannot be walked; nothing was moved"))?;
    for ent in rd.flatten() {
        *visited += 1;
        if *visited > MAX_VISITED {
            return Err(err3("over the entries-visited cap (100000); nothing was moved"));
        }
        let p = dir.join(ent.file_name());
        let meta = fs::symlink_metadata(&p).map_err(|_| err3("an entry in a root cannot be examined; nothing was moved"))?;
        if live.iter().any(|l| p.starts_with(l)) {
            continue; // owned: its contents are owned, skip whole
        }
        if live.iter().any(|l| l.starts_with(&p)) && meta.is_dir() {
            // an ancestor of a live path entry: descend (never through a symlink)
            if depth + 1 > MAX_DEPTH {
                return Err(err3("over the walk depth cap (64); nothing was moved"));
            }
            let mut sub = chain.to_vec();
            sub.push((p.clone(), meta.dev(), meta.ino()));
            walk_dir(ws, &p, live, &sub, out, visited, depth + 1, max_items)?;
            continue;
        }
        if out.len() as u64 >= max_items {
            return Err(err3("over the --max-items cap; nothing was moved"));
        }
        // the relpath is taken against the WORKSPACE ROOT (the event's `from` and the journal's bytes both
        // use it), never against the walked root: a later multi-root manifest keeps one spelling.
        let rel = p.strip_prefix(ws).map_err(|_| err3("an entry in a root cannot be named; nothing was moved"))?;
        out.push(Leftover { rel: rel.as_os_str().as_encoded_bytes().to_vec(), src: p, chain: chain.to_vec() });
    }
    Ok(())
}

/// Mint one fresh batch directory `<shelf>/b-<ts_ms>-<pid>-<seq>` with an EXCLUSIVE create (exists → next seq, never
/// reused) at mode 0700. The fresh exclusive batch is the no-overwrite guarantee for the whole sweep.
fn make_batch(shelf: &Path, ts_ms: u64) -> Result<(String, PathBuf), Stop> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true).mode(0o700);
    b.create(shelf).map_err(|_| err3("the quarantine shelf could not be made"))?;
    for seq in 0..MAX_BATCH_SEQ {
        let name = format!("b-{}-{}-{}", ts_ms, std::process::id(), seq);
        let dir = shelf.join(&name);
        let mut db = std::fs::DirBuilder::new();
        match db.mode(0o700).create(&dir) {
            Ok(()) => return Ok((name, dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err(err3("a quarantine batch could not be created")),
        }
    }
    Err(err3("a quarantine batch could not be created"))
}

/// The real device id of a path (no-follow stat); `None` when it cannot be examined.
fn real_device(p: &Path) -> Option<u64> {
    fs::symlink_metadata(p).ok().map(|m| m.dev())
}

// `open(2)` directory flags std exposes no constants for (review r1, F1, #187). The spellings differ per
// platform and are NOT guessable (Darwin's `O_NOFOLLOW` is 0x100; 0x40 — the FreeBSD value — is another
// flag there and compiles silently, which is exactly how a wrong constant would sneak in), so only the two
// platforms this crate is built on are listed, each value read from its own `fcntl.h`: Linux from
// `asm-generic/fcntl.h`, macOS from the SDK `sys/fcntl.h`. Any other platform fails the build here rather
// than opening without the flags.
#[cfg(target_os = "linux")]
const O_DIRECTORY: i32 = 0o200000;
#[cfg(target_os = "linux")]
const O_NOFOLLOW: i32 = 0o400000;
#[cfg(target_os = "macos")]
const O_DIRECTORY: i32 = 0x100000;
#[cfg(target_os = "macos")]
const O_NOFOLLOW: i32 = 0x000100;

/// Open one path as a directory WITHOUT following a link and WITHOUT blocking (review r1, F1, #187):
/// `O_DIRECTORY` makes a swapped-in FIFO, device node or regular file fail the open AT ONCE (ENOTDIR) — a
/// plain `File::open` on a FIFO waits for a writer FOREVER and hangs the sweep (and finalize behind it) —
/// and `O_NOFOLLOW` makes a symlink fail the open itself (ELOOP), so nothing behind a link is ever opened.
/// Any failure is the caller's fail-closed refusal: skip, never block, never follow.
fn open_dir_no_follow(p: &Path) -> Option<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new().read(true).custom_flags(O_DIRECTORY | O_NOFOLLOW).open(p).ok()
}

/// Re-check one leftover's ancestor chain immediately before its move (#187): every ancestor the walk recorded
/// is opened with `open_dir_no_follow` — a directory, never a link, never a blocking file type — and the
/// handle's own stat (an fstat — no path is re-resolved after the open) must be the SAME real directory the
/// walk recorded: a directory, same dev, same ino. An ancestor swapped for a symlink, a FIFO or any other
/// type fails the OPEN itself; an ancestor swapped for a DIFFERENT real directory (a moved-in decoy: the
/// open succeeds, `is_dir` passes, the device is the same) fails the dev/ino COMPARE — the compare is the
/// only guard left in that case, so both checks are load-bearing. Either way `None` is fail-closed: the item
/// is skipped. The LAST ancestor — the leftover's immediate parent — is returned with its HANDLE HELD: the
/// rename then names the item relative to this verified directory, so no swap landing after the check can
/// redirect the move (the rule of this fix round: act on what was checked, never on a path looked up again
/// afterwards). The walk never descended a link, so a dev/ino match is by construction the real in-roots
/// ancestor.
fn verified_parent(chain: &[(PathBuf, u64, u64)]) -> Option<File> {
    let mut held: Option<File> = None;
    for (p, dev, ino) in chain {
        let f = open_dir_no_follow(p)?;
        let m = f.metadata().ok()?;
        if !m.is_dir() || m.dev() != *dev || m.ino() != *ino {
            return None;
        }
        held = Some(f);
    }
    held
}

/// Move one leftover onto the shelf with one `renameat(2)` RELATIVE TO TWO HELD DIRECTORY HANDLES (#187): the
/// source name is resolved inside the parent directory `verified_parent` just checked (its handle, held from
/// the check to this call), the item name inside the freshly minted batch directory (this process created it
/// exclusively at mode 0700 and holds its handle from before the first move). No full source path is
/// re-resolved at rename time, so an ancestor swapped for a symlink between the re-check and this call cannot
/// redirect the move onto a file outside the roots. This is the crate's second and last `unsafe`: one libc
/// call taking two borrowed descriptors and two relative names — a filesystem name can never hold a NUL byte,
/// so the two `CString`s are total, nothing is allocated beyond them, nothing is returned to free.
fn rename_verified(parent: &File, name: &std::ffi::OsStr, batch: &File, item: &str) -> std::io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let old = CString::new(name.as_bytes()).map_err(|_| std::io::Error::other("a move name holds a nul byte"))?;
    let new = CString::new(item).map_err(|_| std::io::Error::other("a move name holds a nul byte"))?;
    let r = unsafe { renameat(parent.as_raw_fd(), old.as_ptr(), batch.as_raw_fd(), new.as_ptr()) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

extern "C" {
    fn renameat(olddirfd: c_int, oldpath: *const c_char, newdirfd: c_int, newpath: *const c_char) -> c_int;
}

// ---------------------------------------------------------------- the sweep

/// `harness quarantine sweep` with the real device probe. Returns the success stdout line or a stop.
pub fn sweep_cli(a: &[String]) -> Result<String, Stop> {
    let args = parse_sweep(a)?;
    sweep_with_device_probe(&args, &real_device)
}

/// The sweep, with the device ids supplied by `probe` instead of the filesystem. The seam exists because a real
/// second filesystem is not reproducible in CI: the cross-device refusal is exercised by a probe that reports
/// differing ids (a test seam, named as such in the test; the real two-filesystem rename error is still covered
/// fail-closed by construction — any rename error stops the sweep with exit 3).
pub fn sweep_with_device_probe(a: &SweepArgs, device_of: &dyn Fn(&Path) -> Option<u64>) -> Result<String, Stop> {
    sweep_probed(a, device_of, &|_| {})
}

/// TEST SEAM (#187, named and documented exactly like `sweep_with_device_probe` above — no product caller may
/// grow on it): the sweep with a caller-supplied step run between an item's ancestor re-check and its move, so a
/// test can land an ancestor swap exactly inside that gap — deterministically, with no thread and no timing luck.
/// The step receives the item's source path and runs only for items whose chain re-check PASSED (a skipped item
/// never reaches its move, so the step never runs for it); everything else is the production code path, including
/// the move itself.
pub fn sweep_with_step_before_each_move(
    a: &SweepArgs,
    device_of: &dyn Fn(&Path) -> Option<u64>,
    step: &dyn Fn(&Path),
) -> Result<String, Stop> {
    sweep_probed(a, device_of, step)
}

fn sweep_probed(a: &SweepArgs, device_of: &dyn Fn(&Path) -> Option<u64>, step: &dyn Fn(&Path)) -> Result<String, Stop> {
    // Refusals first: every folder and placement check runs before anything is written.
    let ws = resolve_base_dir("--workspace-root", &a.workspace_root)?;
    let qd = resolve_base_dir("--quarantine-dir", &a.quarantine_dir)?;
    let md = resolve_base_dir("--manifest-dir", &a.manifest_dir)?;
    if qd.starts_with(&md) || md.starts_with(&qd) {
        return Err(err2("the quarantine dir and the manifest dir must lie outside each other"));
    }
    // The manifest is the ONE source of the roots: never a CLI argument, never guessed. A damaged manifest (missing,
    // torn, corrupt) owns nothing, so it refuses the whole sweep.
    let read = manifest::read_manifest(&md, &a.ticket).map_err(|_| err2("the manifest could not be read (missing or unreadable); nothing was moved"))?;
    if !read.is_clean() {
        return Err(err2("the manifest is damaged (torn tail or corrupt); nothing was moved"));
    }
    if read.roots.is_empty() {
        return Err(err2("the manifest holds no root; nothing was moved"));
    }
    let mut roots: Vec<PathBuf> = Vec::new();
    for r in &read.roots {
        let p = PathBuf::from(r);
        let meta = fs::symlink_metadata(&p).map_err(|_| err2("a manifest root is missing; nothing was moved"))?;
        if meta.file_type().is_symlink() {
            return Err(err2("a manifest root is a symlink; nothing was moved"));
        }
        if !meta.is_dir() {
            return Err(err2("a manifest root is not a directory; nothing was moved"));
        }
        let canon = p.canonicalize().map_err(|_| err2("a manifest root cannot be resolved; nothing was moved"))?;
        if canon == ws || !canon.starts_with(&ws) {
            return Err(err2("a manifest root resolves outside the workspace root; nothing was moved"));
        }
        if qd.starts_with(&canon) || canon.starts_with(&qd) {
            return Err(err2("the quarantine dir must lie outside every root (equality counts as inside)"));
        }
        roots.push(canon);
    }
    // The event log is fenced like the quarantine dir (review r1, F6): a log written inside a root is itself an
    // unmanifested leftover the NEXT sweep would carry off. The log path is resolved through its nearest
    // existing ancestor (the manifest's resolver), so a not-yet-created log dir inside a root is refused too.
    match manifest::resolve_for_manifest(&a.log) {
        Ok(lp) => {
            if roots.iter().any(|r| lp.starts_with(r)) {
                return Err(err2("the event log must lie outside every root; nothing was moved"));
            }
        }
        Err(_) => return Err(err2("the event log path does not resolve; nothing was moved")),
    }
    // Cross-device, before any move: there is NO copy fallback — copy-then-delete would be a delete.
    let q_dev = device_of(&qd).ok_or_else(|| err3("the quarantine dir cannot be examined"))?;
    for r in &roots {
        let dev = device_of(r).ok_or_else(|| err3("a root cannot be examined"))?;
        if dev != q_dev {
            return Err(err3("cross-device: a root and the quarantine dir are on different filesystems; nothing was moved"));
        }
    }
    // Crash recovery first, then the walk, then the moves.
    let shelf = qd.join(&a.ticket);
    // The shelf is examined with a no-follow stat BEFORE it is read (reconcile) or made (make_batch): a symlink
    // there would carry every batch, journal and item OUTSIDE the quarantine dir with exit 0 (review r1, F3,
    // Decision 5: placement is checked, never trusted); a regular file is refused the same way.
    match fs::symlink_metadata(&shelf) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(err2("the quarantine shelf cannot be examined; nothing was moved")),
        Ok(m) if m.file_type().is_symlink() => return Err(err2("the quarantine shelf is a symlink; nothing was moved")),
        Ok(m) if !m.is_dir() => return Err(err2("the quarantine shelf is not a directory; nothing was moved")),
        Ok(_) => {}
    }
    reconcile(a, &shelf, &ws)?;
    let live: Vec<PathBuf> = read
        .live
        .iter()
        .filter_map(|e| match &e.target {
            Target::Path(s) => Some(PathBuf::from(s)),
            _ => None,
        })
        .collect();
    let leftovers = walk(&ws, &roots, &live, a.max_items)?;
    if leftovers.is_empty() {
        return Ok(format!("quarantine=done ticket={} moved=0", a.ticket));
    }
    // The moves. Per item, in order: ANCESTOR RE-CHECK (every recorded ancestor opened and the handle's stat
    // matched against the walk's dev/ino), journal intent (write-ahead), absent check, rename RELATIVE TO THE
    // VERIFIED PARENT HANDLE, event. The re-check (review r2, F2 / RACE-S, window closed by #187): every
    // ancestor the walk recorded must still be the SAME real directory — same dev, same ino, a directory,
    // opened with O_DIRECTORY|O_NOFOLLOW so a swapped-in link, FIFO or other type fails the open itself
    // (never followed, never blocked) — and the immediate parent's HANDLE is held from the check to the rename, which names the item relative to that
    // handle, never by a path re-resolved at rename time. An ancestor swapped for a symlink before the re-check
    // makes the sweep SKIP the item: nothing moved for it, no journal line, no event, and the run ends exit 3
    // naming the item token (never a path). A swap landing between the re-check and the rename can no longer
    // redirect the move: the rename acts on the verified directory itself, so the file behind the swapped-in
    // link — outside the roots — is never touched.
    let (batch_name, batch) = make_batch(&shelf, a.ts_ms)?;
    // The batch directory is opened with the same no-follow, non-blocking directory open as the ancestor
    // re-check (review r1, F1): between `make_batch` and this open a same-user writer could swap the fresh
    // batch dir for a symlink or a FIFO; the flags make that fail closed here — the link is never followed,
    // the pipe never blocks — instead of handing the rename a handle onto something unverified.
    let batch_dir = open_dir_no_follow(&batch).ok_or_else(|| err3("a quarantine batch could not be examined"))?;
    let mut moved = 0usize;
    let mut skipped: Vec<String> = Vec::new();
    for (n, lo) in leftovers.iter().enumerate() {
        let item = format!("i-{}", n + 1);
        let Some(parent) = verified_parent(&lo.chain) else {
            skipped.push(item);
            continue; // fail closed for THIS item: nothing moved for it, no journal line, no event
        };
        step(&lo.src); // the #187 TEST SEAM: runs exactly between the re-check and the move (no product effect)
        let dest = batch.join(&item);
        append_intent(&batch, &item, &lo.rel, "unmanifested")?;
        if fs::symlink_metadata(&dest).is_ok() {
            return Err(err3("a quarantine item name is already taken; nothing further was moved"));
        }
        let name = lo
            .src
            .file_name()
            .ok_or_else(|| err3("a move failed mid-run; already-moved items stay moved"))?;
        rename_verified(&parent, name, &batch_dir, &item)
            .map_err(|_| err3("a move failed mid-run; already-moved items stay moved"))?;
        let from = from_rel(&a.ticket, &item, &lo.rel);
        let to = format!("{}/{}/{}", a.ticket, batch_name, item);
        write_quarantined_event(a, &from, &to, "unmanifested")?;
        moved += 1;
    }
    if !skipped.is_empty() {
        return Err(err3(&format!(
            "a leftover's parent chain changed mid-run ({}); nothing was moved for it, the rest was moved",
            skipped.join(", ")
        )));
    }
    Ok(format!("quarantine=done ticket={} moved={}", a.ticket, moved))
}

// ---------------------------------------------------------------- delete-listed

/// One parsed `quarantine delete-listed` command line.
pub struct DeleteArgs {
    pub ticket: String,
    pub id: String,
    pub manifest_dir: PathBuf,
    pub ts_ms: u64,
}

fn parse_delete(a: &[String]) -> Result<DeleteArgs, Stop> {
    let mut ticket: Option<String> = None;
    let (mut id, mut md, mut ts): (Option<&str>, Option<&str>, Option<&str>) = (None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--id" | "--manifest-dir" | "--ts-ms" => {
                let val = it.next().ok_or_else(|| usage_err("an option is missing its value"))?;
                let slot = match arg.as_str() {
                    "--id" => &mut id,
                    "--manifest-dir" => &mut md,
                    _ => &mut ts,
                };
                if slot.replace(val.as_str()).is_some() {
                    return Err(usage_err("an option was given twice"));
                }
            }
            s if s.starts_with("--") => return Err(usage_err("unknown option")),
            s => {
                if ticket.replace(s.to_string()).is_some() {
                    return Err(usage_err("more than one ticket given"));
                }
            }
        }
    }
    let ticket = ticket.ok_or_else(|| usage_err("no ticket given"))?;
    if !token_ok(&ticket) {
        return Err(usage_err("ticket must be a plain token (lowercase letters, digits, '-', '_', '.')"));
    }
    let id = id.ok_or_else(|| usage_err("--id is required"))?;
    if !token_ok(id) {
        return Err(usage_err("--id must be a plain token (lowercase letters, digits, '-', '_', '.')"));
    }
    let md = md.ok_or_else(|| usage_err("--manifest-dir is required"))?;
    if !dir_arg_ok(md) {
        return Err(usage_err("--manifest-dir must be an absolute path with no `..` component"));
    }
    let ts_ms = match ts {
        Some(s) => {
            if !num_ok(s) {
                return Err(usage_err("--ts-ms must be a non-negative integer"));
            }
            s.parse::<u64>().map_err(|_| usage_err("--ts-ms out of range"))?
        }
        None => SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).map_err(|_| err2("the clock is before the epoch"))?,
    };
    Ok(DeleteArgs { ticket, id: id.to_string(), manifest_dir: PathBuf::from(md), ts_ms })
}

/// One examined component of the listed path's parent chain: `Absent` (the path is not there — the clean
/// no-op re-run), or the identity of a REAL directory (dev, ino). A component that IS a link — a swapped
/// directory or a symlink LOOP — is not a step, it is a refusal: the listed path does not resolve inside a root.
enum Step {
    Absent,
    RealDir(u64, u64),
}

/// Examine ONE component, relative to the current directory, with a no-follow stat.
fn step(comp: &std::ffi::OsStr) -> Result<Step, Stop> {
    match fs::symlink_metadata(comp) {
        Ok(m) if m.file_type().is_symlink() => Err(err2(
            "the listed path does not resolve inside a root (a parent component is a symlink); nothing was deleted",
        )),
        Ok(m) if !m.is_dir() => Err(err2(
            "the listed path does not resolve inside a root (a parent component is not a directory); nothing was deleted",
        )),
        Ok(m) => Ok(Step::RealDir(m.dev(), m.ino())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Step::Absent),
        Err(_) => Err(err3("the listed path cannot be examined; nothing was deleted")),
    }
}

/// Examine ONE component and ENTER it: a no-follow stat, then a `chdir`, then a verification that the chdir
/// landed on the SAME directory the stat described (dev/ino of `.`) — a component swapped for a symlink in the
/// gap between the check and the act is never followed into (the rule of this fix round: act on what was
/// checked). Entering one component at a time also means no path longer than a single name is ever handed to
/// the filesystem, so a listed tree deeper than the path limit (PATH_MAX) is still reachable (review r2, F3).
fn chdir_verified(comp: &std::ffi::OsStr) -> Result<Step, Stop> {
    match step(comp)? {
        Step::Absent => Ok(Step::Absent),
        Step::RealDir(dev, ino) => {
            std::env::set_current_dir(comp).map_err(|_| err3("the listed path cannot be examined; nothing was deleted"))?;
            let now = fs::symlink_metadata(".").map_err(|_| err3("the listed path cannot be examined; nothing was deleted"))?;
            if now.dev() != dev || now.ino() != ino {
                return Err(err2(
                    "the listed path does not resolve inside a root (a parent component changed under the walk); nothing was deleted",
                ));
            }
            Ok(Step::RealDir(dev, ino))
        }
    }
}

/// The delete fence walk (review r2, F1/F3): starting from the re-validated `anchor` (a canonical root),
/// descend the listed path's intermediate components one at a time — each examined with a no-follow stat and
/// entered by a chdir verified against the stat — then act on the FINAL component by its relative name alone.
/// The whole spelled path is never stat'ed or removed as one string: a tree deeper than the filesystem's path
/// limit could not be examined that way at all (A8). A real directory is removed with `std::fs::remove_dir_all`,
/// the standard library's openat-based recursive delete — it never follows a symlink inside the tree, unlinks a
/// swapped-in link as the link, and recurses with no path length limit (review r2, F1: the hand-rolled
/// stat-then-open walk followed a child swapped for a link between the stat and the `read_dir`, deleting every
/// file in the link's target OUTSIDE the roots, 3 of 3 trials). A symlink entry is unlinked as the link only
/// (the `node_modules` rule); an absent path deletes nothing (the clean no-op re-run).
fn walk_and_delete(anchor: &Path, intermediates: &[std::ffi::OsString], final_name: &std::ffi::OsStr) -> Result<(), Stop> {
    if let Step::Absent = chdir_verified(anchor.as_os_str())? {
        return Ok(()); // the root vanished since validation: nothing there, the clean no-op
    }
    for comp in intermediates {
        if let Step::Absent = chdir_verified(comp.as_os_str())? {
            return Ok(()); // a missing intermediate: the listed path is not there, the clean no-op
        }
    }
    match fs::symlink_metadata(final_name) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()), // already gone: the clean no-op re-run
        Err(_) => Err(err3("the listed path cannot be examined; nothing was deleted")),
        Ok(m) if m.file_type().is_symlink() => {
            fs::remove_file(final_name).map_err(|_| err3("the listed link could not be removed"))
        }
        Ok(m) if m.is_dir() => fs::remove_dir_all(final_name).map_err(|_| err3("a listed folder could not be deleted")),
        Ok(_) => fs::remove_file(final_name).map_err(|_| err3("the listed file could not be removed")),
    }
}

/// The ONE delete path for listed `temp_dir` entries (P1B-14 teardown calls THIS function, never a rebuild):
/// delete exactly one live manifest entry of class `temp_dir`, named by id. The stored path is re-fenced on the
/// REAL WORLD, never on its spelling: any `..`/`.` component is refused, every root is re-validated as the
/// sweep validates it, and the stored spelling must start at a canonical root's spelling; the path is then
/// walked COMPONENT BY COMPONENT from that root (each intermediate verified a real directory, never a link,
/// and entered by a chdir verified against its stat), so a swapped component or a symlink LOOP in the parent
/// chain resolves to nothing and is refused (exit 2). The final component is examined with a no-follow stat
/// (a symlink entry is removed as the link only — the `node_modules` rule); a real directory is removed with
/// the standard library's openat-based `remove_dir_all`, which never follows a link inside, unlinks a
/// swapped-in link as the link, and has no path length limit. Only after the
/// path is gone is the manifest `remove` line written (a crash between leaves a listed half-deleted thing, never an
/// orphan; a re-run on an already-absent path just writes the remove).
/// The walk also changes the process working directory while it runs (saved and restored before returning), and the
/// working directory is shared by EVERY thread in the process — so the FIRST thing this function does is prove the
/// process is single-threaded and REFUSE otherwise (exit 3, one fixed folder-free stderr line naming threads, nothing
/// deleted, nothing written, before the manifest is even read); a host where the thread count cannot be read is
/// refused the same way. P1B-14 teardown therefore runs each delete as the `harness quarantine delete-listed` verb in
/// its own process (single-threaded by construction), one id per call, and treats exit 3 as teardown failure — never
/// an in-process retry.
pub fn delete_listed(a: &DeleteArgs) -> Result<String, Stop> {
    refuse_unless_single_thread()?;
    let md = resolve_base_dir("--manifest-dir", &a.manifest_dir)?;
    let read = manifest::read_manifest(&md, &a.ticket).map_err(|_| err2("the manifest could not be read (missing or unreadable); nothing was deleted"))?;
    if !read.is_clean() {
        return Err(err2("the manifest is damaged (torn tail or corrupt); nothing was deleted"));
    }
    let Some(ent) = read.live.iter().find(|e| e.id == a.id) else {
        return Err(err2("no live manifest entry with that id"));
    };
    if ent.class != Class::TempDir {
        return Err(err2("only temp_dir entries may be deleted through this verb"));
    }
    let Target::Path(p) = &ent.target else {
        return Err(err2("only temp_dir entries may be deleted through this verb"));
    };
    let path = PathBuf::from(p);
    // The delete fence is made on the RESOLVED path, never on its spelling (review r1, F2: a lexical
    // `starts_with` accepted `..` spellings and symlinked components, deleting outside data with exit 0).
    // First: any `..` or `.` component is refused outright — the RAW TEXT is scanned because `Path` itself
    // hides interior and trailing `.` (the same discipline `manifest.rs::resolve_for_manifest` uses); no
    // folding, no dropping: what would be deleted is not what was fenced. A non-UTF-8 spelling fails closed.
    match path.to_str() {
        Some(t) if t.split('/').any(|seg| seg == "." || seg == "..") => {
            return Err(err2("the listed path holds a `..` or `.` component; nothing was deleted"));
        }
        None => return Err(err2("the listed path is not utf-8; nothing was deleted")),
        Some(_) => {}
    }
    // Second: every root is re-validated exactly as the sweep validates it (a no-follow stat, a real directory,
    // canonicalizable, never `/`): a root swapped for a symlink or gone missing since the manifest was written
    // is a different world, and deleting into it is refused before anything is examined.
    let mut canon_roots: Vec<PathBuf> = Vec::new();
    for r in &read.roots {
        let rp = Path::new(r.as_str());
        let meta = fs::symlink_metadata(rp).map_err(|_| err2("a manifest root is missing; nothing was deleted"))?;
        if meta.file_type().is_symlink() {
            return Err(err2("a manifest root is a symlink; nothing was deleted"));
        }
        if !meta.is_dir() {
            return Err(err2("a manifest root is not a directory; nothing was deleted"));
        }
        let canon = rp.canonicalize().map_err(|_| err2("a manifest root cannot be resolved; nothing was deleted"))?;
        if canon == Path::new("/") {
            return Err(err2("a manifest root may not be the filesystem root; nothing was deleted"));
        }
        canon_roots.push(canon);
    }
    // Third: the fence is a COMPONENT WALK from a re-validated root (review r2, F1/F3), never a stat or a
    // removal of the whole spelled path as one string. The stored spelling must start at a canonical root's
    // spelling (a hand-appended line with a foreign spelling is refused, fail closed); every INTERMEDIATE
    // component is examined and entered ONE AT A TIME — a no-follow stat, then a chdir verified to have landed
    // on the same directory by dev/ino, so no symlink anywhere in the parent chain (a swapped component, a
    // symlink LOOP) is ever followed — and the final component is acted on by its RELATIVE name alone, so a
    // tree deeper than the filesystem's path limit is still deletable. Containment holds by construction: the
    // walk starts inside a validated root and never leaves it through a link.
    let Some(anchor) = canon_roots.iter().find(|r| path.starts_with(r.as_path())) else {
        return Err(err2("the listed path does not resolve inside a root; nothing was deleted"));
    };
    let rel = path.strip_prefix(anchor).map_err(|_| err2("the listed path does not resolve inside a root; nothing was deleted"))?;
    let mut comps: Vec<std::ffi::OsString> = Vec::new();
    for c in rel.components() {
        match c {
            Component::Normal(x) => comps.push(x.to_os_string()),
            _ => return Err(err2("the listed path holds a `..` or `.` component; nothing was deleted")),
        }
    }
    let Some((final_name, intermediates)) = comps.split_last() else {
        return Err(err2("the listed path is a root itself; nothing was deleted"));
    };
    // The walk changes the process directory; it is saved and restored (every path used afterwards is
    // absolute), and the removal itself happens inside the walk, on the relative final name only.
    let saved_cwd = std::env::current_dir().map_err(|_| err3("the current folder cannot be examined; nothing was deleted"))?;
    let walked = walk_and_delete(anchor, intermediates, final_name);
    let _ = std::env::set_current_dir(&saved_cwd);
    walked?;
    manifest::apply(&md, &a.ticket, &Op::Remove { id: &a.id }, a.ts_ms)
        .map_err(|_| err3("the manifest remove line could not be written"))?;
    Ok(format!("delete=done ticket={} id={}", a.ticket, a.id))
}

/// The single-thread guard (#185): the delete and drop-build walks change the process working directory, which
/// every thread in the process shares, so before `delete_listed` or `drop_build` reads anything or takes one step
/// the guard proves THIS process holds exactly one thread, and refuses (exit 3, nothing deleted, nothing removed)
/// when it does not or when it cannot tell. The proof is sound for the whole walk: a process holding exactly one
/// thread cannot gain another while the walk runs, because only the running thread could spawn one. macOS: the one
/// libSystem call `pthread_is_threaded_np` (no arguments), which is STICKY — any thread ever started, even one
/// long joined, keeps answering threaded — stricter, still fail-closed. Every other Unix: the thread count is read
/// from procfs; an unreadable answer (no procfs, an odd container) is a refusal, never a pass. The child verbs
/// `harness quarantine delete-listed` and `harness quarantine drop-build` are single-threaded by construction, so
/// the library callers (P1B-14 teardown, the later wrapper finish step) run them as their own processes.
fn refuse_unless_single_thread() -> Result<(), Stop> {
    #[cfg(target_os = "macos")]
    let alone = {
        // One of the crate's two `unsafe` blocks (the other is `renameat` in the sweep's move): one call into
        // libSystem, no arguments, a boolean query on this process's own thread list — it touches no resource,
        // returns nothing to free, and cannot fail.
        extern "C" {
            fn pthread_is_threaded_np() -> i32;
        }
        let threaded = unsafe { pthread_is_threaded_np() };
        threaded == 0
    };
    #[cfg(not(target_os = "macos"))]
    let alone = {
        let rd = fs::read_dir("/proc/self/task")
            .map_err(|_| err3("the thread count cannot be read (procfs); nothing was deleted"))?;
        let mut n = 0u32;
        for e in rd {
            let _ = e.map_err(|_| err3("the thread count cannot be read (procfs); nothing was deleted"))?;
            n += 1; // procfs task entries are the thread ids themselves (no `.` or `..`); anything at all counts
            if n > 1 {
                break;
            }
        }
        n == 1
    };
    if alone {
        Ok(())
    } else {
        Err(err3(
            "this verb refuses to run while another thread exists in this process (the walk changes the process working directory); run it as the harness quarantine verb in its own process; nothing was deleted, nothing was removed",
        ))
    }
}

// ---------------------------------------------------------------- drop-build

/// Fixed cap on `target` matches removed by one drop-build. Over it: exit 3, nothing removed.
const DROP_MAX_MATCHES: usize = 1000;
/// Fixed cap on entries visited by the drop-build walk (all roots together). Over it: exit 3, nothing removed.
const DROP_MAX_VISITED: usize = 200_000;
/// Fixed cap on drop-build walk depth below a root. Over it: exit 3, nothing removed.
const DROP_MAX_DEPTH: usize = 16;

// `open(2)` non-blocking flag, spelled per platform like `O_DIRECTORY`/`O_NOFOLLOW` above (the tag open must
// refuse a FIFO at once instead of waiting for a writer).
#[cfg(target_os = "linux")]
const O_NONBLOCK: i32 = 0o4000;
#[cfg(target_os = "macos")]
const O_NONBLOCK: i32 = 0x0004;

/// The cache tag file name (the cache directory tag standard; the tag cargo writes).
const CACHEDIR_TAG: &str = "CACHEDIR.TAG";
/// The cache tag signature line, newline included: the first line the standard (and cargo) writes.
const CACHE_TAG_SIG: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55\n";

/// One parsed `quarantine drop-build` command line. `dry_run` set means the walk runs and nothing is removed.
pub struct DropArgs {
    pub ticket: String,
    pub roots: Vec<PathBuf>,
    pub manifest_dir: PathBuf,
    pub caps_dir: PathBuf,
    pub dry_run: bool,
}

fn parse_drop(a: &[String]) -> Result<DropArgs, Stop> {
    let mut ticket: Option<String> = None;
    let mut roots: Vec<String> = Vec::new();
    let (mut md, mut cd): (Option<&str>, Option<&str>) = (None, None);
    let mut dry = false;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--root" => {
                let val = it.next().ok_or_else(|| usage_err("an option is missing its value"))?;
                roots.push(val.clone());
                if roots.len() > 8 {
                    return Err(usage_err("--root may be given at most 8 times"));
                }
            }
            "--manifest-dir" | "--caps-dir" => {
                let val = it.next().ok_or_else(|| usage_err("an option is missing its value"))?;
                let slot = if arg.as_str() == "--manifest-dir" { &mut md } else { &mut cd };
                if slot.replace(val.as_str()).is_some() {
                    return Err(usage_err("an option was given twice"));
                }
            }
            "--dry-run" => {
                if dry {
                    return Err(usage_err("an option was given twice"));
                }
                dry = true;
            }
            s if s.starts_with("--") => return Err(usage_err("unknown option")),
            s => {
                if ticket.replace(s.to_string()).is_some() {
                    return Err(usage_err("more than one ticket given"));
                }
            }
        }
    }
    let ticket = ticket.ok_or_else(|| usage_err("no ticket given"))?;
    if !token_ok(&ticket) {
        return Err(usage_err("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')"));
    }
    if roots.is_empty() {
        return Err(usage_err("--root is required"));
    }
    for (flag, val) in [("--manifest-dir", req("manifest-dir", md)?), ("--caps-dir", req("caps-dir", cd)?)] {
        if !dir_arg_ok(val) {
            return Err(usage_err(&format!("{} must be an absolute path with no `..` component", flag)));
        }
    }
    Ok(DropArgs {
        ticket,
        roots: roots.iter().map(|r| PathBuf::from(r.as_str())).collect(),
        manifest_dir: PathBuf::from(req("manifest-dir", md)?),
        caps_dir: PathBuf::from(req("caps-dir", cd)?),
        dry_run: dry,
    })
}

/// One digit group of a ticket folder suffix: 1 to 20 ASCII digits.
fn digit_group_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

/// `<d>.<d>`, both groups plain digit groups.
fn two_digit_groups(s: &str) -> bool {
    match s.split_once('.') {
        Some((a, b)) => digit_group_ok(a) && digit_group_ok(b) && !b.contains('.'),
        None => false,
    }
}

/// The three ticket folder forms: exactly `<t>`, `<t>.old.<d>.<d>` or `<t>.<d>.<d>` (the private wrapper's
/// gate-clone, moved-aside clone and finish-time build-output names).
fn ticket_folder_ok(name: &str, t: &str) -> bool {
    if name == t {
        return true;
    }
    let Some(rest) = name.strip_prefix(t) else { return false };
    if let Some(tail) = rest.strip_prefix(".old.") {
        return two_digit_groups(tail);
    }
    rest.starts_with('.') && two_digit_groups(&rest[1..])
}

/// Other ticket ids `name` also matches under the three forms: its own whole name (`a.1.2` is also ticket
/// `a.1.2`), its `<id>.<d>.<d>` reading and its `<id>.old.<d>.<d>` reading (`foo.old.1.2` is also `foo.old`).
/// Each is kept only if it passes `token_ok`.
fn other_ticket_ids(name: &str, t: &str) -> Vec<String> {
    let parts: Vec<&str> = name.split('.').collect();
    let n = parts.len();
    let mut out: Vec<String> = Vec::new();
    if name != t && token_ok(name) {
        out.push(name.to_string());
    }
    if n >= 3 && digit_group_ok(parts[n - 2]) && digit_group_ok(parts[n - 1]) {
        let id = parts[..n - 2].join(".");
        if id != t && token_ok(&id) {
            out.push(id);
        }
    }
    if n >= 4 && parts[n - 3] == "old" && digit_group_ok(parts[n - 2]) && digit_group_ok(parts[n - 1]) {
        let id = parts[..n - 3].join(".");
        if id != t && token_ok(&id) {
            out.push(id);
        }
    }
    out
}

/// Does a manifest file for `oid` sit in `md`? `NotFound` is a plain no; any other stat error is undecidable,
/// so the run refuses: exit 3, one line, nothing removed.
fn manifest_present(md: &Path, oid: &str) -> Result<bool, Stop> {
    match fs::symlink_metadata(md.join(format!("{oid}.jsonl"))) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(err3("a manifest file cannot be examined; nothing was removed")),
    }
}

/// Read the cache tag at `p`: opened with no link follow and no blocking (`O_NOFOLLOW | O_NONBLOCK`), the
/// handle must be a regular file, at most 64 bytes read, and the bytes must start with the signature line.
/// Any miss is `false`: the `target` folder is kept.
fn cache_tag_ok(p: &Path) -> bool {
    use std::io::Read as _;
    use std::os::unix::fs::OpenOptionsExt;
    let Ok(f) = OpenOptions::new().read(true).custom_flags(O_NOFOLLOW | O_NONBLOCK).open(p) else {
        return false;
    };
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return false;
    }
    let mut buf = Vec::new();
    f.take(64).read_to_end(&mut buf).is_ok() && buf.starts_with(CACHE_TAG_SIG)
}

/// One `target` match of the walk: the root it was found under (index into the canonical roots) and the
/// components from that root to the `target` folder (the last component is `target`).
struct DropMatch(usize, Vec<std::ffi::OsString>);

/// Counters carried across the walk of every root.
struct DropWalk {
    visited: usize,
    kept: usize,
    matches: Vec<DropMatch>,
}

/// Walk one folder inside a ticket folder (read only): a `target` that is a real directory on the root's device
/// holding the cache tag is a match; any other `target` is kept (counted) or skipped (a link, a file); links
/// and `.git` are not entered.
fn drop_walk_inside(
    dir: &Path,
    child_depth: usize,
    root: &Path,
    root_idx: usize,
    root_dev: u64,
    w: &mut DropWalk,
) -> Result<(), Stop> {
    let rd = fs::read_dir(dir).map_err(|_| err3("a ticket folder cannot be read; nothing was removed"))?;
    for ent in rd {
        let ent = ent.map_err(|_| err3("an entry in a ticket folder cannot be examined; nothing was removed"))?;
        w.visited += 1;
        if w.visited > DROP_MAX_VISITED {
            return Err(err3("over the entries-visited cap (200000); nothing was removed"));
        }
        if child_depth > DROP_MAX_DEPTH {
            return Err(err3("over the walk depth cap (16); nothing was removed"));
        }
        let name = ent.file_name();
        let p = dir.join(&name);
        let meta = fs::symlink_metadata(&p).map_err(|_| err3("an entry in a ticket folder cannot be examined; nothing was removed"))?;
        if name.as_encoded_bytes() == b".git" {
            continue; // not entered
        }
        if name.as_encoded_bytes() == b"target" {
            if meta.file_type().is_symlink() || !meta.is_dir() {
                continue; // a link named target is not followed, a file is not a folder: neither counted
            }
            if meta.dev() != root_dev || !cache_tag_ok(&p.join(CACHEDIR_TAG)) {
                w.kept += 1;
                continue;
            }
            if w.matches.len() >= DROP_MAX_MATCHES {
                return Err(err3("over the matches cap (1000); nothing was removed"));
            }
            let rel = p.strip_prefix(root).map_err(|_| err3("an entry in a ticket folder cannot be named; nothing was removed"))?;
            let comps: Vec<std::ffi::OsString> = rel
                .components()
                .filter_map(|c| match c {
                    Component::Normal(x) => Some(x.to_os_string()),
                    _ => None,
                })
                .collect();
            w.matches.push(DropMatch(root_idx, comps));
            continue;
        }
        if meta.file_type().is_symlink() || !meta.is_dir() {
            continue; // links are not entered, files are not walked
        }
        drop_walk_inside(&p, child_depth + 1, root, root_idx, root_dev, w)?;
    }
    Ok(())
}

/// What one component step of the removal walk found.
enum DropIn {
    /// Not there: the match is kept, not an error.
    Absent,
    Dir,
}

/// One no-follow stat, then a chdir verified to have landed on that same directory by dev/ino (the same
/// discipline as `chdir_verified`): a component that is a link, not a directory, or was swapped between
/// the stat and the chdir is refused; a vanished component is reported absent.
fn drop_chdir(comp: &OsStr) -> Result<DropIn, ()> {
    let m = match fs::symlink_metadata(comp) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(DropIn::Absent),
        Err(_) => return Err(()),
        Ok(m) => m,
    };
    if m.file_type().is_symlink() || !m.is_dir() {
        return Err(());
    }
    std::env::set_current_dir(comp).map_err(|_| ())?;
    let now = fs::symlink_metadata(".").map_err(|_| ())?;
    if now.dev() != m.dev() || now.ino() != m.ino() {
        return Err(());
    }
    Ok(DropIn::Dir)
}

/// Remove one matched `target` folder, reached from its canonical root ONE COMPONENT AT A TIME (each entered
/// by a chdir verified against its stat, so no link in the chain is followed). After the last chdir the final
/// `target` is checked AGAIN — a real directory, on the root's device, holding the cache tag; a check that
/// fails now is `Ok(false)`: the folder is kept. Inside `target`, each entry other than the tag is removed
/// (a folder with `remove_dir_all`, a file or link with `remove_file`), then the tag, then `target` itself by
/// `remove_dir` from its parent reached again from the root — so a removal that stops before the tag is
/// removed leaves the tag in place and the next run finds the folder again, while a failure after the tag is
/// removed (re-reaching the parent or removing the emptied `target` itself) leaves an empty `target` with no
/// tag, which later runs keep.
/// `Err(())` is a mid-removal failure: the run stops at this match.
fn remove_target(root: &Path, comps: &[std::ffi::OsString], root_dev: u64) -> Result<bool, ()> {
    let Some((final_name, mids)) = comps.split_last() else {
        return Err(());
    };
    drop_chdir(root.as_os_str())?;
    for c in mids {
        if let DropIn::Absent = drop_chdir(c.as_os_str())? {
            return Ok(false);
        }
    }
    // the second check, on the final component by its relative name alone
    match fs::symlink_metadata(final_name) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(()),
        Ok(m) => {
            if m.file_type().is_symlink() || !m.is_dir() || m.dev() != root_dev {
                return Ok(false);
            }
            if !cache_tag_ok(&Path::new(final_name).join(CACHEDIR_TAG)) {
                return Ok(false);
            }
        }
    }
    if let DropIn::Absent = drop_chdir(final_name.as_os_str())? {
        return Ok(false);
    }
    let rd = fs::read_dir(".").map_err(|_| ())?;
    for ent in rd {
        let ent = ent.map_err(|_| ())?;
        let name = ent.file_name();
        if name.as_encoded_bytes() == CACHEDIR_TAG.as_bytes() {
            continue;
        }
        let m = fs::symlink_metadata(&name).map_err(|_| ())?;
        if m.is_dir() {
            fs::remove_dir_all(&name).map_err(|_| ())?;
        } else {
            fs::remove_file(&name).map_err(|_| ())?; // a file or a link: unlinked, not followed
        }
    }
    fs::remove_file(CACHEDIR_TAG).map_err(|_| ())?;
    // the parent is reached again from the root, then the folder itself goes by relative name
    drop_chdir(root.as_os_str())?;
    for c in mids {
        if let DropIn::Absent = drop_chdir(c.as_os_str())? {
            return Ok(false);
        }
    }
    fs::remove_dir(final_name).map_err(|_| ())?;
    Ok(true)
}

/// `harness quarantine drop-build <ticket> --root <abs> [--root <abs> ...] --manifest-dir <abs>
/// --caps-dir <abs> [--dry-run]`: remove the cache-tagged build output of a CLOSED ticket under the given
/// roots, or refuse. Order: usage, ticket id, the single-thread guard (the removal changes the process
/// working directory), the manifest and caps dirs (real directories, canonicalized once), each root (real,
/// not a link, canonicalized once, its canonical parent equal to the manifest dir's, so the roots sit beside
/// the manifests folder in one state folder; the manifest and caps dirs lie inside no root; no two roots
/// overlap), the per-ticket caps lock (Busy: exit 3 — a run for this ticket is in progress), the closed
/// proof (the manifest reads, is clean, holds no live entry and a remove line for the id `worktree`), then
/// the read-only walk, then the removals. A `--dry-run` runs the same walk and removes nothing. The first
/// failing step before the removals is one stderr line (no folder on either stream; a usage error prints its
/// error line and the USAGE text) and stops with nothing removed; a failure inside the removals stops the
/// run with what it already removed kept.
pub fn drop_build(a: &DropArgs) -> Result<String, Stop> {
    refuse_unless_single_thread()?;
    if a.roots.is_empty() {
        return Err(usage_err("--root is required"));
    }
    let md = resolve_base_dir("--manifest-dir", &a.manifest_dir)?;
    let cd = resolve_base_dir("--caps-dir", &a.caps_dir)?;
    let state = md.parent().ok_or_else(|| err2("--manifest-dir has no parent folder"))?;
    let mut canon: Vec<PathBuf> = Vec::new();
    for (i, r) in a.roots.iter().enumerate() {
        let n = i + 1;
        let flag = format!("--root {n}");
        if !r.is_absolute() {
            return Err(err2(&format!("{flag} must be an absolute path with no `.` or `..` component")));
        }
        // the no-follow stat runs on the path rebuilt from its components: lstat on a spelling with a trailing
        // "/" or "/." resolves through a link at the final component (review r1, B2), which must be refused
        let spelled: PathBuf = r.components().collect();
        let meta = fs::symlink_metadata(&spelled).map_err(|_| err2(&format!("{flag} is missing or unreadable")))?;
        if meta.file_type().is_symlink() {
            return Err(err2(&format!("{flag} must be a real directory (a link is refused)")));
        }
        if !meta.is_dir() {
            return Err(err2(&format!("{flag} must be a real directory (a file is refused)")));
        }
        // the raw text is scanned because `Path` hides interior `.`
        let text = r.to_str().ok_or_else(|| err2(&format!("{flag} is not utf-8")))?;
        if text.split('/').any(|seg| seg == "." || seg == "..") {
            return Err(err2(&format!("{flag} must be an absolute path with no `.` or `..` component")));
        }
        let c = r.canonicalize().map_err(|_| err2(&format!("{flag} cannot be resolved")))?;
        if c == Path::new("/") {
            return Err(err2(&format!("{flag} may not be the filesystem root")));
        }
        if c.parent() != Some(state) {
            return Err(err2(&format!("{flag} must sit beside the manifest dir in one state folder")));
        }
        if cd.starts_with(&c) {
            return Err(err2("the caps dir must lie outside every --root (equality counts as inside)"));
        }
        if md.starts_with(&c) {
            return Err(err2("the manifest dir must lie outside every --root (equality counts as inside)"));
        }
        for (j, prev) in canon.iter().enumerate() {
            if c == *prev || c.starts_with(prev) || prev.starts_with(&c) {
                return Err(err2(&format!("{flag} and --root {} must lie outside each other", j + 1)));
            }
        }
        canon.push(c);
    }
    // the per-ticket lock, taken without waiting and held until the verb ends: a gate run for this ticket
    // gets Busy from the other side too. The closed proof is read UNDER the lock.
    let caps = Caps::open(&a.caps_dir, &canon[0]).map_err(|m| err2(&m))?;
    let _lock = caps.lock(&a.ticket).map_err(|e| match e {
        LockError::Busy => err3("a run for this ticket is in progress; nothing was removed"),
        LockError::Other(m) => err2(&m),
    })?;
    let read = manifest::read_manifest(&md, &a.ticket)
        .map_err(|_| err2("the manifest could not be read (missing or unreadable); nothing was removed"))?;
    if !read.is_clean() {
        return Err(err2("the manifest is damaged (torn tail or corrupt); nothing was removed"));
    }
    if !read.live.is_empty() {
        return Err(err2("the manifest holds a live entry, so the ticket is not closed; nothing was removed"));
    }
    if !read.lines.iter().any(|l| matches!(l, Line::Remove { id, .. } if id == "worktree")) {
        return Err(err2("the manifest holds no worktree remove line, so the ticket is not closed; nothing was removed"));
    }
    // the walk: read only, complete before the first removal
    let mut w = DropWalk { visited: 0, kept: 0, matches: Vec::new() };
    let mut root_devs: Vec<u64> = Vec::new();
    for (idx, c) in canon.iter().enumerate() {
        let meta = fs::symlink_metadata(c).map_err(|_| err3("a root cannot be walked; nothing was removed"))?;
        root_devs.push(meta.dev());
        let rd = fs::read_dir(c).map_err(|_| err3("a root cannot be walked; nothing was removed"))?;
        for ent in rd {
            let ent = ent.map_err(|_| err3("a root cannot be walked; nothing was removed"))?;
            w.visited += 1;
            if w.visited > DROP_MAX_VISITED {
                return Err(err3("over the entries-visited cap (200000); nothing was removed"));
            }
            let name = ent.file_name();
            let Some(name_str) = name.to_str() else { continue };
            if !ticket_folder_ok(name_str, &a.ticket) {
                continue; // another ticket's folder, or no ticket folder at all
            }
            let p = c.join(&name);
            let meta = fs::symlink_metadata(&p)
                .map_err(|_| err3("an entry in a root cannot be examined; nothing was removed"))?;
            if meta.file_type().is_symlink() || !meta.is_dir() {
                continue; // a linked ticket folder is not followed
            }
            // a child that also matches another id with its own manifest belongs to that ticket: skipped
            let mut other = false;
            for oid in other_ticket_ids(name_str, &a.ticket) {
                if manifest_present(&md, &oid)? {
                    other = true;
                    break;
                }
            }
            if other {
                continue;
            }
            drop_walk_inside(&p, 2, c, idx, root_devs[idx], &mut w)?;
        }
    }
    if a.dry_run {
        return Ok(format!("drop-build=dry-run ticket={} would_remove={} kept={}", a.ticket, w.matches.len(), w.kept));
    }
    // the removals, in a deterministic order (root path, then components), so a partial run is reproducible
    w.matches.sort_by(|x, y| (&canon[x.0], &x.1).cmp(&(&canon[y.0], &y.1)));
    let saved = std::env::current_dir().map_err(|_| err3("the current folder cannot be examined; nothing was removed"))?;
    let mut removed = 0usize;
    let mut kept = w.kept;
    for DropMatch(ri, comps) in &w.matches {
        match remove_target(&canon[*ri], comps, root_devs[*ri]) {
            Ok(true) => removed += 1,
            Ok(false) => kept += 1,
            Err(()) => {
                let _ = std::env::set_current_dir(&saved);
                return Err(err3(&format!(
                    "a build output folder could not be removed completely; removed={removed} before it"
                )));
            }
        }
    }
    let _ = std::env::set_current_dir(&saved);
    Ok(format!("drop-build=done ticket={} removed={} kept={}", a.ticket, removed, kept))
}

// ---------------------------------------------------------------- the verb

/// `harness quarantine sweep <ticket> ... | delete-listed <ticket> ... | drop-build <ticket> ...` (the arguments
/// after `quarantine`). The exit lines here are those of sweep and delete-listed; drop-build's are at `drop_build`. Exit 0
/// done (one stdout line of fixed fields, no folder); 2 refused (usage, bad or misplaced folders, a root that is a
/// symlink or outside the base, a damaged manifest, wrong class or id); 3 cannot proceed (cross-device, over a cap,
/// an undecidable journal intent, an event log that cannot be read whole by reconcile, a rename/journal/event write
/// failure mid-run).
pub fn quarantine_cmd(a: &[String]) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{}", USAGE);
        return 2;
    };
    let r = match sub.as_str() {
        "sweep" => sweep_cli(rest),
        "delete-listed" => parse_delete(rest).and_then(|args| delete_listed(&args)),
        "drop-build" => parse_drop(rest).and_then(|args| drop_build(&args)),
        _ => Err(err2("unknown quarantine subcommand")),
    };
    match r {
        Ok(line) => {
            println!("{}", line);
            0
        }
        Err(Stop(code, line)) => {
            eprintln!("{}", line);
            code
        }
    }
}
