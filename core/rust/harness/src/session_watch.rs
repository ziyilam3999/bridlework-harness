//! `harness session watch` (A29; std only, unix). One verdict line per check for a builder seat, from the host listing, the tree, the
//! transcript file time, the commits since the launch and the ticket gate. Report only: it writes no file, stops no session, changes no
//! lane and writes no event; the host is called for the listing only. The transcript is read by metadata only (kind and mtime): this
//! code opens no transcript and reads no byte of it. See docs/session-watch.md.

use crate::lane::stale::open_launch_records;
use crate::launcher::{read_launches, Launch};
use crate::session_close::{raw_listing, Entry};
use crate::session_watch_log::{self, Block};
use crate::status;
use std::collections::HashMap;
use std::fs::{canonicalize, read_dir, symlink_metadata};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness session watch --host <abs> --records <file> --name <seat name> --ticket <file> --repo-map <id>=<tree> --base <sha> --result-dir <dir> --caps-dir <dir> --transcript-root <abs dir> --idle-s <n> --silence-s <n> [--stopped-at <epoch s> --stop <stopped|absent>] [--follow --poll-s <n> --max-s <n>] [--seat-log <abs file>]";

/// At most this many entries of the work tree are looked at.
const WALK_CAP: usize = 200_000;
/// A transcript time this far ahead of the clock is a refusal.
const AHEAD_S: u64 = 5;
/// Follow mode: this many faulted checks in a row end the run as the refusal of the last fault.
const FAULT_BOUND: u32 = 3;
/// The text `raw_listing` gives a host that could not be run or timed out.
const LISTING_FAULT: &str = "the host's listing could not be run or timed out";
const MAX_ARG: usize = 4096;

type R<T> = Result<T, String>;

/// Why a check gave no verdict. `--once` refuses both kinds; follow mode looks again after a transient fault.
#[derive(Debug)]
enum Fault {
    /// A fault of one look: the next look may judge.
    Transient(String),
    /// A real refusal. A plain error text becomes this kind, so a transient fault is named where it is raised.
    Final(String),
}

impl From<String> for Fault {
    fn from(m: String) -> Fault {
        Fault::Final(m)
    }
}

impl From<&str> for Fault {
    fn from(m: &str) -> Fault {
        Fault::Final(m.to_string())
    }
}

struct Opts {
    host: PathBuf,
    records: PathBuf,
    name: String,
    ticket: String,
    repo_map: String,
    tree: PathBuf,
    base: String,
    result_dir: String,
    caps_dir: String,
    root: PathBuf,
    idle: u64,
    silence: u64,
    /// `--stop`, present with `--stopped-at`.
    stop: Option<String>,
    /// `--poll-s` and `--max-s`, present with `--follow`.
    follow: Option<(u64, u64)>,
    /// `--seat-log`, read only on a blocked verdict.
    seat_log: Option<PathBuf>,
}

fn secs(v: &str, flag: &str) -> R<u64> {
    let ok = !v.is_empty() && v.len() <= 6 && v.bytes().all(|b| b.is_ascii_digit());
    match v.parse::<u64>() {
        Ok(n) if ok && (1..=86400).contains(&n) => Ok(n),
        _ => Err(format!("{flag} must be a whole number of seconds, 1 to 86400")),
    }
}

fn abs(v: &str, flag: &str) -> R<PathBuf> {
    if Path::new(v).is_absolute() {
        Ok(PathBuf::from(v))
    } else {
        Err(format!("{flag} must be an absolute path"))
    }
}

