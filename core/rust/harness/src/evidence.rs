//! Runtime evidence (P1B-08, #209, unix only, std only): every program `process start` starts gets a
//! small private notebook — what it printed (`stdout`, `stderr`), how it ended (`exit`) and how long it
//! ran — inside the ticket's own root, listed on the ticket's resource manifest BEFORE the spawn, so
//! nothing captured is ever an unlisted orphan and nothing unlisted is ever captured.
//!
//! Layout: `<root>/evidence/proc/<id>.<ts_ms>/` holds exactly `stdout`, `stderr`, `exit`. `<root>` is
//! the manifest root that contains the resolved `--cwd` (the longest such root when roots nest); `<id>`
//! is the validated token and `<ts_ms>` the start's wall ms, so the same `--id` may run again after a
//! stop without ever colliding. No ticket title, body or free text ever appears in a path. Dirs are
//! real, mode 0700, owned by the running user; files are 0600, created EXCLUSIVELY (a pre-planted name,
//! a symlink included, is refused and never written through). An existing `evidence/` or
//! `evidence/proc/` is CHECKED, never trusted: a symlink, a non-directory, another owner or any
//! group/other mode bit refuses the start — exit 2, nothing spawned, nothing appended (fail closed).
//!
//! Because `start` returns while the program keeps running, somebody must outlive the program: that is
//! the WAITER — `start` spawns the harness binary (its own executable) in the hidden `process waiter`
//! subcommand, in its OWN process group, with a commit pipe in and a one-line handshake pipe out. The
//! waiter re-verifies the run dir, creates the capture files, spawns the program in the program's OWN
//! group (group id = program pid, exactly as before, so `stop`'s checks are unchanged), reports
//! `ok <pid> <start_ms>`, reaps the program as soon as it exits, drains both pipes (a bounded 2000 ms
//! past the exit for grandchildren that hold them — and the bound HOLDS even while a grandchild keeps
//! a pipe open forever: the copiers publish their progress and never hold a lock across a read, so the
//! record with `output_open_after_exit:true` lands and the waiter ends), and writes the ending
//! atomically (temp name, sync,
//! rename): a reader sees a whole record or no `exit` file at all. No `exit` file means "outcome
//! unknown", NEVER success. It is not a daemon: it lives exactly as long as the program (plus the
//! bounded drain) and nothing runs when no ticket program runs.
//!
//! The commit handshake closes most of the start-to-record window: after the manifest lines are
//! appended, `start` writes a `commit` line and closes the pipe. A waiter that sees end-of-input
//! WITHOUT the commit word kills its program (SIGKILL through `/bin/kill` by ABSOLUTE path — PATH
//! never decides; the std `Child::kill` needs the Child handle, which the event-driven reaper below
//! holds inside a blocking `wait()`), but only while the program is not yet reaped: past the reap the
//! pid is free and a recycled pid must never be signalled. It records `committed:false`, so a start
//! that refuses after the spawn, or dies hard after it, no longer leaves a running unlisted program —
//! unless the waiter itself is killed too (then there is no record at all: unknown).
//!
//! The reap itself is EVENT-DRIVEN, not polled: the waiter's main thread blocks in `Child::wait()`, so
//! the program is reaped the instant it dies. A polled `try_wait` would leave a zombie for up to one
//! poll period, and a zombie still answers `kill -0` — a window in which `stop`'s post-signal proof
//! would answer 4 "death not confirmed" for a process it had just killed (#209's named zombie race).
//!
//! Privacy of what a program printed (P1B-10): capture itself stays raw, byte-for-byte, secret-shaped
//! text kept verbatim, but EVIDENCE never leaves private state unscanned — every renderer builds its
//! text and hands it to `emit_scanned`, the ONLY sanctioned way an evidence byte leaves the process
//! (later renderers — finalize, status/report cards, briefs — must call it too). The reader verbs and
//! the Done link are P1B-09, below; retention/teardown is P1B-14.

use crate::manifest;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::{Path, PathBuf};
use std::process::{ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness process waiter <run-dir> <cwd> <port|0> -- <program> [args...] (hidden: the supervisor's own waiter, #209)";

/// Per-stream capture cap: the first 4,194,304 bytes are kept byte-for-byte (a constant, no flag).
pub const STREAM_CAP: u64 = 4_194_304;
/// The one-time truncation marker appended past the cap (exactly 46 ASCII bytes).
pub const TRUNCATION_MARKER: &[u8] = b"\n[harness: output truncated at 4194304 bytes]\n";
/// After the program exits, the pipes may stay open at most this long (a grandchild that left the
/// group may hold them); past the bound `output_open_after_exit` records it.
const DRAIN_BOUND_MS: u64 = 2000;
/// After the program exits, the commit word may still be a moment away (start appends, then writes
/// it); past this bound the run counts as uncommitted.
const COMMIT_BOUND_MS: u64 = 5000;
/// How long start waits for the waiter's one handshake line.
pub const HANDSHAKE_BOUND_MS: u64 = 5000;
/// Poll cadence of the waiter's main loop.
const POLL_MS: u64 = 25;
/// Copy chunk size (64 KiB, the usual pipe buffer).
const CHUNK: usize = 65_536;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---------------------------------------------------------------- layout and the fail-closed dir rules

/// The evidence root of a start: `<root>/evidence` where `<root>` is the manifest root that CONTAINS
/// the resolved `--cwd` — the longest such root when roots nest, so a nested root's evidence stays in
/// the innermost fence. None when no root contains the cwd (the caller refuses earlier).
pub fn evidence_root_for(resolved_cwd: &Path, roots: &[String]) -> Option<PathBuf> {
    roots
        .iter()
        .filter(|r| resolved_cwd.starts_with(Path::new(r.as_str())))
        .max_by_key(|r| r.len())
        .map(|r| Path::new(r.as_str()).join("evidence"))
}

/// The running user's uid, std only: `/proc/self` names it on Linux; elsewhere the system `id -u` by
/// ABSOLUTE path (never through PATH, the supervisor's helper rule). Unknown is refused, never skipped.
pub fn own_uid() -> Result<u32, String> {
    #[cfg(target_os = "linux")]
    if let Ok(m) = fs::metadata("/proc/self") {
        return Ok(m.uid());
    }
    match Command::new("/usr/bin/id")
        .arg("-u")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout)
            .trim()
            .parse::<u32>()
            .map_err(|_| "cannot determine the running user".to_string()),
        _ => Err("cannot determine the running user".to_string()),
    }
}

/// The decision table for one existing evidence dir (#209 decision 4, fail closed): a symlink, a
/// non-directory, another owner, or any group/other mode bit refuses. `name` is the RELATIVE name
/// (`evidence` or `evidence/proc`): no absolute path is ever printed (docs/workspace.md's rule).
pub fn check_evidence_dir(m: &fs::Metadata, uid: u32, name: &str) -> Result<(), String> {
    let ft = m.file_type();
    if ft.is_symlink() {
        return Err(format!("{name} is a symlink"));
    }
    if !ft.is_dir() {
        return Err(format!("{name} is not a directory"));
    }
    if m.mode() & 0o077 != 0 {
        return Err(format!("{name} is readable by group or others"));
    }
    if m.uid() != uid {
        return Err(format!("{name} is owned by another user"));
    }
    Ok(())
}

/// Check (never trust) `<root>/evidence` and `<root>/evidence/proc`; create whichever is missing at
/// mode 0700, no-follow (a name that exists is judged by `check_evidence_dir`, a name that does not is
/// made, and a create failure names its kind). Answers both dir paths.
pub fn ensure_evidence_dirs(evidence: &Path, uid: u32) -> Result<(PathBuf, PathBuf), String> {
    let proc = evidence.join("proc");
    ensure_dir(evidence, uid, "evidence")?;
    ensure_dir(&proc, uid, "evidence/proc")?;
    Ok((evidence.to_path_buf(), proc))
}

fn ensure_dir(p: &Path, uid: u32, name: &str) -> Result<(), String> {
    match fs::symlink_metadata(p) {
        Ok(m) => check_evidence_dir(&m, uid, name),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new().mode(0o700).create(p).map_err(|e| format!("cannot create {name}: {}", e.kind()))
        }
        Err(e) => Err(format!("cannot check {name}: {}", e.kind())),
    }
}

/// Create the run dir `<proc>/<id>.<ts_ms>` EXCLUSIVELY at mode 0700 (a same-user swap window stays,
/// named as a limit in the doc). An existing name — symlink included — refuses (`run dir already
/// exists`, retryable: the next ms is a new run).
pub fn create_run_dir(proc_dir: &Path, id: &str, ts_ms: u64) -> Result<PathBuf, String> {
    let name = format!("{id}.{ts_ms}");
    match fs::DirBuilder::new().mode(0o700).create(proc_dir.join(&name)) {
        Ok(()) => Ok(proc_dir.join(name)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err("run dir already exists".to_string()),
        Err(e) => Err(format!("cannot create evidence/proc/{name}: {}", e.kind())),
    }
}

/// The run dir as the WAITER must find it: a real dir (a symlink included in the refusal), the running
/// user's, with no group/other bit. Same table as `check_evidence_dir`, another owner's dir swapped in
/// between start and waiter is refused before anything is created.
pub fn verify_run_dir(run_dir: &Path, uid: u32, rel: &str) -> Result<(), String> {
    let m = fs::symlink_metadata(run_dir).map_err(|e| format!("run dir {rel} does not verify: {}", e.kind()))?;
    check_evidence_dir(&m, uid, rel).map_err(|r| format!("run dir {rel} does not verify: {r}"))
}

/// Create one capture file EXCLUSIVELY at mode 0600: a pre-planted name — a symlink to an outside file
/// included — refuses and nothing is ever written through it.
pub fn create_capture_file(run_dir: &Path, stream: &str, rel: &str) -> Result<File, String> {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(run_dir.join(stream))
        .map_err(|e| format!("cannot create {rel}/{stream}: {}", e.kind()))
}

// ---------------------------------------------------------------- the capped copy

/// What one stream's copy produced: the TOTAL bytes seen (past the cap too), whether the cap cut it,
/// and the first I/O error kind ("" = none). A write error stops the WRITING, never the READING: the
/// pipe keeps being drained so a program that prints forever cannot block on a full pipe.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StreamStat {
    pub bytes: u64,
    pub truncated: bool,
    pub error: String,
}

impl StreamStat {
    fn err_kind(&mut self, e: &std::io::Error) {
        if self.error.is_empty() {
            self.error = e.kind().to_string();
        }
    }
}

