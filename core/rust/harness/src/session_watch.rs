//! `harness session watch` (A29; std only, unix). One verdict line per check for a builder seat, from the host listing, the tree, the
//! transcript file time, the commits since the launch and the ticket gate. Report only: it writes no file, stops no session, changes no
//! lane and writes no event; the host is called for the listing only. The transcript is read by metadata only (kind and mtime): this
//! code opens no transcript and reads no byte of it. A working seat past both limits is first judged `finished` or not. When it is not,
//! it is `slow-alive` while a live (not zombie) process at depth 2 or more under its entry pid started at or after (transcript mtime -
//! `CHILD_SLACK_S`, 60 s, or `--child-slack-s`); entry pid 1 is not walked, and the watcher's own pid and subtree are left out. That is
//! one process list per such check, read for pid, parent pid, age and state only (no command name is read or printed). See
//! docs/session-watch.md.

use crate::lane::stale::open_launch_records;
use crate::launcher::{read_launches, Launch};
use crate::session_close::{raw_listing, Entry};
use crate::session_watch_log::{self, Block};
use crate::status;
use std::collections::{HashMap, HashSet};
use std::fs::{canonicalize, read_dir, symlink_metadata};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness session watch --host <abs> --records <file> --name <seat name> --ticket <file> --repo-map <id>=<tree> --base <sha> --result-dir <dir> --caps-dir <dir> --transcript-root <abs dir> --idle-s <n> --silence-s <n> [--child-slack-s <n>] [--stopped-at <epoch s> --stop <stopped|absent>] [--follow --poll-s <n> --max-s <n>] [--seat-log <abs file>]";

/// At most this many entries of the work tree are looked at.
const WALK_CAP: usize = 200_000;
/// At most this many rows of the process list the child check looks at (a busy Mac lists about a thousand).
pub const PROC_CAP: usize = 40_000;
/// A transcript time this far ahead of the clock is a refusal.
const AHEAD_S: u64 = 5;
/// Follow mode: this many transient faults in a row end the run as the refusal of the last fault.
const FAULT_BOUND: u32 = 3;
/// A process under the seat counts as fresh when it started this many seconds before the transcript mtime or later (`--child-slack-s`).
const CHILD_SLACK_S: u64 = 60;
/// The process list gave no row, could not be run or exited non-zero.
const PROC_UNREAD: &str = "the process list cannot be read";
/// The process list has more rows than `PROC_CAP`, or the walk under the entry pid does not end.
const PROC_OVER: &str = "the process list is over its cap";
/// The text `raw_listing` gives a host that could not be run or timed out.
const LISTING_FAULT: &str = "the host's listing could not be run or timed out";
const MAX_ARG: usize = 4096;

type R<T> = Result<T, String>;

