//! `harness lane stale` (std only, Unix): the stale-lane check. It reads the live lane records, the launch records and the host session
//! list, and prints one verdict per live record; it runs no stop, rm or fetch (`docs/lanes.md`, *The stale check*). Without `--apply` it
//! writes nothing, calls the host once (`agents --json --all`) and the lock is released before the host or git runs. With `--apply` a
//! record judged `stale gone-clean-pushed` goes through the park step: one critical section under the lanes dir lock that reads the lanes
//! dir, the host listing, the launch records and the tree again, and parks the lane only when the verdict is still that one. The clean and
//! pushed answers are the child runs `harness workspace check` and `harness workspace check --pushed` of this same binary, each under a
//! 30 second limit and a 1 MiB cap per stream.
//! Exit 0: no verdict is `unknown`; 3: one or more are; 2: a usage error, a whole-run failure (`lane: error: <cause>`, empty stdout) or a
//! park body error (the lines of the records handled before it stay on stdout).

use super::{
    check_dir, fail, lane_path, list_records, lock, name_cap, park_locked, path_spelling_ok, read_parked, record_spec, state_locked, Record, ReleaseSpec,
    OPEN_NOFOLLOW_NONBLOCK,
};
use crate::config::Config;
use crate::launcher::{kill_group, read_launches, Launch};
use crate::repo_id::RepoId;
use crate::session_close::{listing, slug as slug_of, Entry};
use crate::session_name::{self, Verdict};
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The lane bound given to `session_name::check`: the 15-digit cap of a lane number, so a number above `lanes.count` still reads back.
const ANY_LANE: u64 = 999_999_999_999_999;
const CHILD_LIMIT_MS: u64 = 30_000;
const CHILD_CAP: usize = 1 << 20;
const MAX_MAPS: usize = 16;
const MAX_ALLOW: usize = 16;
const NO_UPSTREAM: &str = "harness: error: the ticket branch has no upstream";
const MAX_REASON: usize = 900;

type Out = (&'static str, &'static str);

/// The `--apply` options: the park line parts and the records dir (checked by `record_spec`).
struct Apply {
    reason: String,
    at: String,
    records_dir: PathBuf,
}

struct Opts {
    apply: Option<Apply>,
    lanes_dir: PathBuf,
    config: PathBuf,
    host: PathBuf,
    launch_records: PathBuf,
    manifest_dir: PathBuf,
    maps: Vec<(String, PathBuf)>,
    allow: Vec<String>,
}

fn path_opt(name: &str, v: &str) -> Result<PathBuf, String> {
    if !v.starts_with('/') || !path_spelling_ok(v) {
        return Err(format!("--{name} must be an absolute path with no trailing slash and no . or .. part"));
    }
    Ok(PathBuf::from(v))
}

/// A tree-relative `--allow` path: `A-Z a-z 0-9 . _ -` and `/`, not starting or ending with `/`, no empty, `.` or `..` part.
fn allow_ok(v: &str) -> bool {
    !v.is_empty()
        && v.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
        && !v.split('/').any(|p| p.is_empty() || p == "." || p == "..")
}