/// Copy `r` into `w` up to `STREAM_CAP` bytes, append the one-time marker at the cut, and keep
/// reading and discarding to end-of-input whatever happens to the writer. Raw bytes: nothing is ever
/// decoded, lossified or rewritten.
pub fn capped_copy<R: Read, W: Write>(r: &mut R, w: &mut W) -> StreamStat {
    capped_copy_publishing(r, w, &mut |_| {})
}

/// `capped_copy` with a LIVE publisher: after every chunk (and at every return) the current stat is
/// handed to `publish` — with NO lock of the caller's held across a read (#209 fix round 1, F1). A
/// copier thread publishes into a shared stat by locking only around the publish itself, so a main
/// thread that snapshots the shared stat past the drain bound is never blocked by a pipe a grandchild
/// still holds open. The returned stat is the final one (identical to the last publish).
pub fn capped_copy_publishing<R: Read, W: Write>(
    r: &mut R,
    w: &mut W,
    publish: &mut dyn FnMut(&StreamStat),
) -> StreamStat {
    // `truncated` claims ONLY the cap: `kept` reached STREAM_CAP with bytes still coming. A WRITE
    // error loses data too, but that is `<stream>_error`'s story — a flag named "truncated" must not
    // claim a marker that was never written.
    let cut_by_cap = |kept: u64, bytes: u64| kept >= STREAM_CAP && bytes > kept;
    let mut buf = vec![0u8; CHUNK];
    let mut kept: u64 = 0; // bytes of the program's own output actually written
    let mut marked = false; // the one-time marker
    let mut stat = StreamStat::default();
    loop {
        match r.read(&mut buf) {
            Ok(0) => {
                stat.truncated = cut_by_cap(kept, stat.bytes);
                publish(&stat);
                return stat;
            }
            Ok(n) => {
                stat.bytes += n as u64;
                if kept < STREAM_CAP && stat.error.is_empty() {
                    let take = ((STREAM_CAP - kept) as usize).min(n);
                    if let Err(e) = w.write_all(&buf[..take]) {
                        stat.err_kind(&e);
                    } else {
                        kept += take as u64;
                    }
                }
                if !marked && kept == STREAM_CAP && stat.bytes > kept && stat.error.is_empty() {
                    // the cut itself, exactly once, byte-for-byte
                    marked = true;
                    if let Err(e) = w.write_all(TRUNCATION_MARKER) {
                        stat.err_kind(&e);
                    }
                }
                stat.truncated = cut_by_cap(kept, stat.bytes);
                publish(&stat);
            }
            Err(e) => {
                stat.err_kind(&e);
                stat.truncated = cut_by_cap(kept, stat.bytes);
                publish(&stat);
                return stat;
            }
        }
    }
}

// ---------------------------------------------------------------- the exit record

/// The `exit` record's facts. `duration_ms` is the waiter's own MONOTONIC span (between its spawn
/// returning and its wait returning, inside one process); `start_ms`/`end_ms` are wall clock for humans.
pub struct ExitRecord {
    pub pid: u32,
    pub committed: bool,
    pub outcome: &'static str, // exited | signalled | spawn_failed | wait_failed
    pub code: i32,             // -1 when not exited
    pub signal: i32,           // 0 when not signalled
    pub start_ms: u64,
    pub end_ms: u64,
    pub duration_ms: u64,
    pub stdout: StreamStat,
    pub stderr: StreamStat,
    pub output_open_after_exit: bool,
}

/// One line, compact JSON, fixed key order, every key present (#209 decision 10). The encoder is pure:
/// it carries exactly the `duration_ms` it is given, never a wall-clock derivation of it.
pub fn encode_exit(r: &ExitRecord) -> String {
    fn e(s: &str) -> &str {
        if s.is_empty() { "none" } else { s }
    }
    format!(
        concat!(
            r#"{{"v":1,"pid":{},"committed":{},"outcome":"{}","code":{},"signal":{},"start_ms":{},"end_ms":{},"duration_ms":{},"stdout_bytes":{},"stderr_bytes":{},"stdout_truncated":{},"stderr_truncated":{},"stdout_error":"{}","stderr_error":"{}","output_open_after_exit":{}}}"#,
            "\n"
        ),
        r.pid,
        r.committed,
        r.outcome,
        r.code,
        r.signal,
        r.start_ms,
        r.end_ms,
        r.duration_ms,
        r.stdout.bytes,
        r.stderr.bytes,
        r.stdout.truncated,
        r.stderr.truncated,
        e(&r.stdout.error),
        e(&r.stderr.error),
        r.output_open_after_exit,
    )
}

/// Write the record atomically: a temp name in the run dir, sync, rename — a reader sees a whole
/// record or no `exit` file at all. Any failure leaves no `exit` file (outcome unknown, never
/// fabricated), and never a leftover temp name.
pub fn write_exit_atomic(run_dir: &Path, rec: &ExitRecord) -> bool {
    let tmp = run_dir.join("exit.tmp");
    let wrote = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .and_then(|mut f| {
            f.write_all(encode_exit(rec).as_bytes())?;
            f.sync_all()
        });
    match wrote {
        Ok(()) => match fs::rename(&tmp, run_dir.join("exit")) {
            Ok(()) => true,
            Err(_) => {
                let _ = fs::remove_file(&tmp);
                false
            }
        },
        Err(_) => {
            let _ = fs::remove_file(&tmp);
            false
        }
    }
}

// ---------------------------------------------------------------- the handshake (start side)

/// What the waiter's one handshake line said: `ok <pid> <start_ms>`, or the waiter's own composed
/// refusal (`fail <message>`, e.g. today's `cannot start <program>: <kind>` or an `evidence refused:
/// ...` line). Anything else — EOF, no line, a shape we do not know — is garbage.
pub enum Handshake {
    Ok { pid: u32, start_ms: u64 },
    Fail(String),
}

/// Read the handshake line, bounded: silence past `bound_ms` is `the waiter did not report the spawn
/// within <bound> ms`; a line that is neither shape is `the waiter reported garbage`.
pub fn read_handshake(stdout: ChildStdout, bound_ms: u64) -> Result<Handshake, String> {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut line = String::new();
        let got = BufReader::new(stdout).read_line(&mut line).unwrap_or(0);
        let _ = tx.send(if got == 0 { None } else { Some(line) });
    });
    let line = match rx.recv_timeout(Duration::from_millis(bound_ms)) {
        Ok(Some(l)) => l,
        Ok(None) => return Err("the waiter reported garbage".to_string()),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            return Err(format!("the waiter did not report the spawn within {bound_ms} ms"));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => return Err("the waiter reported garbage".to_string()),
    };
    let line = line.trim_end().to_string();
    if let Some(rest) = line.strip_prefix("ok ") {
        let mut it = rest.split(' ');
        let parsed = match (it.next(), it.next(), it.next()) {
            (Some(a), Some(b), None) => Some((a.parse::<u32>(), b.parse::<u64>())),
            _ => None,
        };
        return match parsed {
            Some((Ok(pid), Ok(start_ms))) if pid > 1 => Ok(Handshake::Ok { pid, start_ms }),
            _ => Err("the waiter reported garbage".to_string()),
        };
    }
    match line.strip_prefix("fail ") {
        Some(msg) if !msg.is_empty() && !msg.contains('\n') => Ok(Handshake::Fail(msg.to_string())),
        _ => Err("the waiter reported garbage".to_string()),
    }
}

// ---------------------------------------------------------------- the waiter

struct WaiterArgs {
    run_dir: PathBuf,
    cwd: PathBuf,
    port: Option<u64>,
    program: String,
    prog_args: Vec<String>,
}

/// Parse the hidden subcommand's line: `<run-dir> <cwd> <port|0> -- <program> [args...]`, the program
/// and its arguments verbatim after the literal `--` (the same shape as `process start`'s tail).
fn parse_waiter(a: &[String]) -> Result<WaiterArgs, String> {
    let dash = a.iter().position(|x| x == "--").ok_or_else(|| "the waiter needs -- before the program".to_string())?;
    let (before, after) = a.split_at(dash);
    if before.len() != 3 {
        return Err("the waiter takes exactly <run-dir> <cwd> <port|0> before --".to_string());
    }
    let run_dir = PathBuf::from(&before[0]);
    let cwd = PathBuf::from(&before[1]);
    let port = before[2].parse::<u64>().map_err(|_| "the waiter port must be 0 or a positive integer".to_string())?;
    let after = &after[1..];
    let program = after.first().cloned().ok_or_else(|| "no program after --".to_string())?;
    if !run_dir.is_absolute() || !cwd.is_absolute() {
        return Err("the waiter takes absolute run-dir and cwd paths".to_string());
    }
    Ok(WaiterArgs { run_dir, cwd, port: (port > 0).then_some(port), program, prog_args: after[1..].to_vec() })
}

/// Report the waiter's own refusal on the handshake pipe (one line), then end: start turns the message
/// into its exit-2 refusal verbatim.
fn report_fail(msg: &str) -> i32 {
    println!("fail {msg}");
    0
}

/// The waiter's ending facts for the early-failure paths (spawn failed, start time unreadable), where
/// there is nothing to wait for: `committed` is false — start never commits a start it refuses.
fn early_exit_record(outcome: &'static str, pid: u32, signal: i32, duration_ms: u64, start_ms: u64) -> ExitRecord {
    ExitRecord {
        pid,
        committed: false,
        outcome,
        code: -1,
        signal,
        start_ms,
        end_ms: now_ms(),
        duration_ms,
        stdout: StreamStat::default(),
        stderr: StreamStat::default(),
        output_open_after_exit: false,
    }
}

