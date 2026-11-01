//! Process supervisor (unix only, std only): `start` and `stop` ticket processes under ticket ownership (P1B-05).
//!
//! `start` runs a program in its OWN process group (group id = its pid), stdin closed, stdout and stderr discarded
//! (capture is P1B-08), then signs it out on the ticket's resource manifest: one existing `add` process line
//! (`id`, `pid`, `pid_start_ms` read by the writer) and, when `--port` is given, one existing `add` port line with
//! id `<id>.port` — that id convention is the only link between a process and its port. Nothing new in the manifest
//! format and no event kind: the manifest line IS the durable record. A start that cannot record its spawn stops the
//! whole group it just made, so a refused start leaves nothing running that the sheet does not list.
//!
//! `stop` signals only when ALL hold: the manifest reads clean; the target is a LIVE process entry of THIS ticket
//! (a raw pid that is not listed, including another ticket's pid, is never signalled); and the live process at that
//! pid has the recorded start time — exact on Linux, within 2000 ms elsewhere (the manifest doc's tolerance) — so a
//! re-used pid is never touched. Then TERM to the process GROUP, a bounded wait (`--grace-ms`), KILL to the group if
//! it lives, and the `remove` lines for the process entry and for `<id>.port` if live. Signals go through the system
//! `kill` command with a negative group id, so no new crate is needed.
//!
//! A per-ticket supervisor lock (`<manifest-dir>/<ticket>.proc.lock`, the std file lock the manifest already uses)
//! is held for the WHOLE start or stop, so two concurrent stops of one id serialise: one exits 0, the other finds
//! the id not live and exits 2, with exactly one `remove` line; and a stop is safe to repeat after a crash between
//! the signal and the `remove` (the next stop sees `already-gone`).
//!
//! Exit codes: `0` started / stopped; `2` a usage error or any refusal (nothing started, nothing signalled, nothing
//! written); `3` the recorded process is already gone (`already-gone`) or the pid was re-used (`pid-reused`) — the
//! entries are removed and nothing was signalled.

use crate::manifest::{self, ManifestRead, Target};
use std::fs::{File, OpenOptions};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness process start <ticket> --id <id> --cwd <dir> [--port <n>] [--dir <manifest-dir>] -- <program> [args...] | process stop <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>]";

/// Default manifest dir, the same default as `harness manifest`.
const DEFAULT_DIR: &str = ".harness/manifests";
/// How long `stop` waits between TERM and KILL unless `--grace-ms` says otherwise.
const DEFAULT_GRACE_MS: u64 = 5000;
/// Off Linux a start time read from `ps` jitters between reads (whole-second precision); this is the manifest
/// doc's comparison tolerance.
const START_TOLERANCE_MS: u64 = 2000;
/// Poll cadence while waiting for a group to empty, and the settle time after a KILL.
const POLL_MS: u64 = 25;
const KILL_SETTLE_MS: u64 = 2000;

// ---------------------------------------------------------------- command entry

/// `harness process start|stop` dispatch.
pub fn process_cmd(a: &[String]) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{}", USAGE);
        return 2;
    };
    match sub.as_str() {
        "start" => start_cmd(rest),
        "stop" => stop_cmd(rest),
        _ => {
            eprintln!("{}", USAGE);
            2
        }
    }
}

fn fail(m: &str) -> i32 {
    eprintln!("harness: error: {}", m);
    2
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// An unsigned integer flag value: digits only, else a usage error naming the flag (the manifest rule).
fn num_flag(v: &str, name: &str) -> Result<u64, String> {
    if !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) {
        v.parse::<u64>().map_err(|_| format!("--{name} is out of range"))
    } else {
        Err(format!("--{name} must be a non-negative integer"))
    }
}

/// The damage message for a read that is not clean (corruption wins over a torn tail, the manifest rule).
fn damage_msg(r: &ManifestRead) -> String {
    if let Some(c) = &r.corrupt {
        format!("manifest is corrupt at line {}: {}", c.line, c.msg)
    } else if let Some(t) = &r.torn_tail {
        format!("manifest has a torn tail at line {} ({} bytes, no newline); not read as a line", t.line, t.bytes)
    } else {
        "manifest is not clean".to_string()
    }
}

/// The per-ticket supervisor lock: `<manifest-dir>/<ticket>.proc.lock`, the std exclusive file lock, held for the
/// whole start or stop (until the `ProcLock` is dropped) so supervisor operations on one ticket serialise.
/// The File IS the lock: keeping it alive holds the exclusive lock until the command returns and it is dropped.
#[allow(dead_code)]
struct ProcLock(File);

