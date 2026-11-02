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
//! past the exit for grandchildren that hold them), and writes the ending atomically (temp name, sync,
//! rename): a reader sees a whole record or no `exit` file at all. No `exit` file means "outcome
//! unknown", NEVER success. It is not a daemon: it lives exactly as long as the program (plus the
//! bounded drain) and nothing runs when no ticket program runs.
//!
//! The commit handshake closes most of the start-to-record window: after the manifest lines are
//! appended, `start` writes a `commit` line and closes the pipe. A waiter that sees end-of-input
//! WITHOUT the commit word kills its program (SIGKILL through `/bin/kill` by ABSOLUTE path — PATH
//! never decides; the std `Child::kill` needs the Child handle, which the event-driven reaper below
//! holds inside a blocking `wait()`) and records `committed:false`, so a start that refuses after the
//! spawn, or dies hard after it, no longer leaves a running unlisted program — unless the waiter
//! itself is killed too (then there is no record at all: unknown).
//!
//! The reap itself is EVENT-DRIVEN, not polled: the waiter's main thread blocks in `Child::wait()`, so
//! the program is reaped the instant it dies. A polled `try_wait` would leave a zombie for up to one
//! poll period, and a zombie still answers `kill -0` — a window in which `stop`'s post-signal proof
//! would answer 4 "death not confirmed" for a process it had just killed (#209's named zombie race).
//!
//! Privacy of what a program printed is NOT handled here: capture is raw, byte-for-byte, secret-shaped
//! text kept verbatim (P1B-10 owns redaction); the reader verb and the Done link are P1B-09;
//! retention/teardown is P1B-09/P1B-14.

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
    let mut stat = StreamStat::default();
    capped_copy_into(r, w, &mut stat);
    stat
}

/// `capped_copy` writing into a caller-owned stat (so a copier thread's partial progress is readable
/// while it runs).
pub fn capped_copy_into<R: Read, W: Write>(r: &mut R, w: &mut W, stat: &mut StreamStat) {
    // `truncated` claims ONLY the cap: `kept` reached STREAM_CAP with bytes still coming. A WRITE
    // error loses data too, but that is `<stream>_error`'s story — a flag named "truncated" must not
    // claim a marker that was never written.
    let cut_by_cap = |kept: u64, bytes: u64| kept >= STREAM_CAP && bytes > kept;
    let mut buf = vec![0u8; CHUNK];
    let mut kept: u64 = 0; // bytes of the program's own output actually written
    let mut marked = false; // the one-time marker
    loop {
        match r.read(&mut buf) {
            Ok(0) => {
                stat.truncated = cut_by_cap(kept, stat.bytes);
                return;
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
            }
            Err(e) => {
                stat.err_kind(&e);
                stat.truncated = cut_by_cap(kept, stat.bytes);
                return;
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
/// known, and a `No` verdict KILLS the program at once (while it still runs — a refused or dead start
/// must not leave a running unlisted program behind).
fn watch_commit(verdict: std::sync::Arc<std::sync::Mutex<Option<bool>>>, pid: u32) {
    thread::spawn(move || {
        let mut sin = std::io::stdin();
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
        if !committed {
            // the program is killed while it still runs; if it is already dead the kill is a no-op
            let _ = kill_pid_abs(pid);
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
    // record can carry partial truth if a pipe stays open past the drain bound.
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
            let mut st = stat.lock().expect("the stdout stat lock");
            capped_copy_into(&mut r, &mut w, &mut st);
            drop(st);
            let _ = out_done_tx.send(());
        });
    }
    {
        let stat = err_stat.clone();
        thread::spawn(move || {
            let mut w = err_file;
            let mut r = stderr_pipe;
            let mut st = stat.lock().expect("the stderr stat lock");
            capped_copy_into(&mut r, &mut w, &mut st);
            drop(st);
            let _ = err_done_tx.send(());
        });
    }
    let verdict = std::sync::Arc::new(std::sync::Mutex::new(None::<bool>));
    watch_commit(verdict.clone(), pid);

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

// ---------------------------------------------------------------- #209: unit tests

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
}
