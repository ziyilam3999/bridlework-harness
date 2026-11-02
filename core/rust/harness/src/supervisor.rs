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
//! Honest limits (#207): `process start --timeout-ms <n>` records a `limit` line on the manifest right
//! after the process line, and two one-shot checks enforce it — there is NO always-on watcher. `process
//! check <ticket>` (the driver runs it before every step and at finalize) stops every entry past its time
//! through the SAME stop checks 2-7 factored below, answering 5 (resolved), 4 (death not confirmed), 2
//! (refusal — including an unreadable age or a clock that reads before the limit line: it never answers
//! "fine" when it does not know), or 0. `process step <ticket> --repo <r> --steps <s> --soft <a> --hard <b>`
//! is stateless (the driver owns the count): past the soft line it writes one `check_in` event and says
//! carry on (exit 0); at the hard line it writes `step_limit_stop` FIRST (a hard stop is never silent),
//! then stops every live process of the ticket — exit 6 all proven dead, 4 any death unproven, 2 the event
//! could not be written (nothing signalled). ONLY exit 0 means "take another step".
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

use crate::death;
use crate::manifest::{self, ManifestRead, Target};
use std::fs::{File, OpenOptions};
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness process start <ticket> --id <id> --cwd <dir> [--port <n>] [--timeout-ms <n>] [--dir <manifest-dir>] -- <program> [args...] | process stop <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>] [--session <uuid> --checkpoint <sha> --repo <token> [--log <path>] [--root <dir>]] | process check <ticket> [--grace-ms <n>] [--dir <manifest-dir>] [--session <uuid> --checkpoint <sha> --repo <token> [--log <path>] [--root <dir>]] | process step <ticket> --repo <r> --steps <s> --soft <a> --hard <b> [--grace-ms <n>] [--dir <manifest-dir>] [--log <path>] [--root <dir>] [--session <uuid> --checkpoint <sha>]";

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

