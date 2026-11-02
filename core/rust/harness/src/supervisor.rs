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
//! bounded wait — with no KILL to a group already provably empty, an ALIVE leader still passing the
//! identity re-check, and a PROVABLY-dead leader's non-empty group still getting the KILL (while any
//! member lives the group id cannot be re-issued as a pid, so that KILL cannot hit a stranger) — the
//! answer 0 needs the WHOLE GROUP provably empty AND the recorded pid PROVABLY dead, never the leader
//! alone; otherwise the answer is 4 ("signalled, death not confirmed") and the entry is KEPT, so a later
//! stop can retry.
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

use crate::evidence::{self, Handshake};
use crate::manifest::{self, Class, ManifestRead, Target};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
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
        // hidden: the supervisor's own waiter (#209, `evidence.rs`). An implementation verb of `start`,
        // never typed by a user, so it stays out of USAGE.
        "waiter" => crate::evidence::waiter_cmd(rest),
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

/// Stop what THIS invocation just made (the start rollback ONLY). Since #209 the spawn is a WAITER (the
/// harness binary in its own group) and `pid` is the PROGRAM's group when the handshake named one — the
/// waiter is not in that group, survives the signal and records the ending — else the waiter's own group.
/// The commit pipe closes FIRST: the uncommitted end-of-input is what makes a LIVE waiter std-kill its
/// program (the only PATH-free way to a program the group signal could not reach), so the close must reach
/// the waiter before any signal aimed at the waiter itself. Then TERM, a bounded wait, KILL to the group
/// through `/bin/kill` by absolute path (a helper that cannot be RUN skips the group waits entirely —
/// unknown is not empty); then a bounded reap of the WAITER (it ends on its own once its program is dead
/// and its record is written) and the std kill of the waiter as the LAST resort. Ok(()) only when
/// everything reachable is provably dead; Err carries the reason.
fn kill_own_spawn(child: &mut Child, pid: u64) -> Result<(), String> {
    let mut reason = None;
    match kill_helper(pid, "TERM") {
        Ran::NotRun(e) => reason = Some(e),
        Ran::Yes(signalled) => {
            if signalled && !wait_group_empty(pid, DEFAULT_GRACE_MS) {
                match kill_helper(pid, "KILL") {
                    Ran::NotRun(e) => {
                        reason.get_or_insert(e);
                    }
                    Ran::Yes(_) => {
                        // The same proof rule as stop's check 7: "left nothing running" is claimed only
                        // when the group is PROVABLY empty, never on a discarded probe answer.
                        if !wait_group_empty(pid, KILL_SETTLE_MS) {
                            reason.get_or_insert(format!("the spawned group {pid} is not provably empty after the KILL"));
                        }
                    }
                }
            }
        }
    }
    // The uncommitted close must reach a LIVE waiter; then a bounded reap, and the std kill only as the
    // last resort on a waiter that would not end.
    drop(child.stdin.take());
    if let Err(e) = reap_bounded(child, REAP_BOUND_MS) {
        reason.get_or_insert(e);
        if let Err(e) = child.kill() {
            reason.get_or_insert_with(|| format!("cannot kill the waiter pid {} (std): {}", child.id(), e.kind()));
        }
        if let Err(e) = reap_bounded(child, REAP_BOUND_MS) {
            reason.get_or_insert(e);
        }
    }
    match reason {
        Some(r) => Err(r),
        None => Ok(()),
    }
}