/// Kill one pid with SIGKILL through `/bin/kill` by ABSOLUTE path (never through PATH). Used only on
/// the uncommitted end-of-input path, where the `Child` handle is held by the blocked reaping `wait()`
/// and the std kill is therefore out of reach; PATH never decides anything here. False when the helper
/// could not run or refused — the caller records what it knows and never turns False into a
/// success-shaped answer.
fn kill_pid_abs(pid: u32) -> bool {
    Command::new("/bin/kill")
        .arg("-KILL")
        .arg("--")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Watch the commit pipe for the literal `commit` line. The verdict is delivered the moment it is
/// known, and a `No` verdict KILLS the program at once — but only while it is NOT yet reaped (F5):
/// after the main thread's `wait()` the pid is free, and a recycled same-user pid must never be
/// signalled. `reaped` is that fact, shared with the main thread.
fn watch_commit(
    verdict: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
    pid: u32,
    reaped: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    watch_commit_on(std::io::stdin(), kill_pid_abs, pid, verdict, reaped);
}

/// `watch_commit` with the input and the kill injected, so the decision table is unit-testable: the
/// reader and the killer are parameters, the verdict lands in the shared slot exactly once.
fn watch_commit_on<R: Read + Send + 'static, K: Fn(u32) -> bool + Send + 'static>(
    sin: R,
    kill: K,
    pid: u32,
    verdict: std::sync::Arc<std::sync::Mutex<Option<bool>>>,
    reaped: std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    thread::spawn(move || {
        let mut sin = sin;
        let kill = kill;
        let mut line: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        // one verdict, then the thread ends: the `commit` word, or end-of-input (a read error counts
        // the same: unknown is uncommitted, fail safe).
        let committed = loop {
            match sin.read(&mut byte) {
                Ok(0) => break false,
                Ok(_) => {
                    if byte[0] == b'\n' {
                        if line == b"commit" {
                            break true;
                        }
                        line.clear();
                    } else {
                        line.push(byte[0]);
                        if line.len() > 4096 {
                            line.clear(); // no line we care about is that long
                        }
                    }
                }
                Err(_) => break false,
            }
        };
        // the program is killed while it still runs and is not yet reaped; the reaped check is what
        // keeps a freed — possibly recycled — pid out of the signal's way (F5)
        if !committed && !reaped.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = kill(pid);
        }
        *verdict.lock().expect("the commit verdict lock") = Some(committed);
    });
}

/// The hidden `harness process waiter` — the supervisor's own waiter, never typed by a user. See the
/// module header for the full contract; every failure is reported on the handshake pipe (or recorded
/// in `exit`), never printed as an absolute path.
pub fn waiter_cmd(a: &[String]) -> i32 {
    let args = match parse_waiter(a) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("harness: error: {e}");
            return 2;
        }
    };
    let rel = match args.run_dir.file_name().and_then(|n| n.to_str()) {
        Some(n) => format!("evidence/proc/{n}"),
        None => return report_fail("evidence refused: run dir has no name"),
    };
    let uid = match own_uid() {
        Ok(u) => u,
        Err(e) => return report_fail(&format!("evidence refused: {e}")),
    };
    if let Err(r) = verify_run_dir(&args.run_dir, uid, &rel) {
        return report_fail(&format!("evidence refused: {r}"));
    }
    let out_file = match create_capture_file(&args.run_dir, "stdout", &rel) {
        Ok(f) => f,
        Err(r) => return report_fail(&format!("evidence refused: {r}")),
    };
    let err_file = match create_capture_file(&args.run_dir, "stderr", &rel) {
        Ok(f) => f,
        Err(r) => return report_fail(&format!("evidence refused: {r}")),
    };

    // The program: its OWN process group (group id = its pid, exactly as `start` always spawned it, so
    // `stop`'s checks 5-7 are unchanged), stdin closed, both streams piped to this waiter.
    let mut cmd = Command::new(&args.program);
    cmd.args(&args.prog_args)
        .current_dir(&args.cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if let Some(p) = args.port {
        cmd.env("PORT", p.to_string());
    }
    let start_ms_wall = now_ms();
    let spawned = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let rec = early_exit_record("spawn_failed", 0, 0, 0, start_ms_wall);
            let _ = write_exit_atomic(&args.run_dir, &rec);
            return report_fail(&format!("cannot start {}: {}", args.program, e.kind()));
        }
    };
    let pid = child.id();

    // The program's OS start time is read HERE, before any reap could make the pid unreadable (a
    // zombie still answers ps; a reaped pid does not), and handed to start in the handshake: start
    // records the PROGRAM's pid and start time on its manifest line, exactly what that line means today.
    let pid_start_ms = match manifest::process_start_ms(pid as u64) {
        Ok(ms) => ms,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            let rec = early_exit_record("signalled", pid, 9, spawned.elapsed().as_millis() as u64, start_ms_wall);
            let _ = write_exit_atomic(&args.run_dir, &rec);
            return report_fail(&format!("cannot read the start time of the started pid {pid}: {e}"));
        }
    };

    // The handshake: one line on stdout (the pipe start holds), then stdout is closed for good.
    {
        let mut own = std::io::stdout();
        let _ = writeln!(own, "ok {} {}", pid, pid_start_ms);
    }

    // The copier threads own the program's pipes and the capture files; the stats are shared so the
    // record can carry partial truth if a pipe stays open past the drain bound. Each publish locks
    // the shared stat only for the instant of the assignment — NEVER across a read — so the main
    // thread's snapshot after the drain bound cannot block on a pipe a grandchild still holds open
    // (#209 fix round 1, F1: the bound is a bound in every path).
    let stdout_pipe = child.stdout.take().expect("stdout was piped");
    let stderr_pipe = child.stderr.take().expect("stderr was piped");
    let (out_done_tx, out_done_rx) = mpsc::channel::<()>();
    let (err_done_tx, err_done_rx) = mpsc::channel::<()>();
    let out_stat = std::sync::Arc::new(std::sync::Mutex::new(StreamStat::default()));
    let err_stat = std::sync::Arc::new(std::sync::Mutex::new(StreamStat::default()));
    {
        let stat = out_stat.clone();
        thread::spawn(move || {
            let mut w = out_file;
            let mut r = stdout_pipe;
            capped_copy_publishing(&mut r, &mut w, &mut |s| {
                *stat.lock().expect("the stdout stat lock") = s.clone();
            });
            let _ = out_done_tx.send(());
        });
    }
    {
        let stat = err_stat.clone();
        thread::spawn(move || {
            let mut w = err_file;
            let mut r = stderr_pipe;
            capped_copy_publishing(&mut r, &mut w, &mut |s| {
                *stat.lock().expect("the stderr stat lock") = s.clone();
            });
            let _ = err_done_tx.send(());
        });
    }
    let verdict = std::sync::Arc::new(std::sync::Mutex::new(None::<bool>));
    // The reaped flag (#209 fix round 1, F5): once the main thread's `wait()` has reaped the program,
    // the pid is free and the watcher's uncommitted `/bin/kill -KILL <pid>` must never fire again — a
    // recycled same-user pid could be standing in it.
    let reaped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    watch_commit(verdict.clone(), pid, reaped.clone());

    // Main thread: the EVENT-DRIVEN reap. A blocking `wait()` returns the instant the program dies, so
    // no zombie outlives it by even one poll period (`stop`'s post-signal proof would read a zombie as
    // "still alive"). While the program runs, this thread sleeps in the kernel: the waiter lives
    // exactly as long as the program. An uncommitted end-of-input needs no help from this thread — the
    // watcher kills the program, and this wait returns.
    let wait_failed;
    let status: Option<std::process::ExitStatus> = match child.wait() {
        Ok(s) => {
            wait_failed = false;
            Some(s)
        }
        Err(_) => {
            wait_failed = true;
            None
        }
    };
    // Reaped: from here the program's pid is free (F5) — the watcher's uncommitted kill must not fire.
    reaped.store(true, std::sync::atomic::Ordering::SeqCst);
    let duration_ms = spawned.elapsed().as_millis() as u64;
    // The program ended; the commit word may still be a moment away (start appends, then writes it).
    // Bounded wait for the watcher's verdict; past the bound the run counts as uncommitted.
    let deadline = Instant::now() + Duration::from_millis(COMMIT_BOUND_MS);
    let committed = loop {
        if let Some(v) = *verdict.lock().expect("the commit verdict lock") {
            break v;
        }
        if Instant::now() >= deadline {
            break false;
        }
        thread::sleep(Duration::from_millis(POLL_MS));
    };

    // A grandchild that left the group may hold the pipes; wait at most DRAIN_BOUND_MS for both to
    // close, then record what is known and end (the waiter lives exactly as long as the program, plus
    // this bounded drain).
    let drain_deadline = Instant::now() + Duration::from_millis(DRAIN_BOUND_MS);
    let out_closed = out_done_rx.recv_timeout(drain_deadline.saturating_duration_since(Instant::now())).is_ok();
    let err_closed = out_closed && err_done_rx.recv_timeout(drain_deadline.saturating_duration_since(Instant::now())).is_ok();
    let output_open_after_exit = !(out_closed && err_closed);

    let (outcome, code, signal) = if wait_failed {
        ("wait_failed", -1, 0)
    } else if let Some(s) = status {
        match s.code() {
            Some(c) => ("exited", c, 0),
            None => ("signalled", -1, s.signal().unwrap_or(0)),
        }
    } else {
        ("wait_failed", -1, 0)
    };
    let rec = ExitRecord {
        pid,
        committed,
        outcome,
        code,
        signal,
        start_ms: start_ms_wall,
        end_ms: now_ms(),
        duration_ms,
        stdout: out_stat.lock().expect("the stdout stat lock").clone(),
        stderr: err_stat.lock().expect("the stderr stat lock").clone(),
        output_open_after_exit,
    };
    let _ = write_exit_atomic(&args.run_dir, &rec);
    0
}

// ---------------------------------------------------------------- P1B-09 (#214): the evidence reader

/// The evidence READER (P1B-09, unix only, std only, read-only): `harness evidence show|check` read a
/// ticket's runtime evidence and turn it into one line per run plus the Done link. A "run" is every
/// `<id>.<ts>` the ticket's manifest EVER signed out (the FULL append log, so a removed line does not
/// erase what happened); each run gets exactly one state from the table below; `check` refuses Done
/// (exit 1) while any run is failed, running, unknown or damaged — unknown is NEVER success — and
/// allows zero runs (most tickets never start a program; whether one was NEEDED is the gate's
/// business). `show`/`check` print no captured byte and no path: ids are manifest tokens, numbers are
/// re-printed from parsed integers, never copied text (a tail would echo secret-shaped bytes
/// unscanned). Neither verb writes, locks, chmods or deletes anything, takes no
/// supervisor lock, never opens a FIFO (type is checked before any open) and never reads unbounded
/// (the manifest through its capped reader, `exit` at most 4097 bytes, streams never read at all).
pub const READER_USAGE: &str = "usage: harness evidence show|check <ticket> --manifest-dir <absolute-dir> | evidence quote <ticket> --run <id>.<ts> --stream stdout|stderr [--last <n>] --manifest-dir <absolute-dir>";

/// The `exit` record's read cap: a record is ~400 bytes; a bigger file is damaged and the read itself
/// never takes more than this many bytes plus one (fail closed, never an unbounded read).
pub const EXIT_RECORD_CAP: u64 = 4096;