/// Why a check gave no verdict. Without `--follow` both kinds refuse. After a transient fault, follow mode looks again until
/// `FAULT_BOUND` faults in a row or the time limit.
#[derive(Debug)]
enum Fault {
    /// A fault of one look: the next look may judge.
    Transient(String),
    /// A real refusal. A plain error text becomes this kind. A transient fault is built by name: a tree fault where `walk` raises it,
    /// the host fault in `watch_listing`, which matches the `raw_listing` text against `LISTING_FAULT`.
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
    /// `--child-slack-s`, else `CHILD_SLACK_S`.
    slack: u64,
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
    const VALUED: [&str; 17] = [
        "host", "records", "name", "ticket", "repo-map", "base", "result-dir", "caps-dir", "transcript-root", "idle-s", "silence-s",
        "child-slack-s", "stopped-at", "stop", "poll-s", "max-s", "seat-log",
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
        slack: m.get("child-slack-s").map_or(Ok(CHILD_SLACK_S), |v| secs(v, "--child-slack-s"))?,
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

/// The entry pid of the listing as a number.
fn pid_num(pid: Option<&str>) -> R<u32> {
    pid.filter(|p| !p.is_empty() && p.len() <= 10 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u32>().is_ok_and(|n| n > 0))
        .and_then(|p| p.parse().ok())
        .ok_or("the entry pid is missing or not a whole number".into())
}

fn pid_live(pid: Option<&str>) -> R<bool> {
    let n = pid_num(pid)?;
    let p = n.to_string();
    let out = Command::new("ps")
        .args(["-p", p.as_str(), "-o", "pid="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| "the pid check cannot be run".to_string())?;
    Ok(out.status.success() && String::from_utf8_lossy(&out.stdout).trim() == p)
}

/// One process of the list. No command name is read.
struct Proc {
    pid: u32,
    ppid: u32,
    /// Seconds since the process started.
    etime: u64,
    /// A zombie: it ended and waits to be reaped, so it is not a live process.
    zombie: bool,
}

/// Elapsed seconds of an etime field. The `ps` forms `[[dd-]hh:]mm:ss` and bare seconds of two digits or more are read. The
/// parser is lenient beyond them: the forms with `:` have no width or range check (`1:2` is read), `dd-mm:ss` is read, and so is
/// a `+` in front of the days or of a field of those forms. No value: an empty text or field, any other character that is not a
/// digit or a separator in its place, one digit alone, bare seconds with a `+` or with days, more than three `:` fields, and a
/// number too big for a u64. The arithmetic that adds the fields up is not checked for overflow.
fn etime_secs(s: &str) -> Option<u64> {
    if s.len() >= 2 && s.bytes().all(|b| b.is_ascii_digit()) {
        return s.parse().ok();
    }
    let (days, rest) = match s.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r),
        None => (0, s),
    };
    let (h, m, s) = match rest.split(':').collect::<Vec<_>>().as_slice() {
        [m, s] => ("0", *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    if h.is_empty() || m.is_empty() || s.is_empty() {
        return None;
    }
    Some(days * 86_400 + h.parse::<u64>().ok()? * 3600 + m.parse::<u64>().ok()? * 60 + s.parse::<u64>().ok()?)
}

fn proc_row(l: &str) -> Option<Proc> {
    let f: Vec<&str> = l.split_whitespace().collect();
    let [pid, ppid, etime, state] = f.as_slice() else { return None };
    Some(Proc { pid: pid.parse::<u32>().ok().filter(|p| *p > 0)?, ppid: ppid.parse().ok()?, etime: etime_secs(etime)?, zombie: state.starts_with('Z') })
}

/// The rows of one process list text, and how many of its lines did not parse. A list with no row cannot be judged, and more lines
/// than `cap` is the cap refusal.
fn parse_procs(text: &str, cap: usize) -> Result<(Vec<Proc>, usize), &'static str> {
    let (mut rows, mut skipped) = (Vec::new(), 0usize);
    for l in text.lines() {
        if rows.len() + skipped >= cap {
            return Err(PROC_OVER);
        }
        match proc_row(l) {
            Some(r) => rows.push(r),
            None => skipped += 1,
        }
    }
    if rows.is_empty() {
        Err(PROC_UNREAD)
    } else {
        Ok((rows, skipped))
    }
}

/// One read of the process list: pid, parent pid, etime and state of every process, most `cap` rows.
fn procs(cap: usize) -> Result<(Vec<Proc>, usize), String> {
    let out = Command::new("ps")
        .args(["-A", "-o", "pid=,ppid=,etime=,state="])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|_| PROC_UNREAD.to_string())?;
    if !out.status.success() {
        return Err(PROC_UNREAD.to_string());
    }
    parse_procs(&String::from_utf8_lossy(&out.stdout), cap).map_err(str::to_string)
}

/// Every pid under `root` with its depth (a child of `root` is 1), each pid once, without `skip` and its subtree (nothing at all when
/// `root` is `skip`). The set of pids seen ends the walk on a ppid cycle. More pids pushed than the list has rows means the set no
/// longer holds (a walk that would not end): the cap refusal.
fn descend(rows: &[Proc], root: u32, skip: u32) -> Result<HashMap<u32, u32>, &'static str> {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    for r in rows {
        kids.entry(r.ppid).or_default().push(r.pid);
    }
    let (mut seen, mut under, mut pushed) = (HashSet::from([root]), HashMap::new(), 0usize);
    let (mut level, mut depth) = (if root == skip { Vec::new() } else { vec![root] }, 1u32);
    loop {
        let mut next = Vec::new();
        for p in level {
            for k in kids.get(&p).into_iter().flatten() {
                // `skip` is never walked, so neither is anything under it
                if *k != skip && seen.insert(*k) {
                    under.insert(*k, depth);
                    next.push(*k);
                }
            }
        }
        pushed += next.len();
        if next.is_empty() {
            return Ok(under);
        }
        if pushed > rows.len() {
            return Err(PROC_OVER);
        }
        level = next;
        depth += 1;
    }
}