/// `harness process start|stop|check|step` dispatch.
pub fn process_cmd(a: &[String]) -> i32 {
    let Some((sub, rest)) = a.split_first() else {
        eprintln!("{}", USAGE);
        return 2;
    };
    match sub.as_str() {
        "start" => start_cmd(rest),
        "stop" => stop_cmd(rest),
        "check" => check_cmd(rest),
        "step" => step_cmd(rest),
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
    timeout_ms: Option<u64>,
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
    let mut timeout: Option<String> = None;
    let mut dir: Option<String> = None;
    let mut it = before.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("{} needs a value", arg))?;
            let slot = match name {
                "id" => &mut id,
                "cwd" => &mut cwd,
                "port" => &mut port,
                "timeout-ms" => &mut timeout,
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
    // The timeout range is validated HERE, in the parser, before the lock, the manifest read, the bind
    // check or the spawn (#207 AC 2): 0 is already expired and anything past 7 days is not a limit a
    // driver means, so both are usage errors that leave nothing started, nothing written and no lock file.
    let timeout_ms = match &timeout {
        Some(v) => {
            let n = num_flag(v, "timeout-ms")?;
            if !(1..=604_800_000).contains(&n) {
                return Err("--timeout-ms must be an integer in 1..=604800000 (7 days)".to_string());
            }
            Some(n)
        }
        None => None,
    };
    Ok(StartArgs { ticket, id, cwd, port, timeout_ms, dir: dir.unwrap_or_else(|| DEFAULT_DIR.to_string()), program, prog_args: after[1..].to_vec() })
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
    // The limit line (#207): right after the process line and BEFORE the port line, under the same lock.
    // A failed limit write runs the same rollback as a failed port write (the remove below ends the limit
    // too, because the limit rides the entry it bounds).
    if let Some(tmo) = args.timeout_ms {
        if let Err(e) = manifest::apply(&dir, &args.ticket, &manifest::Op::Limit { id: &args.id, timeout_ms: tmo }, ts_ms) {
            let _ = manifest::apply(&dir, &args.ticket, &manifest::Op::Remove { id: &args.id }, now_ms());
            return refuse_after_spawn(&mut child, pid, e.to_string());
        }
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

/// The pre-stage probes of `check`'s expiry arm and `step`'s hard line (fix r2, N1): the SAME readers
/// the factored stop's own checks 3 and 4 use, gathered behind one door so the rule they serve is stated
/// once — an entry may be SIGNALLED for a limit only through a probe this run found CONCLUSIVE. In a
/// release build each wrapper is its plain probe, byte for byte; under `cfg(test)` a fault seam lets a
/// test make exactly one pid's pre-stage probe answer Unknown, to prove the inconclusive path refuses
/// (nothing staged, nothing signalled, the entry kept) instead of signalling an unstaged limit kill.
fn prestage_liveness(pid: u64) -> Liveness {
    #[cfg(test)]
    if prestage_fault::liveness_unknown(pid) {
        return Liveness::Unknown("the pre-stage liveness probe was made inconclusive (test seam)".to_string());
    }
    probe_alive_at(Path::new("/bin/kill"), pid)
}

fn prestage_identity(pid: u64, recorded: u64) -> Identity {
    #[cfg(test)]
    if prestage_fault::identity_unknown(pid) {
        return Identity::Unknown("the pre-stage identity read was made inconclusive (test seam)".to_string());
    }
    probe_identity(pid, recorded)
}

/// The `cfg(test)`-only fault injection behind the pre-stage probes (fix r2, N1): the pid whose
/// liveness / identity pre-stage probe answers Unknown. THREAD-LOCAL and keyed by pid: every test
/// runs on its own thread and drives the command functions on that same thread, so a fault is
/// invisible to the tests running in parallel (a process-wide seam made one test's `clear()` wipe
/// another test's mid-flight fault and its entries get SIGNALLED — a real measured flake), and it
/// cannot leak past the test's own thread. Nothing exists in a non-test build — no env var, no flag,
/// nothing in a release binary can trip it.
#[cfg(test)]
mod prestage_fault {
    use std::cell::Cell;

    thread_local! {
        static LIVENESS_PID: Cell<u64> = const { Cell::new(0) };
        static IDENTITY_PID: Cell<u64> = const { Cell::new(0) };
    }

    pub fn liveness_unknown_for(pid: u64) {
        LIVENESS_PID.with(|c| c.set(pid));
    }
    pub fn identity_unknown_for(pid: u64) {
        IDENTITY_PID.with(|c| c.set(pid));
    }
    pub fn clear() {
        LIVENESS_PID.with(|c| c.set(0));
        IDENTITY_PID.with(|c| c.set(0));
    }
    pub(super) fn liveness_unknown(pid: u64) -> bool {
        pid != 0 && LIVENESS_PID.with(|c| c.get()) == pid
    }
    pub(super) fn identity_unknown(pid: u64) -> bool {
        pid != 0 && IDENTITY_PID.with(|c| c.get()) == pid
    }
}

/// The pure pre-stage decision (fix r2, N1), ONE table shared by check's expiry arm and step's hard
/// line: may this entry be STAGED (proven live and still the recorded process — the one the limit is
/// about to signal), handed to the factored stop UNSTAGED (a CONCLUSIVE other answer: already-gone and
/// pid-reused record `died` through the stop's own arms, a denied pid is refused with nothing
/// signalled), or REFUSED outright (an INCONCLUSIVE probe: nothing staged, nothing signalled, the
/// entry kept — because the stop RE-PROBES, and its own later probe succeeding would signal a limit
/// kill with nothing staged, which the stopped arm would read as a deliberate stop and remove with NO
/// record)?
enum PreStage {
    Stage,
    Resolve,
    Refuse(String),
}

fn prestage_verdict(pid: u64, live: Liveness, identity: impl FnOnce() -> Identity) -> PreStage {
    match live {
        Liveness::Alive => match identity() {
            Identity::Same => PreStage::Stage,
            Identity::Reused => PreStage::Resolve,
            Identity::Unknown(why) => PreStage::Refuse(format!(
                "cannot prove pid {pid} is still the recorded process before staging its limit stop: {why}"
            )),
        },
        Liveness::NoSuch | Liveness::Denied => PreStage::Resolve,
        Liveness::Unknown(why) => PreStage::Refuse(format!("cannot prove pid {pid} is alive before staging its limit stop: {why}")),
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
    record: Option<RecordArgs>,
}

/// Parse `process stop`'s command line: the ticket plus exactly one of `--id` / `--pid`, an optional
/// `--grace-ms`, the usual `--dir`, and the all-or-none record flags (#210). Usage errors — including
/// any partial or bad record set — answer 2 before the lock, so they write nothing.
fn parse_stop(a: &[String]) -> Result<StopArgs, String> {
    let mut ticket: Option<String> = None;
    let mut id: Option<String> = None;
    let mut pid: Option<u64> = None;
    let mut grace: Option<u64> = None;
    let mut dir: Option<String> = None;
    let mut session: Option<String> = None;
    let mut checkpoint: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut log: Option<String> = None;
    let mut root: Option<String> = None;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("{} needs a value", arg))?;
            let twice = || format!("{} given twice", arg);
            match name {
                "id" => {
                    if id.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "pid" => {
                    let v = num_flag(value, "pid")?;
                    if pid.replace(v).is_some() {
                        return Err(twice());
                    }
                }
                "grace-ms" => {
                    let v = num_flag(value, "grace-ms")?;
                    if grace.replace(v).is_some() {
                        return Err(twice());
                    }
                }
                "dir" => {
                    if dir.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "session" => {
                    if session.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "checkpoint" => {
                    if checkpoint.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "repo" => {
                    if repo.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "log" => {
                    if log.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "root" => {
                    if root.replace(value.clone()).is_some() {
                        return Err(twice());
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
    // Record mode is all-or-none (decision 2): the three required flags come together, the optional
    // log/root only with them, and the shapes are checked HERE — a partial or bad set is a usage error
    // that leaves no lock file, no log and no deaths folder behind.
    let record = match (session, checkpoint, repo) {
        (None, None, None) => {
            if log.is_some() || root.is_some() {
                return Err("--log/--root come with --session, --checkpoint and --repo (record mode) together".to_string());
            }
            None
        }
        (Some(s), Some(c), Some(r)) => {
            check_record_shapes(&s, &c, &r)?;
            Some(RecordArgs { session_id: s, checkpoint: c, repo: r, log, root })
        }
        _ => return Err("record mode needs --session, --checkpoint and --repo together".to_string()),
    };
    Ok(StopArgs {
        ticket,
        id,
        pid,
        grace_ms: grace.unwrap_or(DEFAULT_GRACE_MS),
        dir: dir.unwrap_or_else(|| DEFAULT_DIR.to_string()),
        record,
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

// ---------------------------------------------------------------- record mode (#210: the worker death record)

/// The parsed record flags of `check`/`stop` (decision 2): opt-in per call, ALL-OR-NONE — the three
/// required flags come together or not at all — and shape-checked in the parser with the event table's
/// own rules, BEFORE the lock, so a partial or bad set writes nothing at all (no lock file either).
struct RecordArgs {
    session_id: String,
    checkpoint: String,
    repo: String,
    log: Option<String>,
    root: Option<String>,
}

/// Shape-check the record flags (decision 2): a full lowercase uuid, a 40/64 lowercase hex checkpoint, a
/// plain-token repo — the `timed_out` table's own field rules, restated for the parser.
fn check_record_shapes(session: &str, checkpoint: &str, repo: &str) -> Result<(), String> {
    if !death::uuid_ok(session) {
        return Err("--session must be a full lowercase uuid (8-4-4-4-12 hex; a short id is refused)".to_string());
    }
    if !death::sha_ok(checkpoint) {
        return Err("--checkpoint must be 40 or 64 lowercase hex digits".to_string());
    }
    if !crate::events::token_ok(repo) {
        return Err("--repo must be a plain token (lowercase letters, digits, '-', '_', '.')".to_string());
    }
    Ok(())
}

/// The resolved log/root pair of a record-mode call, following the `events append` defaults:
/// `.harness/events.jsonl` with root `.` when neither is given, an explicit log's parent as the root
/// when only the log is given. `log_is_default` drives the create-only-inside-the-root folder rule.
struct LogRoot {
    log_is_default: bool,
    log: PathBuf,
    root: PathBuf,
}

fn resolve_log_root(log: Option<&str>, root: Option<&str>) -> LogRoot {
    let log_is_default = log.is_none();
    let log = PathBuf::from(log.unwrap_or(".harness/events.jsonl"));
    let root = match root {
        Some(r) => PathBuf::from(r),
        None => match log.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => PathBuf::from("."),
        },
    };
    LogRoot { log_is_default, log, root }
}

/// The default log folder is created only after the working folder is proven inside the root (a refusal
/// leaves no empty folder behind), the same rule `events append` and `step` use. An EXPLICIT log is
/// never pre-created: a missing parent is the path check's own refusal.
fn ensure_default_log_folder(lr: &LogRoot) -> Result<(), String> {
    if !lr.log_is_default {
        return Ok(());
    }
    if let Some(folder) = lr.log.parent() {
        if !folder.exists() {
            let inside = match (std::path::Path::new(".").canonicalize(), lr.root.canonicalize()) {
                (Ok(cwd), Ok(r)) => cwd.starts_with(r),
                _ => false,
            };
            if !inside {
                return Err("log path refused: the working folder is outside the root".to_string());
            }
            if let Err(e) = std::fs::create_dir_all(folder) {
                return Err(format!("cannot create the default log folder: {}", e.kind()));
            }
        }
    }
    Ok(())
}

/// The record-mode run context: the caller-supplied continuation values (trusted like `--steps` is — a
/// lying driver is not caught, an honest limit), the log/root the `timed_out` events go to, and the step
/// context the handover carries (0 outside `step`).
struct RecordRun<'a> {
    repo: &'a str,
    checkpoint: &'a str,
    session_id: &'a str,
    lr: &'a LogRoot,
    steps: u64,
    hard_limit: u64,
}

/// The ts_ms of the entry's own add line (the LAST `add` process line of this id — the live one), for
/// the pure clock rule: a wall clock that reads before it is a detectable backward jump.
fn add_ts_of(read: &ManifestRead, id: &str) -> u64 {
    read.lines.iter().rev().find_map(|l| match l {
        manifest::Line::AddProcess { id: i, ts_ms, .. } if i == id => Some(*ts_ms),
        _ => None,
    }).unwrap_or(0)
}

/// The limit fields the handover carries for an entry: its recorded time limit, or none.
fn limit_of(read: &ManifestRead, id: &str) -> (death::LimitKind, u64) {
    match read.live.iter().find(|e| e.id == id).and_then(|e| e.limit.as_ref()) {
        Some(l) => (death::LimitKind::Time, l.timeout_ms),
        None => (death::LimitKind::None, 0),
    }
}

/// The identity of one process life on the sheet (decision 3's key), for the record step.
struct Life<'a> {
    ticket: &'a str,
    id: &'a str,
    pid: u64,
    pid_start_ms: u64,
}

/// The death half `stage_death` needs beyond the run and the entry: the fresh cause and the limit
/// fields (decision 1/4). A struct so the stage call stays under the argument cap.
struct FreshDeath {
    cause: death::Cause,
    limit: death::LimitKind,
    timeout_ms: u64,
}

/// Stage one death (decision 6 a+b): the clock rule, the log pre-flight, then the handover on disk —
/// for a timeout, all BEFORE any signal. Answers the staged handover (its cause is the EFFECTIVE one:
/// an existing handover of the same life wins, decision 7).
fn stage_death(dir: &Path, read: &ManifestRead, life: &Life<'_>, run: &RecordRun<'_>, fresh: FreshDeath) -> Result<death::Staged, String> {
    ensure_default_log_folder(run.lr).map_err(|e| format!("event log refused: {e}"))?;
    let spec = death::DeathSpec {
        ticket: life.ticket,
        id: life.id,
        pid: life.pid,
        pid_start_ms: life.pid_start_ms,
        cause: fresh.cause,
        limit: fresh.limit,
        timeout_ms: fresh.timeout_ms,
        steps: run.steps,
        hard_limit: run.hard_limit,
        repo: run.repo,
        checkpoint: run.checkpoint,
        session_id: run.session_id,
    };
    death::stage(dir, &spec, &run.lr.log, &run.lr.root, now_ms(), add_ts_of(read, life.id))
}

/// Record a death found WITHOUT a signal (already-gone, pid-reused — cause `died`, decision 1): stage
/// (reusing an existing handover of the same life, whose cause then wins) and commit, BEFORE the caller
/// removes the entry. Answers the `recorded` stdout line for the caller to print after its own line.
fn record_unsignalled_death(dir: &Path, read: &ManifestRead, life: &Life<'_>, run: &RecordRun<'_>) -> Result<String, String> {
    let (limit, timeout_ms) = limit_of(read, life.id);
    let staged = stage_death(dir, read, life, run, FreshDeath { cause: death::Cause::Died, limit, timeout_ms })?;
    let spec = death::DeathSpec {
        ticket: life.ticket,
        id: life.id,
        pid: life.pid,
        pid_start_ms: life.pid_start_ms,
        cause: staged.cause,
        limit,
        timeout_ms,
        steps: run.steps,
        hard_limit: run.hard_limit,
        repo: run.repo,
        checkpoint: run.checkpoint,
        session_id: run.session_id,
    };
    death::commit(&spec, &staged, &run.lr.log, &run.lr.root, now_ms())?;
    Ok(format!("recorded\t{}\t{}\t{}", life.id, staged.cause.as_str(), staged.relpath))
}

/// The record-failure stderr (decision 8): the reason, then the fixed second line. The entry is KEPT
/// (the caller does not remove it), so the death is retried, never lost.
fn death_not_recorded(id: &str, reason: &str) {
    eprintln!("harness: error: {id}: {reason}");
    eprintln!("harness: DEATH NOT RECORDED");
}

/// What record mode asks of the factored stop (fix r2, N1). A DELIBERATE stop (the stop verb) records
/// nothing on the stopped arm — it was asked for. A LIMIT stop (check's expiry, step's hard line, and
/// check's vanished-entry resolution) must arrive with the entry's handover already STAGED: a limit
/// stop that reaches the stopped arm with nothing staged refuses with `DEATH NOT RECORDED` and keeps
/// the entry (rc 2), because the stopped arm otherwise reads "nothing staged" as a deliberate stop and
/// would remove the entry with NO record — the one outcome this ticket promises cannot happen.
enum RecordStop<'a> {
    Deliberate(&'a RecordRun<'a>),
    Limit { run: &'a RecordRun<'a>, staged: Option<death::Staged> },
}

impl RecordStop<'_> {
    /// The run both variants carry (the ends-without-signal arms record through it).
    fn run(&self) -> &RecordRun<'_> {
        match self {
            RecordStop::Deliberate(run) | RecordStop::Limit { run, .. } => run,
        }
    }
}

/// The outcome of stopping ONE live process entry through checks 2-7 (#207: the factored stop body —
/// `stop`, `check` and `step` all call it under the per-ticket lock the CALLER holds). Every stderr
/// message prints HERE, byte-identical to the old inline stop; the stdout line is the CALLER's (stop
/// prints `stopped`, check prints `timed_out`, step prints `stopped`).
enum StopDone {
    /// Stopped and removed; carries the pid for the caller's stdout line and, in record mode, the
    /// `recorded` line for the caller to print AFTER its own (decision 9's order).
    Stopped { pid: u64, recorded: Option<String> },
    /// already-gone (3): nothing signalled, the entries removed.
    Gone { recorded: Option<String> },
    /// pid-reused (3): nothing signalled, the entries removed.
    Reused { recorded: Option<String> },
    /// signalled, death not confirmed (4): the entry kept so a later stop can retry (and, in record
    /// mode, the death is NOT recorded yet — a later run records it, decision 1).
    NotConfirmed,
    /// a refusal (2): nothing signalled; the caller prints the message with `fail`.
    Refused(String),
    /// the death record could not be written (#210, decision 8): the two stderr lines have already
    /// printed; the entry is KEPT so a later record-mode run retries. The caller answers 2.
    DeathNotRecorded,
}

/// Check 3's already-gone tail (#207 r1 F1, factored so `check`'s liveness-first pass resolves a gone
/// pid through the SAME path stop does): stop's stderr line verbatim, the entries removed. Err names the
/// manifest refusal (the file is unchanged on Err).
fn already_gone(dir: &Path, ticket: &str, id: &str, pid: u64, port_live: bool) -> Result<(), String> {
    remove_stopped(dir, ticket, id, port_live)?;
    eprintln!("harness: {id} is already-gone: pid {pid} is provably no such process; nothing signalled, entries removed");
    Ok(())
}

/// Checks 2b-7 for the live process entry `id` of `ticket`: the entry must be a LIVE `process` entry of
/// this ticket, then liveness, identity, leadership, TERM, the KILL gate and the group-and-leader death
/// proof (the full order is the module header). `read` is the clean manifest the caller read under its own
/// lock; the `remove` lines write through `dir`. A stop signal goes ONLY when every check passes.
///
/// `record` is the record-mode ask (#210, decision 6, fix r2 N1): every REMOVING arm records the death
/// first — the arms that end without a signal (already-gone, pid-reused) stage and commit `died`; the
/// stopped arm commits the handover the CALLER staged before the signal (a limit stop), records nothing
/// for a DELIBERATE stop, and REFUSES (`DeathNotRecorded`, entry kept) for a limit stop that arrives
/// unstaged. A failed record keeps the entry (the caller answers 2). The `recorded` stdout line rides
/// the variant so the caller prints it after its own line.
fn stop_live_entry(
    dir: &Path,
    ticket: &str,
    read: &ManifestRead,
    id: &str,
    grace_ms: u64,
    record: Option<RecordStop<'_>>,
) -> StopDone {
    let Some(entry) = read.live.iter().find(|e| e.id == id) else {
        return StopDone::Refused(format!("id {id} is not a live entry of ticket {ticket}"));
    };
    let Target::Process { pid, pid_start_ms: recorded } = &entry.target else {
        return StopDone::Refused(format!("id {id} is not a live process entry of ticket {ticket}"));
    };
    let (pid, recorded) = (*pid, *recorded);
    let port_live = read.live.iter().any(|e| e.id == format!("{id}.port"));
    // The record step rides every removing arm below (#210): `recorded_line` is the caller's stdout
    // line, printed only when the death reached the log.
    let mut recorded_line: Option<String> = None;
    // Check 3: liveness through the absolute-path helper. Provably no such process -> already-gone (3,
    // entries removed); a helper that cannot run, or an answer that proves nothing -> unknown -> 2, kept.
    match probe_alive_at(Path::new("/bin/kill"), pid) {
        Liveness::Alive => {}
        Liveness::NoSuch => {
            if let Some(rec) = &record {
                match record_unsignalled_death(dir, read, &Life { ticket, id, pid, pid_start_ms: recorded }, rec.run()) {
                    Ok(line) => recorded_line = Some(line),
                    Err(reason) => {
                        death_not_recorded(id, &reason);
                        return StopDone::DeathNotRecorded;
                    }
                }
            }
            return match already_gone(dir, ticket, id, pid, port_live) {
                Ok(()) => StopDone::Gone { recorded: recorded_line },
                Err(e) => StopDone::Refused(e),
            };
        }
        Liveness::Denied => {
            return StopDone::Refused(format!("pid {pid} answers the liveness probe but is not ours to signal (kill reports no permission); nothing signalled"));
        }
        Liveness::Unknown(e) => return StopDone::Refused(e),
    }
    // Check 4: identity — the live pid's start time must equal the recorded one, else the number was
    // re-used: nothing signalled, the stale entries removed (the recorded process must have exited).
    match probe_identity(pid, recorded) {
        Identity::Same => {}
        Identity::Reused => {
            if let Some(rec) = &record {
                match record_unsignalled_death(dir, read, &Life { ticket, id, pid, pid_start_ms: recorded }, rec.run()) {
                    Ok(line) => recorded_line = Some(line),
                    Err(reason) => {
                        death_not_recorded(id, &reason);
                        return StopDone::DeathNotRecorded;
                    }
                }
            }
            return match remove_stopped(dir, ticket, id, port_live) {
                Ok(()) => {
                    eprintln!("harness: {id} pid-reused: pid {pid} no longer starts at the time the sheet records; nothing signalled, entries removed");
                    StopDone::Reused { recorded: recorded_line }
                }
                Err(e) => StopDone::Refused(e),
            };
        }
        Identity::Unknown(e) => return StopDone::Refused(format!("cannot read the start time of pid {pid}: {e}")),
    }
    // Check 5: group leadership — only a leader's own group provably ends with a signal to -<pid>.
    match probe_leader(pid) {
        Leadership::Leader => {}
        Leadership::NotLeader(pgid) => {
            return StopDone::Refused(format!(
                "pid {pid} is not its own process-group leader (pgid {pgid}): a group signal could not prove it ends this process; nothing signalled"
            ));
        }
        Leadership::Unknown(e) => return StopDone::Refused(format!("cannot prove pid {pid} leads its process group: {e}")),
    }
    // Check 6: TERM to the whole group, then a bounded wait. A helper that cannot run refuses with
    // nothing signalled; whatever happened, the post-signal proof (check 7) decides the answer.
    if let Ran::NotRun(e) = kill_helper(pid, "TERM") {
        return StopDone::Refused(e);
    }
    if !wait_group_empty(pid, grace_ms) {
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
        // Decision 6c: for a death the caller STAGED before the signal (a limit stop), the `timed_out`
        // event is appended BEFORE the remove lines; a deliberate stop (the stop verb) records nothing.
        // Fix r2 (N1, the second line): a LIMIT stop that arrives with nothing staged can never be read
        // as a deliberate stop — it refuses with `DEATH NOT RECORDED` and keeps the entry, so no limit
        // kill is ever removed from the sheet without a committed death record.
        match &record {
            Some(RecordStop::Limit { run, staged: Some(staged) }) => {
                let spec = death::DeathSpec {
                    ticket,
                    id,
                    pid,
                    pid_start_ms: recorded,
                    cause: staged.cause,
                    limit: limit_of(read, id).0,
                    timeout_ms: limit_of(read, id).1,
                    steps: run.steps,
                    hard_limit: run.hard_limit,
                    repo: run.repo,
                    checkpoint: run.checkpoint,
                    session_id: run.session_id,
                };
                if let Err(reason) = death::commit(&spec, staged, &run.lr.log, &run.lr.root, now_ms()) {
                    death_not_recorded(id, &reason);
                    return StopDone::DeathNotRecorded;
                }
                recorded_line = Some(format!("recorded\t{id}\t{}\t{}", staged.cause.as_str(), staged.relpath));
            }
            Some(RecordStop::Limit { staged: None, .. }) => {
                death_not_recorded(id, "a limit stop reached the stopped arm with no staged handover");
                return StopDone::DeathNotRecorded;
            }
            Some(RecordStop::Deliberate(_)) | None => {}
        }
        return match remove_stopped(dir, ticket, id, port_live) {
            Ok(()) => StopDone::Stopped { pid, recorded: recorded_line },
            Err(e) => {
                eprintln!("harness: {id} signalled, death not confirmed: the removal failed: {e}; entry kept so a later stop can retry");
                StopDone::NotConfirmed
            }
        };
    }
    eprintln!(
        "harness: {id} signalled, death not confirmed: pid {pid} or its group still answers the liveness probe, or the probe could not run; entry kept so a later stop can retry"
    );
    StopDone::NotConfirmed
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
    if !matches!(target, Target::Process { .. }) {
        return fail(&format!("id {id} is not a live process entry of ticket {}", args.ticket));
    }
    // Record mode (#210, decision 1): a DELIBERATE stop is not a death and records nothing — the run is
    // passed only so the ends-without-signal arms (already-gone, pid-reused) record their `died`.
    let rec_lr = args.record.as_ref().map(|rc| (rc, resolve_log_root(rc.log.as_deref(), rc.root.as_deref())));
    let rec_run = rec_lr.as_ref().map(|(rc, lr)| RecordRun { repo: &rc.repo, checkpoint: &rc.checkpoint, session_id: &rc.session_id, lr, steps: 0, hard_limit: 0 });
    // Checks 2b-7 run in the factored body (#207) under THIS command's lock, so `check` and `step` can
    // stop an entry under a lock they already hold; stop's own behaviour and messages are unchanged.
    match stop_live_entry(&dir, &args.ticket, &read, &id, args.grace_ms, rec_run.as_ref().map(RecordStop::Deliberate)) {
        StopDone::Stopped { pid, recorded } => {
            println!("{id}\t{pid}\tstopped");
            if let Some(line) = recorded {
                println!("{line}");
            }
            0
        }
        StopDone::Gone { recorded } | StopDone::Reused { recorded } => {
            if let Some(line) = recorded {
                println!("{line}");
            }
            3
        }
        StopDone::NotConfirmed => 4,
        StopDone::Refused(m) => fail(&m),
        StopDone::DeathNotRecorded => 2,
    }
}

// ---------------------------------------------------------------- check (#207: one-shot timeout enforcement)

/// The pure expiry decision (#207 decision 4). Fail closed: an age that cannot be READ, or a wall clock
/// that reads BEFORE the `limit` line's own `ts_ms` (a detectable backward jump), is `Refuse` — nothing
/// signalled, the entry kept; check never answers "fine" when it does not know. The CALLER proves the pid
/// live before calling (r1 F1: the age of a gone pid is unreadable, and a gone pid resolves as
/// already-gone instead, so this refusal is only ever about a live process). Expired means
/// `age >= timeout_ms`; the age is the OS's own age of the process (boot-relative on Linux), so restarting
/// the harness cannot reset the timer and a wall jump changes WHEN a process stops, never WHOM.
enum Expiry {
    Within,
    Expired,
    Refuse(String),
}

fn expiry_decision(pid: u64, age: Result<u64, String>, timeout_ms: u64, limit_ts_ms: u64, now: u64) -> Expiry {
    if now < limit_ts_ms {
        return Expiry::Refuse("the clock reads before the recorded limit; nothing signalled".to_string());
    }
    match age {
        Err(why) => Expiry::Refuse(format!("cannot read the age of pid {pid}: {why}; nothing signalled")),
        Ok(a) if a >= timeout_ms => Expiry::Expired,
        Ok(_) => Expiry::Within,
    }
}

/// Fold the per-entry answers of one check run: precedence `2 > 4 > 5 > 0` (decision 7) — every entry is
/// still visited, so a refusal on one entry never hides an expiry on another.
fn check_fold(codes: &[i32]) -> i32 {
    if codes.contains(&2) {
        2
    } else if codes.contains(&4) {
        4
    } else if codes.contains(&5) {
        5
    } else {
        0
    }
}

struct CheckArgs {
    ticket: String,
    grace_ms: u64,
    dir: String,
    record: Option<RecordArgs>,
}

/// Parse `process check`'s command line: the ticket, an optional `--grace-ms`, the usual `--dir`, and
/// the all-or-none record flags (#210). Usage errors — including any partial or bad record set — answer
/// 2 before the lock, so they write nothing.
fn parse_check(a: &[String]) -> Result<CheckArgs, String> {
    let mut ticket: Option<String> = None;
    let mut grace: Option<u64> = None;
    let mut dir: Option<String> = None;
    let mut session: Option<String> = None;
    let mut checkpoint: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut log: Option<String> = None;
    let mut root: Option<String> = None;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("{} needs a value", arg))?;
            let twice = || format!("{} given twice", arg);
            match name {
                "grace-ms" => {
                    let v = num_flag(value, "grace-ms")?;
                    if grace.replace(v).is_some() {
                        return Err(twice());
                    }
                }
                "dir" => {
                    if dir.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "session" => {
                    if session.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "checkpoint" => {
                    if checkpoint.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "repo" => {
                    if repo.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "log" => {
                    if log.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "root" => {
                    if root.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                _ => return Err(format!("unknown option {}", arg)),
            }
        } else if ticket.replace(arg.clone()).is_some() {
            return Err("more than one ticket given".to_string());
        }
    }
    let ticket = ticket.ok_or_else(|| "a ticket is required".to_string())?;
    // Record mode is all-or-none (decision 2), shape-checked HERE, before the lock.
    let record = match (session, checkpoint, repo) {
        (None, None, None) => {
            if log.is_some() || root.is_some() {
                return Err("--log/--root come with --session, --checkpoint and --repo (record mode) together".to_string());
            }
            None
        }
        (Some(s), Some(c), Some(r)) => {
            check_record_shapes(&s, &c, &r)?;
            Some(RecordArgs { session_id: s, checkpoint: c, repo: r, log, root })
        }
        _ => return Err("record mode needs --session, --checkpoint and --repo together".to_string()),
    };
    Ok(CheckArgs { ticket, grace_ms: grace.unwrap_or(DEFAULT_GRACE_MS), dir: dir.unwrap_or_else(|| DEFAULT_DIR.to_string()), record })
}

/// `harness process check <ticket>` (#207): one-shot, NO daemon — whoever drives the ticket runs it before
/// every step and at finalize, so the bound is "stopped at the first check at or after the deadline", never
/// "stopped at the deadline". Every live process entry WITH a limit is visited under the per-ticket lock;
/// LIVENESS comes first (r1 F1: a gone pid resolves through the already-gone tail stop uses, never a
/// refusal, because its age cannot be read either), then the age is read only for a proven-live pid and an
/// expired one is stopped through the factored stop checks 2-7 (a clock jump can change WHEN a process
/// is stopped, never WHOM). Exit codes: `0` nothing expired; `5` at least one visited entry resolved (an
/// expired one stopped, or any visited one already-gone or pid-reused); `4` an expired one was signalled
/// but its death was not confirmed (entry kept); `2` a refusal (usage, unclean manifest, unreadable age of
/// a LIVE pid, a clock before the limit line, a pid that is not ours to signal, a probe that cannot run)
/// — precedence `2 > 4 > 5 > 0`, every entry still visited.
fn check_cmd(a: &[String]) -> i32 {
    let args = match parse_check(a) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    if !crate::events::token_ok(&args.ticket) {
        return fail("ticket must be a plain token (lowercase letters, digits, '-', '_', '.', not empty, not '.' or '..')");
    }
    let dir = PathBuf::from(&args.dir);
    // The supervisor lock is held for the whole check (like the whole stop), so a check racing a stop on
    // one entry cannot both pass the live-entry check: the loser re-reads the manifest after the winner's
    // removes and simply does not see the entry.
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
    // Record mode (#210, decision 5): the log/root pair the `timed_out` events go to, resolved once;
    // the run is built per entry only when a death is actually found, so a no-death run touches no log.
    let rec = args.record.as_ref().map(|rc| (rc, resolve_log_root(rc.log.as_deref(), rc.root.as_deref())));
    let mut codes: Vec<i32> = Vec::new();
    for e in &read.live {
        let Target::Process { pid, pid_start_ms: recorded } = &e.target else { continue };
        // Record mode widens the visit to EVERY live process entry (decision 5): an entry with no limit
        // is probed for liveness only, so a program that vanished is found at the driver's next check.
        // Legacy mode visits only entries WITH a limit — today's behaviour, byte-identical.
        if rec.is_none() && e.limit.is_none() {
            continue;
        }
        let port_live = read.live.iter().any(|x| x.id == format!("{}.port", e.id));
        // Liveness FIRST (#207 r1 F1): the age of a GONE pid cannot be read either (ps does not know it,
        // /proc/<pid>/stat is gone), and a program that finished on its own is the already-gone path —
        // resolved through the same tail stop uses — never a refusal. The age is read only for a
        // proven-LIVE pid, so "cannot read the age" is a refusal about a live process, and a pid that
        // answers the probe but is not ours, or a probe that cannot run, is a fail-closed refusal.
        // The probe runs through `prestage_liveness` (fix r2 N1): in record mode this IS the pre-stage
        // liveness probe, and an inconclusive answer must refuse before anything is staged or signalled.
        match prestage_liveness(*pid) {
            Liveness::NoSuch => {
                // Record mode: the vanished program is a death (`died`, decision 1), recorded BEFORE the
                // remove through the same factored body (its already-gone arm carries the record step).
                // Legacy: exactly today's resolution.
                if let Some((rc, lr)) = &rec {
                    let run = RecordRun { repo: &rc.repo, checkpoint: &rc.checkpoint, session_id: &rc.session_id, lr, steps: 0, hard_limit: 0 };
                    match stop_live_entry(&dir, &args.ticket, &read, &e.id, args.grace_ms, Some(RecordStop::Limit { run: &run, staged: None })) {
                        // stop's already-gone stderr line has already printed, verbatim; resolved.
                        StopDone::Gone { recorded } | StopDone::Reused { recorded } => {
                            if let Some(line) = recorded {
                                println!("{line}");
                            }
                            codes.push(5);
                        }
                        StopDone::DeathNotRecorded => codes.push(2),
                        StopDone::NotConfirmed => codes.push(4),
                        StopDone::Refused(m) => {
                            fail(&m);
                            codes.push(2);
                        }
                        StopDone::Stopped { .. } => codes.push(5),
                    }
                } else {
                    match already_gone(&dir, &args.ticket, &e.id, *pid, port_live) {
                        // stop's already-gone stderr line has already printed, verbatim; resolved.
                        Ok(()) => codes.push(5),
                        Err(m) => {
                            fail(&m);
                            codes.push(2);
                        }
                    }
                }
            }
            Liveness::Denied => {
                fail(&format!(
                    "{}: pid {} answers the liveness probe but is not ours to signal (kill reports no permission); nothing signalled",
                    e.id, pid
                ));
                codes.push(2);
            }
            Liveness::Unknown(why) => {
                fail(&format!("{}: cannot prove pid {} is alive: {}; nothing signalled", e.id, pid, why));
                codes.push(2);
            }
            Liveness::Alive => {
                // Probed only in record mode (no limit): alive and unbounded is not a death.
                let Some(limit) = &e.limit else { continue };
                match expiry_decision(*pid, manifest::process_age_ms(*pid), limit.timeout_ms, limit.ts_ms, now_ms()) {
                Expiry::Within => {}
                Expiry::Expired => {
                    // Record mode (decision 6): the handover is on disk BEFORE any signal; a failed
                    // stage refuses with NOTHING signalled (the entry is kept, so a later run records
                    // the death — it is never silently lost).
                    let mut staged = None;
                    if let Some((rc, lr)) = &rec {
                        // Fix r1 (the F1 class): a `timeout` handover is staged ONLY for a pid this run
                        // has just identified as the recorded process. A live pid whose number was
                        // re-used is a `died` the factored stop's own pid-reused arm records; staging
                        // `timeout` here would put a cause the very next probe contradicts on disk, and
                        // decision 7 makes that cause permanent.
                        // Fix r2 (N1): the pre-stage probe must be CONCLUSIVE. An UNREADABLE identity
                        // is not "the stop will refuse on it too" — the stop RE-PROBES, and when its own
                        // later probe succeeds it signals a limit kill with nothing staged, and the
                        // stopped arm reads "nothing staged" as a deliberate stop: the entry would be
                        // removed with NO record and NO event. An inconclusive probe refuses HERE —
                        // rc 2, nothing signalled, the entry kept, the cause named — and a mislabelled
                        // intent file is never left behind either.
                        // The liveness answer is already conclusive here (Alive — the loop's own probe
                        // routed the entry to this arm), so the table sees Alive and decides on the
                        // identity probe alone.
                        match prestage_verdict(*pid, Liveness::Alive, || prestage_identity(*pid, *recorded)) {
                            PreStage::Stage => {
                                let run = RecordRun { repo: &rc.repo, checkpoint: &rc.checkpoint, session_id: &rc.session_id, lr, steps: 0, hard_limit: 0 };
                                match stage_death(&dir, &read, &Life { ticket: &args.ticket, id: &e.id, pid: *pid, pid_start_ms: *recorded }, &run, FreshDeath { cause: death::Cause::Timeout, limit: death::LimitKind::Time, timeout_ms: limit.timeout_ms }) {
                                    Ok(st) => staged = Some(st),
                                    Err(reason) => {
                                        death_not_recorded(&e.id, &reason);
                                        codes.push(2);
                                        continue;
                                    }
                                }
                            }
                            // Conclusive: the pid was re-used. Nothing is staged; the factored stop's
                            // own pid-reused arm stages and commits `died` (limit from the entry).
                            PreStage::Resolve => {}
                            PreStage::Refuse(why) => {
                                fail(&format!("{}: {}; nothing signalled", e.id, why));
                                codes.push(2);
                                continue;
                            }
                        }
                    }
                    let rec_pair = rec.as_ref().map(|(rc, lr)| {
                        (RecordRun { repo: &rc.repo, checkpoint: &rc.checkpoint, session_id: &rc.session_id, lr, steps: 0, hard_limit: 0 }, staged.clone())
                    });
                    match stop_live_entry(&dir, &args.ticket, &read, &e.id, args.grace_ms, rec_pair.as_ref().map(|(run, st)| RecordStop::Limit { run, staged: st.clone() })) {
                        StopDone::Stopped { pid: p, recorded } => {
                            println!("{}\t{}\ttimed_out", e.id, p);
                            if let Some(line) = recorded {
                                println!("{line}");
                            }
                            codes.push(5);
                        }
                        // already-gone / pid-reused: stop's own stderr line has already printed, verbatim; resolved.
                        StopDone::Gone { recorded } | StopDone::Reused { recorded } => {
                            if let Some(line) = recorded {
                                println!("{line}");
                            }
                            codes.push(5);
                        }
                        StopDone::NotConfirmed => codes.push(4),
                        StopDone::Refused(m) => {
                            fail(&m);
                            codes.push(2);
                        }
                        StopDone::DeathNotRecorded => codes.push(2),
                    }
                }
                Expiry::Refuse(m) => {
                    fail(&format!("{}: {}", e.id, m));
                    codes.push(2);
                }
            }
            }
        }
    }
    check_fold(&codes)
}

// ---------------------------------------------------------------- step (#207: soft and hard step limits)

/// The pure step decision (#207 decision 6): at or past the hard line, at or past the soft line, or below
/// both. Both compares are `>=`: at the line IS the line (the driver contract counts the current step).
enum StepClass {
    Continue,
    Soft,
    Hard,
}

fn step_class(steps: u64, soft: u64, hard: u64) -> StepClass {
    if steps >= hard {
        StepClass::Hard
    } else if steps >= soft {
        StepClass::Soft
    } else {
        StepClass::Continue
    }
}

/// The pure outcome fold of a hard stop (#207 AC 12): any refusal (2) is a refusal, any unconfirmed death
/// (4) is 4, otherwise every entry answered stopped or already-gone/pid-reused (or there were none) and
/// the answer is 6.
fn fold_stops(answers: &[i32]) -> i32 {
    if answers.contains(&2) {
        2
    } else if answers.contains(&4) {
        4
    } else {
        6
    }
}

/// The hard-limit body (#207 decision 6): the event FIRST — a hard stop is never silent — then one stop
/// per live process entry, through the factored stop checks. `write_event` and `stop_one` are closures so
/// the ORDER (the event on disk before any signal) is unit-testable. Answers step's exit code; a failed
/// event write answers 2 with `harness: EVENT NOT RECORDED` and NOTHING signalled.
fn hard_limit_body(
    steps: u64,
    hard: u64,
    write_event: impl FnOnce() -> Result<(), String>,
    ids: &[String],
    mut stop_one: impl FnMut(&str) -> i32,
) -> i32 {
    if let Err(e) = write_event() {
        eprintln!("harness: error: {}", e);
        eprintln!("harness: EVENT NOT RECORDED");
        return 2;
    }
    println!("step_limit_stop\t{steps}\t{hard}");
    let answers: Vec<i32> = ids.iter().map(|id| stop_one(id)).collect();
    fold_stops(&answers)
}

/// The largest `--soft`/`--hard` step limit (a driver that means more than a million steps is not using limits).
const STEP_LIMIT_CAP: u64 = 1_000_000;

struct StepArgs {
    ticket: String,
    repo: String,
    steps: u64,
    soft: u64,
    hard: u64,
    grace_ms: u64,
    dir: String,
    log: Option<String>,
    root: Option<String>,
    /// The record pair (#210, decision 2): `--session` and `--checkpoint`, all-or-none, shape-checked
    /// in the parser (step already owns `--repo`/`--log`/`--root`).
    record: Option<(String, String)>,
}

/// Parse `process step`'s command line. Every input is validated HERE, before the lock, the manifest read
/// or any event write, so a usage error leaves the log and the manifest untouched: `1 <= soft < hard <=
/// 1000000`, numeric `--steps`, and a plain-token `--repo` (the event carries it verbatim).
fn parse_step(a: &[String]) -> Result<StepArgs, String> {
    let mut ticket: Option<String> = None;
    let mut repo: Option<String> = None;
    let mut steps: Option<u64> = None;
    let mut soft: Option<u64> = None;
    let mut hard: Option<u64> = None;
    let mut grace: Option<u64> = None;
    let mut dir: Option<String> = None;
    let mut log: Option<String> = None;
    let mut root: Option<String> = None;
    let mut session: Option<String> = None;
    let mut checkpoint: Option<String> = None;
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        if let Some(name) = arg.strip_prefix("--") {
            let value = it.next().ok_or_else(|| format!("{} needs a value", arg))?;
            let twice = || format!("{} given twice", arg);
            match name {
                "session" => {
                    if session.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "checkpoint" => {
                    if checkpoint.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "repo" => {
                    if repo.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "steps" => {
                    let v = num_flag(value, "steps")?;
                    if steps.replace(v).is_some() {
                        return Err(twice());
                    }
                }
                "soft" => {
                    let v = num_flag(value, "soft")?;
                    if soft.replace(v).is_some() {
                        return Err(twice());
                    }
                }
                "hard" => {
                    let v = num_flag(value, "hard")?;
                    if hard.replace(v).is_some() {
                        return Err(twice());
                    }
                }
                "grace-ms" => {
                    let v = num_flag(value, "grace-ms")?;
                    if grace.replace(v).is_some() {
                        return Err(twice());
                    }
                }
                "dir" => {
                    if dir.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "log" => {
                    if log.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                "root" => {
                    if root.replace(value.clone()).is_some() {
                        return Err(twice());
                    }
                }
                _ => return Err(format!("unknown option {}", arg)),
            }
        } else if ticket.replace(arg.clone()).is_some() {
            return Err("more than one ticket given".to_string());
        }
    }
    let ticket = ticket.ok_or_else(|| "a ticket is required".to_string())?;
    let repo = repo.ok_or_else(|| "--repo is required".to_string())?;
    let steps = steps.ok_or_else(|| "--steps is required".to_string())?;
    let soft = soft.ok_or_else(|| "--soft is required".to_string())?;
    let hard = hard.ok_or_else(|| "--hard is required".to_string())?;
    if !crate::events::token_ok(&repo) {
        return Err("--repo must be a plain token (lowercase letters, digits, '-', '_', '.')".to_string());
    }
    if !(1..=STEP_LIMIT_CAP).contains(&soft) {
        return Err(format!("--soft must be an integer in 1..={STEP_LIMIT_CAP}"));
    }
    if !(1..=STEP_LIMIT_CAP).contains(&hard) {
        return Err(format!("--hard must be an integer in 1..={STEP_LIMIT_CAP}"));
    }
    if soft >= hard {
        return Err("--soft must be less than --hard".to_string());
    }
    // The record pair is all-or-none (decision 2), shape-checked HERE, before the lock.
    let record = match (session, checkpoint) {
        (None, None) => None,
        (Some(s), Some(c)) => {
            if !death::uuid_ok(&s) {
                return Err("--session must be a full lowercase uuid (8-4-4-4-12 hex; a short id is refused)".to_string());
            }
            if !death::sha_ok(&c) {
                return Err("--checkpoint must be 40 or 64 lowercase hex digits".to_string());
            }
            Some((s, c))
        }
        _ => return Err("record mode needs --session and --checkpoint together".to_string()),
    };
    Ok(StepArgs {
        ticket,
        repo,
        steps,
        soft,
        hard,
        grace_ms: grace.unwrap_or(DEFAULT_GRACE_MS),
        dir: dir.unwrap_or_else(|| DEFAULT_DIR.to_string()),
        log,
        root,
        record,
    })
}

/// Write one step event (#207: `check_in` / `step_limit_stop`) through the library record writer — the
/// ONLY writer for the P1b kinds, the same door `events append` refuses them at. `limit_key` is
/// `soft_limit` or `hard_limit`, the kind's own second count.
fn write_step_event(
    kind: crate::events::Kind,
    ticket: &str,
    repo: &str,
    steps: u64,
    limit: (&'static str, u64),
    root: &Path,
    log: &Path,
) -> Result<(), String> {
    use crate::events::{Field, Record};
    let rec = Record::build(
        kind,
        now_ms(),
        &[
            ("ticket", Field::Str(ticket.to_string())),
            ("repo", Field::Str(repo.to_string())),
            ("steps", Field::Num(steps)),
            (limit.0, Field::Num(limit.1)),
        ],
    )?;
    crate::events::append_record(root, log, &rec).map_err(|e| e.to_string())
}

/// `harness process step` (#207, decision 6). STATELESS on purpose: the driver owns the count (an honest
/// limit — a driver that lies about `--steps` is not caught), so the check is a pure compare of the caller's
/// three numbers. Exit codes: `0` continue (below the soft line, silent; at/past it, one `check_in` event
/// per call and `check_in<TAB>s<TAB>a` on stdout — the job continues); `6` the hard line: `step_limit_stop`
/// is written FIRST, then every live process of the ticket is stopped through the stop checks (`6` every
/// death proven or already resolved, `4` any death unproven); `2` any refusal (usage, unclean manifest, or
/// the event could not be written — `harness: EVENT NOT RECORDED`, nothing signalled). ONLY exit 0 means
/// "take another step".
fn step_cmd(a: &[String]) -> i32 {
    let args = match parse_step(a) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    if !crate::events::token_ok(&args.ticket) {
        return fail("ticket must be a plain token (lowercase letters, digits, '-', '_', '.', not empty, not '.' or '..')");
    }
    let dir = PathBuf::from(&args.dir);
    // The supervisor lock is held for the whole step (decision 9), so a hard stop cannot race a stop or a
    // check of the same ticket's entries.
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
    // The log/root pair follows the `events append` defaults (the same resolver the record-mode verbs
    // use, #210); the default folder is created only after the working folder is proven inside the root
    // (a refusal leaves no empty folder behind), the same rule `events append` uses.
    let lr = resolve_log_root(args.log.as_deref(), args.root.as_deref());
    if let Err(e) = ensure_default_log_folder(&lr) {
        return fail(&e);
    }
    let not_recorded = |e: String| -> i32 {
        eprintln!("harness: error: {}", e);
        eprintln!("harness: EVENT NOT RECORDED");
        2
    };
    match step_class(args.steps, args.soft, args.hard) {
        StepClass::Continue => 0,
        StepClass::Soft => {
            if let Err(e) = write_step_event(
                crate::events::Kind::CheckIn,
                &args.ticket,
                &args.repo,
                args.steps,
                ("soft_limit", args.soft),
                &lr.root,
                &lr.log,
            ) {
                return not_recorded(e);
            }
            println!("check_in\t{}\t{}", args.steps, args.soft);
            0
        }
        StepClass::Hard => {
            // Record mode (#210, decision 6) + fix r2 (N1): the handover of every entry THIS RUN IS
            // ABOUT TO SIGNAL is staged BEFORE any signal — and before the `step_limit_stop` event,
            // which stays the FIRST thing a hard stop writes to the log. A failed stage answers 2 with
            // `DEATH NOT RECORDED`, nothing signalled, every entry kept (the record precedes the
            // signal). An entry whose pre-stage probe is INCONCLUSIVE is not signalled AT ALL: the hard
            // line refuses it (rc 2, nothing signalled, the entry kept) and goes on resolving the
            // entries it CAN prove, exactly as a refused stop inside the fold always has.
            let mut staged: Vec<(String, death::Staged)> = Vec::new();
            let mut unproven_refusal = false;
            let rec_run = args.record.as_ref().map(|(session, checkpoint)| RecordRun {
                repo: &args.repo,
                checkpoint,
                session_id: session,
                lr: &lr,
                steps: args.steps,
                hard_limit: args.hard,
            });
            let mut ids: Vec<String> = Vec::new();
            for e in read.live.iter().filter(|e| matches!(e.target, Target::Process { .. })) {
                let Target::Process { pid, pid_start_ms } = &e.target else { continue };
                if let Some(run) = &rec_run {
                    // Fix r1 (F1): a cause is decided by what is OBSERVED about the process BEFORE any
                    // record is written, never by which verb is running. A `timeout`/`step` handover is
                    // staged ONLY for an entry this run has just proven LIVE and still the recorded
                    // process — the one the hard line is about to signal. An entry that is provably
                    // GONE, or whose pid was re-used, is NOT staged here at all: the factored stop's
                    // already-gone / pid-reused arms stage and commit `died` (limit from the entry) with
                    // no pre-existing handover to override, exactly as in check and stop. A DENIED pid
                    // is conclusive too — the stop refuses on it with nothing signalled.
                    // Fix r2 (N1): an UNKNOWN probe is INCONCLUSIVE, and inconclusive is not "the stop
                    // will refuse on it too" — the stop RE-PROBES, and when its own later probe
                    // succeeds it signals a limit kill with nothing staged, and the stopped arm reads
                    // "nothing staged" as a deliberate stop: the entry would be removed with NO record
                    // and NO event. The entry is excluded from the stop, refused (rc 2) and kept.
                    match prestage_verdict(*pid, prestage_liveness(*pid), || prestage_identity(*pid, *pid_start_ms)) {
                        PreStage::Stage => {
                            let (_limit_kind, timeout_ms) = limit_of(&read, &e.id);
                            match stage_death(&dir, &read, &Life { ticket: &args.ticket, id: &e.id, pid: *pid, pid_start_ms: *pid_start_ms }, run, FreshDeath { cause: death::Cause::Timeout, limit: death::LimitKind::Step, timeout_ms }) {
                                Ok(st) => staged.push((e.id.clone(), st)),
                                Err(reason) => {
                                    death_not_recorded(&e.id, &reason);
                                    return 2;
                                }
                            }
                        }
                        // Conclusive: provably gone, re-used or denied. Nothing is staged; the entry
                        // still goes to the stop, whose own arms resolve it (already-gone and
                        // pid-reused record `died`; a denied pid is refused, nothing signalled).
                        PreStage::Resolve => {}
                        // Inconclusive: excluded from the stop, refused (rc 2) and kept.
                        PreStage::Refuse(why) => {
                            fail(&format!("{}: {}; nothing signalled", e.id, why));
                            unproven_refusal = true;
                            continue;
                        }
                    }
                }
                ids.push(e.id.clone());
            }
            let ticket = args.ticket.clone();
            let repo = args.repo.clone();
            let steps = args.steps;
            let hard = args.hard;
            let root = lr.root.clone();
            let log = lr.log.clone();
            let folded = hard_limit_body(
                steps,
                hard,
                move || {
                    write_step_event(crate::events::Kind::StepLimitStop, &ticket, &repo, steps, ("hard_limit", hard), &root, &log)
                },
                &ids,
                |id| {
                    let rec = rec_run
                        .as_ref()
                        .map(|run| RecordStop::Limit { run, staged: staged.iter().find(|(i, _)| i == id).map(|(_, st)| st.clone()) });
                    match stop_live_entry(&dir, &args.ticket, &read, id, args.grace_ms, rec) {
                        StopDone::Stopped { pid, recorded } => {
                            println!("{id}\t{pid}\tstopped");
                            if let Some(line) = recorded {
                                println!("{line}");
                            }
                            0
                        }
                        StopDone::Gone { recorded } | StopDone::Reused { recorded } => {
                            if let Some(line) = recorded {
                                println!("{line}");
                            }
                            3
                        }
                        StopDone::NotConfirmed => 4,
                        StopDone::Refused(m) => {
                            fail(&m);
                            2
                        }
                        StopDone::DeathNotRecorded => 2,
                    }
                },
            );
            // Fix r2 (N1): an entry the pre-stage pass could not prove was refused and KEPT above; its
            // refusal is a 2 at the top of the fold, like every other stop refusal.
            if unproven_refusal {
                2
            } else {
                folded
            }
        }
    }
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

// ---------------------------------------------------------------- #207: limits (tests)

#[cfg(test)]
mod limit_tests {
    use super::*;

    /// #207 AC 8 (pure decision): the expiry table. The clock check comes first — a wall clock before the
    /// limit line's own `ts_ms` is a detectable backward jump and refuses even an already-expired age —
    /// then an unreadable age refuses, and expired means `age >= timeout` (at the line IS expired).
    #[test]
    fn supervisor_expiry_decision_table() {
        // fail closed on an unreadable age (AC 7)
        assert!(matches!(
            expiry_decision(9, Err("ps does not know pid 9".to_string()), 1000, 100, 5000),
            Expiry::Refuse(m) if m.contains("cannot read the age of pid 9") && m.contains("nothing signalled")
        ));
        // a backward clock refuses, and it refuses even when the age alone says expired
        assert!(matches!(expiry_decision(9, Ok(500), 1000, 100, 99), Expiry::Refuse(m) if m.contains("the clock reads before the recorded limit")));
        assert!(matches!(expiry_decision(9, Ok(9999), 1000, 100, 99), Expiry::Refuse(_)));
        // expired at and past the timeout, within below it
        assert!(matches!(expiry_decision(9, Ok(1000), 1000, 100, 5000), Expiry::Expired), "age == timeout is expired");
        assert!(matches!(expiry_decision(9, Ok(1001), 1000, 100, 5000), Expiry::Expired), "age > timeout is expired");
        assert!(matches!(expiry_decision(9, Ok(999), 1000, 100, 5000), Expiry::Within), "age < timeout is within");
    }

    /// #207 r1 F3 (pure fold): check's exit precedence `2 > 4 > 5 > 0`, over every shape the loop can
    /// push. Folding 4 into 5 (the review's mutant O1) or losing the 2-beats-everything rule goes red
    /// here.
    #[test]
    fn supervisor_check_fold_table() {
        assert_eq!(check_fold(&[]), 0, "nothing visited: nothing expired");
        assert_eq!(check_fold(&[0]), 0, "a within-limit entry is silent");
        assert_eq!(check_fold(&[0, 0]), 0);
        assert_eq!(check_fold(&[5]), 5, "one resolved entry is 5");
        assert_eq!(check_fold(&[0, 5]), 5, "a resolved entry beats a silent one");
        assert_eq!(check_fold(&[5, 0, 5]), 5);
        assert_eq!(check_fold(&[4]), 4, "an unproven death is 4, never 5");
        assert_eq!(check_fold(&[4, 5]), 4, "an unproven death beats a resolved one");
        assert_eq!(check_fold(&[5, 4, 0]), 4);
        assert_eq!(check_fold(&[2]), 2, "a refusal is a refusal");
        assert_eq!(check_fold(&[2, 4]), 2, "a refusal beats an unproven death");
        assert_eq!(check_fold(&[4, 2, 5, 0]), 2, "a refusal beats everything");
    }

    /// #207 AC 7 (reader half): an age reader pointed at a helper that cannot run answers Err — the same
    /// not-runnable-is-unknown shape `probe_alive_at` uses — and check maps any Err to refusal 2.
    #[test]
    fn supervisor_age_reader_unreadable_is_err() {
        let bad = Path::new("/nonexistent-harness-age-probe/ps");
        assert!(!bad.exists(), "the age-probe fixture path must not exist on this host");
        #[cfg(not(target_os = "linux"))]
        {
            assert!(
                manifest::process_age_ms_at(bad, std::process::id() as u64).is_err(),
                "an age helper that cannot run must answer Err, never an age"
            );
        }
        #[cfg(target_os = "linux")]
        {
            // On Linux the age is read from /proc (no helper to point); the equivalent failure is a pid
            // /proc cannot know, which must also answer Err.
            assert!(manifest::process_age_ms_at(bad, 4_000_000_000).is_err(), "a pid /proc cannot know has no readable age");
        }
    }

    /// #207 AC 10 mutant (`>` for `>=` goes red here): the soft line counts AT the line, and so does the
    /// hard one — `steps == soft` is a check-in, `steps == hard` is the hard stop.
    #[test]
    fn supervisor_step_class_table() {
        use StepClass::{Continue, Hard, Soft};
        assert!(matches!(step_class(4, 5, 9), Continue), "below the soft line continues silently");
        assert!(matches!(step_class(5, 5, 9), Soft), "at the soft line a check-in is written");
        assert!(matches!(step_class(8, 5, 9), Soft), "between the lines the job continues with check-ins");
        assert!(matches!(step_class(9, 5, 9), Hard), "at the hard line the job stops");
        assert!(matches!(step_class(99, 5, 9), Hard), "past the hard line the job stops");
    }

    /// #207 AC 12 (pure fold): any stop refusal (2) is a refusal, any unconfirmed death (4) is 4, and
    /// only all-proven (or none live) is 6. Folding 4 into 6 goes red here.
    #[test]
    fn supervisor_step_outcome_fold() {
        assert_eq!(fold_stops(&[]), 6, "nothing live: the hard stop is still complete");
        assert_eq!(fold_stops(&[0]), 6);
        assert_eq!(fold_stops(&[0, 3]), 6, "stopped plus already-gone is complete");
        assert_eq!(fold_stops(&[3, 3]), 6);
        assert_eq!(fold_stops(&[0, 4]), 4, "one unconfirmed death is 4");
        assert_eq!(fold_stops(&[4, 0, 3]), 4);
        assert_eq!(fold_stops(&[0, 2]), 2, "a refusal is a refusal");
        assert_eq!(fold_stops(&[2, 4]), 2);
    }

    /// #207 AC 11/12 (ordering, via a recorder): the event is on disk BEFORE any stop runs, and a failed
    /// write stops nothing and answers 2. Writing the event only after the stops goes red here.
    #[test]
    fn supervisor_hard_limit_writes_the_event_before_any_stop() {
        let seen = std::cell::RefCell::new(Vec::<String>::new());
        let ids = vec!["a".to_string(), "b".to_string()];
        let rc = hard_limit_body(
            9,
            9,
            || {
                seen.borrow_mut().push("event".to_string());
                Ok(())
            },
            &ids,
            |id| {
                seen.borrow_mut().push(format!("stop:{id}"));
                0
            },
        );
        assert_eq!(rc, 6);
        assert_eq!(*seen.borrow(), vec!["event".to_string(), "stop:a".to_string(), "stop:b".to_string()], "the event precedes every stop");

        // A failed write: nothing signalled, no stop runs, exit 2.
        let seen2 = std::cell::RefCell::new(Vec::<String>::new());
        let rc2 = hard_limit_body(
            9,
            9,
            || {
                seen2.borrow_mut().push("event-attempt".to_string());
                Err("log path refused: path resolves outside the root".to_string())
            },
            &ids,
            |id| {
                seen2.borrow_mut().push(format!("stop:{id}"));
                0
            },
        );
        assert_eq!(rc2, 2, "an unwritable event answers 2");
        assert_eq!(*seen2.borrow(), vec!["event-attempt".to_string()], "nothing was signalled");
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

// ---------------------------------------------------------------- fix r2 N1: the pre-stage rule (tests)

/// Fix r2 (N1): an entry that `check`/`step` signals for a LIMIT must have its handover staged first,
/// or must not be signalled at all. The tests drive the REAL command functions in-process (the
/// `prestage_fault` seam exists only under `cfg(test)`, never in the binary the CLI tests run) with
/// REAL sleeper processes, and assert the full contract: refused (2), NOTHING signalled, the entry
/// kept, no handover, no event — and the second line, that a limit stop reaching the stopped arm
/// unstaged refuses with `DEATH NOT RECORDED` and keeps the entry.
#[cfg(test)]
mod prestage_tests {
    use super::*;
    use std::fs;
    use std::process::{Child, Command, Stdio};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    const SESSION: &str = "0b6e3c1a-2f4d-4c8e-9a7b-1d2e3f405162";
    const CHECKPOINT: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c";
    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!("sup-prestage-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    /// Kills the whole group of every registered pid when dropped (TERM then KILL).
    #[derive(Default)]
    struct Guard(Vec<u32>);
    impl Guard {
        fn add(&mut self, pid: u32) {
            self.0.push(pid);
        }
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            for (pid, sig) in self.0.iter().flat_map(|p| [(*p, "TERM"), (*p, "KILL")]) {
                let _ = Command::new("kill").arg(format!("-{sig}")).arg("--").arg(format!("-{pid}")).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            }
        }
    }

    /// One REAL live entry: a sleeper in its own process group on a fresh sheet with root `root`.
    fn live_entry(dir: &Path, root: &Path, ticket: &str, id: &str, limit_ms: Option<u64>) -> (Child, u64, u64) {
        let ts = now_ms();
        let c = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = c.id() as u64;
        let start_ms = manifest::process_start_ms(pid).unwrap();
        manifest::apply(dir, ticket, &manifest::Op::Init { roots: std::slice::from_ref(&root.to_path_buf()) }, ts).unwrap();
        manifest::apply(dir, ticket, &manifest::Op::AddProcess { id, pid, pid_start_ms: start_ms }, ts).unwrap();
        if let Some(ms) = limit_ms {
            manifest::apply(dir, ticket, &manifest::Op::Limit { id, timeout_ms: ms }, ts).unwrap();
        }
        (c, pid, start_ms)
    }

    fn entry_live(dir: &Path, ticket: &str, id: &str) -> bool {
        manifest::read_manifest(dir, ticket).unwrap().live.iter().any(|e| e.id == id)
    }

    /// "Provably alive" for the nothing-was-signalled asserts: a BOUNDED poll in which one `Alive`
    /// answer wins and a proven `NoSuch` fails the test — a transient `Unknown` (a probe that could
    /// not run under the test harness's parallel fork load) is retried, never read as "dead".
    fn provably_alive(pid: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match probe_alive_at(Path::new("/bin/kill"), pid) {
                Liveness::Alive => return,
                Liveness::NoSuch | Liveness::Denied => panic!("pid {pid} was signalled or gone despite the refusal"),
                Liveness::Unknown(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(25)),
                Liveness::Unknown(_) => panic!("pid {pid}: the liveness probe stayed inconclusive for 5 s"),
            }
        }
    }

    /// Poll (bounded, a state poll) until the pid's age reads at least `ms` — off Linux the age is
    /// whole seconds, so a 1 ms limit is expired only once `ps etime` ticks past zero.
    fn wait_age(pid: u64, ms: u64) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if manifest::process_age_ms(pid).is_ok_and(|a| a >= ms) {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("pid {pid} never reached an age of {ms} ms");
    }

    /// Reap a signalled child while the stop runs: in-process, THIS test is the parent, and a killed
    /// child stays a ZOMBIE (still answering `kill -0`) until waited — the CLI tests get reaping for
    /// free because their parent is the harness binary, which exits. A BLOCKING wait reaps at the
    /// instant of death, comfortably before the stop's own post-signal probes (each a fork+exec of
    /// /bin/kill) can run.
    fn reaper(child: Arc<Mutex<Child>>) {
        thread::spawn(move || {
            let _ = child.lock().unwrap().wait();
        });
    }

    fn no_record(dir: &Path, ticket: &str, log: &Path) {
        assert!(!dir.join(format!("{ticket}.deaths")).exists(), "no deaths folder may exist");
        assert!(!log.exists(), "no event may be written");
    }

    /// N1 line 1, the pure table: Stage only for a conclusive Alive+Same; Resolve for every conclusive
    /// other answer (reused, gone, denied); Refuse for either inconclusive probe. Collapsing Refuse
    /// into Resolve (the r1 behaviour — stage nothing but signal anyway) goes red here.
    #[test]
    fn prestage_verdict_table() {
        assert!(matches!(prestage_verdict(9, Liveness::Alive, || Identity::Same), PreStage::Stage), "Alive+Same is the one staged case");
        assert!(matches!(prestage_verdict(9, Liveness::Alive, || Identity::Reused), PreStage::Resolve), "a re-used pid resolves through the stop's own arm");
        assert!(matches!(prestage_verdict(9, Liveness::NoSuch, || panic!("a gone liveness must not read identity")), PreStage::Resolve), "provably gone resolves through the stop's own arm");
        assert!(matches!(prestage_verdict(9, Liveness::Denied, || panic!("a denied liveness must not read identity")), PreStage::Resolve), "denied resolves through the stop's own refusal");
        let why = "helper gone once".to_string();
        assert!(
            matches!(prestage_verdict(9, Liveness::Alive, || Identity::Unknown(why.clone())), PreStage::Refuse(m) if m.contains("cannot prove pid 9 is still the recorded process")),
            "an inconclusive IDENTITY refuses"
        );
        assert!(
            matches!(prestage_verdict(9, Liveness::Unknown(why), || panic!("an unknown liveness must not read identity")), PreStage::Refuse(m) if m.contains("cannot prove pid 9 is alive")),
            "an inconclusive LIVENESS refuses"
        );
    }

    /// N1 line 1, check arm: an expired live entry whose pre-stage IDENTITY probe is inconclusive is
    /// refused — rc 2, nothing signalled, the entry kept, no handover, no event. The r1 behaviour
    /// (stage nothing but stop anyway; the stop's own re-probe then succeeds and the stopped arm reads
    /// "nothing staged" as a deliberate stop) removes the entry with no record and goes red here.
    #[test]
    fn check_inconclusive_prestage_identity_refuses_unsignalled() {
        let dir = tmp();
        let root = dir.join("r");
        fs::create_dir_all(&root).unwrap();
        let log = root.join("events.jsonl");
        let (mut child, pid, _start) = live_entry(&dir, &root, "tk", "a", Some(1));
        let mut guard = Guard::default();
        guard.add(child.id());
        wait_age(pid, 1);
        prestage_fault::identity_unknown_for(pid);
        let rc = check_cmd(&[
            "tk".to_string(),
            "--dir".into(),
            dir.to_string_lossy().into_owned(),
            "--session".into(),
            SESSION.into(),
            "--checkpoint".into(),
            CHECKPOINT.into(),
            "--repo".into(),
            "r".into(),
            "--log".into(),
            log.to_string_lossy().into_owned(),
            "--root".into(),
            root.to_string_lossy().into_owned(),
        ]);
        prestage_fault::clear();
        assert_eq!(rc, 2, "an inconclusive pre-stage identity is a refusal");
        assert!(entry_live(&dir, "tk", "a"), "the entry is kept for a later run");
        provably_alive(pid);
        no_record(&dir, "tk", &log);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// N1 line 1, check arm (held-out v2): an expired live entry whose pre-stage LIVENESS probe is
    /// inconclusive is refused the same way — rc 2, nothing signalled, kept, no record.
    #[test]
    fn check_inconclusive_prestage_liveness_refuses_unsignalled() {
        let dir = tmp();
        let root = dir.join("r");
        fs::create_dir_all(&root).unwrap();
        let log = root.join("events.jsonl");
        let (mut child, pid, _start) = live_entry(&dir, &root, "tk", "a", Some(1));
        let mut guard = Guard::default();
        guard.add(child.id());
        prestage_fault::liveness_unknown_for(pid);
        let rc = check_cmd(&[
            "tk".to_string(),
            "--dir".into(),
            dir.to_string_lossy().into_owned(),
            "--session".into(),
            SESSION.into(),
            "--checkpoint".into(),
            CHECKPOINT.into(),
            "--repo".into(),
            "r".into(),
            "--log".into(),
            log.to_string_lossy().into_owned(),
            "--root".into(),
            root.to_string_lossy().into_owned(),
        ]);
        prestage_fault::clear();
        assert_eq!(rc, 2, "an inconclusive pre-stage liveness is a refusal");
        assert!(entry_live(&dir, "tk", "a"), "the entry is kept");
        provably_alive(pid);
        no_record(&dir, "tk", &log);
        let _ = child.kill();
        let _ = child.wait();
    }

    /// N1 line 1, step arm (held-out v1): at the hard line with TWO entries whose pre-stage probes are
    /// inconclusive (one liveness, one identity) and ONE proven live entry, the two are refused
    /// unsignalled and KEPT, the live one alone is stopped and recorded `timeout`/`step`, and the run
    /// answers 2.
    #[test]
    fn step_inconclusive_prestage_refuses_kept_live_entry_stopped() {
        let dir = tmp();
        let root = dir.join("r");
        fs::create_dir_all(&root).unwrap();
        let log = root.join("events.jsonl");
        let ts = now_ms();
        manifest::apply(&dir, "tk", &manifest::Op::Init { roots: std::slice::from_ref(&root) }, ts).unwrap();
        let mut guard = Guard::default();
        let mk = |id: &str| {
            let c = Command::new("sleep").arg("30").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().unwrap();
            let pid = c.id() as u64;
            let start_ms = manifest::process_start_ms(pid).unwrap();
            manifest::apply(&dir, "tk", &manifest::Op::AddProcess { id, pid, pid_start_ms: start_ms }, ts).unwrap();
            (c, pid, start_ms)
        };
        let (mut ca, pid_a, _sa) = mk("a");
        let (mut cb, pid_b, _sb) = mk("b");
        let (cc, pid_c, sc) = mk("c");
        guard.add(ca.id());
        guard.add(cb.id());
        guard.add(cc.id());
        let cc_shared = Arc::new(Mutex::new(cc));
        reaper(Arc::clone(&cc_shared));
        prestage_fault::liveness_unknown_for(pid_a);
        prestage_fault::identity_unknown_for(pid_b);
        let rc = step_cmd(&[
            "tk".to_string(),
            "--repo".into(),
            "r".into(),
            "--steps".into(),
            "9".into(),
            "--soft".into(),
            "5".into(),
            "--hard".into(),
            "9".into(),
            "--grace-ms".into(),
            "500".into(),
            "--dir".into(),
            dir.to_string_lossy().into_owned(),
            "--log".into(),
            log.to_string_lossy().into_owned(),
            "--root".into(),
            root.to_string_lossy().into_owned(),
            "--session".into(),
            SESSION.into(),
            "--checkpoint".into(),
            CHECKPOINT.into(),
        ]);
        prestage_fault::clear();
        assert_eq!(rc, 2, "the two unproven entries make the run a refusal");
        assert!(entry_live(&dir, "tk", "a"), "a is refused and kept");
        provably_alive(pid_a);
        assert!(entry_live(&dir, "tk", "b"), "b is refused and kept");
        provably_alive(pid_b);
        assert!(!entry_live(&dir, "tk", "c"), "the proven-live entry is resolved");
        let hand_c = dir.join("tk.deaths").join(format!("c.{pid_c}.{sc}.json"));
        let text = fs::read_to_string(&hand_c).unwrap_or_default();
        assert!(text.contains("\"cause\":\"timeout\"") && text.contains("\"limit\":\"step\""), "c's handover is timeout/step: {text}");
        let log_text = fs::read_to_string(&log).unwrap();
        assert!(log_text.contains("step_limit_stop"), "the hard-stop event is first: {log_text}");
        assert_eq!(log_text.lines().filter(|l| l.contains("\"kind\":\"timed_out\"")).count(), 1, "exactly one death event: {log_text}");
        assert!(log_text.contains(&format!("tk.deaths/c.{pid_c}.{sc}.json")), "the event names c's handover: {log_text}");
        for c in [&mut ca, &mut cb] {
            let _ = c.kill();
            let _ = c.wait();
        }
        let mut cc = cc_shared.lock().unwrap();
        let _ = cc.kill();
        let _ = cc.wait();
    }

    /// N1 line 2: a LIMIT stop that reaches the stopped arm with NOTHING staged refuses with
    /// `DEATH NOT RECORDED` and keeps the entry — the stopped arm never reads "nothing staged" as a
    /// deliberate stop for a limit stop. Driven at the factored body itself (the only caller-visible
    /// way to arrive unstaged now that line 1 refuses first).
    #[test]
    fn limit_stop_unstaged_refuses_on_the_stopped_arm() {
        let dir = tmp();
        let root = dir.join("r");
        fs::create_dir_all(&root).unwrap();
        let log = root.join("events.jsonl");
        let ts = now_ms();
        manifest::apply(&dir, "tk", &manifest::Op::Init { roots: std::slice::from_ref(&root) }, ts).unwrap();
        let c = Command::new("sleep").arg("30").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).process_group(0).spawn().unwrap();
        let pid = c.id() as u64;
        let start_ms = manifest::process_start_ms(pid).unwrap();
        manifest::apply(&dir, "tk", &manifest::Op::AddProcess { id: "a", pid, pid_start_ms: start_ms }, ts).unwrap();
        let read = manifest::read_manifest(&dir, "tk").unwrap();
        let c_shared = Arc::new(Mutex::new(c));
        reaper(Arc::clone(&c_shared));
        let lr = resolve_log_root(Some(log.to_str().unwrap()), Some(root.to_str().unwrap()));
        let run = RecordRun { repo: "r", checkpoint: CHECKPOINT, session_id: SESSION, lr: &lr, steps: 0, hard_limit: 0 };
        let done = stop_live_entry(&dir, "tk", &read, "a", 500, Some(RecordStop::Limit { run: &run, staged: None }));
        assert!(matches!(done, StopDone::DeathNotRecorded), "an unstaged limit stop refuses, never resolves as deliberate");
        assert!(entry_live(&dir, "tk", "a"), "the entry is kept");
        assert!(!dir.join("tk.deaths").exists(), "no handover was invented by the refusal");
        assert!(!log.exists(), "no event was written");
        let mut c = c_shared.lock().unwrap();
        let _ = c.kill();
        let _ = c.wait();
    }
}