/// Refuse a start AFTER its spawn, running the rollback first: the original reason always leads, and a
/// rollback that could not confirm its kill appends that fact (exit 2 either way, nothing left running
/// that the rollback could reach; the WRITE-AHEAD evidence entries STAY listed since #209 — they record
/// the failed attempt, and removing them would make the run dir's files unlisted leftovers).
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
    // Runtime evidence (#209, P1B-08). The layout lives in the manifest root that CONTAINS the resolved
    // --cwd (the longest such root when roots nest), so nothing ever lands outside a fence the manifest
    // already enforces, and every captured byte is listed before it exists: the run dir is made, then the
    // three write-ahead evidence entries, all BEFORE any spawn. Existing evidence dirs are CHECKED, never
    // trusted (fail closed); every refusal here answers 2 with nothing spawned and nothing appended.
    let own_uid = match evidence::own_uid() {
        Ok(u) => u,
        Err(e) => return fail(&format!("evidence refused: {e}")),
    };
    let evidence_root = match evidence::evidence_root_for(&resolved_cwd, &read.roots) {
        Some(r) => r,
        None => return fail(&format!("--cwd {} resolves inside no root of ticket {}", resolved_cwd.display(), args.ticket)),
    };
    let (_evidence_dir, proc_dir) = match evidence::ensure_evidence_dirs(&evidence_root, own_uid) {
        Ok(x) => x,
        Err(r) => return fail(&format!("evidence refused: {r}")),
    };
    let ev_ts = now_ms();
    let run_dir = match evidence::create_run_dir(&proc_dir, &args.id, ev_ts) {
        Ok(d) => d,
        Err(r) => return fail(&format!("evidence refused: {r}")),
    };
    // Write-ahead: the three entries (stdout, stderr, exit) go on the sheet BEFORE the spawn, so the
    // files can never be unlisted orphans; they stay live after a stop (evidence outlives the process,
    // retention is P1B-09/P1B-14) and the run stamp lets the same --id run again after one.
    let mut write_ahead: Vec<String> = Vec::new();
    for stream in ["stdout", "stderr", "exit"] {
        let ev_id = format!("{}.{}.{}", args.id, ev_ts, stream);
        match manifest::apply(
            &dir,
            &args.ticket,
            &manifest::Op::AddPath { id: &ev_id, class: Class::Evidence, path: &run_dir.join(stream) },
            ev_ts,
        ) {
            Ok(()) => write_ahead.push(ev_id),
            Err(e) => return unmake_write_ahead(&dir, &args.ticket, &write_ahead, &run_dir, e.to_string()),
        }
    }
    // The spawn is now a WAITER: this same binary, hidden `process waiter` subcommand, in ITS OWN group,
    // a commit pipe in (start's stdin write end) and a one-line handshake pipe out. The waiter re-verifies
    // the run dir, creates the files, spawns the PROGRAM in the program's own group (group id = its pid,
    // exactly as before, so stop's checks are unchanged), hands back the pid and its OS start time, and
    // outlives the program just long enough to record its ending.
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            let reason = format!("cannot start the evidence waiter: {}", e.kind());
            return unmake_write_ahead(&dir, &args.ticket, &write_ahead, &run_dir, reason);
        }
    };
    let mut waiter = match Command::new(exe)
        .arg("process")
        .arg("waiter")
        .arg(&run_dir)
        .arg(&resolved_cwd)
        .arg(args.port.unwrap_or(0).to_string())
        .arg("--")
        .arg(&args.program)
        .args(&args.prog_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
    {
        Ok(w) => w,
        Err(e) => {
            let reason = format!("cannot start the evidence waiter: {}", e.kind());
            return unmake_write_ahead(&dir, &args.ticket, &write_ahead, &run_dir, reason);
        }
    };
    // Handshake: `ok <pid> <start_ms>` (the waiter reads the program's OS start time before any reap
    // could make the pid unreadable), its own composed refusal, or nothing within the bound.
    let waiter_pid = waiter.id() as u64;
    let handshake = evidence::read_handshake(waiter.stdout.take().expect("the waiter's stdout was piped"), evidence::HANDSHAKE_BOUND_MS);
    let hs = match handshake {
        Ok(h) => h,
        Err(m) => return refuse_after_spawn(&mut waiter, waiter_pid, format!("evidence refused: {m}")),
    };
    let (pid, start_ms) = match hs {
        // start records the PROGRAM's pid (not the waiter's): the manifest's process line means what it
        // has always meant, and `stop` reads that line exactly as before.
        Handshake::Ok { pid, start_ms } => (pid as u64, start_ms),
        Handshake::Fail(msg) => {
            // The waiter refused (spawn failure, hostile run dir, unreadable start time) and ends by
            // itself; nothing it made is left running. The evidence entries stay listed: they record the
            // failed attempt (#209 decision 7).
            if reap_bounded(&mut waiter, REAP_BOUND_MS).is_err() {
                let _ = waiter.kill();
                let _ = waiter.wait();
            }
            return fail(&msg);
        }
    };
    // Record: the process line, then the port line. Any failure stops everything this invocation just
    // made, and a failure AFTER the process line also removes that line, so the sheet never lists a
    // start that was refused; the write-ahead evidence entries stay (decision 7).
    let ts_ms = now_ms();
    if let Err(e) = manifest::apply(&dir, &args.ticket, &manifest::Op::AddProcess { id: &args.id, pid, pid_start_ms: start_ms }, ts_ms) {
        return refuse_after_spawn(&mut waiter, pid, e.to_string());
    }
    if let Some(p) = args.port {
        if let Err(e) = manifest::apply(&dir, &args.ticket, &manifest::Op::AddPort { id: &port_id, port: p }, ts_ms) {
            // The process line is already on the sheet: take it back off (best effort — the refusal below
            // names the original failure either way), then stop everything, so a refused start leaves
            // nothing running and nothing listed but the attempt's evidence entries.
            let _ = manifest::apply(&dir, &args.ticket, &manifest::Op::Remove { id: &args.id }, now_ms());
            return refuse_after_spawn(&mut waiter, pid, e.to_string());
        }
    }
    // Commit handshake: the lines are on the sheet, so the waiter may let its program run to its natural
    // end. Writing the commit word and closing the pipe is what a waiter that instead sees a bare
    // end-of-input treats as "kill your program and record committed:false".
    if let Some(mut commit) = waiter.stdin.take() {
        let _ = commit.write_all(b"commit\n");
        let _ = commit.flush();
    }
    println!("{}\t{}\t{}", args.id, pid, start_ms);
    0
}

