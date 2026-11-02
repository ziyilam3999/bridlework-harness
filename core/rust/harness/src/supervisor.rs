//! Process supervisor (unix only, std only): `start` a ticket process under ticket ownership (P1B-05).
//!
//! `start` runs a program in its OWN process group (group id = its pid), stdin closed, stdout and stderr
//! discarded (capture is P1B-08), then signs it out on the ticket's resource manifest: one existing `add`
//! process line (`id`, `pid`, `pid_start_ms` read by the writer) and, when `--port` is given, one existing
//! `add` port line with id `<id>.port` — that id convention is the only link between a process and its port.
//! Nothing new in the manifest format and no event kind: the manifest line IS the durable record.
//!
//! `process stop <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>]` (#183) ends a
//! process the sheet lists, and it is the dangerous half: a pid is just a number and numbers get re-used,
//! so a stop SIGNALS only when every check passes, in order, under the per-ticket lock held for the whole
//! stop: the target is a LIVE `process` entry of THIS ticket (a pid listed only on another ticket is never
//! signalled); the pid provably exists (a helper that cannot RUN is "unknown", never "gone"); the live
//! pid's start time still equals the recorded `pid_start_ms` (exact on Linux, 2000 ms elsewhere — a
//! re-used pid is never signalled, its stale entry is removed with exit 3); and the pid is its OWN
//! process-group leader, so a signal to the group provably ends the family. After TERM to the group and a
//! bounded wait — with the identity re-checked before any KILL, and no KILL to a group already provably
//! empty — the recorded pid must be PROVABLY dead before the answer 0 and the `remove` lines; otherwise
//! the answer is 4 ("signalled, death not confirmed") and the entry is KEPT, so a later stop can retry.
//! Every helper call goes by ABSOLUTE path (`/bin/kill`, `/bin/ps`), never through PATH.
//!
//! The other signal path is the start ROLLBACK, and it touches only the group this run just spawned: a
//! start that cannot record its spawn stops that group and answers 2, so a refused start leaves nothing
//! running and nothing listed. The rollback signals through `/bin/kill` by ABSOLUTE path (never
//! through PATH), falls back to std `Child::kill()` (which needs no PATH) BEFORE reaping, and never waits
//! longer than REAP_BOUND_MS on a child it could not signal. A helper that cannot be RUN is "unknown", never
//! "gone" or "empty": a rollback that cannot confirm its kill says so in the refusal instead.
//!
//! A per-ticket supervisor lock (`<manifest-dir>/<ticket>.proc.lock`, the std file lock the manifest already
//! uses) is held for the WHOLE start, so supervisor operations on one ticket serialise. A parser or token
//! refusal (bad `--port`, bad ticket or id) exits BEFORE the lock, so it creates no lock file either.
//!
//! Exit codes for `start`: `0` started; `2` a usage error or any refusal (nothing listed, nothing left running — except
//! that a refusal AFTER the spawn has already killed the group it made, and a rollback that could not confirm
//! its kill names that in the message). Every input is validated before the first spawn or write: `--port`
//! must be an integer in 1..=65535 (checked in the parser, so an out-of-range port is refused with nothing
//! run), and the port-free bind check covers 127.0.0.1 only — a bind refused for privilege (ports below 1024
//! need privilege even on 127.0.0.1) is reported as its error kind, not as "already bound"; only `AddrInUse`
//! says "already bound".
//!
//! Exit codes for `stop`: `0` stopped and removed; `2` a refusal (nothing signalled, the manifest
//! byte-identical); `3` `already-gone` or `pid-reused` (nothing signalled, the entries removed); `4`
//! signalled but the death could not be confirmed (the entry kept).

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
/// How long the rollback waits between TERM and KILL for the group it signalled to empty.
const DEFAULT_GRACE_MS: u64 = 5000;
/// Poll cadence while waiting for a group to empty or a child to die.
const POLL_MS: u64 = 25;
/// Settle time after a KILL to the group.
const KILL_SETTLE_MS: u64 = 2000;
/// The rollback NEVER waits longer than this on a child it could not signal (the std kill runs first, so
/// this budget only ever names a child that is somehow still alive after it).
const REAP_BOUND_MS: u64 = 3000;

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
/// whole start (until the `ProcLock` is dropped) so supervisor operations on one ticket serialise.
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

// ---------------------------------------------------------------- signals (std only, via the absolute-path kill)