/// One parsed `exit` record: the strict inverse of `encode_exit` (same key order, same shapes), with
/// the value rules the reader adds (`v` is the literal 1, a known outcome, no `committed:true` with
/// `spawn_failed`, `code` >= -1, `signal` >= 0). Numbers live as integers here, so printers re-print
/// parsed values, never copied text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitParsed {
    pub pid: u32,
    pub committed: bool,
    pub outcome: String, // exited | signalled | spawn_failed | wait_failed
    pub code: i32,
    pub signal: i32,
    pub start_ms: u64,
    pub end_ms: u64,
    pub duration_ms: u64,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_error: String,
    pub stderr_error: String,
    pub output_open_after_exit: bool,
}

/// Parse the record EXACTLY as `encode_exit` writes it: one newline-terminated line, compact JSON,
/// the fixed key order, no spaces, string bodies printable and escape-free. Any deviation — a swapped
/// key, `v` 2, an unknown outcome, `committed:true` with `spawn_failed`, a second line, a missing
/// newline, a trailing byte — is `None` (the caller's `malformed`). This is the reader's whole trust
/// in the record's SHAPE; authorship (a same-uid forgery) is a named limit, not detectable here.
pub fn parse_exit_record(bytes: &[u8]) -> Option<ExitParsed> {
    /// A byte cursor that only ever moves forward and answers None the moment the input deviates.
    struct Cur<'a> {
        b: &'a [u8],
        i: usize,
    }
    impl<'a> Cur<'a> {
        fn lit(&mut self, lit: &[u8]) -> Option<()> {
            if self.b.len() - self.i < lit.len() || &self.b[self.i..self.i + lit.len()] != lit {
                return None;
            }
            self.i += lit.len();
            Some(())
        }
        fn uint(&mut self) -> Option<u64> {
            let start = self.i;
            while self.i < self.b.len() && self.b[self.i].is_ascii_digit() {
                self.i += 1;
            }
            if self.i == start || self.i - start > 19 {
                return None;
            }
            std::str::from_utf8(&self.b[start..self.i]).ok()?.parse::<u64>().ok()
        }
        fn sint(&mut self) -> Option<i64> {
            let neg = matches!(self.b.get(self.i), Some(b'-'));
            if neg {
                self.i += 1;
            }
            let v = self.uint()? as i64;
            if neg {
                Some(-v)
            } else {
                Some(v)
            }
        }
        fn boolean(&mut self) -> Option<bool> {
            if self.b.len() - self.i >= 5 && &self.b[self.i..self.i + 5] == b"false" {
                self.i += 5;
                return Some(false);
            }
            if self.b.len() - self.i >= 4 && &self.b[self.i..self.i + 4] == b"true" {
                self.i += 4;
                return Some(true);
            }
            None
        }
        fn qstring(&mut self) -> Option<String> {
            let start = self.i;
            while let Some(&c) = self.b.get(self.i) {
                if c == b'"' {
                    let s = std::str::from_utf8(&self.b[start..self.i]).ok()?.to_string();
                    self.i += 1;
                    return Some(s);
                }
                if c < b' ' || c == b'\\' {
                    return None; // printable and escape-free, exactly what the encoder can write
                }
                self.i += 1;
            }
            None
        }
    }
    let mut c = Cur { b: bytes, i: 0 };
    c.lit(b"{\"v\":1,\"pid\":")?;
    let pid = u32::try_from(c.uint()?).ok()?;
    c.lit(b",\"committed\":")?;
    let committed = c.boolean()?;
    c.lit(b",\"outcome\":\"")?;
    let outcome = c.qstring()?;
    c.lit(b",\"code\":")?;
    let code = i32::try_from(c.sint()?).ok()?;
    c.lit(b",\"signal\":")?;
    let signal = i32::try_from(c.sint()?).ok()?;
    c.lit(b",\"start_ms\":")?;
    let start_ms = c.uint()?;
    c.lit(b",\"end_ms\":")?;
    let end_ms = c.uint()?;
    c.lit(b",\"duration_ms\":")?;
    let duration_ms = c.uint()?;
    c.lit(b",\"stdout_bytes\":")?;
    let stdout_bytes = c.uint()?;
    c.lit(b",\"stderr_bytes\":")?;
    let stderr_bytes = c.uint()?;
    c.lit(b",\"stdout_truncated\":")?;
    let stdout_truncated = c.boolean()?;
    c.lit(b",\"stderr_truncated\":")?;
    let stderr_truncated = c.boolean()?;
    c.lit(b",\"stdout_error\":\"")?;
    let stdout_error = c.qstring()?;
    c.lit(b",\"stderr_error\":\"")?;
    let stderr_error = c.qstring()?;
    c.lit(b",\"output_open_after_exit\":")?;
    let output_open_after_exit = c.boolean()?;
    c.lit(b"}\n")?;
    if c.i != bytes.len() {
        return None; // exactly one line, nothing after its newline
    }
    if !matches!(outcome.as_str(), "exited" | "signalled" | "spawn_failed" | "wait_failed") {
        return None;
    }
    if committed && outcome == "spawn_failed" {
        return None; // start never commits a spawn it refused
    }
    if code < -1 || signal < 0 {
        return None;
    }
    Some(ExitParsed {
        pid,
        committed,
        outcome,
        code,
        signal,
        start_ms,
        end_ms,
        duration_ms,
        stdout_bytes,
        stderr_bytes,
        stdout_truncated,
        stderr_truncated,
        stdout_error,
        stderr_error,
        output_open_after_exit,
    })
}

/// One run's state — the ONE decision table (#214 decision 3). Every run gets exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    /// `committed:true`, `exited`, `code` 0.
    Ok,
    /// `committed:true`, `signalled`, signal 15 or 9 (what a harness stop sends).
    Stopped,
    /// A valid record with `committed:false` (start answered 2 for it at the time).
    Attempt,
    /// `committed:true` and `exited` non-zero, or `signalled` with any other signal, or `wait_failed`.
    Failed,
    /// No `exit` yet, the newest run of its id, and a LIVE `process` entry of that id exists.
    Running,
    /// No `exit` otherwise (waiter killed, host crash, files removed) — never success.
    Unknown,
    /// The evidence shape itself is broken; `reason` names the token.
    Damaged,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            RunState::Ok => "ok",
            RunState::Stopped => "stopped",
            RunState::Attempt => "attempt",
            RunState::Failed => "failed",
            RunState::Running => "running",
            RunState::Unknown => "unknown",
            RunState::Damaged => "damaged",
        }
    }

    /// The Done rule (#214 decision 4): these states refuse Done.
    fn blocks_done(self) -> bool {
        matches!(self, RunState::Failed | RunState::Running | RunState::Unknown | RunState::Damaged)
    }
}

/// One run as the printers see it: the id and stamp (tokens/digits, safe to print), the state, the
/// damaged reason token, and the parsed record when one landed whole.
#[derive(Debug, Clone)]
pub struct RunView {
    pub id: String,
    pub ts: u64,
    pub state: RunState,
    pub reason: Option<&'static str>,
    pub record: Option<ExitParsed>,
}

/// The dir decision table as a reason token (`check_evidence_dir`'s rules, token spellings): a
/// symlink, a non-directory, any group/other mode bit, another owner. A MISSING dir is not damaged —
/// its runs simply have no files and read `unknown` (#214: a run dir moved out of the root is
/// unknown, and unknown blocks Done just the same).
fn dir_reason(m: &fs::Metadata, uid: u32) -> Option<&'static str> {
    let ft = m.file_type();
    if ft.is_symlink() {
        return Some("symlink");
    }
    if !ft.is_dir() {
        return Some("not-dir");
    }
    if m.mode() & 0o077 != 0 {
        return Some("bad-mode");
    }
    if m.uid() != uid {
        return Some("foreign-owner");
    }
    None
}

/// The `exit` file's own table: a symlink (never opened), not a regular file (a FIFO is never opened
/// either — the type is judged BEFORE any open), another owner, any group/other bit, over the cap.
fn exit_file_reason(m: &fs::Metadata, uid: u32) -> Option<&'static str> {
    let ft = m.file_type();
    if ft.is_symlink() {
        return Some("symlink");
    }
    if !ft.is_file() {
        return Some("not-regular");
    }
    if m.uid() != uid {
        return Some("foreign-owner");
    }
    if m.mode() & 0o077 != 0 {
        return Some("bad-mode");
    }
    if m.size() > EXIT_RECORD_CAP {
        return Some("over-cap");
    }
    None
}

/// Same file, twice: the no-follow look and the opened handle must name the SAME device and inode, or
/// the name was `swapped` between the look and the open.
fn same_identity(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    a.dev() == b.dev() && a.ino() == b.ino()
}

/// One manifest `evidence` entry as a run entry: the id must be `<id>.<ts>.<stream>` (stream one of
/// the three, `<ts>` all digits, `<id>` a plain token — split from the RIGHT, so an id may itself
/// hold dots) and the stored path exactly `<root>/evidence/proc/<id>.<ts>/<stream>` for one of the
/// manifest's roots. Anything else is not a run (the caller counts it `other`, never judges it).
fn run_of_entry(id: &str, path: &str, roots: &[String]) -> Option<(String, u64, PathBuf)> {
    let mut parts = id.rsplitn(3, '.');
    let stream = parts.next()?;
    let ts_text = parts.next()?;
    let proc_id = parts.next()?;
    if !matches!(stream, "stdout" | "stderr" | "exit") {
        return None;
    }
    if ts_text.is_empty() || !ts_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let ts: u64 = ts_text.parse().ok()?;
    if !crate::events::token_ok(proc_id) {
        return None;
    }
    let stored = Path::new(path);
    for r in roots {
        let dir = Path::new(r.as_str()).join("evidence").join("proc").join(format!("{proc_id}.{ts_text}"));
        if stored == dir.join(stream) {
            return Some((proc_id.to_string(), ts, dir));
        }
    }
    None
}

/// Group the FULL append log into runs (sorted by `<ts>` then `<id>` — the BTreeMap's own order) and
/// count every other `evidence` entry. A removed entry is still a run: removal does not erase what
/// happened. The input is bounded by the manifest reader's 1 MiB cap, so both counts are bounded too.
pub fn collect_runs(read: &manifest::ManifestRead) -> (Vec<(String, u64, PathBuf)>, usize) {
    let mut runs: std::collections::BTreeMap<(u64, String), PathBuf> = std::collections::BTreeMap::new();
    let mut other = 0usize;
    for line in &read.lines {
        if let manifest::Line::AddPath { id, class: manifest::Class::Evidence, path, .. } = line {
            match run_of_entry(id, path, &read.roots) {
                Some((rid, ts, dir)) => {
                    runs.entry((ts, rid)).or_insert(dir);
                }
                None => other += 1,
            }
        }
    }
    (runs.into_iter().map(|((ts, id), dir)| (id, ts, dir)).collect(), other)
}