/// Undo the write-ahead of a start that never spawned anything (an evidence append or the waiter spawn
/// itself failed): best effort — remove the entries added so far and the empty run dir, then refuse with
/// the original reason. Once the WAITER is running the entries are kept instead (decision 7: they record
/// the attempt), so this runs only on the nothing-ever-started path.
fn unmake_write_ahead(dir: &Path, ticket: &str, added: &[String], run_dir: &Path, reason: String) -> i32 {
    for id in added {
        let _ = manifest::apply(dir, ticket, &manifest::Op::Remove { id }, now_ms());
    }
    let _ = fs::remove_dir_all(run_dir);
    fail(&reason)
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
        Ok(o) => liveness_from_ran(pid, o.status.success(), &String::from_utf8_lossy(&o.stderr).trim().to_ascii_lowercase()),
        Err(e) => Liveness::Unknown(format!("cannot run the liveness probe {}: {}", helper.display(), e.kind())),
    }
}

/// Classify the kill -0 helper's outcome. Success is Alive; a stderr naming "no such process" is the ONLY
/// "gone"; a stderr naming permission is Denied — a pid of ANOTHER user, read as not ours and never
/// signalled, never blessed as a live entry of ours either; anything else proves nothing (Unknown).
fn liveness_from_ran(pid: u64, helper_ok: bool, say: &str) -> Liveness {
    if helper_ok {
        Liveness::Alive
    } else if say.contains("no such process") {
        Liveness::NoSuch
    } else if say.contains("not permitted") || say.contains("permission") {
        Liveness::Denied
    } else {
        Liveness::Unknown(format!("kill -0 of pid {pid} failed: {say}"))
    }
}

/// The start-time tolerance between the live pid's start time and the recorded one, the manifest doc's
/// ownership rule. Linux is EXACT (`/proc` knows clock ticks), so it compares with `==` and needs no
/// tolerance at all; elsewhere `ps etime` knows whole seconds, so a 2000 ms tolerance applies. The
/// constant is compiled ONLY on the platforms that use it and is never 0 there, so no target OS the CI
/// builds compiles a comparison against a zero tolerance (which the lint `clippy::absurd_extreme_comparisons`
/// denies, and rightly: on Linux `<= 0` on an `abs_diff` is only ever `==`).
#[cfg(not(target_os = "linux"))]
const START_TOLERANCE_MS: u64 = 2000;