fn parse(a: &[String]) -> Result<Opts, String> {
    let mut one: Vec<(&str, String)> = Vec::new();
    let (mut maps, mut allow): (Vec<(String, PathBuf)>, Vec<String>) = (Vec::new(), Vec::new());
    let mut apply = false;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let Some(k) = arg.strip_prefix("--") else { return Err(format!("unexpected argument {arg}")) };
        if !["lanes-dir", "config", "host", "launch-records", "manifest-dir", "repo-map", "allow", "apply", "reason", "at", "records-dir"].contains(&k) {
            return Err(format!("unknown option {arg}"));
        }
        if k == "apply" {
            if apply {
                return Err("--apply was given twice".to_string());
            }
            apply = true;
            continue;
        }
        let v = it.next().ok_or_else(|| format!("{arg} is missing its value"))?;
        match k {
            "repo-map" => {
                let (id, dir) = v.split_once('=').ok_or("--repo-map must be <repo-id>=<absolute-dir>")?;
                RepoId::parse(id).map_err(|_| "--repo-map has a repo id that is not a plain name".to_string())?;
                if maps.iter().any(|(i, _)| i == id) {
                    return Err(format!("--repo-map names the repo id {id} twice"));
                }
                if maps.len() >= MAX_MAPS {
                    return Err(format!("more than {MAX_MAPS} --repo-map values"));
                }
                maps.push((id.to_string(), path_opt("repo-map", dir)?));
            }
            "allow" => {
                if !allow_ok(v) {
                    return Err("--allow must be a tree-relative path of A-Z a-z 0-9 . _ - / with no empty, . or .. part".to_string());
                }
                if allow.len() >= MAX_ALLOW {
                    return Err(format!("more than {MAX_ALLOW} --allow values"));
                }
                allow.push(v.clone());
            }
            _ => {
                if one.iter().any(|(n, _)| *n == k) {
                    return Err(format!("{arg} was given twice"));
                }
                one.push((k, v.clone()));
            }
        }
    }
    let get = |k: &str| -> Result<PathBuf, String> {
        let v = one.iter().find(|(n, _)| *n == k).map(|(_, v)| v.as_str()).ok_or_else(|| format!("--{k} is required"))?;
        path_opt(k, v)
    };
    let (lanes_dir, config, host) = (get("lanes-dir")?, get("config")?, get("host")?);
    let (launch_records, manifest_dir) = (get("launch-records")?, get("manifest-dir")?);
    if maps.is_empty() {
        return Err("--repo-map is required".to_string());
    }
    let val = |k: &str| one.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
    let given = [apply, val("reason").is_some(), val("at").is_some(), val("records-dir").is_some()];
    let apply = if given.iter().all(|g| !g) {
        None
    } else if given.iter().all(|g| *g) {
        let m: std::collections::HashMap<String, String> = ["reason", "at", "records-dir"].iter().map(|k| (k.to_string(), val(k).unwrap_or_default())).collect();
        record_spec(&m, &lanes_dir)?;
        if m["reason"].len() > MAX_REASON {
            return Err(format!("--reason is over {MAX_REASON} bytes"));
        }
        Some(Apply { reason: m["reason"].clone(), at: m["at"].clone(), records_dir: PathBuf::from(&m["records-dir"]) })
    } else {
        return Err("--apply, --reason, --at and --records-dir go together".to_string());
    };
    Ok(Opts { apply, lanes_dir, config, host, launch_records, manifest_dir, maps, allow })
}

fn real_dir(opt: &str, p: &Path) -> Result<(), String> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.file_type().is_dir() => Ok(()),
        _ => Err(format!("--{opt} must be an existing real directory, not a link")),
    }
}

/// Open the launch records without following a link and without blocking; the handle must be a regular file.
fn open_launch_records(p: &Path) -> Result<fs::File, String> {
    let f = fs::OpenOptions::new().read(true).custom_flags(OPEN_NOFOLLOW_NONBLOCK).open(p).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            "the launch records file is missing".to_string()
        } else {
            "the launch records file is a link or cannot be opened".to_string()
        }
    })?;
    if !f.metadata().map(|m| m.is_file()).unwrap_or(false) {
        return Err("the launch records file is not a regular file".to_string());
    }
    Ok(f)
}

fn check_paths(o: &Opts) -> Result<(), String> {
    check_dir(&o.lanes_dir, false)?;
    real_dir("manifest-dir", &o.manifest_dir)?;
    for (_, d) in &o.maps {
        real_dir("repo-map", d)?;
    }
    if !fs::metadata(&o.host).map(|m| m.is_file()).unwrap_or(false) {
        return Err("--host is missing or does not resolve to a regular file".to_string());
    }
    Ok(())
}

fn now_ms(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------------------------- child runs

struct ChildOut {
    code: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Read one stream under the cap; past it the flag is set and the read stops.
fn reader<R: Read + Send + 'static>(mut r: R, over: Arc<AtomicBool>) -> Receiver<Vec<u8>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let mut keep = Vec::new();
        let mut buf = [0u8; 8192];
        while let Ok(n) = r.read(&mut buf) {
            if n == 0 {
                break;
            }
            keep.extend_from_slice(&buf[..n]);
            if keep.len() > CHILD_CAP {
                over.store(true, Ordering::SeqCst);
                break;
            }
        }
        let _ = tx.send(keep);
    });
    rx
}