fn parse(a: &[String]) -> R<Opts> {
    const VALUED: [&str; 16] = [
        "host", "records", "name", "ticket", "repo-map", "base", "result-dir", "caps-dir", "transcript-root", "idle-s", "silence-s",
        "stopped-at", "stop", "poll-s", "max-s", "seat-log",
    ];
    let mut m: HashMap<&str, &str> = HashMap::new();
    let mut follow = false;
    let mut it = a.iter();
    while let Some(f) = it.next() {
        let Some(k) = f.strip_prefix("--") else { return Err("a value with no option before it".into()) };
        if k == "follow" {
            if std::mem::replace(&mut follow, true) {
                return Err("--follow given twice".into());
            }
            continue;
        }
        let Some(key) = VALUED.iter().find(|v| **v == k) else { return Err("unknown option".into()) };
        let v = it.next().ok_or_else(|| format!("--{key} needs a value"))?;
        if v.len() > MAX_ARG {
            return Err(format!("--{key} value is too long"));
        }
        if m.insert(*key, v.as_str()).is_some() {
            return Err(format!("--{key} given twice"));
        }
    }
    let need = |k: &str| m.get(k).copied().ok_or_else(|| format!("--{k} is required"));
    let (spec, base) = (need("repo-map")?, need("base")?);
    let (id, tree) = spec.split_once('=').ok_or("--repo-map must be <id>=<tree>")?;
    if id.is_empty() || !Path::new(tree).is_absolute() {
        return Err("--repo-map must be <id>=<absolute tree>".into());
    }
    if base.len() != 40 || !base.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        return Err("--base must be a 40-character lowercase hex sha".into());
    }
    let name = need("name")?;
    if name.is_empty() {
        return Err("--name is empty".into());
    }
    let stop = match (m.get("stopped-at"), m.get("stop")) {
        (None, None) => None,
        (Some(t), Some(s)) => {
            if t.is_empty() || t.len() > 12 || !t.bytes().all(|b| b.is_ascii_digit()) {
                return Err("--stopped-at must be an epoch time in whole seconds".into());
            }
            if !matches!(*s, "stopped" | "absent") {
                return Err("--stop must be stopped or absent".into());
            }
            Some(s.to_string())
        }
        _ => return Err("--stopped-at and --stop come together".into()),
    };
    let follow = match (follow, m.get("poll-s"), m.get("max-s")) {
        (false, None, None) => None,
        (true, Some(p), Some(x)) => Some((secs(p, "--poll-s")?, secs(x, "--max-s")?)),
        (true, _, _) => return Err("--follow needs --poll-s and --max-s".into()),
        (false, _, _) => return Err("--poll-s and --max-s go with --follow".into()),
    };
    let seat_log = match m.get("seat-log") {
        None => None,
        Some(v) => Some(abs(v, "--seat-log")?),
    };
    Ok(Opts {
        seat_log,
        host: abs(need("host")?, "--host")?,
        records: PathBuf::from(need("records")?),
        name: name.to_string(),
        ticket: need("ticket")?.to_string(),
        repo_map: spec.to_string(),
        tree: PathBuf::from(tree),
        base: base.to_string(),
        result_dir: need("result-dir")?.to_string(),
        caps_dir: need("caps-dir")?.to_string(),
        root: abs(need("transcript-root")?, "--transcript-root")?,
        idle: secs(need("idle-s")?, "--idle-s")?,
        silence: secs(need("silence-s")?, "--silence-s")?,
        stop,
        follow,
    })
}

// ---------------------------------------------------------------------------------------------- facts