/// Does the live pid's start time still match the recorded one? Exact on Linux, within the tolerance
/// elsewhere. Two bodies (not a cfg'd constant inside one) so each target compiles exactly one comparison,
/// and neither is against a value that is 0 on that target.
#[cfg(target_os = "linux")]
fn same_start(read: u64, recorded: u64) -> bool {
    read == recorded
}
#[cfg(not(target_os = "linux"))]
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
        // The grace window ran out with the group not yet PROVABLY empty. The KILL gate (below) needs the
        // group to be provably OURS before so blunt a signal: a group already provably empty gets no KILL;
        // a leader still ALIVE must still pass the identity check (the window is long enough for a pid to
        // die and its number be re-used); and a leader that is PROVABLY dead with a non-empty group still
        // gets the KILL — while any member lives, the group id cannot be re-issued as a pid, so that KILL
        // cannot hit a stranger either. The head before fix r2 skipped the KILL in exactly that last case
        // (a dead leader has no identity to re-read) and then answered 0 on the leader alone.
        let group_empty_now = matches!(kill_helper(pid, "0"), Ran::Yes(false));
        let leader_now = probe_alive_at(Path::new("/bin/kill"), pid);
        match kill_decision(leader_now, group_empty_now, || probe_identity(pid, recorded)) {
            KillDecision::Send => {
                if let Ran::Yes(_) = kill_helper(pid, "KILL") {
                    let _ = wait_group_empty(pid, KILL_SETTLE_MS);
                }
            }
            KillDecision::AlreadyEmpty => {}
            KillDecision::Barred(why) => {
                eprintln!("harness: {id} signalled, death not confirmed: no KILL was sent: {why}; entry kept so a later stop can retry")
            }
        }
    }
    // Check 7: the answer 0 needs the WHOLE GROUP provably empty — the leader alone dying proves nothing
    // about a member that ignored TERM — and the recorded pid itself provably dead (a member of its own
    // group, but pid and group are probed separately so neither proof leans on the other). Anything else —
    // a survivor, an unreaped zombie, or a probe that could not run — is 4 with the entry KEPT, so a later
    // stop can retry.
    let group_gone = matches!(kill_helper(pid, "0"), Ran::Yes(false));
    let leader_gone = matches!(probe_alive_at(Path::new("/bin/kill"), pid), Liveness::NoSuch);
    if death_proven(group_gone, leader_gone) {
        return match remove_stopped(&dir, &args.ticket, &id, port_live) {
            Ok(()) => {
                println!("{id}\t{pid}\tstopped");
                0
            }
            Err(e) => {
                eprintln!("harness: {id} signalled, death not confirmed: the removal failed: {e}; entry kept so a later stop can retry");
                4
            }
        };
    }
    eprintln!(
        "harness: {id} signalled, death not confirmed: pid {pid} or its group still answers the liveness probe, or the probe could not run; entry kept so a later stop can retry"
    );
    4
}

/// Check 7's proof: the answer 0 needs BOTH proofs — the whole group provably empty AND the recorded pid
/// provably dead. Neither alone suffices: a dead leader proves nothing about a TERM-ignoring member, and
/// an empty group probe leans on the leader being a member of its own group (it is, but the two probes
/// are kept independent so neither proof stands in for the other).
fn death_proven(group_gone: bool, leader_gone: bool) -> bool {
    group_gone && leader_gone
}

/// The KILL gate of check 6, after the grace window ran out: may the KILL go to the group at all? `Send`
/// needs TWO proofs — the group is not already provably empty (nothing to signal), and it is provably
/// still OURS: either the leader is alive and STILL the recorded process (the pre-KILL identity re-check),
/// or the leader is PROVABLY dead, in which case the group id (the dead leader's pid) cannot have been
/// re-issued as a new process's pid while any member lives. A leader only "denied" or "unknown", or an
/// alive leader whose identity no longer matches (its number was re-used inside the grace window), bars
/// the KILL: check 7 then answers 4 with the entry kept.
enum KillDecision {
    /// The group is already provably empty: nothing to signal.
    AlreadyEmpty,
    /// The group is provably ours and non-empty: send the KILL.
    Send,
    /// The KILL must not go; the string names why (it reaches the stop's stderr).
    Barred(String),
}