/// The age of the newest live process at depth 2 or more of `under` (from `descend`) that started at or after (transcript mtime -
/// `slack`); `None` when there is no such process.
fn fresh_under(rows: &[Proc], under: &HashMap<u32, u32>, t_age: u64, slack: u64) -> Option<u64> {
    rows.iter()
        .filter(|r| under.get(&r.pid).is_some_and(|d| *d >= 2) && !r.zombie && r.etime <= t_age.saturating_add(slack))
        .map(|r| r.etime)
        .min()
}

/// The age of the newest live process at depth 2 or more under the entry pid that started at or after (transcript mtime - `slack`),
/// and how many rows of the list did not parse. `None` when there is no such process. The watcher's own pid and subtree are left
/// out, so neither the watcher nor the `ps` child of this check counts. Pid 1 is not walked: on macOS, and on Linux without a
/// subreaper, a process whose parent ended is reparented to it, so a process under it need not belong to the seat; the list is not
/// read then.
fn child_age(pid: u32, t_age: u64, slack: u64) -> Result<(Option<u64>, usize), String> {
    if pid == 1 {
        return Ok((None, 0));
    }
    let (rows, skipped) = procs(PROC_CAP)?;
    let under = descend(&rows, pid, std::process::id()).map_err(str::to_string)?;
    Ok((fresh_under(&rows, &under, t_age, slack), skipped))
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
    /// The `why` of the line, empty when none: follow mode prints a line when it changes too.
    reason: &'static str,
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
    Out { verdict, reason: "", code, line, working_age, detail: None }
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
            // A quiet seat the host still lists as working can be busy in a long tool call: a live process at depth 2 or more under
            // the entry pid (depth 1 holds the host's own helpers) that started at or after (transcript mtime - slack). `ps_skip`
            // counts the rows of the process list that did not parse. A list that cannot be read refuses: it is never read as "no
            // child".
            let (child, skipped) = child_age(pid_num(e.pid.as_deref())?, t_age, o.slack)?;
            let why = child.map_or_else(String::new, |a| format!(" why=child child_age={a}"));
            let mut r = out(if child.is_some() { "slow-alive" } else { "stalled" }, &s, &format!("{why} ps_skip={skipped}"), w);
            r.reason = if child.is_some() { "child" } else { "" };
            Ok(r)
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
    // the verdict and why of the last printed line: a line is printed when either changes
    let (mut printed, mut top): (Option<(&str, &str)>, Option<u64>) = (None, None);
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
                if printed != Some((o1.verdict, o1.reason)) {
                    println!("{}", o1.line);
                    printed = Some((o1.verdict, o1.reason));
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
    println!("verdict=watch-limit last={} max_transcript_age={}", printed.map_or("-", |p| p.0), age(top));
    16
}

/// `harness session watch ...`: 0 working, 10 slow-alive (plain, or `why=child`), 11 stalled, 12 stopped-early, 13 finished, 14 stopped,
/// 15 gone, 16 time limit, 3 refused (cannot judge), 2 usage.
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

    #[test]
    fn an_etime_field_is_whole_seconds() {
        let cases: Vec<(&str, Option<u64>)> = vec![
            ("01:05", Some(65)),
            ("02:01:05", Some(7265)),
            ("3-02:01:05", Some(266_465)),
            ("12-00:00:00", Some(1_036_800)),
            // bare seconds: two digits or more
            ("05", Some(5)),
            ("00", Some(0)),
            ("125", Some(125)),
            ("5", None),
            ("+05", None),
            ("0a", None),
            ("3-05", None),
            ("99999999999999999999", None),
            ("1:2:3:4", None),
            ("-05", None),
            ("aa:bb", None),
            ("", None),
        ];
        for (s, want) in cases {
            assert_eq!(etime_secs(s), want, "{s:?}");
        }
        // a row: four fields, a positive pid; anything else is skipped
        assert!(proc_row("100 1 10:00 S").is_some() && proc_row("  100     1   00:03 Z\n").is_some());
        for bad in ["x y z", "0 1 10:00 S", "100 1 10:00", "100 1 10:00 S more", "100 1 0 S"] {
            assert!(proc_row(bad).is_none(), "{bad:?}");
        }
    }

    fn r(pid: u32, ppid: u32) -> Proc {
        Proc { pid, ppid, etime: 0, zombie: false }
    }

    #[test]
    fn the_walk_under_the_entry_pid_ends_on_a_cycle() {
        // 101 -> 102 -> 103 -> 101: a cycle of three pids under the entry pid 100
        let rows = [r(101, 100), r(102, 101), r(103, 102), r(101, 103)];
        let under = descend(&rows, 100, 0).unwrap();
        assert_eq!(under.len(), 3, "each pid once: {under:?}");
        assert_eq!((under[&101], under[&102], under[&103]), (1, 2, 3));
        // the entry pid is part of the cycle
        let rows = [r(101, 100), r(100, 101), r(102, 101)];
        assert_eq!(descend(&rows, 100, 0).unwrap().len(), 2);
        // the skipped pid is left out with its subtree; a branch beside it stays
        let rows = [r(101, 100), r(102, 101), r(103, 100), r(104, 103)];
        let under = descend(&rows, 100, 101).unwrap();
        assert_eq!(under, HashMap::from([(103, 1), (104, 2)]), "101 and its child 102 are left out");
        // deeper: the skipped pid at depth 2 is not in the result, nor is its child
        let rows = [r(101, 100), r(102, 101), r(103, 102)];
        assert_eq!(descend(&rows, 100, 102).unwrap(), HashMap::from([(101, 1)]));
        // an entry pid that is the skipped pid has nothing under it
        assert!(descend(&rows, 100, 100).unwrap().is_empty());
    }

    #[test]
    fn the_process_list_stops_past_its_cap() {
        let rows = |n: usize| (0..n).map(|i| format!("{} 1 00:01 S", 100 + i)).collect::<Vec<_>>().join("\n");
        assert_eq!(parse_procs(&rows(5), 4).err(), Some(PROC_OVER), "5 rows are over a cap of 4");
        assert!(parse_procs(&rows(5), 5).is_ok(), "twin: 5 rows fit a cap of 5");
        // no row at all cannot be judged; a bad row among good ones is skipped and counted
        assert_eq!(parse_procs("", 5).err(), Some(PROC_UNREAD));
        assert_eq!(parse_procs("x y z", 5).err(), Some(PROC_UNREAD));
        let (p, skipped) = parse_procs(&format!("{}\nx y z", rows(2)), 5).unwrap();
        assert_eq!((p.len(), skipped), (2, 1));
    }

    #[test]
    fn a_fresh_child_needs_depth_two_no_zombie_and_a_start_after_the_transcript() {
        // the entry 100, its helper 101 at depth 1, 102 and the zombie 103 at depth 2, 104 at depth 3, 105 under another parent
        let mut rows = [r(101, 100), r(102, 101), r(103, 101), r(104, 102), r(105, 1)];
        rows[1].etime = 10;
        rows[2].etime = 1;
        rows[2].zombie = true;
        rows[3].etime = 5;
        rows[4].etime = 1;
        let under = descend(&rows, 100, std::process::id()).unwrap();
        assert_eq!(fresh_under(&rows, &under, 0, 60), Some(5), "the newest live process at depth 2 or more: no helper, no zombie");
        assert_eq!(fresh_under(&rows, &under, 4, 1), Some(5), "a start at transcript time minus the slack counts");
        assert_eq!(fresh_under(&rows, &under, 2, 1), None, "older than that counts for nothing");
        // the subtree of the skipped pid is left out
        assert_eq!(fresh_under(&rows, &descend(&rows, 100, 101).unwrap(), 100, 0), None);
        // pid 1 is not walked, so the list is not read for it
        assert_eq!(child_age(1, 700, 60).unwrap(), (None, 0));
    }
}