/// Run this binary with `args`: stdin closed, both streams read under the cap, 30 seconds, its own process group. `None` is a failed call
/// (cannot start, time limit, a stream over the cap, a stream not closed 2 seconds after the exit, no exit code).
fn run_child(args: &[&OsStr]) -> Option<ChildOut> {
    let exe = std::env::current_exe().ok()?;
    let mut child = Command::new(exe).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).spawn().ok()?;
    let over = Arc::new(AtomicBool::new(false));
    let rx_out = reader(child.stdout.take()?, over.clone());
    let rx_err = reader(child.stderr.take()?, over.clone());
    let deadline = Instant::now() + Duration::from_millis(CHILD_LIMIT_MS);
    let kill = |c: &mut std::process::Child| {
        kill_group(c.id());
        let _ = c.kill();
        let _ = c.wait();
    };
    let status = loop {
        if over.load(Ordering::SeqCst) || Instant::now() >= deadline {
            kill(&mut child);
            return None;
        }
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) => {}
            Err(_) => {
                kill(&mut child);
                return None;
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let stdout = rx_out.recv_timeout(Duration::from_secs(2)).ok()?;
    let stderr = rx_err.recv_timeout(Duration::from_secs(2)).ok()?;
    if over.load(Ordering::SeqCst) {
        return None;
    }
    Some(ChildOut { code: status.code()?, stdout, stderr })
}

enum Answer {
    Clean,
    Stray,
    Failed,
}

/// The clean answer: exit 0 with exactly `workspace=clean ticket=<t>`; exit 2 with `stray <kind> <path>` lines then
/// `workspace=stray ticket=<t> count=<n>`, clean only when all lines are untracked or ignored paths byte-equal to an `--allow` value.
fn clean_answer(ticket: &str, o: &ChildOut, allow: &[String]) -> Answer {
    let Ok(text) = std::str::from_utf8(&o.stdout) else { return Answer::Failed };
    if o.code == 0 {
        return if text == format!("workspace=clean ticket={ticket}\n") { Answer::Clean } else { Answer::Failed };
    }
    if o.code != 2 {
        return Answer::Failed;
    }
    let Some(body) = text.strip_suffix('\n') else { return Answer::Failed };
    let mut lines: Vec<&str> = body.split('\n').collect();
    let Some(last) = lines.pop() else { return Answer::Failed };
    if lines.is_empty() || last != format!("workspace=stray ticket={ticket} count={}", lines.len()) {
        return Answer::Failed;
    }
    let mut all_allowed = true;
    for l in lines {
        let Some((kind, path)) = l.strip_prefix("stray ").and_then(|r| r.split_once(' ')) else { return Answer::Failed };
        if !["untracked", "ignored", "modified", "hidden", "submodule", "missing"].contains(&kind) {
            return Answer::Failed;
        }
        if !(matches!(kind, "untracked" | "ignored") && allow.iter().any(|a| a == path)) {
            all_allowed = false;
        }
    }
    if all_allowed {
        Answer::Clean
    } else {
        Answer::Stray
    }
}

/// The tree part of a verdict for a gone session: the clean check, then the pushed check.
fn tree_verdict(o: &Opts, r: &Record) -> Out {
    let Some((_, repo)) = o.maps.iter().find(|(id, _)| *id == r.repo_id) else { return ("unknown", "no-repo-map") };
    let md = o.manifest_dir.as_os_str();
    let ticket = OsStr::new(r.ticket.as_str());
    let clean = run_child(&[OsStr::new("workspace"), OsStr::new("check"), ticket, OsStr::new("--manifest-dir"), md]);
    match clean.map(|c| clean_answer(&r.ticket, &c, &o.allow)) {
        Some(Answer::Clean) => {}
        Some(Answer::Stray) => return ("unsaved", "stray"),
        _ => return ("unknown", "clean-check-failed"),
    }
    let pushed = run_child(&[OsStr::new("workspace"), OsStr::new("check"), ticket, OsStr::new("--manifest-dir"), md, OsStr::new("--pushed"), OsStr::new("--repo"), repo.as_os_str()]);
    match pushed {
        Some(c) if c.code == 0 => ("stale", "gone-clean-pushed"),
        Some(c) if c.code == 4 => ("unsaved", "unpushed"),
        Some(c) if c.code == 3 && std::str::from_utf8(&c.stderr).is_ok_and(|s| s.trim_end_matches('\n') == NO_UPSTREAM) => ("unknown", "no-upstream"),
        _ => ("unknown", "pushed-check-failed"),
    }
}

// ---------------------------------------------------------------------------------------------- the facts

fn read_back(name: &str) -> Option<Verdict> {
    session_name::check(name, ANY_LANE, false).ok()
}