/// Decide the KILL. `identity` is read LAZILY — only a LIVE leader has a start time worth re-reading, so a
/// dead leader costs no identity probe at all.
fn kill_decision(leader: Liveness, group_empty: bool, identity: impl FnOnce() -> Identity) -> KillDecision {
    if group_empty {
        return KillDecision::AlreadyEmpty;
    }
    match leader {
        // A provably-dead leader with a live group: the group is still ours (its id cannot be re-issued as
        // a pid while any member lives), so the KILL cannot hit a stranger. This is the fix-r2 case.
        Liveness::NoSuch => KillDecision::Send,
        Liveness::Alive => match identity() {
            Identity::Same => KillDecision::Send,
            Identity::Reused => KillDecision::Barred(
                "the leader's pid was re-used inside the grace window; a KILL to its group could hit a stranger".to_string(),
            ),
            Identity::Unknown(e) => KillDecision::Barred(format!("cannot re-check the leader's start time: {e}")),
        },
        Liveness::Denied => KillDecision::Barred(
            "the leader answers the probe but is not ours to signal; a KILL to its group could hit a stranger".to_string(),
        ),
        Liveness::Unknown(e) => KillDecision::Barred(e),
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

    /// #183 fix r2, F3 (review mutant own-4): a kill -0 that answers "operation not permitted" names
    /// another user's pid — Denied, read as NOT OURS and never signalled. Mapping it to Alive would let a
    /// foreign pid be probed as a live entry of ours; this test goes red the moment that mapping changes.
    #[test]
    fn supervisor_eperm_liveness_is_denied_never_alive() {
        assert!(
            matches!(liveness_from_ran(123, false, "kill: (123) - operation not permitted"), Liveness::Denied),
            "EPERM must classify as Denied (not ours), never Alive"
        );
        assert!(
            matches!(liveness_from_ran(123, false, "kill: (123) - permission denied"), Liveness::Denied),
            "the other permission spelling must classify as Denied too"
        );
        assert!(
            matches!(liveness_from_ran(123, false, "kill: (123) - no such process"), Liveness::NoSuch),
            "no-such-process is the only 'gone' there is"
        );
        assert!(
            matches!(liveness_from_ran(123, false, ""), Liveness::Unknown(_)),
            "an empty failure proves nothing: Unknown"
        );
        assert!(
            matches!(liveness_from_ran(123, true, ""), Liveness::Alive),
            "a successful probe is Alive"
        );
    }

    /// #183 fix r2, F3 (review mutant own-2) + F2: the KILL gate's decision table. The identity re-check
    /// is load-bearing ONLY for a LIVE leader (its number could have been re-used inside the grace
    /// window), and a PROVABLY-dead leader with a non-empty group still gets the KILL (the fix-r2 case:
    /// the head skipped it and answered 0 while a TERM-ignoring member lived).
    #[test]
    fn supervisor_kill_gate_decision_table() {
        use KillDecision::{AlreadyEmpty, Barred, Send};

        // A group already provably empty gets no KILL, whatever the leader says.
        assert!(matches!(kill_decision(Liveness::NoSuch, true, || panic!("an empty group must not read identity")), AlreadyEmpty));
        assert!(matches!(kill_decision(Liveness::Alive, true, || panic!("an empty group must not read identity")), AlreadyEmpty));

        // The fix-r2 case: a PROVABLY-dead leader with a non-empty group still gets the KILL — the group
        // id cannot be re-issued as a pid while any member lives, so the KILL cannot hit a stranger.
        assert!(matches!(kill_decision(Liveness::NoSuch, false, || panic!("a dead leader must not be identity-read")), Send));

        // A LIVE leader is KILLed only when it is STILL the recorded process (review mutant own-2:
        // still_ours forced true — red here).
        assert!(matches!(kill_decision(Liveness::Alive, false, || Identity::Same), Send));
        assert!(matches!(kill_decision(Liveness::Alive, false, || Identity::Reused), Barred(_)));
        assert!(matches!(kill_decision(Liveness::Alive, false, || Identity::Unknown("read failed".into())), Barred(_)));

        // A leader that is only denied or unknown bars the KILL: not provably ours.
        assert!(matches!(kill_decision(Liveness::Denied, false, || panic!("a denied leader must not be identity-read")), Barred(_)));
        assert!(matches!(kill_decision(Liveness::Unknown("probe failed".into()), false, || panic!("an unknown leader must not be identity-read")), Barred(_)));
    }

    /// #183 fix r2 (check 7, the second half of F2): "stopped" needs the GROUP provably empty, never the
    /// leader alone — this is the exact rule the head violated when it answered 0 while a TERM-ignoring
    /// member lived (the head read only the leader here).
    #[test]
    fn supervisor_stopped_proof_needs_group_and_leader() {
        assert!(death_proven(true, true), "both proofs dead: stopped");
        assert!(!death_proven(false, true), "a dead leader alone must never prove 'stopped' (the fix-r2 rule)");
        assert!(!death_proven(true, false), "an empty group alone must never prove 'stopped' (the pid could be a zombie elsewhere)");
        assert!(!death_proven(false, false), "nothing proven");
    }
}