/// The outcome of running the `/bin/kill` helper: it RAN (carrying its exit success), or it could NOT be
/// run at all (no such program, not executable) — which is "unknown" and is NEVER turned into a
/// success-shaped answer ("gone", "empty", "already dead").
enum Ran {
    Yes(bool),
    NotRun(String),
}

/// Signal a whole process group (`TERM`/`KILL`) or probe it (`0`) through `/bin/kill` by ABSOLUTE path, so
/// the call never depends on PATH.
fn kill_helper(pgid: u64, what: &str) -> Ran {
    match Command::new("/bin/kill")
        .arg(format!("-{}", what))
        .arg("--")
        .arg(format!("-{}", pgid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(s) => Ran::Yes(s.success()),
        Err(e) => Ran::NotRun(format!("cannot run /bin/kill: {}", e.kind())),
    }
}

/// Poll until the group is PROVABLY empty, the budget runs out, or the probe cannot be run. True ONLY when
/// the probe ran and said no such group exists; a probe that could not run is "unknown", never "empty".
fn wait_group_empty(pgid: u64, budget_ms: u64) -> bool {
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    loop {
        match kill_helper(pgid, "0") {
            Ran::Yes(false) => return true,
            Ran::Yes(true) => {}
            Ran::NotRun(_) => return false,
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// Reap the child with a BOUNDED poll, never a bare `wait()` — a bare wait blocks until the child exits on
/// its own, which for a server is forever. Ok only when the child is provably reaped.
fn reap_bounded(child: &mut Child, budget_ms: u64) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_millis(budget_ms);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(e) => return Err(format!("cannot reap the started pid: {}", e.kind())),
        }
        if Instant::now() >= deadline {
            return Err(format!("the started pid did not die within {} ms", budget_ms));
        }
        std::thread::sleep(Duration::from_millis(POLL_MS));
    }
}

/// Stop the process THIS invocation just spawned (the start rollback ONLY). TERM, a bounded wait, then KILL
/// go to the whole group through `/bin/kill` by absolute path; when that helper cannot be RUN, the group
/// waits are skipped entirely — unknown is not empty, and waiting on a group nothing signalled is how a
/// rollback hangs. Whatever happened to the group, std `Child::kill()` — which needs no PATH and cannot fail
/// to run — ends the DIRECT child BEFORE the bounded reap, so the rollback never waits longer than
/// REAP_BOUND_MS on a child it could not signal. Ok(()) only when the kill is confirmed; Err carries the
/// reason (the group signal was not runnable, or the child is not provably dead).
fn kill_own_spawn(child: &mut Child, pid: u64) -> Result<(), String> {
    let mut reason = None;
    match kill_helper(pid, "TERM") {
        Ran::NotRun(e) => reason = Some(e),
        Ran::Yes(signalled) => {
            if signalled && !wait_group_empty(pid, DEFAULT_GRACE_MS) {
                let _ = kill_helper(pid, "KILL");
                let _ = wait_group_empty(pid, KILL_SETTLE_MS);
            }
        }
    }
    if let Err(e) = child.kill() {
        reason.get_or_insert_with(|| format!("cannot kill the started pid {} (std): {}", pid, e.kind()));
    }
    if let Err(e) = reap_bounded(child, REAP_BOUND_MS) {
        reason.get_or_insert(e);
    }
    match reason {
        Some(r) => Err(r),
        None => Ok(()),
    }
}

/// Refuse a start AFTER its spawn, running the rollback first: the original reason always leads, and a
/// rollback that could not confirm its kill appends that fact (exit 2 either way, nothing listed, and
/// nothing left running that the rollback could reach).
fn refuse_after_spawn(child: &mut Child, pid: u64, reason: String) -> i32 {
    let tail = match kill_own_spawn(child, pid) {
        Ok(()) => String::new(),
        Err(r) => format!(" (the rollback could not confirm the kill: {})", r),
    };
    fail(&format!("{}{}", reason, tail))
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
    // The port range is validated HERE, in the parser, before the lock, the manifest read, the bind check or
    // the spawn: the manifest would refuse an out-of-range port only AFTER the program had run and the process
    // line had been written (70000 truncates to 4464 as u16, 0 always binds), so the range is a usage error
    // that leaves nothing started and nothing written.
    let port = match &port {
        Some(v) => {
            let p = num_flag(v, "port")?;
            if p == 0 || p > 65535 {
                return Err("--port must be an integer in 1..=65535".to_string());
            }
            Some(p)
        }
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
    // The supervisor lock is held for the whole start, so supervisor operations on one ticket serialise and
    // cannot read a manifest that does not yet list the process this start is about to spawn.
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
        // Only AddrInUse means "taken": the bind can also fail for reasons of privilege (an unprivileged bind on
        // 127.0.0.1 below 1024 is EACCES on macOS/BSD and EPERM on Linux), which is not a claim on the port —
        // so the refusal names the error kind instead of a false "already bound".
        match TcpListener::bind(("127.0.0.1", p as u16)) {
            Ok(l) => drop(l),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
                return fail(&format!("port {} is already bound on 127.0.0.1", p));
            }
            Err(e) => {
                return fail(&format!(
                    "cannot check port {} on 127.0.0.1: {} (ports below 1024 need privilege for the bind check)",
                    p,
                    e.kind()
                ));
            }
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
    // process line, then the port line. Any failure stops the whole group this invocation just made, and a failure
    // AFTER the process line also removes that line, so the sheet never lists a start that was refused.
    let start_ms = match manifest::process_start_ms(pid) {
        Ok(ms) => ms,
        Err(e) => {
            return refuse_after_spawn(&mut child, pid, format!("cannot read the start time of the started pid {}: {}", pid, e));
        }
    };
    let ts_ms = now_ms();
    if let Err(e) = manifest::apply(&dir, &args.ticket, &manifest::Op::AddProcess { id: &args.id, pid, pid_start_ms: start_ms }, ts_ms) {
        return refuse_after_spawn(&mut child, pid, e.to_string());
    }
    if let Some(p) = args.port {
        if let Err(e) = manifest::apply(&dir, &args.ticket, &manifest::Op::AddPort { id: &port_id, port: p }, ts_ms) {
            // The process line is already on the sheet: take it back off (best effort — the refusal below
            // names the original failure either way), then stop the whole group, so a refused start leaves
            // nothing running and nothing listed.
            let _ = manifest::apply(&dir, &args.ticket, &manifest::Op::Remove { id: &args.id }, now_ms());
            return refuse_after_spawn(&mut child, pid, e.to_string());
        }
    }
    println!("{}\t{}\t{}", args.id, pid, start_ms);
    0
}


// ---------------------------------------------------------------- stop (#183: by pid under ticket ownership)

/// What a liveness probe of ONE pid (`kill -0 <pid>`, never its group) answered. `Unknown` covers both a
/// helper that could not RUN and a failure that proves nothing about the pid: unknown is NEVER turned into
/// a success-shaped answer ("gone", "empty", "already dead") — stop refuses with 2 and keeps the entry.
enum Liveness {
    /// The pid answers the probe.
    Alive,
    /// The helper ran and PROVED no such process exists (its stderr names it) — the only "gone" there is.
    NoSuch,
    /// The pid exists but is not ours to signal (the probe answered "operation not permitted").
    Denied,
    /// The probe could not run, or its failure proves nothing.
    Unknown(String),
}

/// Probe one pid through the kill helper at `helper` (an ABSOLUTE path, so PATH never decides liveness).
/// The path is a parameter so the not-runnable case is unit-testable: it must answer Unknown, which stop
/// maps to refusal 2 with nothing written.
fn probe_alive_at(helper: &Path, pid: u64) -> Liveness {
    match Command::new(helper)
        .arg("-0")
        .arg("--")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => Liveness::Alive,
        Ok(o) => {
            let say = String::from_utf8_lossy(&o.stderr).trim().to_ascii_lowercase();
            if say.contains("no such process") {
                Liveness::NoSuch
            } else if say.contains("not permitted") || say.contains("permission") {
                Liveness::Denied
            } else {
                Liveness::Unknown(format!("kill -0 of pid {pid} failed: {say}"))
            }
        }
        Err(e) => Liveness::Unknown(format!("cannot run the liveness probe {}: {}", helper.display(), e.kind())),
    }
}

/// The start-time tolerance between the live pid's start time and the recorded one, the manifest doc's
/// ownership rule: exact on Linux (`/proc` knows ticks), 2000 ms elsewhere (`ps etime` knows whole seconds).
#[cfg(target_os = "linux")]
const START_TOLERANCE_MS: u64 = 0;
#[cfg(not(target_os = "linux"))]
const START_TOLERANCE_MS: u64 = 2000;

/// Does the live pid's start time still match the recorded one, within the platform tolerance?
fn same_start(read: u64, recorded: u64) -> bool {
    read.abs_diff(recorded) <= START_TOLERANCE_MS
}

/// The identity answer: Same (this is the process the sheet recorded), Reused (the recorded process must
/// have exited and the number was re-used), or Unknown (the start time could not be read).
enum Identity {
    Same,
    Reused,
    Unknown(String),
}

/// Read the live pid's start time and compare it with the recorded one, through the same reader `start`
/// uses (`manifest::process_start_ms`, absolute-path helpers). An unreadable start time is Unknown: stop
/// refuses (2, entry kept) rather than signal a pid it cannot identify.
fn probe_identity(pid: u64, recorded: u64) -> Identity {
    match manifest::process_start_ms(pid) {
        Ok(ms) => {
            if same_start(ms, recorded) {
                Identity::Same
            } else {
                Identity::Reused
            }
        }
        Err(e) => Identity::Unknown(e),
    }
}

/// Is the pid its OWN process-group leader (pgid == pid), so a signal to the group provably ends the
/// family it heads? Linux reads `/proc/<pid>/stat` (field 5); elsewhere `/bin/ps` by ABSOLUTE path prints
/// the pgid. A helper that cannot run, or a pgid that cannot be read, is Unknown: stop refuses (2, entry
/// kept) rather than signal a group it cannot prove ends the listed process.
enum Leadership {
    Leader,
    NotLeader(u64),
    Unknown(String),
}

fn probe_leader(pid: u64) -> Leadership {
    #[cfg(target_os = "linux")]
    {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => {
                let after_paren = match stat.rsplit_once(')') {
                    Some((_, r)) => r,
                    None => return Leadership::Unknown("stat has no command field".to_string()),
                };
                let fields: Vec<&str> = after_paren.split_whitespace().collect();
                // fields[0] is stat field 3 (state); pgrp is stat field 5, so index 5 - 3 = 2.
                match fields.get(2).and_then(|f| f.parse::<u64>().ok()) {
                    Some(pgid) if pgid == pid => Leadership::Leader,
                    Some(pgid) => Leadership::NotLeader(pgid),
                    None => Leadership::Unknown(format!("cannot read the pgid of pid {pid} from /proc")),
                }
            }
            Err(e) => Leadership::Unknown(format!("cannot read /proc/{pid}/stat: {}", e.kind())),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        match Command::new("/bin/ps")
            .arg("-o")
            .arg("pgid=")
            .arg("-p")
            .arg(pid.to_string())
            .stdin(Stdio::null())
            .output()
        {
            Ok(o) if o.status.success() => match String::from_utf8_lossy(&o.stdout).trim().parse::<u64>() {
                Ok(pgid) if pgid == pid => Leadership::Leader,
                Ok(pgid) => Leadership::NotLeader(pgid),
                Err(_) => Leadership::Unknown(format!("ps printed no pgid for pid {pid}")),
            },
            Ok(o) => Leadership::Unknown(format!(
                "ps does not know pid {pid}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            )),
            Err(e) => Leadership::Unknown(format!("cannot run /bin/ps: {}", e.kind())),
        }
    }
}

struct StopArgs {
    ticket: String,
    id: Option<String>,
    pid: Option<u64>,
    grace_ms: u64,
    dir: String,
}

/// Parse `process stop`'s command line: the ticket plus exactly one of `--id` / `--pid`, an optional
/// `--grace-ms`, and the usual `--dir`. Usage errors answer 2 before the lock, so they write nothing.
fn parse_stop(a: &[String]) -> Result<StopArgs, String> {
    let mut ticket: Option<String> = None;
    let mut id: Option<String> = None;
    let mut pid: Option<u64> = None;
    let mut grace: Option<u64> = None;
    let mut dir: Option<String> = None;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("{} needs a value", arg))?;
            match name {
                "id" => {
                    if id.replace(value.clone()).is_some() {
                        return Err(format!("{} given twice", arg));
                    }
                }
                "pid" => {
                    let v = num_flag(value, "pid")?;
                    if pid.replace(v).is_some() {
                        return Err(format!("{} given twice", arg));
                    }
                }
                "grace-ms" => {
                    let v = num_flag(value, "grace-ms")?;
                    if grace.replace(v).is_some() {
                        return Err(format!("{} given twice", arg));
                    }
                }
                "dir" => {
                    if dir.replace(value.clone()).is_some() {
                        return Err(format!("{} given twice", arg));
                    }
                }
                _ => return Err(format!("unknown option {}", arg)),
            }
        } else if ticket.replace(arg.clone()).is_some() {
            return Err("more than one ticket given".to_string());
        }
    }
    let ticket = ticket.ok_or_else(|| "a ticket is required".to_string())?;
    let (id, pid) = match (id, pid) {
        (Some(i), None) => (Some(i), None),
        (None, Some(p)) => (None, Some(p)),
        (Some(_), Some(_)) => return Err("stop takes exactly one of --id or --pid, not both".to_string()),
        (None, None) => return Err("stop needs exactly one of --id or --pid".to_string()),
    };
    Ok(StopArgs {
        ticket,
        id,
        pid,
        grace_ms: grace.unwrap_or(DEFAULT_GRACE_MS),
        dir: dir.unwrap_or_else(|| DEFAULT_DIR.to_string()),
    })
}