/// A name that reads back the exec role (short or long form) for this slug.
fn is_exec_name(name: &str, slug: &str) -> bool {
    read_back(name).is_some_and(|v| matches!(v.role, "exec" | "execution") && v.ticket.as_deref() == Some(slug))
}

/// Not ended: state done or stopped is ended; any other state, or none, is running.
fn running(e: &Entry) -> bool {
    !matches!(e.state.as_deref(), Some("done") | Some("stopped"))
}

struct Facts<'a> {
    o: &'a Opts,
    live: &'a [Record],
    parked: &'a [Record],
    launches: &'a [Launch],
    entries: &'a [Entry],
    mtimes: &'a [u64],
}

/// The first matching rule of the stale check, for the live record at index `i`.
fn judge(f: &Facts, i: usize) -> Out {
    let r = &f.live[i];
    let Some(slug) = slug_of(&r.ticket) else { return ("hand", "no-exec-launch") };
    let mut tickets: Vec<&str> = Vec::new();
    for t in f.live.iter().chain(f.parked.iter()).map(|x| x.ticket.as_str()) {
        if slug_of(t).as_deref() == Some(slug.as_str()) && !tickets.contains(&t) {
            tickets.push(t);
        }
    }
    if tickets.len() > 1 {
        return ("unknown", "slug-shared");
    }
    if f.parked.iter().any(|p| p.ticket == r.ticket) {
        return ("unknown", "live-and-parked");
    }
    let exec: Vec<&Launch> = f.launches.iter().filter(|l| is_exec_name(&l.name, &slug)).collect();
    if exec.iter().any(|l| l.state == "pending") {
        return ("alive", "launch-pending");
    }
    let Some(newest) = exec.iter().filter(|l| l.state == "bound").map(|l| l.ts_ms).max() else { return ("hand", "no-exec-launch") };
    if f.mtimes[i] > newest {
        return ("alive", "claimed-after-launch");
    }
    if f.entries.iter().any(|e| running(e) && is_exec_name(&e.name, &slug)) {
        return ("alive", "listed-running");
    }
    let want = format!("t{slug}");
    for e in f.entries.iter().filter(|e| running(e)) {
        let rb = read_back(&e.name);
        let ticket = rb.as_ref().and_then(|v| v.ticket.as_deref());
        let part = e.name.split('-').any(|p| p.eq_ignore_ascii_case(&want));
        let flat = ticket.is_some_and(|t| t.contains('-') && t.replace('-', "") == slug);
        if (part && ticket != Some(slug.as_str())) || flat {
            return ("unknown", "name-unclear");
        }
    }
    tree_verdict(f.o, r)
}

/// The launch records, read through the validated handle.
fn read_launch_records(p: &Path) -> Result<Vec<Launch>, String> {
    let handle = open_launch_records(p)?;
    // Read through the validated handle when /dev/fd exists, so a path swapped after the check is not opened again.
    let fd_path = PathBuf::from(format!("/dev/fd/{}", handle.as_raw_fd()));
    let read_from = if fd_path.exists() { fd_path } else { p.to_path_buf() };
    let launches = read_launches(&read_from).map_err(|e| format!("the launch records cannot be used: {e}"))?;
    drop(handle);
    Ok(launches)
}

/// The exec launches of `slug` as (run_id, state), to compare two reads of the launch records.
fn exec_launches<'a>(ls: &'a [Launch], slug: &str) -> Vec<(&'a str, &'static str)> {
    ls.iter().filter(|l| is_exec_name(&l.name, slug)).map(|l| (l.run_id.as_str(), l.state)).collect()
}

/// What the park step gave for one record.
enum Step {
    Parked,
    /// Not parked: the line shows this verdict instead.
    Seen(Out),
    /// A re-read failed: nothing is parked.
    Recheck,
    /// The park body failed.
    Failed(String),
}