/// The start-time tolerance between the live pid's start and the recorded one, the manifest doc's
/// ownership rule (same discipline as `supervisor`'s stop): exact on Linux, 2000 ms elsewhere.
#[cfg(not(target_os = "linux"))]
const START_TOLERANCE_MS: u64 = 2000;

/// Same discipline as the supervisor's stop: the sheet's entry names the process only while the pid's
/// start time still matches the recorded one.
#[cfg(target_os = "linux")]
fn same_start(read: u64, recorded: u64) -> bool {
    read == recorded
}
#[cfg(not(target_os = "linux"))]
fn same_start(read: u64, recorded: u64) -> bool {
    read.abs_diff(recorded) <= START_TOLERANCE_MS
}

/// The state table for one run (#214 decision 3). `newest_of_id` and `live_process` carry the facts a
/// no-`exit` run needs: this is the newest run of its id, and a live `process` entry of that id exists
/// whose pid still IS the process the sheet recorded (start time unchanged; a dead or recycled pid is
/// no longer it) — then the run is `running`; anything else with no record is `unknown`, never
/// success, and an unreadable start time reads unknown too (fail closed).
pub fn classify_run(uid: u32, id: &str, ts: u64, dir: &Path, newest_of_id: bool, live_process: Option<(u64, u64)>) -> RunView {
    let damaged = |reason: &'static str| RunView {
        id: id.to_string(),
        ts,
        state: RunState::Damaged,
        reason: Some(reason),
        record: None,
    };
    // The dir chain: `evidence`, `evidence/proc`, the run dir — checked, never trusted; a MISSING
    // link in the chain means no files, which the no-record rule below reads as unknown.
    let chain = [dir.parent().and_then(|p| p.parent()), dir.parent(), Some(dir)];
    for (name, at) in [("evidence", chain[0]), ("evidence/proc", chain[1]), ("run dir", Some(dir))] {
        let _ = name; // the token spellings below carry the meaning; names kept for the reader of this code
        match at.map(fs::symlink_metadata) {
            Some(Ok(m)) => {
                if let Some(r) = dir_reason(&m, uid) {
                    return damaged(r);
                }
            }
            Some(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {}
            Some(Err(_)) => return damaged("unreadable"),
            None => return damaged("not-dir"),
        }
    }
    // The record: a no-follow look FIRST (a symlink or a FIFO is never opened), then the open, then
    // the opened handle's device and inode against the looked-at one (a swap in between is caught).
    let exit_path = dir.join("exit");
    let looked = match fs::symlink_metadata(&exit_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let still_it = live_process.is_some_and(|(pid, recorded)| {
                manifest::process_start_ms(pid).map(|read| same_start(read, recorded)).unwrap_or(false)
            });
            let state = if newest_of_id && still_it { RunState::Running } else { RunState::Unknown };
            return RunView { id: id.to_string(), ts, state, reason: None, record: None };
        }
        Err(_) => return damaged("unreadable"),
    };
    if let Some(r) = exit_file_reason(&looked, uid) {
        return damaged(r);
    }
    let mut f = match File::open(&exit_path) {
        Ok(f) => f,
        Err(_) => return damaged("unreadable"),
    };
    let opened = match f.metadata() {
        Ok(m) => m,
        Err(_) => return damaged("unreadable"),
    };
    if !same_identity(&looked, &opened) {
        return damaged("swapped");
    }
    // Bounded read: at most the cap plus one byte, so an oversized file is judged, never drained.
    let mut buf = vec![0u8; EXIT_RECORD_CAP as usize + 1];
    let mut len = 0usize;
    loop {
        match f.read(&mut buf[len..]) {
            Ok(0) => break,
            Ok(n) => {
                len += n;
                if len > EXIT_RECORD_CAP as usize {
                    return damaged("over-cap");
                }
            }
            Err(_) => return damaged("unreadable"),
        }
    }
    let rec = match parse_exit_record(&buf[..len]) {
        Some(r) => r,
        None => return damaged("malformed"),
    };
    // A whole record must still have its two streams behind it — missing, a symlink or not regular
    // is damage; their BYTES are never read (sizes in the printers come from the parsed record).
    for stream in ["stdout", "stderr"] {
        match fs::symlink_metadata(dir.join(stream)) {
            Ok(m) if m.file_type().is_symlink() => return damaged("symlink"),
            Ok(m) if !m.file_type().is_file() => return damaged("not-regular"),
            Ok(_) => {}
            Err(_) => return damaged("missing-stream"),
        }
    }
    let state = if !rec.committed {
        RunState::Attempt
    } else {
        match rec.outcome.as_str() {
            "exited" if rec.code == 0 => RunState::Ok,
            "exited" => RunState::Failed,
            "signalled" if rec.signal == 15 || rec.signal == 9 => RunState::Stopped,
            "signalled" => RunState::Failed,
            "wait_failed" => RunState::Failed,
            _ => return damaged("malformed"), // unreachable: the parser already refused other outcomes
        }
    };
    RunView { id: id.to_string(), ts, state, reason: None, record: Some(rec) }
}

/// `<ticket> --manifest-dir <absolute-dir>`: the same shape `workspace check` takes — one ticket
/// (a plain manifest token, refused never rewritten), one required absolute `--manifest-dir` with no
/// `..` component. An unknown flag, a missing value, a repeat or a second ticket is a usage error.
fn parse_reader_args(a: &[String]) -> Result<(String, String), String> {
    let (mut ticket, mut md) = (None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manifest-dir" => {
                let Some(v) = it.next() else { return Err("--manifest-dir is missing its value".to_string()) };
                if md.replace(v.clone()).is_some() {
                    return Err("--manifest-dir was given twice".to_string());
                }
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => {
                if ticket.replace(s.to_string()).is_some() {
                    return Err("more than one ticket given".to_string());
                }
            }
        }
    }
    let ticket = ticket.ok_or_else(|| "a ticket is required".to_string())?;
    let md = md.ok_or_else(|| "--manifest-dir is required".to_string())?;
    check_ticket_md(&ticket, &md)?;
    Ok((ticket, md))
}

/// The ticket must be a plain token and the manifest dir absolute with no `..` component.
fn check_ticket_md(ticket: &str, md: &str) -> Result<(), String> {
    if !crate::events::token_ok(ticket) {
        return Err("the ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')".to_string());
    }
    let p = Path::new(md);
    if !p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err("--manifest-dir must be an absolute path with no `..` component".to_string());
    }
    Ok(())
}

/// The refused-line detail: the damaged reason token, else the record's own outcome detail; `-` when
/// there is nothing to name (running and unknown have no record).
fn refused_detail(v: &RunView) -> String {
    if let Some(r) = v.reason {
        return r.to_string();
    }
    match &v.record {
        Some(rec) if rec.outcome == "signalled" => format!("{} {}", rec.outcome, rec.signal),
        Some(rec) if rec.outcome == "exited" => format!("{} {}", rec.outcome, rec.code),
        Some(rec) => rec.outcome.clone(),
        None => "-".to_string(),
    }
}

/// Why `emit_scanned` printed nothing.
#[derive(Debug, PartialEq, Eq)]
pub enum EmitRefused {
    /// No privacy list is configured (or it could not be loaded): unscanned is never clean.
    NoList,
    /// The text has hits: only the count, the class ids and `stream:line` places are kept, never bytes.
    Hit { count: usize, classes: Vec<&'static str>, places: Vec<String> },
}

/// THE one sanctioned way an evidence byte leaves the process (P1B-10, G2.4). Every evidence renderer
/// builds its whole stdout text `out` and stderr text `err` first, then calls this. It loads the privacy
/// list (none: `NoList`), scans BOTH texts with the same engine `privacy scan --text` and `friction add`
/// use, and on any hit prints NOTHING and returns `Hit` (the caller prints one line naming the surface,
/// the count, the classes and the lines — never a scanned byte). A clean scan writes `out` to stdout and
/// `err` to stderr in one write each. Later renderers (finalize P1B-15, status/report cards, briefs)
/// must go through this function.
pub fn emit_scanned(_surface: &str, out: &str, err: &str, testmode: bool) -> Result<(), EmitRefused> {
    let list = crate::overlay::load().map_err(|_| EmitRefused::NoList)?;
    let ctx = crate::scan::Ctx { list: &list, testmode };
    let mut hits = crate::scan::scan_text(&ctx, "stdout", out);
    hits.extend(crate::scan::scan_text(&ctx, "stderr", err));
    if !hits.is_empty() {
        let mut classes: Vec<&'static str> = Vec::new();
        for h in &hits {
            if !classes.contains(&h.class) {
                classes.push(h.class);
            }
        }
        let places = hits.iter().map(|h| format!("{}:{}", h.name, h.line)).collect();
        return Err(EmitRefused::Hit { count: hits.len(), classes, places });
    }
    if !out.is_empty() {
        let mut so = std::io::stdout();
        let _ = so.write_all(out.as_bytes());
        let _ = so.flush();
    }
    if !err.is_empty() {
        let mut se = std::io::stderr();
        let _ = se.write_all(err.as_bytes());
        let _ = se.flush();
    }
    Ok(())
}

/// Emit through `emit_scanned`; a refusal prints exactly one stderr line (surface, count, classes,
/// places) and returns `hit_code` for a hit, 2 for no list. A clean emit returns `ok_code`.
fn emit_or_refuse(surface: &str, out: &str, err: &str, testmode: bool, ok_code: i32, hit_code: i32) -> i32 {
    match emit_scanned(surface, out, err, testmode) {
        Ok(()) => ok_code,
        Err(EmitRefused::NoList) => {
            eprintln!("harness: error: no privacy list configured: evidence is never printed unscanned");
            2
        }
        Err(EmitRefused::Hit { count, classes, places }) => {
            eprintln!(
                "harness: error: {surface}: {count} privacy hit(s) ({}) at {}; nothing printed",
                classes.join(", "),
                places.join(", ")
            );
            hit_code
        }
    }
}

/// The last-window size `quote` reads from a stream, and the `--last` default and cap.
pub const QUOTE_WINDOW: u64 = 65_536;
pub const QUOTE_LAST_DEFAULT: usize = 20;
pub const QUOTE_LAST_MAX: usize = 200;
/// The fixed harness line printed after a quote of a stream cut at capture (not evidence).
pub const QUOTE_TRUNCATED_NOTE: &str = "[harness: stream was truncated at capture; the cut line is not quoted]";

#[cfg(target_os = "linux")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0o400000 | 0o4000;
#[cfg(target_os = "macos")]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0x0100 | 0x0004;
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
const OPEN_NOFOLLOW_NONBLOCK: i32 = 0;

/// A capture stream's own table (the `exit` file's rules minus the size cap): a symlink, not a regular
/// file (a FIFO is never opened), another owner, any group/other bit.
fn stream_file_reason(m: &fs::Metadata, uid: u32) -> Option<&'static str> {
    let ft = m.file_type();
    if ft.is_symlink() {
        return Some("symlink");
    }
    if !ft.is_file() {
        return Some("not-regular");
    }
    if m.uid() != uid {
        return Some("foreign-owner");
    }
    if m.mode() & 0o077 != 0 {
        return Some("bad-mode");
    }
    None
}