/// Remove the process entry (and the `<id>.port` entry when `port_live`) — the tail of the 0 answer
/// (stopped) and of both 3 answers (already-gone, pid-reused). Err names the manifest refusal; the file is
/// unchanged on Err.
fn remove_stopped(dir: &Path, ticket: &str, id: &str, port_live: bool) -> Result<(), String> {
    manifest::apply(dir, ticket, &manifest::Op::Remove { id }, now_ms()).map_err(|e| e.to_string())?;
    if port_live {
        let port_id = format!("{id}.port");
        manifest::apply(dir, ticket, &manifest::Op::Remove { id: &port_id }, now_ms()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// `harness process stop` (#183). Every refusal answers 2 with NOTHING signalled and the manifest
/// byte-identical; the two ends-without-signalling answers are 3 (already-gone, pid-reused — the entries
/// are removed, a stale record must not survive); 4 means signalled but the death could not be PROVEN (the
/// entry is KEPT so a later stop can retry). The full check order is the module header.
fn stop_cmd(a: &[String]) -> i32 {
    let args = match parse_stop(a) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    if !crate::events::token_ok(&args.ticket) {
        return fail("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')");
    }
    if let Some(id) = &args.id {
        if !crate::events::token_ok(id) {
            return fail("id must be a plain token (lowercase letters, digits, '-', '_', '.')");
        }
    }
    let dir = PathBuf::from(&args.dir);
    // The supervisor lock is held for the whole stop (like the whole start), so two stops of one entry
    // cannot both pass the live-entry check: the loser reads the sheet AFTER the winner's removes.
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
    // Check 2: the target must be a LIVE process entry of THIS ticket — by `--id`, or a `--pid` matched
    // against the live process entries. A pid not listed here, including one listed only on another
    // ticket, is never signalled.
    let found = match (&args.id, args.pid) {
        (Some(want), _) => read.live.iter().find(|e| e.id == *want).map(|e| (e.id.clone(), e.target.clone())),
        (None, Some(pid)) => read
            .live
            .iter()
            .find(|e| matches!(&e.target, Target::Process { pid: p, .. } if *p == pid))
            .map(|e| (e.id.clone(), e.target.clone())),
        (None, None) => None,
    };
    let Some((id, target)) = found else {
        return fail(&match (&args.id, args.pid) {
            (Some(w), _) => format!("id {w} is not a live entry of ticket {}", args.ticket),
            (None, Some(p)) => format!("pid {p} is not a live entry of ticket {}", args.ticket),
            (None, None) => unreachable!("parse_stop answers a usage error when no selector is given"),
        });
    };
    let Target::Process { pid, pid_start_ms: recorded } = target else {
        return fail(&format!("id {id} is not a live process entry of ticket {}", args.ticket));
    };
    let port_live = read.live.iter().any(|e| e.id == format!("{id}.port"));
    // Check 3: liveness through the absolute-path helper. Provably no such process -> already-gone (3,
    // entries removed); a helper that cannot run, or an answer that proves nothing -> unknown -> 2, kept.
    match probe_alive_at(Path::new("/bin/kill"), pid) {
        Liveness::Alive => {}
        Liveness::NoSuch => {
            return match remove_stopped(&dir, &args.ticket, &id, port_live) {
                Ok(()) => {
                    eprintln!("harness: {id} is already-gone: pid {pid} is provably no such process; nothing signalled, entries removed");
                    3
                }
                Err(e) => fail(&e),
            };
        }
        Liveness::Denied => {
            return fail(&format!("pid {pid} answers the liveness probe but is not ours to signal (kill reports no permission); nothing signalled"));
        }
        Liveness::Unknown(e) => return fail(&e),
    }
    // Check 4: identity — the live pid's start time must equal the recorded one, else the number was
    // re-used: nothing signalled, the stale entries removed (the recorded process must have exited).
    match probe_identity(pid, recorded) {
        Identity::Same => {}
        Identity::Reused => {
            return match remove_stopped(&dir, &args.ticket, &id, port_live) {
                Ok(()) => {
                    eprintln!("harness: {id} pid-reused: pid {pid} no longer starts at the time the sheet records; nothing signalled, entries removed");
                    3
                }
                Err(e) => fail(&e),
            };
        }
        Identity::Unknown(e) => return fail(&format!("cannot read the start time of pid {pid}: {e}")),
    }
    // Check 5: group leadership — only a leader's own group provably ends with a signal to -<pid>.
    match probe_leader(pid) {
        Leadership::Leader => {}
        Leadership::NotLeader(pgid) => {
            return fail(&format!(
                "pid {pid} is not its own process-group leader (pgid {pgid}): a group signal could not prove it ends this process; nothing signalled"
            ));
        }
        Leadership::Unknown(e) => return fail(&format!("cannot prove pid {pid} leads its process group: {e}")),
    }
    // Check 6: TERM to the whole group, then a bounded wait. A helper that cannot run refuses with
    // nothing signalled; whatever happened, the post-signal proof (check 7) decides the answer.
    if let Ran::NotRun(e) = kill_helper(pid, "TERM") {
        return fail(&e);
    }
    if !wait_group_empty(pid, args.grace_ms) {
        // Before any KILL: a leader that is still alive must STILL pass the identity check (the grace
        // window is long enough for a pid to die and its number be re-used), and a group that is already
        // provably empty gets no KILL. While any member lives, the group id cannot be re-issued as a pid,
        // so a KILL to a non-empty group cannot hit a stranger.
        let still_ours = matches!(probe_identity(pid, recorded), Identity::Same);
        let group_gone = matches!(kill_helper(pid, "0"), Ran::Yes(false));
        if still_ours && !group_gone {
            if let Ran::Yes(_) = kill_helper(pid, "KILL") {
                let _ = wait_group_empty(pid, KILL_SETTLE_MS);
            }
        }
    }
    // Check 7: the recorded pid must be PROVABLY dead before the answer 0 and the removes. Anything else —
    // it still answers the probe, or the probe could not run — is 4 with the entry KEPT, so a later stop
    // can retry.
    match probe_alive_at(Path::new("/bin/kill"), pid) {
        Liveness::NoSuch => match remove_stopped(&dir, &args.ticket, &id, port_live) {
            Ok(()) => {
                println!("{id}\t{pid}\tstopped");
                0
            }
            Err(e) => {
                eprintln!("harness: {id} signalled, death not confirmed: the removal failed: {e}; entry kept so a later stop can retry");
                4
            }
        },
        _ => {
            eprintln!("harness: {id} signalled, death not confirmed: pid {pid} still answers the liveness probe; entry kept so a later stop can retry");
            4
        }
    }
}

// ---------------------------------------------------------------- #183: stop (tests)

#[cfg(test)]
mod stop_tests {
    use super::*;

    /// #183 AC 6 (liveness half): a probe whose helper path does not EXIST answers Unknown — never
    /// NoSuch. "The helper could not run" is "I don't know", and stop maps it to refusal 2 with nothing
    /// written; mapping it to "gone" (mutant e) turns this red.
    #[test]
    fn supervisor_liveness_probe_helper_not_runnable_is_unknown() {
        let bad = Path::new("/nonexistent-harness-liveness-probe/kill");
        assert!(!bad.exists(), "the probe helper fixture path must not exist on this host");
        assert!(
            matches!(probe_alive_at(bad, std::process::id() as u64), Liveness::Unknown(_)),
            "a liveness probe whose helper cannot run must answer Unknown, never NoSuch"
        );
    }

    /// #183 AC 6 (identity half): a pid whose start time cannot be READ answers Unknown, which stop maps
    /// to refusal 2 with nothing written — never a blessed match, never "gone".
    #[test]
    fn supervisor_identity_probe_unreadable_is_unknown() {
        // 4_000_000_000 exceeds every mainstream pid space, so no live process can hold it.
        assert!(
            matches!(probe_identity(4_000_000_000, 1730000000000), Identity::Unknown(_)),
            "an unreadable identity must answer Unknown, never Same or Reused"
        );
    }
}