fn lock_ticket(dir: &Path, ticket: &str) -> Result<ProcLock, String> {
    let path = dir.join(format!("{ticket}.proc.lock"));
    let f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("cannot open the supervisor lock {}: {}", path.display(), e.kind()))?;
    f.lock().map_err(|e| format!("cannot lock {}: {}", path.display(), e))?;
    Ok(ProcLock(f))
}

// ---------------------------------------------------------------- signals (std only, via the system kill)

/// Signal a whole process group through the system `kill` command (`kill -<sig> -- -<pgid>`).
fn signal_group(pgid: u64, sig: &str) -> bool {
    Command::new("kill")
        .arg(format!("-{}", sig))
        .arg("--")
        .arg(format!("-{}", pgid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// True when no process group with this id exists (nothing left to signal or wait for).
fn group_empty(pgid: u64) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg("--")
        .arg(format!("-{}", pgid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
}

/// Poll until the group is empty or the budget runs out. True when it is empty.
fn wait_group_empty(pgid: u64, budget_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    while Instant::now() < deadline {
        if group_empty(pgid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
    group_empty(pgid)
}

/// Stop a process this invocation just spawned: TERM to the group, a bounded wait, KILL if it lives, then reap the
/// direct child. Used only by the start rollback, so a refused start leaves nothing running that the sheet does not list.
fn kill_own_spawn(child: &mut Child, pid: u64) {
    let _ = signal_group(pid, "TERM");
    wait_group_empty(pid, DEFAULT_GRACE_MS);
    let _ = signal_group(pid, "KILL");
    let _ = wait_group_empty(pid, KILL_SETTLE_MS);
    let _ = child.wait();
}

// ---------------------------------------------------------------- pid identity

/// Does a live start time name the recorded process? Exact on Linux (`/proc` is precise); off Linux `ps` gives whole
/// seconds that jitter between reads, so the manifest doc's tolerance of at least 2000 ms applies.
fn start_matches(recorded: u64, live: u64) -> bool {
    if cfg!(target_os = "linux") {
        recorded == live
    } else {
        recorded.abs_diff(live) <= START_TOLERANCE_MS
    }
}

/// A resolved path lies inside one of the ticket's roots (equal counts as inside), the manifest fence rule.
fn inside_roots(p: &Path, roots: &[String]) -> bool {
    roots.iter().any(|r| p.starts_with(Path::new(r.as_str())))
}

// ---------------------------------------------------------------- start

struct StartArgs {
    ticket: String,
    id: String,
    cwd: String,
    port: Option<u64>,
    dir: String,
    program: String,
    prog_args: Vec<String>,
}

/// Parse `process start`'s command line: flags up to the literal `--`, then the program and its arguments verbatim.
fn parse_start(a: &[String]) -> Result<StartArgs, String> {
    let dash = a.iter().position(|x| x == "--").ok_or_else(|| "start needs -- before the program and its arguments".to_string())?;
    let (before, after) = a.split_at(dash);
    let mut ticket: Option<String> = None;
    let mut id: Option<String> = None;
    let mut cwd: Option<String> = None;
    let mut port: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut it = before.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("{} needs a value", arg))?;
            let slot = match name {
                "id" => &mut id,
                "cwd" => &mut cwd,
                "port" => &mut port,
                "dir" => &mut dir,
                _ => return Err(format!("unknown option {}", arg)),
            };
            if slot.replace(value.clone()).is_some() {
                return Err(format!("{} given twice", arg));
            }
        } else if ticket.replace(arg.clone()).is_some() {
            return Err("more than one ticket given".to_string());
        }
    }
    let after = &after[1..];
    let program = after.first().cloned().ok_or_else(|| "no program after --".to_string())?;
    let ticket = ticket.ok_or_else(|| "a ticket is required".to_string())?;
    let id = id.ok_or_else(|| "--id is required".to_string())?;
    let cwd = cwd.ok_or_else(|| "--cwd is required".to_string())?;
    let port = match &port {
        Some(v) => Some(num_flag(v, "port")?),
        None => None,
    };
    Ok(StartArgs { ticket, id, cwd, port, dir: dir.unwrap_or_else(|| DEFAULT_DIR.to_string()), program, prog_args: after[1..].to_vec() })
}

fn start_cmd(a: &[String]) -> i32 {
    let args = match parse_start(a) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    if !crate::events::token_ok(&args.ticket) {
        return fail("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')");
    }
    if !crate::events::token_ok(&args.id) {
        return fail("id must be a plain token (lowercase letters, digits, '-', '_', '.')");
    }
    let dir = PathBuf::from(&args.dir);
    // The supervisor lock is held for the whole start, so a concurrent stop of this ticket cannot read a manifest
    // that does not yet list the process this start is about to spawn (and vice versa).
    let _lock = match lock_ticket(&dir, &args.ticket) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    let read = match manifest::read_manifest(&dir, &args.ticket) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    if !read.is_clean() {
        return fail(&damage_msg(&read));
    }
    let port_id = format!("{}.port", args.id);
    if read.live.iter().any(|e| e.id == args.id) {
        return fail(&format!("id {} is already live in ticket {}", args.id, args.ticket));
    }
    if args.port.is_some() && read.live.iter().any(|e| e.id == port_id) {
        return fail(&format!("id {} is already live in ticket {}", port_id, args.ticket));
    }
    // --cwd must resolve inside one of the ticket's roots (stored resolved, so the comparison is like for like).
    let cwd_path = Path::new(&args.cwd);
    if !cwd_path.is_absolute() {
        return fail("--cwd must be absolute");
    }
    let resolved_cwd = match cwd_path.canonicalize() {
        Ok(c) => c,
        Err(_) => return fail(&format!("--cwd {} does not resolve", args.cwd)),
    };
    if !resolved_cwd.is_dir() {
        return fail(&format!("--cwd {} is not a directory", args.cwd));
    }
    if !inside_roots(&resolved_cwd, &read.roots) {
        return fail(&format!("--cwd {} resolves outside every root of ticket {}", resolved_cwd.display(), args.ticket));
    }
    // The port must be free in the manifest AND on 127.0.0.1. The bind check is a snapshot: it is released before
    // the child could bind, so a racer can still take it (an honest limit, see docs/process-supervisor.md).
    if let Some(p) = args.port {
        if read.live.iter().any(|e| matches!(&e.target, Target::Port(n) if *n == p)) {
            return fail(&format!("port {} is already live in ticket {}", p, args.ticket));
        }
        match TcpListener::bind(("127.0.0.1", p as u16)) {
            Ok(l) => drop(l),
            Err(_) => return fail(&format!("port {} is already bound on 127.0.0.1", p)),
        }
    }
    // Spawn in its OWN process group (group id = its pid), stdin closed, output discarded, PORT in the environment.
    let mut cmd = Command::new(&args.program);
    cmd.args(&args.prog_args)
        .current_dir(&resolved_cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
    if let Some(p) = args.port {
        cmd.env("PORT", p.to_string());
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(&format!("cannot start {}: {}", args.program, e.kind())),
    };
    let pid = child.id() as u64;
    // Record: the start time first (identity cannot be read -> nothing recorded, nothing left running), then the
    // process line, then the port line. Any failure stops the whole group this invocation just made.
    let start_ms = match manifest::process_start_ms(pid) {
        Ok(ms) => ms,
        Err(e) => {
            kill_own_spawn(&mut child, pid);
            return fail(&format!("cannot read the start time of the started pid {}: {}", pid, e));
        }
    };
    let ts_ms = now_ms();
    if let Err(e) = manifest::apply(&dir, &args.ticket, &manifest::Op::AddProcess { id: &args.id, pid, pid_start_ms: start_ms }, ts_ms) {
        kill_own_spawn(&mut child, pid);
        return fail(&e.to_string());
    }
    if let Some(p) = args.port {
        if let Err(e) = manifest::apply(&dir, &args.ticket, &manifest::Op::AddPort { id: &port_id, port: p }, ts_ms) {
            kill_own_spawn(&mut child, pid);
            return fail(&e.to_string());
        }
    }
    println!("{}\t{}\t{}", args.id, pid, start_ms);
    0
}

// ---------------------------------------------------------------- stop

struct StopArgs {
    ticket: String,
    id: Option<String>,
    pid: Option<u64>,
    grace_ms: u64,
    dir: String,
}

/// Parse `process stop`'s command line.
fn parse_stop(a: &[String]) -> Result<StopArgs, String> {
    let mut ticket: Option<String> = None;
    let mut id: Option<String> = None;
    let mut pid: Option<String> = None;
    let mut grace: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("{} needs a value", arg))?;
            let slot = match name {
                "id" => &mut id,
                "pid" => &mut pid,
                "grace-ms" => &mut grace,
                "dir" => &mut dir,
                _ => return Err(format!("unknown option {}", arg)),
            };
            if slot.replace(value.clone()).is_some() {
                return Err(format!("{} given twice", arg));
            }
        } else if ticket.replace(arg.clone()).is_some() {
            return Err("more than one ticket given".to_string());
        }
    }
    let ticket = ticket.ok_or_else(|| "a ticket is required".to_string())?;
    let pid = match &pid {
        Some(v) => Some(num_flag(v, "pid")?),
        None => None,
    };
    let grace_ms = match &grace {
        Some(v) => num_flag(v, "grace-ms")?,
        None => DEFAULT_GRACE_MS,
    };
    Ok(StopArgs { ticket, id, pid, grace_ms, dir: dir.unwrap_or_else(|| DEFAULT_DIR.to_string()) })
}

/// Append the `remove` lines for a stopped (or gone) entry: the process entry, and `<id>.port` when it is live.
fn remove_entries(dir: &Path, ticket: &str, id: &str, port_live: bool, ts_ms: u64) -> Result<(), String> {
    let port_id = format!("{}.port", id);
    manifest::apply(dir, ticket, &manifest::Op::Remove { id }, ts_ms).map_err(|e| e.to_string())?;
    if port_live {
        manifest::apply(dir, ticket, &manifest::Op::Remove { id: &port_id }, ts_ms).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn stop_cmd(a: &[String]) -> i32 {
    let args = match parse_stop(a) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    if !crate::events::token_ok(&args.ticket) {
        return fail("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')");
    }
    if args.id.is_some() == args.pid.is_some() {
        return fail("stop takes exactly one of --id and --pid");
    }
    let dir = PathBuf::from(&args.dir);
    // The supervisor lock is held for the whole stop, so two stops of one id serialise (one 0, one 2, exactly one
    // remove line) and a stop cannot race a start's spawn-and-record window on the same ticket.
    let _lock = match lock_ticket(&dir, &args.ticket) {
        Ok(l) => l,
        Err(e) => return fail(&e),
    };
    let read = match manifest::read_manifest(&dir, &args.ticket) {
        Ok(r) => r,
        Err(e) => return fail(&e),
    };
    if !read.is_clean() {
        return fail(&damage_msg(&read));
    }
    // The target must be a LIVE process entry of THIS ticket. A raw pid not listed here — including a pid listed
    // only on another ticket's manifest — is never signalled.
    let (id, pid, recorded_start) = match (args.id.as_deref(), args.pid) {
        (Some(want), _) => {
            let Some(e) = read.live.iter().find(|e| e.id == want) else {
                return fail(&format!("id {} is not a live entry of ticket {}", want, args.ticket));
            };
            match &e.target {
                Target::Process { pid, pid_start_ms } => (want.to_string(), *pid, *pid_start_ms),
                _ => return fail(&format!("id {} is not a live process entry of ticket {}", want, args.ticket)),
            }
        }
        (None, Some(p)) => {
            let Some(e) = read.live.iter().find(|e| matches!(&e.target, Target::Process { pid: q, .. } if *q == p)) else {
                return fail(&format!("pid {} is not a live process entry of ticket {}", p, args.ticket));
            };
            let Target::Process { pid_start_ms, .. } = &e.target else { unreachable!("matched on Process") };
            (e.id.clone(), p, *pid_start_ms)
        }
        (None, None) => unreachable!("exactly one of --id and --pid was checked above"),
    };
    let port_id = format!("{}.port", id);
    let port_live = read.live.iter().any(|e| e.id == port_id);
    let ts_ms = now_ms();
    // Identity first, signal second: a pid with no process is `already-gone`, a live pid with a different start
    // time is a re-used pid. Neither is ever signalled; both remove the entries, so a stop is safe to repeat.
    match manifest::process_start_ms(pid) {
        Err(_) => {
            if let Err(e) = remove_entries(&dir, &args.ticket, &id, port_live, ts_ms) {
                return fail(&e);
            }
            println!("already-gone");
            return 3;
        }
        Ok(live_start) if !start_matches(recorded_start, live_start) => {
            if let Err(e) = remove_entries(&dir, &args.ticket, &id, port_live, ts_ms) {
                return fail(&e);
            }
            println!("pid-reused");
            return 3;
        }
        Ok(_) => {}
    }
    // TERM to the process group, a bounded wait, KILL if the group lives, then the remove lines.
    let _ = signal_group(pid, "TERM");
    if !wait_group_empty(pid, args.grace_ms) {
        let _ = signal_group(pid, "KILL");
        let _ = wait_group_empty(pid, KILL_SETTLE_MS);
    }
    if let Err(e) = remove_entries(&dir, &args.ticket, &id, port_live, ts_ms) {
        return fail(&e);
    }
    println!("stopped\t{}\t{}", id, pid);
    0
}