/// The whole lines of a stream's last window, and whether the stream was cut at capture.
struct Tail {
    lines: Vec<String>,
    truncated: bool,
}

/// The cuts that make a window only whole, finished lines (pure; `read_capture_tail` feeds it). `file_len`
/// is the file's size, `start` the window's file offset, `win` the window's bytes. In order: (a) the
/// truncation marker, when the file is exactly cap+marker long and ends with it, is stripped (its leading
/// `\n` would make the cap-cut line look finished); (b) a window not at offset 0 drops everything up to
/// and including its first `\n`; (c) a final run with no `\n` after it is dropped; (d) only then must
/// what is left be UTF-8 (never a lossy conversion).
fn whole_lines(file_len: u64, start: u64, win: &[u8]) -> Result<Tail, String> {
    let mut b = win;
    let mut truncated = false;
    if file_len == STREAM_CAP + TRUNCATION_MARKER.len() as u64 && b.ends_with(TRUNCATION_MARKER) {
        b = &b[..b.len() - TRUNCATION_MARKER.len()];
        truncated = true;
    }
    if start > 0 {
        match b.iter().position(|&c| c == b'\n') {
            Some(i) => b = &b[i + 1..],
            None => b = &[],
        }
    }
    match b.iter().rposition(|&c| c == b'\n') {
        Some(i) => b = &b[..=i],
        None => b = &[],
    }
    let text = std::str::from_utf8(b).map_err(|_| "the stream is not valid UTF-8".to_string())?;
    let lines = text.split_terminator('\n').map(|l| l.to_string()).collect();
    Ok(Tail { lines, truncated })
}

/// Read one stream's last window (at most `QUOTE_WINDOW` bytes). PRIVATE on purpose: the only caller
/// that can emit its bytes is the quote verb, which goes through `emit_scanned`.
fn read_capture_tail(path: &Path, uid: u32) -> Result<Tail, String> {
    use std::io::{Seek, SeekFrom};
    let looked = fs::symlink_metadata(path).map_err(|_| "the stream cannot be read".to_string())?;
    if let Some(r) = stream_file_reason(&looked, uid) {
        return Err(format!("the stream is not readable ({r})"));
    }
    let mut f = OpenOptions::new()
        .read(true)
        .custom_flags(OPEN_NOFOLLOW_NONBLOCK)
        .open(path)
        .map_err(|_| "the stream cannot be opened".to_string())?;
    let opened = f.metadata().map_err(|_| "the stream cannot be read".to_string())?;
    if !same_identity(&looked, &opened) || !opened.file_type().is_file() {
        return Err("the stream was swapped".to_string());
    }
    let len = opened.len();
    let start = len.saturating_sub(QUOTE_WINDOW);
    f.seek(SeekFrom::Start(start)).map_err(|_| "the stream cannot be read".to_string())?;
    let mut win = Vec::new();
    f.take(len - start).read_to_end(&mut win).map_err(|_| "the stream cannot be read".to_string())?;
    whole_lines(len, start, &win)
}

/// `quote <ticket> --run <id>.<ts> --stream stdout|stderr [--last <n>] --manifest-dir <abs>`.
fn parse_quote_args(a: &[String]) -> Result<(String, String, u64, String, usize, String), String> {
    let (mut ticket, mut run, mut stream, mut last, mut md) = (None, None, None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--run" => &mut run,
            "--stream" => &mut stream,
            "--last" => &mut last,
            "--manifest-dir" => &mut md,
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => {
                if ticket.replace(s.to_string()).is_some() {
                    return Err("more than one ticket given".to_string());
                }
                continue;
            }
        };
        let Some(v) = it.next() else { return Err(format!("{arg} is missing its value")) };
        if slot.replace(v.clone()).is_some() {
            return Err(format!("{arg} was given twice"));
        }
    }
    let ticket = ticket.ok_or_else(|| "a ticket is required".to_string())?;
    let md = md.ok_or_else(|| "--manifest-dir is required".to_string())?;
    check_ticket_md(&ticket, &md)?;
    let run = run.ok_or_else(|| "--run is required".to_string())?;
    let (rid, rts) = run.rsplit_once('.').ok_or_else(|| "--run must be <id>.<ts>".to_string())?;
    if !crate::events::token_ok(rid) || rts.is_empty() || !rts.bytes().all(|b| b.is_ascii_digit()) {
        return Err("--run must be <id>.<ts> (a plain token, then digits)".to_string());
    }
    let ts: u64 = rts.parse().map_err(|_| "--run must be <id>.<ts> (a plain token, then digits)".to_string())?;
    let stream = stream.ok_or_else(|| "--stream is required".to_string())?;
    if stream != "stdout" && stream != "stderr" {
        return Err("--stream must be stdout or stderr".to_string());
    }
    let last = match last {
        None => QUOTE_LAST_DEFAULT,
        Some(v) => match v.parse::<usize>() {
            Ok(n) if (1..=QUOTE_LAST_MAX).contains(&n) => n,
            _ => return Err(format!("--last must be a number from 1 to {QUOTE_LAST_MAX}")),
        },
    };
    Ok((ticket, rid.to_string(), ts, stream, last, md))
}

/// `harness evidence quote ...`: the first renderer that quotes a captured line. The run must be one the
/// ticket's manifest signed out; the dir chain and the stream file pass the reader's type/owner checks;
/// only whole finished lines of the last 64 KiB are quoted, and they go through `emit_scanned` — a hit
/// exits 1 with stdout empty, no list or any cannot-quote exits 2.
fn quote_cmd(a: &[String], testmode: bool) -> i32 {
    let (ticket, rid, ts, stream, last, md) = match parse_quote_args(a) {
        Ok(v) => v,
        Err(m) => {
            eprintln!("harness: error: {m}");
            return 2;
        }
    };
    let read = match manifest::read_manifest(Path::new(&md), &ticket) {
        Ok(r) if r.is_clean() => r,
        _ => {
            eprintln!("harness: error: the manifest could not be read");
            return 2;
        }
    };
    let Ok(uid) = own_uid() else {
        eprintln!("harness: error: cannot determine the running user");
        return 2;
    };
    let (runs, _other) = collect_runs(&read);
    let Some((_, _, dir)) = runs.iter().find(|(id, t, _)| *id == rid && *t == ts) else {
        eprintln!("harness: error: that run is not signed out by the ticket's manifest");
        return 2;
    };
    for at in [dir.parent().and_then(|p| p.parent()), dir.parent(), Some(dir.as_path())] {
        let ok = at.and_then(|p| fs::symlink_metadata(p).ok()).is_some_and(|m| dir_reason(&m, uid).is_none());
        if !ok {
            eprintln!("harness: error: the run folder is missing or not private");
            return 2;
        }
    }
    let tail = match read_capture_tail(&dir.join(&stream), uid) {
        Ok(t) => t,
        Err(m) => {
            eprintln!("harness: error: {m}");
            return 2;
        }
    };
    let skip = tail.lines.len().saturating_sub(last);
    let mut out = String::new();
    for l in &tail.lines[skip..] {
        out.push_str(l);
        out.push('\n');
    }
    if tail.truncated {
        out.push_str(QUOTE_TRUNCATED_NOTE);
        out.push('\n');
    }
    emit_or_refuse("evidence.quote", &out, "", testmode, 0, 1)
}

/// `harness evidence show|check <ticket> --manifest-dir <absolute-dir>` (#214) and `evidence quote`
/// (P1B-10). Exit codes — `show`: 0 read (whatever the states), 2 cannot check; `check`: 0 Done allowed by
/// evidence, 1 Done refused, 2 cannot check (usage error, a missing/torn/corrupt manifest, an unreadable
/// manifest dir, no privacy list, a privacy hit in the rendered text — a missing manifest is NEVER zero
/// runs; fail closed). "Cannot check" prints nothing on stdout and one folder-free `harness: error:` line
/// on stderr. All output goes through `emit_scanned`. The Done link itself is wired by `ticket finalize`
/// (P1B-15), exactly as `workspace check` is: finalize runs this check and refuses Done unless 0.
pub fn evidence_cmd(a: &[String], testmode: bool) -> i32 {
    let check = match a.first().map(|s| s.as_str()) {
        Some("show") => false,
        Some("check") => true,
        Some("quote") => return quote_cmd(&a[1..], testmode),
        _ => {
            eprintln!("harness: error: evidence takes a verb, show, check or quote: {READER_USAGE}");
            return 2;
        }
    };
    let (ticket, md) = match parse_reader_args(&a[1..]) {
        Ok(v) => v,
        Err(m) => {
            eprintln!("harness: error: {m}");
            return 2;
        }
    };
    // The manifest through the existing capped reader only, never written. A missing file is an
    // error, never an empty state; the reader's own message may name a path, so the line is fixed.
    let read = match manifest::read_manifest(Path::new(&md), &ticket) {
        Ok(r) if r.is_clean() => r,
        _ => {
            eprintln!("harness: error: the manifest could not be read");
            return 2;
        }
    };
    let uid = match own_uid() {
        Ok(u) => u,
        Err(_) => {
            eprintln!("harness: error: cannot determine the running user");
            return 2;
        }
    };
    let (runs, other) = collect_runs(&read);
    // The two facts a no-record run needs: the newest stamp per id, and the live process ids.
    let mut newest: std::collections::HashMap<&str, u64> = std::collections::HashMap::new();
    for (id, ts, _) in &runs {
        let e = newest.entry(id.as_str()).or_insert(0);
        if *ts > *e {
            *e = *ts;
        }
    }
    let live: std::collections::HashMap<&str, (u64, u64)> = read
        .live
        .iter()
        .filter(|e| e.class == manifest::Class::Process)
        .filter_map(|e| match &e.target {
            manifest::Target::Process { pid, pid_start_ms } => Some((e.id.as_str(), (*pid, *pid_start_ms))),
            _ => None,
        })
        .collect();
    let views: Vec<RunView> = runs
        .iter()
        .map(|(id, ts, dir)| {
            classify_run(uid, id, *ts, dir, newest.get(id.as_str()) == Some(ts), live.get(id.as_str()).copied())
        })
        .collect();
    let count = |s: RunState| views.iter().filter(|v| v.state == s).count();
    let mut out = String::new();
    if !check {
        for v in &views {
            match &v.record {
                Some(r) => out.push_str(&format!(
                    "run\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                    v.id,
                    v.ts,
                    v.state.as_str(),
                    v.reason.unwrap_or("-"),
                    r.outcome,
                    r.code,
                    r.signal,
                    r.committed,
                    r.duration_ms,
                    r.stdout_bytes,
                    r.stderr_bytes,
                    r.stdout_truncated,
                    r.stderr_truncated
                )),
                None => out.push_str(&format!(
                    "run\t{}\t{}\t{}\t{}\t-\t-\t-\t-\t-\t-\t-\t-\t-\n",
                    v.id,
                    v.ts,
                    v.state.as_str(),
                    v.reason.unwrap_or("-")
                )),
            }
        }
        out.push_str(&format!(
            "evidence ticket={} runs={} ok={} stopped={} attempt={} failed={} running={} unknown={} damaged={} other={}\n",
            ticket,
            views.len(),
            count(RunState::Ok),
            count(RunState::Stopped),
            count(RunState::Attempt),
            count(RunState::Failed),
            count(RunState::Running),
            count(RunState::Unknown),
            count(RunState::Damaged),
            other
        ));
        return emit_or_refuse("evidence.show", &out, "", testmode, 0, 2);
    }
    let blocking: Vec<&RunView> = views.iter().filter(|v| v.state.blocks_done()).collect();
    if blocking.is_empty() {
        out.push_str(&format!("evidence=allowed ticket={} runs={}\n", ticket, views.len()));
        return emit_or_refuse("evidence.check", &out, "", testmode, 0, 2);
    }
    for v in &blocking {
        out.push_str(&format!("refused {} {} {} {}\n", v.id, v.ts, v.state.as_str(), refused_detail(v)));
    }
    out.push_str(&format!("evidence=refused ticket={} blocking={}\n", ticket, blocking.len()));
    let first = blocking[0];
    let err = format!(
        "harness: error: Done refused by evidence: {} run(s) failed, running, unknown or damaged (first: {}.{} {})\n",
        blocking.len(),
        first.id,
        first.ts,
        first.state.as_str()
    );
    emit_or_refuse("evidence.check", &out, &err, testmode, 1, 2)
}