/// The park step for the live record `r` judged `stale` on the first read (`first` are its launch records): one critical section, the lock
/// held from the first line to the return. It calls neither `park` nor `state_locked`, which take the lock again.
fn park_step(o: &Opts, ap: &Apply, bound: u32, r: &Record, first: &[Launch]) -> Step {
    // The lock is held until this function returns.
    let Ok(_guard) = lock(&o.lanes_dir) else { return Step::Recheck };
    let (Ok(live), Ok(parked)) = (list_records(&o.lanes_dir, bound), read_parked(&o.lanes_dir)) else { return Step::Recheck };
    let Some(i) = live.iter().position(|x| x.ticket == r.ticket) else { return Step::Seen(("unknown", "record-changed")) };
    let Ok(mtime) = fs::symlink_metadata(lane_path(&o.lanes_dir, live[i].lane)).and_then(|m| m.modified()) else { return Step::Recheck };
    if live[i] != *r {
        return Step::Seen(("unknown", "record-changed"));
    }
    let Ok(entries) = listing(&o.host) else { return Step::Recheck };
    let Ok(launches) = read_launch_records(&o.launch_records) else { return Step::Recheck };
    // Only the index `i` is read by `judge`.
    let mut mtimes = vec![0; live.len()];
    mtimes[i] = now_ms(mtime);
    let facts = Facts { o, live: &live, parked: &parked, launches: &launches, entries: &entries, mtimes: &mtimes };
    let verdict = judge(&facts, i);
    if verdict != ("stale", "gone-clean-pushed") {
        return Step::Seen(verdict);
    }
    let Some(slug) = slug_of(&r.ticket) else { return Step::Seen(("unknown", "record-changed")) };
    let before = exec_launches(first, &slug);
    if exec_launches(&launches, &slug).iter().any(|l| !before.contains(l)) {
        return Step::Seen(("unknown", "record-changed"));
    }
    let of_state = |s: &str| entries.iter().any(|e| e.state.as_deref() == Some(s) && is_exec_name(&e.name, &slug));
    let listed = if of_state("done") {
        "done"
    } else if of_state("stopped") {
        "stopped"
    } else {
        "none"
    };
    let reason = format!("stale-check listed={listed}: {}", ap.reason);
    let spec = ReleaseSpec { at: &ap.at, reason: &reason, records_dir: ap.records_dir.clone() };
    match park_locked(&o.lanes_dir, &r.ticket, bound, Some(&spec)) {
        Ok(Ok(_)) => Step::Parked,
        Ok(Err(_)) => Step::Seen(("unknown", "park-refused")),
        Err(e) => Step::Failed(e),
    }
}

/// Entry point for `harness lane stale ...` (the arguments after `stale`).
pub fn stale_cmd(a: &[String]) -> i32 {
    let o = match parse(a) {
        Ok(o) => o,
        Err(e) => return fail(&e),
    };
    if let Err(e) = check_paths(&o) {
        return fail(&e);
    }
    let cfg = match Config::load(&o.config) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };
    let launches = match read_launch_records(&o.launch_records) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    let (live, parked) = match state_locked(&o.lanes_dir, name_cap(&cfg)) {
        Ok(v) => v,
        Err(e) => return fail(&e),
    };
    let mut mtimes = Vec::new();
    for r in &live {
        match fs::symlink_metadata(lane_path(&o.lanes_dir, r.lane)).and_then(|m| m.modified()) {
            Ok(t) => mtimes.push(now_ms(t)),
            Err(_) => return fail("a lane record cannot be inspected"),
        }
    }
    let entries = match listing(&o.host) {
        Ok(e) => e,
        Err(e) => return fail(&e),
    };
    let facts = Facts { o: &o, live: &live, parked: &parked, launches: &launches, entries: &entries, mtimes: &mtimes };
    let (mut unknown, mut parks, mut recheck_failed) = (0, 0, false);
    for (i, r) in live.iter().enumerate() {
        let (mut verdict, mut reason) = judge(&facts, i);
        let mut action = "none";
        if let (Some(ap), true) = (&o.apply, verdict == "stale") {
            let step = if recheck_failed { Step::Recheck } else { park_step(&o, ap, name_cap(&cfg), r, &launches) };
            match step {
                Step::Parked => {
                    action = "parked";
                    parks += 1;
                }
                Step::Seen(v) => (verdict, reason) = v,
                Step::Recheck => {
                    recheck_failed = true;
                    (verdict, reason) = ("unknown", "recheck-failed");
                }
                Step::Failed(e) => return fail(&format!("ticket={}: {e}", r.ticket)),
            }
        }
        if verdict == "unknown" {
            unknown += 1;
        }
        println!("stale ticket={} lane={} verdict={verdict} reason={reason} action={action}", r.ticket, r.lane);
    }
    println!("stale checked={} parked={parks} unknown={unknown}", live.len());
    if unknown > 0 {
        3
    } else {
        0
    }
}