fn token_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn epoch(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// The newest launch record of the name; it must be bound. The records file is opened without following a link.
fn load_launch(o: &Opts) -> R<Launch> {
    let handle = open_launch_records(&o.records)?;
    // Read through the checked handle when /dev/fd exists, so a path swapped after the check is not opened again.
    let fd = PathBuf::from(format!("/dev/fd/{}", handle.as_raw_fd()));
    let src = if fd.exists() { fd } else { o.records.clone() };
    let all = read_launches(&src).map_err(|e| format!("the launch records cannot be used: {e}"))?;
    drop(handle);
    let newest = all.iter().enumerate().filter(|(_, l)| l.name == o.name).max_by_key(|(i, l)| (l.ts_ms, *i)).map(|(_, l)| l);
    let l = newest.ok_or("no launch record for the name")?;
    if l.state != "bound" || l.short_id.is_none() || l.session_id.is_none() {
        return Err(format!("the newest launch record of the name is {}, not bound", l.state));
    }
    if !l.short_id.as_deref().is_some_and(token_ok) {
        return Err("the launch record short id is not a plain token".into());
    }
    // read_launches already refuses a non-uuid session id; this keeps the path safe if that rule changes.
    session_id_ok(l.session_id.as_deref().unwrap_or(""))?;
    Ok(l.clone())
}

/// The session id becomes a file name under the transcript root, so it must be a plain token.
fn session_id_ok(s: &str) -> R<()> {
    if token_ok(s) {
        Ok(())
    } else {
        Err("the launch record session id is not a plain token".into())
    }
}

fn git(tree: &Path, args: &[&str]) -> R<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(tree).args(args).stdin(Stdio::null()).stderr(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.output().map_err(|_| "git cannot be run".to_string())?;
    if !out.status.success() {
        return Err("git could not report the tree".into());
    }
    String::from_utf8(out.stdout).map_err(|_| "git output is not text".to_string())
}

/// The newest mtime (epoch s) of the tree and everything in it outside `.git`; no link is followed.
fn walk(root: &Path, cap: usize) -> Result<u64, Fault> {
    let top = symlink_metadata(root).and_then(|m| m.modified()).map_err(|_| Fault::Final("the tree cannot be inspected".into()))?;
    let unlisted = || Fault::Transient("the tree cannot be listed".into());
    let mut newest = epoch(top);
    let (mut stack, mut seen) = (vec![root.to_path_buf()], 0usize);
    while let Some(d) = stack.pop() {
        for e in read_dir(&d).map_err(|_| unlisted())? {
            let e = e.map_err(|_| unlisted())?;
            if e.file_name() == ".git" {
                continue;
            }
            seen += 1;
            if seen > cap {
                return Err("the tree walk is over its entry cap".into());
            }
            let m = e.metadata().map_err(|_| Fault::Transient("the tree cannot be inspected".into()))?;
            if let Ok(t) = m.modified() {
                newest = newest.max(epoch(t));
            }
            if m.is_dir() {
                stack.push(e.path());
            }
        }
    }
    Ok(newest)
}

/// Tree age: now minus the newest of the work tree mtimes, the git index mtime and the committer time of HEAD.
fn tree_age(o: &Opts, now_s: u64) -> Result<u64, Fault> {
    let mut newest = walk(&o.tree, WALK_CAP)?;
    let ip = git(&o.tree, &["rev-parse", "--git-path", "index"])?;
    let ip = Path::new(ip.trim());
    let ip = if ip.is_absolute() { ip.to_path_buf() } else { o.tree.join(ip) };
    if let Ok(t) = symlink_metadata(&ip).and_then(|m| m.modified()) {
        newest = newest.max(epoch(t));
    }
    let head = git(&o.tree, &["log", "-1", "--format=%ct", "HEAD"])?;
    newest = newest.max(head.trim().parse::<u64>().map_err(|_| "git gave no HEAD time".to_string())?);
    Ok(now_s.saturating_sub(newest))
}

/// Transcript age from the file kind and mtime only.
fn transcript_age(o: &Opts, cwd: &str, sid: &str, now: SystemTime) -> R<u64> {
    let dir: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let p = o.root.join(dir).join(format!("{sid}.jsonl"));
    let m = symlink_metadata(&p).map_err(|_| "the transcript file is missing or cannot be inspected".to_string())?;
    if !m.file_type().is_file() {
        return Err("the transcript path is not a regular file".into());
    }
    let t = m.modified().map_err(|_| "the transcript time cannot be read".to_string())?;
    match now.duration_since(t) {
        Ok(d) => Ok(d.as_secs()),
        Err(e) if e.duration() > Duration::from_secs(AHEAD_S) => Err("the transcript time is ahead of the clock".into()),
        Err(_) => Ok(0),
    }
}

struct Facts {
    commits: u64,
    last: String,
    current: bool,
}

/// Commits in base..HEAD with a committer time at or after the launch, and the gate facts from the status line.
fn facts(o: &Opts, floor: u64) -> R<Facts> {
    let log = git(&o.tree, &["log", "--format=%ct", &format!("{}..HEAD", o.base)])?;
    let mut commits = 0;
    for l in log.lines() {
        let t = l.trim().parse::<u64>().map_err(|_| "git gave a bad commit time".to_string())?;
        if t >= floor {
            commits += 1;
        }
    }
    let args: Vec<String> = [&o.ticket, "--repo-map", &o.repo_map, "--result-dir", &o.result_dir, "--caps-dir", &o.caps_dir]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let line = status::run(&args).map_err(|s| format!("status refused (exit {}): {}", s.0, s.1))?;
    let pick = |k: &str| line.split(' ').find_map(|t| t.strip_prefix(k)).map(str::to_string);
    let (Some(last), Some(cur)) = (pick("last_gate="), pick("gate_current=")) else {
        return Err("the status line has no gate fields".into());
    };
    Ok(Facts { commits, last, current: cur == "yes" })
}

fn pid_live(pid: Option<&str>) -> R<bool> {
    let p = pid.filter(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().is_ok_and(|n| n > 0));
    let p = p.ok_or("the entry pid is missing or not a whole number")?;
    let out = Command::new("ps")
        .args(["-p", p, "-o", "pid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "the pid check cannot be run".to_string())?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == p)
}

/// The listing for the join. An entry with no string id or no string name, or whose id is not a plain token, is left out when it cannot
/// be the watched entry: its sessionId is not the record session_id, its id is not the record short_id, its name is not the record
/// name and its cwd, canonicalized, is not the tree. Such an entry that could be the watched one (its sessionId equals the record
/// session_id, its id equals the record short_id, its name equals the record name, or its cwd equals the tree) refuses. Returns the
/// entries to join on and how many were left out.
fn watch_listing(o: &Opts, short: &str, sid: &str, name: &str) -> Result<(Vec<Entry>, usize), Fault> {
    let tree = canonicalize(&o.tree).ok();
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    let raw = raw_listing(&o.host).map_err(|m| if m == LISTING_FAULT { Fault::Transient(m) } else { Fault::Final(m) })?;
    for r in raw {
        let could_be_watched = r.session_id.as_deref() == Some(sid)
            || r.id.as_deref() == Some(short)
            || r.name.as_deref() == Some(name)
            || r.cwd
                .as_deref()
                .and_then(|c| canonicalize(c).ok())
                .is_some_and(|c| tree.as_ref().is_some_and(|t| c == *t));
        if let Some(m) = r.bad() {
            if could_be_watched {
                return Err(m.into());
            }
            skipped += 1;
            continue;
        }
        entries.push(r.into_entry());
    }
    Ok((entries, skipped))
}

// ---------------------------------------------------------------------------------------------- verdicts

struct Out {
    verdict: &'static str,
    code: i32,
    line: String,
    /// The transcript age of a check whose host state was working.
    working_age: Option<u64>,
    /// The seat log detail line of a blocked verdict (only with `--seat-log`).
    detail: Option<String>,
}

struct Show<'a> {
    state: String,
    tree: Option<u64>,
    transcript: Option<u64>,
    commits: Option<u64>,
    gate: String,
    session: &'a str,
    /// Listing entries left out of the join because they cannot be the watched one.
    skipped: usize,
}

fn out(verdict: &'static str, s: &Show, extra: &str, working_age: Option<u64>) -> Out {
    let n = |v: Option<u64>| v.map_or("-".to_string(), |x| x.to_string());
    let code = match verdict {
        "working" => 0,
        "slow-alive" => 10,
        "stalled" => 11,
        "stopped-early" => 12,
        "finished" => 13,
        "stopped" => 14,
        _ => 15,
    };
    let line = format!(
        "verdict={verdict} state={} tree_age={} transcript_age={} commits_since_launch={} gate={} session={} skipped_entries={}{extra}",
        s.state,
        n(s.tree),
        n(s.transcript),
        n(s.commits),
        s.gate,
        s.session,
        s.skipped
    );
    Out { verdict, code, line, working_age, detail: None }
}

fn check(o: &Opts) -> Result<Out, Fault> {
    let now = SystemTime::now();
    let now_s = epoch(now);
    let l = load_launch(o)?;
    let (short, sid) = (l.short_id.clone().unwrap_or_default(), l.session_id.clone().unwrap_or_default());
    let mut s = Show { state: "-/-".into(), tree: None, transcript: None, commits: None, gate: "-".into(), session: &short, skipped: 0 };
    // v1: the bw stop record
    if let Some(kind) = &o.stop {
        return Ok(out("stopped", &s, &format!(" by=bw stop={kind}"), None));
    }
    // v2: join on the short id and the session id; entries that cannot be the watched one are left out (skipped_entries)
    let (entries, skipped) = watch_listing(o, &short, &sid, &l.name)?;
    s.skipped = skipped;
    let hits: Vec<&Entry> = entries.iter().filter(|e| e.id == short && e.session_id.as_deref() == Some(sid.as_str())).collect();
    let e = match hits.as_slice() {
        [] => return Ok(out("gone", &s, "", None)),
        [e] => *e,
        _ => return Err("two or more listing entries match the launch".into()),
    };
    let (Some(state), Some(cwd)) = (e.state.as_deref(), e.cwd.as_deref()) else {
        return Err("the listing entry has no string state or cwd".into());
    };
    s.state = format!("{state}/{}", e.status.as_deref().unwrap_or("-"));
    let same = canonicalize(cwd).ok().zip(canonicalize(&o.tree).ok()).is_some_and(|(a, b)| a == b);
    if !same {
        return Err("the entry cwd is not the tree".into());
    }
    if state == "stopped" {
        return Ok(out("stopped", &s, " by=host", None));
    }
    if !matches!(state, "working" | "done" | "blocked" | "idle") {
        return Err(format!("the host state {} is not one this verb judges", state.chars().take(24).collect::<String>()).into());
    }
    let t_age = transcript_age(o, cwd, &sid, now)?;
    s.transcript = Some(t_age);
    let floor = l.ts_ms / 1000;
    match state {
        "working" => {
            let age = tree_age(o, now_s)?;
            s.tree = Some(age);
            let w = Some(t_age);
            if age < o.idle {
                return Ok(out("working", &s, "", w));
            }
            if t_age < o.silence {
                return Ok(out("slow-alive", &s, "", w));
            }
            if !pid_live(e.pid.as_deref())? {
                return Err("the entry pid is not a live process".into());
            }
            // A quiet seat the host still lists as working can be done: a commit since the launch with a passed gate on the current head.
            let f = facts(o, floor)?;
            s.commits = Some(f.commits);
            s.gate = format!("{}/{}", f.last, if f.current { "yes" } else { "no" });
            if f.commits > 0 && f.last == "pass" && f.current {
                return Ok(out("finished", &s, "", w));
            }
            Ok(out("stalled", &s, "", w))
        }
        "blocked" => {
            if let Ok(f) = facts(o, floor) {
                s.commits = Some(f.commits);
                s.gate = format!("{}/{}", f.last, if f.current { "yes" } else { "no" });
            }
            let wf = e.waiting_for.as_deref().filter(|w| !w.is_empty());
            let kind = match (e.status.as_deref(), wf) {
                (Some("waiting"), Some(_)) => Block::Dialog,
                (Some("idle"), None) => Block::Asked,
                _ => Block::Other,
            };
            let word = match kind {
                Block::Dialog => "dialog",
                Block::Asked => "asked",
                Block::Other => "other",
            };
            let wf_text: String = wf.map_or("-".to_string(), |w| {
                w.chars().take(40).map(|c| c.to_ascii_lowercase()).map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() { c } else { '-' }).collect()
            });
            let mut r = out("stopped-early", &s, &format!(" why=blocked block={word} waiting_for={wf_text}"), None);
            r.detail = o.seat_log.as_deref().map(|p| session_watch_log::detail_line(p, l.ts_ms, kind));
            Ok(r)
        }
        _ => {
            let f = facts(o, floor)?;
            s.commits = Some(f.commits);
            s.gate = format!("{}/{}", f.last, if f.current { "yes" } else { "no" });
            if f.commits == 0 {
                Ok(out("stopped-early", &s, " why=no-commit", None))
            } else if !f.current {
                Ok(out("stopped-early", &s, " why=no-gate", None))
            } else {
                Ok(out("finished", &s, "", None))
            }
        }
    }
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: session watch refused: {m}");
    3
}

fn follow(o: &Opts, poll: u64, max: u64) -> i32 {
    let start = Instant::now();
    let limit = Duration::from_secs(max);
    let (mut printed, mut top): (Option<&str>, Option<u64>) = (None, None);
    // transient faults in a row, tree and host together
    let mut faults = 0u32;
    let age = |t: Option<u64>| t.map_or("-".to_string(), |x| x.to_string());
    loop {
        let fault = match check(o) {
            Ok(o1) => {
                faults = 0;
                if let Some(w) = o1.working_age {
                    top = Some(top.map_or(w, |x: u64| x.max(w)));
                }
                if !matches!(o1.verdict, "working" | "slow-alive") {
                    println!("{} max_transcript_age={}", o1.line, age(top));
                    if let Some(d) = &o1.detail {
                        println!("{d}");
                    }
                    return o1.code;
                }
                if printed != Some(o1.verdict) {
                    println!("{}", o1.line);
                    printed = Some(o1.verdict);
                }
                None
            }
            Err(Fault::Final(m)) => return refuse(&m),
            Err(Fault::Transient(m)) => {
                faults += 1;
                if faults >= FAULT_BOUND {
                    return refuse(&m);
                }
                Some(m)
            }
        };
        let left = limit.saturating_sub(start.elapsed());
        if !left.is_zero() {
            std::thread::sleep(left.min(Duration::from_secs(poll)));
        }
        let over = start.elapsed() >= limit;
        // the note is for a fault that does not end the run, so it waits until the time limit is ruled out
        match fault {
            Some(m) if over => return refuse(&m),
            Some(m) => eprintln!("harness: session watch note: {m} (fault {faults} of {FAULT_BOUND})"),
            None if over => break,
            None => {}
        }
    }
    // the newest check judged, so the last printed verdict is its verdict
    println!("verdict=watch-limit last={} max_transcript_age={}", printed.unwrap_or("-"), age(top));
    16
}

/// `harness session watch ...`: 0 working, 10 slow-alive, 11 stalled, 12 stopped-early, 13 finished, 14 stopped, 15 gone, 16 time limit,
/// 3 refused (cannot judge), 2 usage.
pub fn watch_cmd(a: &[String]) -> i32 {
    let o = match parse(a) {
        Ok(o) => o,
        Err(m) => {
            eprintln!("harness: error: {m}\n{USAGE}");
            return 2;
        }
    };
    match o.follow {
        Some((p, x)) => follow(&o, p, x),
        None => match check(&o) {
            Ok(r) => {
                println!("{}", r.line);
                if let Some(d) = &r.detail {
                    println!("{d}");
                }
                r.code
            }
            Err(Fault::Transient(m) | Fault::Final(m)) => refuse(&m),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_that_is_not_a_token_is_refused() {
        for bad in ["../evil", "a/b", "", "a b", "x\0y"] {
            assert!(session_id_ok(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(session_id_ok("11111111-2222-4333-8444-555555555555").is_ok(), "twin: a uuid is accepted");
    }

    #[test]
    fn a_missing_tree_root_is_a_final_fault() {
        let d = std::env::temp_dir().join(format!("swatch-unit-gone-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert!(matches!(walk(&d, 4), Err(Fault::Final(_))), "a missing root is not a fault of one look");
    }

    #[test]
    fn the_walk_stops_past_its_cap() {
        let d = std::env::temp_dir().join(format!("swatch-unit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for n in 0..5 {
            std::fs::write(d.join(format!("f{n}")), "x").unwrap();
        }
        assert!(walk(&d, 4).is_err(), "5 entries are over a cap of 4");
        assert!(walk(&d, 5).is_ok(), "twin: 5 entries fit a cap of 5");
        let _ = std::fs::remove_dir_all(&d);
    }
}