// ---------------------------------------------------------------- #209 + #214: unit tests

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn stat(bytes: u64, truncated: bool, error: &str) -> StreamStat {
        StreamStat { bytes, truncated, error: error.to_string() }
    }

    /// AC 5 (unit half): the encoder carries the MONOTONIC duration it is GIVEN, never a wall-clock
    /// derivation of it — fed wall values that differ from the duration, the record keeps the duration.
    /// Mutant "duration_ms = end_ms - start_ms" computes 98999 here and turns this red.
    #[test]
    fn encoder_carries_the_given_monotonic_duration() {
        let rec = ExitRecord {
            pid: 4242,
            committed: true,
            outcome: "exited",
            code: 7,
            signal: 0,
            start_ms: 1_000,
            end_ms: 99_999,
            duration_ms: 1_234,
            stdout: stat(3, false, ""),
            stderr: stat(3, false, ""),
            output_open_after_exit: false,
        };
        let line = encode_exit(&rec);
        assert!(line.contains("\"duration_ms\":1234"), "the record carries the given duration: {line}");
        assert!(!line.contains("98999"), "the duration is never the wall difference: {line}");
        // and the full fixed key order, exactly decision 10's
        let keys: Vec<&str> = line
            .trim_end()
            .trim_start_matches('{')
            .trim_end_matches('}')
            .split(',')
            .map(|kv| kv.split(':').next().unwrap().trim_matches('"'))
            .collect();
        assert_eq!(
            keys,
            ["v", "pid", "committed", "outcome", "code", "signal", "start_ms", "end_ms", "duration_ms", "stdout_bytes", "stderr_bytes", "stdout_truncated", "stderr_truncated", "stdout_error", "stderr_error", "output_open_after_exit"]
        );
    }

    /// The truncation marker is exactly the 46 bytes decision 8 spells (the size the cap test asserts).
    #[test]
    fn the_marker_is_forty_six_bytes() {
        assert_eq!(TRUNCATION_MARKER.len(), 46);
        assert_eq!(TRUNCATION_MARKER, b"\n[harness: output truncated at 4194304 bytes]\n");
    }

    /// AC 9 (owner mismatch, unit — tests do not run as root): the dir decision table refuses a dir
    /// owned by ANOTHER user, and each hostile shape names its own reason. The uid is a parameter so
    /// the mismatch is a fixture, not an act of root.
    #[test]
    fn dir_table_refuses_a_foreign_owner_and_each_hostile_shape() {
        let t = std::env::temp_dir().join(format!("evunit-{}-dir", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::DirBuilder::new().mode(0o700).create(&t).unwrap();
        let m = fs::symlink_metadata(&t).unwrap();
        assert_eq!(check_evidence_dir(&m, m.uid(), "evidence"), Ok(()), "our own 0700 dir passes");
        assert_eq!(
            check_evidence_dir(&m, m.uid() + 1, "evidence"),
            Err("evidence is owned by another user".to_string()),
            "a differing uid is refused"
        );
        // group/other bits
        let mut perm = fs::metadata(&t).unwrap().permissions();
        use std::os::unix::fs::PermissionsExt;
        perm.set_mode(0o750);
        fs::set_permissions(&t, perm).unwrap();
        let m = fs::symlink_metadata(&t).unwrap();
        assert_eq!(
            check_evidence_dir(&m, m.uid(), "evidence"),
            Err("evidence is readable by group or others".to_string())
        );
        // a regular file
        let f = t.join("plainfile");
        fs::write(&f, b"x").unwrap();
        let m = fs::symlink_metadata(&f).unwrap();
        assert_eq!(check_evidence_dir(&m, m.uid(), "evidence"), Err("evidence is not a directory".to_string()));
        // a symlink
        let link = t.join("thelink");
        std::os::unix::fs::symlink(&f, &link).unwrap();
        let m = fs::symlink_metadata(&link).unwrap();
        assert_eq!(check_evidence_dir(&m, m.uid(), "evidence"), Err("evidence is a symlink".to_string()));
        let _ = fs::remove_dir_all(&t);
    }

    /// AC 10 (unit): the waiter's file-create step REFUSES a pre-planted name — a symlink to an
    /// outside file included — and nothing is ever written through it; a run dir that is itself a
    /// symlink refuses before anything is created. Mutant "create+truncate instead of exclusive
    /// create" writes through the symlink, the target stops being byte-identical, and this turns red.
    #[test]
    fn capture_create_refuses_swapped_names() {
        let t = std::env::temp_dir().join(format!("evunit-{}-swap", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::DirBuilder::new().mode(0o700).create(&t).unwrap();
        let outside = t.join("outside-target");
        fs::write(&outside, b"precious").unwrap();
        std::os::unix::fs::symlink(&outside, t.join("stdout")).unwrap();

        assert!(create_capture_file(&t, "stdout", "evidence/proc/x.1").is_err(), "a pre-planted name refuses");
        assert_eq!(fs::read(&outside).unwrap(), b"precious", "nothing was written through the symlink");
        assert!(
            verify_run_dir(&t.parent().unwrap().join("no-such-run-dir"), 0, "evidence/proc/x.1").is_err(),
            "a missing run dir does not verify"
        );

        // a run dir that is itself a symlink refuses before any file is created
        let real = t.join("real");
        fs::DirBuilder::new().mode(0o700).create(&real).unwrap();
        let linked = t.join("linked");
        std::os::unix::fs::symlink(&real, &linked).unwrap();
        let uid = own_uid().unwrap();
        assert!(verify_run_dir(&linked, uid, "evidence/proc/linked").is_err(), "a symlinked run dir does not verify");
        assert_eq!(fs::read_dir(&real).unwrap().count(), 0, "nothing was created through the run-dir symlink");
        let _ = fs::remove_dir_all(&t);
    }

    /// The cap itself (unit): exactly-cap input keeps everything with NO marker and no truncation flag;
    /// one byte past the cap marks the cut and keeps counting.
    #[test]
    fn capped_copy_marks_only_what_was_dropped() {
        let exactly: Vec<u8> = vec![b'x'; STREAM_CAP as usize];
        let mut sink = Vec::new();
        let st = capped_copy(&mut Cursor::new(exactly), &mut sink);
        assert_eq!(st, stat(STREAM_CAP, false, ""));
        assert_eq!(sink.len(), STREAM_CAP as usize, "no marker when nothing was dropped");

        let past: Vec<u8> = vec![b'x'; STREAM_CAP as usize + 1];
        let mut sink = Vec::new();
        let st = capped_copy(&mut Cursor::new(past), &mut sink);
        assert_eq!(st.bytes, STREAM_CAP + 1, "the TOTAL is counted past the cap");
        assert!(st.truncated);
        assert_eq!(sink.len(), (STREAM_CAP + 46) as usize, "cap plus the one-time marker");
        assert!(sink.ends_with(TRUNCATION_MARKER));
    }

    /// AC 14 (unit): a writer that fails after N bytes does not stop the READING — the reader is
    /// drained to end-of-input (a program that prints forever can never block on the pipe), and the
    /// error kind lands in the stat. Mutant "return on the first write error" leaves the reader
    /// undrained and turns this red.
    #[test]
    fn a_failing_writer_never_stops_the_drain() {
        struct FailsAfter {
            budget: usize,
        }
        impl Write for FailsAfter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                if self.budget == 0 {
                    return Err(std::io::Error::from(std::io::ErrorKind::WriteZero));
                }
                let take = self.budget.min(buf.len());
                self.budget -= take;
                Ok(take)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let input = vec![7u8; 3 * CHUNK + 123];
        let mut reader = Cursor::new(input.clone());
        let mut writer = FailsAfter { budget: 100 };
        let st = capped_copy(&mut reader, &mut writer);
        assert_eq!(st.bytes, input.len() as u64, "the reader was drained to end-of-input");
        assert_eq!(st.error, std::io::ErrorKind::WriteZero.to_string(), "the error kind is recorded");
        assert!(!st.truncated, "nothing was dropped by the CAP; the writer failed instead");
    }

    /// Fix round 1, F1 (unit half): the copier PUBLISHES its progress — one publish per chunk and one
    /// at every return — and the last publish is exactly the returned stat, so a main thread that
    /// snapshots the shared stat past the drain bound holds the whole truth the copier knew. Mutant
    /// "publish only at return" (the lock held across reads again, in effect) turns this red.
    #[test]
    fn publishing_copy_reports_progress_per_chunk() {
        let input = vec![5u8; 2 * CHUNK + 7];
        let mut reader = Cursor::new(input.clone());
        let mut sink = Vec::new();
        let mut publishes: Vec<StreamStat> = Vec::new();
        let st = capped_copy_publishing(&mut reader, &mut sink, &mut |s| publishes.push(s.clone()));
        assert!(publishes.len() >= 3, "one publish per chunk, not only at return: {}", publishes.len());
        assert_eq!(publishes.last().unwrap(), &st, "the last publish is the returned stat");
        // intermediate publishes carry PARTIAL truth (bytes still growing)
        assert!(publishes.iter().any(|p| p.bytes > 0 && p.bytes < st.bytes), "progress was visible before the end");
        assert_eq!(st.bytes, input.len() as u64);
    }

    /// Fix round 1, F5: the uncommitted kill fires ONLY while the program is not yet reaped — after the
    /// main thread's `wait()` the pid is free, and a recycled same-user pid must never be signalled.
    /// `watch_commit_on` takes the input and the kill as parameters precisely so this table is
    /// testable. Mutant "drop the reaped check" turns the reaped+EOF case red (the kill fires).
    #[test]
    fn the_uncommitted_kill_skips_a_reaped_program() {
        fn run_case(input: &[u8], reaped: bool) -> (bool, bool) {
            let verdict = std::sync::Arc::new(std::sync::Mutex::new(None::<bool>));
            let reaped_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(reaped));
            let killed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let k = killed.clone();
            watch_commit_on(
                Cursor::new(input.to_vec()),
                move |_pid: u32| {
                    k.store(true, std::sync::atomic::Ordering::SeqCst);
                    true
                },
                4242,
                verdict.clone(),
                reaped_flag,
            );
            // bounded wait for the watcher's verdict (it is the thread's last act)
            let deadline = Instant::now() + Duration::from_millis(5000);
            loop {
                if let Some(v) = *verdict.lock().unwrap() {
                    return (v, killed.load(std::sync::atomic::Ordering::SeqCst));
                }
                assert!(Instant::now() < deadline, "the watcher never delivered its verdict");
                thread::sleep(Duration::from_millis(5));
            }
        }
        // end-of-input WITHOUT the commit word, program already reaped: NO kill (F5's whole point)
        let (v, killed) = run_case(b"", true);
        assert_eq!((v, killed), (false, false), "a reaped program's pid is never signalled");
        // end-of-input without the commit word, program still running: the kill fires
        let (v, killed) = run_case(b"", false);
        assert_eq!((v, killed), (false, true), "a live uncommitted program is killed");
        // the commit word: no kill, whatever the reap state
        let (v, killed) = run_case(b"commit\n", true);
        assert_eq!((v, killed), (true, false), "a committed run kills nothing");
        let (v, killed) = run_case(b"commit\n", false);
        assert_eq!((v, killed), (true, false), "a committed run kills nothing");
    }

    // ------------------------------------------------ #214: the reader's parser and tables

    fn rec() -> ExitRecord {
        ExitRecord {
            pid: 4242,
            committed: true,
            outcome: "exited",
            code: 7,
            signal: 0,
            start_ms: 1_000,
            end_ms: 99_999,
            duration_ms: 1_234,
            stdout: stat(3, false, ""),
            stderr: stat(0, true, "timed out"),
            output_open_after_exit: false,
        }
    }

    /// #214 AC 3 (unit half): the reader's parser is the strict inverse of `encode_exit` — the
    /// encoder's own output parses back, with every value landing as the typed integer/bool it
    /// encodes (printers re-print parsed values, never copied text). Mutant "read `code` from the
    /// `signal` key" swaps the values the test compares and turns this red.
    #[test]
    fn parser_is_the_inverse_of_the_encoder() {
        let p = parse_exit_record(encode_exit(&rec()).as_bytes()).expect("the encoder's output parses");
        assert_eq!(p, ExitParsed {
            pid: 4242,
            committed: true,
            outcome: "exited".to_string(),
            code: 7,
            signal: 0,
            start_ms: 1_000,
            end_ms: 99_999,
            duration_ms: 1_234,
            stdout_bytes: 3,
            stderr_bytes: 0,
            stdout_truncated: false,
            stderr_truncated: true,
            stdout_error: "none".to_string(),
            stderr_error: "timed out".to_string(),
            output_open_after_exit: false,
        });
        // a spawn failure is a committed:false record and parses
        let mut r = rec();
        r.committed = false;
        r.outcome = "spawn_failed";
        r.pid = 0;
        r.code = -1;
        let p = parse_exit_record(encode_exit(&r).as_bytes()).expect("a spawn failure parses");
        assert!(!p.committed && p.outcome == "spawn_failed" && p.code == -1);
    }

    /// #214 AC 10(d,e) (unit half): the parser refuses every shape deviation — two keys swapped,
    /// `v` 2, an unknown outcome, `committed:true` with `spawn_failed`, a second line, a missing
    /// newline, a trailing byte, a negative signal, a non-integer, an escape-shaped string. Each is
    /// `malformed` at the file level; here the parser half answers None for each.
    #[test]
    fn parser_refuses_every_deviation() {
        let good = encode_exit(&rec());
        assert!(parse_exit_record(good.as_bytes()).is_some(), "the baseline is good: {good}");
        let swap_two_keys = good.replacen("\"code\":7,\"signal\":0", "\"signal\":0,\"code\":7", 1);
        assert!(parse_exit_record(swap_two_keys.as_bytes()).is_none(), "swapped keys are malformed");
        assert!(parse_exit_record(good.replace("\"v\":1", "\"v\":2").as_bytes()).is_none(), "v 2 is malformed");
        assert!(
            parse_exit_record(good.replace("\"exited\"", "\"vanished\"").as_bytes()).is_none(),
            "an unknown outcome is malformed"
        );
        let mut forged = rec();
        forged.outcome = "spawn_failed";
        // committed stays true: start never commits a spawn it refused
        assert!(parse_exit_record(encode_exit(&forged).as_bytes()).is_none(), "committed spawn_failed is malformed");
        assert!(parse_exit_record(format!("{good}{good}").as_bytes()).is_none(), "two lines are malformed");
        assert!(parse_exit_record(good.trim_end().as_bytes()).is_none(), "a missing newline is malformed");
        assert!(parse_exit_record(format!("{good} ").as_bytes()).is_none(), "a trailing byte is malformed");
        assert!(parse_exit_record(good.replace("\"signal\":0", "\"signal\":-3").as_bytes()).is_none(), "a negative signal is malformed");
        assert!(parse_exit_record(good.replace("\"pid\":4242", "\"pid\":x").as_bytes()).is_none(), "a non-integer is malformed");
        assert!(parse_exit_record(good.replace("timed out", "timed\\u0020out").as_bytes()).is_none(), "an escape-shaped string is malformed");
        assert!(parse_exit_record(b"").is_none(), "an empty file is malformed");
    }

    /// #214 AC 10 (the owner and swap halves, unit — tests do not run as root): the uid is a
    /// PARAMETER, so a foreign owner is a fixture, not an act of root; and the identity compare that
    /// backs `swapped` answers false for two different files and true for one file seen twice.
    #[test]
    fn owner_and_identity_tables() {
        let t = std::env::temp_dir().join(format!("evunit-{}-own", std::process::id()));
        let _ = fs::remove_dir_all(&t);
        fs::DirBuilder::new().mode(0o700).create(&t).unwrap();
        let uid = own_uid().unwrap();
        // dirs: our own 0700 dir passes, another uid's does not (the parameter IS the fixture)
        let m = fs::symlink_metadata(&t).unwrap();
        assert_eq!(dir_reason(&m, uid), None);
        assert_eq!(dir_reason(&m, uid.wrapping_add(1)), Some("foreign-owner"));
        // files: the exit table, uid as a parameter again
        let f = t.join("exit");
        fs::write(&f, encode_exit(&rec())).unwrap();
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        let fm = fs::symlink_metadata(&f).unwrap();
        assert_eq!(exit_file_reason(&fm, uid), None, "our own 0600 record passes");
        assert_eq!(exit_file_reason(&fm, uid.wrapping_add(1)), Some("foreign-owner"), "another owner is refused");
        // identity: two different files never match; one file seen twice always does
        let g = t.join("other");
        fs::write(&g, b"x").unwrap();
        let gm = fs::symlink_metadata(&g).unwrap();
        assert!(!same_identity(&fm, &gm), "two different files are a swap");
        let fm2 = fs::symlink_metadata(&f).unwrap();
        assert!(same_identity(&fm, &fm2), "the same file seen twice matches");
        let _ = fs::remove_dir_all(&t);
    }

    /// #214 decision 2 (unit): an entry is a run only when BOTH halves hold — the id splits
    /// `<id>.<ts>.<stream>` from the right (an id may hold dots) and the stored path is exactly
    /// `<root>/evidence/proc/<id>.<ts>/<stream>`. Everything else is `other`, never judged.
    #[test]
    fn run_grouping_takes_only_the_convention() {
        let root = "/somewhere/r".to_string();
        let roots = [root];
        let dir = |name: &str, stream: &str| PathBuf::from(format!("/somewhere/r/evidence/proc/{name}/{stream}"));
        assert_eq!(
            run_of_entry("a.123.exit", dir("a.123", "exit").to_str().unwrap(), &roots),
            Some(("a".to_string(), 123, dir("a.123", "x").parent().unwrap().to_path_buf()))
        );
        // an id that itself holds dots still splits from the right
        assert_eq!(
            run_of_entry("b.7.45.stdout", dir("b.7.45", "stdout").to_str().unwrap(), &roots),
            Some(("b.7".to_string(), 45, dir("b.7.45", "x").parent().unwrap().to_path_buf()))
        );
        // wrong stream, non-digit stamp, wrong root, wrong path: none of them is a run
        assert_eq!(run_of_entry("a.123.core", dir("a.123", "core").to_str().unwrap(), &roots), None);
        assert_eq!(run_of_entry("a.1x3.exit", dir("a.1x3", "exit").to_str().unwrap(), &roots), None);
        assert_eq!(run_of_entry("a.123.exit", "/elsewhere/evidence/proc/a.123/exit", &roots), None);
        assert_eq!(run_of_entry("a.123.exit", dir("a.124", "exit").to_str().unwrap(), &roots), None);
    }
}
