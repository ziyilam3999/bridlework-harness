//! Process-level tests for `harness process start` and `harness process stop` (P1B-05 / #183). Every check
//! runs the real binary as a child process, spawns REAL programs, and reads the real manifest file, so the
//! exit-code contracts (start: 0 started, 2 usage error or any refusal with nothing started, signalled or
//! written; stop: 0 stopped and removed, 2 refusal with nothing signalled and the manifest byte-identical,
//! 3 already-gone / pid-reused with nothing signalled and the entries removed, 4 signalled but death not
//! confirmed with the entry kept) and "a refused start leaves nothing running that the sheet does not
//! list" are exercised end to end. Every wait is a bounded poll, every port a listener the test itself
//! holds or a fresh free port it retries, and every process a test spawns itself is registered in a guard
//! that kills its whole group on drop, so a failed run leaves nothing behind. No wall-clock assert
//! anywhere: order is proved by bounded polls on observable state, never by elapsed time.
#![cfg(unix)]
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("pcli-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch layout: `R` is the ticket's root, `O` a sibling folder outside every root, `D` holds the manifest.
    fn fence(&self) -> (PathBuf, PathBuf, PathBuf) {
        let r = self.0.join("R");
        let o = self.0.join("O");
        let d = self.0.join("D");
        fs::create_dir_all(&r).unwrap();
        fs::create_dir_all(&o).unwrap();
        fs::create_dir_all(&d).unwrap();
        (r, o, d)
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// A free TCP port on 127.0.0.1 (bound then released; a snapshot, like the supervisor's own check).
fn free_port() -> u64 {
    let l = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    l.local_addr().unwrap().port() as u64
}

/// Is a process alive at this pid? Asked through the system `kill -0 <pid>`.
fn alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Is any process alive in group `pgid`? Asked through the system `kill -0 -- -<pgid>`.
fn group_alive(pgid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg("--")
        .arg(format!("-{}", pgid))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Call `f` every 25 ms until it answers true or `ms` runs out; the answer when the budget ran out.
fn poll(ms: u64, f: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    let mut f = f;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    f()
}

/// The pid in a pid file, once the file PARSES as exactly one whole number. The writer's own shell redirection
/// (`echo $$ > file`) creates the file EMPTY first, so existence alone proves nothing: reading it before the
/// write lands parses the empty string and panics (the round-2 flake). Everything a spawned process writes is
/// read through a poll on one of these parse-gated readers, never on `exists()`.
fn read_pid(f: &Path) -> Option<u32> {
    fs::read_to_string(f).ok()?.trim().parse().ok()
}

/// Kills the whole process group of every registered pid when dropped (TERM, then KILL), so a failed test run
/// leaves nothing behind. Only pids spawned in their OWN group (process_group(0)) may be registered. A test may
/// also register a PID FILE a spawn writes itself: drop then reads it (polling until it parses, because the
/// writer may still be mid-write) and kills that group too, so a panic anywhere after the spawn — before the
/// test ever parsed the pid — still kills what the spawn made.
#[derive(Default)]
struct Guard {
    pids: Vec<u32>,
    pid_file: Option<PathBuf>,
}

impl Guard {
    fn add(&mut self, pid: u32) {
        self.pids.push(pid);
    }

    fn watch_pid_file(&mut self, f: PathBuf) {
        self.pid_file = Some(f);
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(f) = &self.pid_file {
            if poll(2_000, || read_pid(f).is_some()) {
                if let Some(pid) = read_pid(f) {
                    self.pids.push(pid);
                }
            }
        }
        for (pid, sig) in self.pids.iter().flat_map(|p| [(*p, "TERM"), (*p, "KILL")]) {
            let _ = Command::new("kill")
                .arg(format!("-{}", sig))
                .arg("--")
                .arg(format!("-{}", pid))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

/// A test-spawned process in its own process group (so a buggy group signal can never hit the test itself).
fn spawn_own_group(program: &str, arg: &str) -> Child {
    Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap()
}

/// A test-spawned `/bin/sh -c <script>` in its own process group (so a buggy group signal can never hit
/// the test itself).
fn spawn_sh(script: &str) -> Child {
    Command::new("/bin/sh")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .unwrap()
}

/// `harness manifest init <ticket>` into dir `d` with the single root `r`.
fn init_manifest(ticket: &str, r: &Path, d: &Path) {
    let i = run(&["manifest", "init", ticket, "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap()]);
    assert_eq!(code(&i), 0, "init: {}", err(&i));
}

/// Run `process start` for `prog` (the program and its arguments, after `--`) under ticket `ticket`, id `id`,
/// cwd `cwd`; returns (output, pid).
fn start(ticket: &str, id: &str, cwd: &Path, port: Option<u64>, d: &Path, prog: &[&str]) -> (Output, u32) {
    let mut v: Vec<String> = vec![
        "process".into(),
        "start".into(),
        ticket.into(),
        "--id".into(),
        id.into(),
        "--cwd".into(),
        cwd.to_string_lossy().into_owned(),
    ];
    if let Some(p) = port {
        v.push("--port".into());
        v.push(p.to_string());
    }
    v.push("--dir".into());
    v.push(d.to_string_lossy().into_owned());
    v.push("--".into());
    for a in prog {
        v.push((*a).into());
    }
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    let o = run(&args);
    let pid = out(&o).split('\t').nth(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    (o, pid)
}

/// `start` with `--port` set to a fresh free port, retrying (bounded) with a new port when the snapshot port is
/// taken between `free_port()` and `start`'s own bind check — `free_port()` releases the socket, so any other
/// process may grab it in between, and the supervisor's check is a snapshot too. Only the `already bound`
/// refusal (which leaves nothing started, so a retry is clean) is retried; every other outcome is returned for
/// the caller to assert. Answers (run, pid, the port actually used).
fn start_free_port(ticket: &str, id: &str, cwd: &Path, d: &Path, prog: &[&str]) -> (Output, u32, u64) {
    for _ in 0..5 {
        let port = free_port();
        let (o, pid) = start(ticket, id, cwd, Some(port), d, prog);
        if !(code(&o) == 2 && err(&o).contains("already bound")) {
            return (o, pid, port);
        }
    }
    panic!("5 free ports in a row were taken between free_port() and start's own bind check");
}

// ---------------------------------------------------------------- AC 1

/// AC 1: after `start` with `--port`, `manifest list` shows the process line with the live pid and the `<id>.port`
/// line, and `manifest owns --pid` exits 0. (Ending the process and emptying the sheet is the follow-up
/// ticket's AC, not this PR's; the guard ends the process when the test drops.)
#[test]
fn start_records_process_and_port() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-1", &r, &d);
    let mut guard = Guard::default();

    let (s, pid, port) = start_free_port("t-1", "p-1", &r, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1, "start printed no pid: stdout={:?} stderr={:?}", out(&s), err(&s));
    guard.add(pid);
    let stdout = out(&s);
    let fields: Vec<&str> = stdout.trim_end().split('\t').collect();
    assert_eq!(fields.len(), 3, "start prints id<TAB>pid<TAB>pid_start_ms: {:?}", out(&s));
    assert_eq!(fields[0], "p-1");
    let start_ms = fields[2];

    let l = run(&["manifest", "list", "t-1", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    let want_process = format!("p-1\tprocess\t{}\t{}", pid, start_ms);
    let want_port = format!("p-1.port\tport\t{}", port);
    let text = out(&l);
    assert!(text.lines().any(|x| x == want_process), "list lacks the process line {:?}: {:?}", want_process, text);
    assert!(text.lines().any(|x| x == want_port), "list lacks the port line {:?}: {:?}", want_port, text);

    let q = run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--pid", &pid.to_string()]);
    assert_eq!(code(&q), 0, "owns --pid of the started process: {}", err(&q));
}

// ---------------------------------------------------------------- AC 7

/// AC 7: a start with an id already live, a cwd outside every root, and a port already bound each exit 2, and the
/// program (which would create a marker file) never ran: the marker is absent and the manifest is byte-identical.
#[test]
fn refused_start_leaves_nothing_running() {
    let t = Tmp::new();
    let (r, o, d) = t.fence();
    init_manifest("t-8", &r, &d);
    let mut guard = Guard::default();

    let (s, keeper) = start("t-8", "k-1", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "keeper start: {}", err(&s));
    guard.add(keeper);

    let marker = r.join("marker");
    let script = format!("echo ran > {}", marker.to_string_lossy());
    let file = d.join("t-8.jsonl");
    let before = fs::read(&file).unwrap();

    // (a) an id already live
    let a = start("t-8", "k-1", &r, None, &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&a.0), 2, "a live id refuses the start: stdout {:?} stderr {:?}", out(&a.0), err(&a.0));
    // (b) a cwd outside every root
    let b = start("t-8", "m-1", &o, None, &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&b.0), 2, "a cwd outside every root refuses the start: stdout {:?} stderr {:?}", out(&b.0), err(&b.0));
    // (c) a port already bound on 127.0.0.1. Bind port 0 and KEEP the listener: a free-port-then-rebind has a
    // take-over window between the release and the re-bind (anything can grab the socket in between and the
    // re-bind then fails AddrInUse — the round-2 suite flake), while a held listener makes the port provably
    // taken for as long as the case needs.
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let bound = held.local_addr().unwrap().port() as u64;
    let c = start("t-8", "m-2", &r, Some(bound), &d, &["/bin/sh", "-c", &script]);
    drop(held);
    assert_eq!(code(&c.0), 2, "a bound port refuses the start: stdout {:?} stderr {:?}", out(&c.0), err(&c.0));
    // And the refusal is the AddrInUse message, not the privilege one (fix round 2, finding 3).
    assert!(err(&c.0).contains("already bound"), "a taken port names already bound: stderr {:?}", err(&c.0));
    assert!(!err(&c.0).contains("cannot check port"), "a taken port is not a bind-check failure: stderr {:?}", err(&c.0));

    assert!(!marker.exists(), "a refused start ran the program");
    assert_eq!(fs::read(&file).unwrap(), before, "a refused start changed the manifest");
}

// ---------------------------------------------------------------- fix round 1: the port range is a usage error

/// Fix round 1, finding 1: an out-of-range `--port` (0, 65536, 70000, and a value past u64) is a usage error
/// checked in the parser BEFORE the lock, the manifest read, the bind check or the spawn: rc 2, the marker file
/// is absent (the program never ran), and the manifest is byte-identical. Fix round 3, finding 3: the largest
/// accepted port is exercised only through a listener THIS test holds — 65535 is inside the host ephemeral
/// range, so an unheld 65535 fails whenever anything else sits on it (measured 1/30 under load); when the test
/// cannot take the port itself, the held-port half is skipped and the parser-order refusals above still ran.
#[test]
fn port_out_of_range_refuses_before_spawn() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-9", &r, &d);

    let marker = r.join("marker");
    let script = format!("echo ran > {}", marker.to_string_lossy());
    let file = d.join("t-9.jsonl");
    let before = fs::read(&file).unwrap();
    for bad in ["0", "65536", "70000", "99999999999999999999"] {
        let s = run(&[
            "process", "start", "t-9", "--id", "w-1", "--cwd", r.to_str().unwrap(),
            "--port", bad, "--dir", d.to_str().unwrap(), "--", "/bin/sh", "-c", &script,
        ]);
        assert_eq!(code(&s), 2, "--port {bad} must refuse: stdout {:?} stderr {:?}", out(&s), err(&s));
        assert!(err(&s).contains("port"), "--port {bad} must name the flag: stderr {:?}", err(&s));
        assert!(!marker.exists(), "--port {bad}: the refused start ran the program");
        assert_eq!(fs::read(&file).unwrap(), before, "--port {bad}: the refused start changed the manifest");
    }

    // The largest accepted port, taken by THIS test's own listener, refuses with the AddrInUse message (the
    // same refusal the supervisor would give any holder). Only when another process already holds 65535 — a
    // port this test does not own — is this half skipped.
    match TcpListener::bind(("127.0.0.1", 65535)) {
        Ok(held) => {
            let s = run(&[
                "process", "start", "t-9", "--id", "w-1", "--cwd", r.to_str().unwrap(),
                "--port", "65535", "--dir", d.to_str().unwrap(), "--", "/bin/sh", "-c", &script,
            ]);
            drop(held);
            assert_eq!(code(&s), 2, "a held 65535 refuses the start: stdout {:?} stderr {:?}", out(&s), err(&s));
            assert!(err(&s).contains("already bound"), "a held 65535 names already bound: stderr {:?}", err(&s));
            assert!(!err(&s).contains("cannot check port"), "a held port is not a bind-check failure: stderr {:?}", err(&s));
            assert!(!marker.exists(), "the refused start ran the program");
            assert_eq!(fs::read(&file).unwrap(), before, "the refused start changed the manifest");
        }
        Err(_) => {
            println!("SKIP: 127.0.0.1:65535 is held by a process this test does not own; the held-port half is not re-checked");
        }
    }
}

// ---------------------------------------------------------------- fix round 1: the append-failure rollback

/// Fix round 1, finding 2 (mutation M4): when the PORT append fails AFTER the process line was written, `start`
/// appends `remove` for that process line and stops the whole group — rc 2, the pid dead, no process line left.
/// The failure is forced deterministically at a point the parser's range check cannot reach: the test holds the
/// manifest file's own exclusive lock, so `start` (whose pre-check read takes no lock) blocks INSIDE its first
/// append; while it is blocked the test appends a live `w-1.port` line by hand, so the SECOND append (the port
/// line) is refused as a duplicate live id. Deleting the group kill leaves the pid alive; deleting the
/// compensating remove leaves the process line listed; both fail this test.
#[test]
fn append_failure_after_process_line_is_rolled_back() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-c", &r, &d);
    let mut guard = Guard::default();
    let pid_file = r.join("pid");
    // The spawn writes its own pid file; the guard reads that file on drop (polling until it PARSES, because
    // the shell's redirection creates it empty first), so a panic anywhere after the spawn — before this test
    // ever parsed the pid — still kills the group. A leaked process after a failed run is itself a defect.
    guard.watch_pid_file(pid_file.clone());

    // Up to 5 attempts, each with a fresh free port. The only retried outcome is the `already bound` refusal
    // of the snapshot port BEFORE the spawn (a free-port-then-check race, the same class as finding 2): that
    // refusal starts nothing and writes nothing, so the attempt repeats cleanly on a new port.
    let mut done: Option<(Output, u32)> = None;
    for _attempt in 0..5 {
        let port = free_port();
        let file = d.join("t-c.jsonl");

        // Hold the manifest lock across the whole pre-check window: start's read (a plain fs::read) sees a
        // clean sheet, but its first `apply` blocks here until the injection below is in place.
        let mut lock = OpenOptions::new().read(true).append(true).open(&file).unwrap();
        lock.lock().unwrap();

        let script = format!("echo $$ > {}; sleep 300", pid_file.to_string_lossy());
        let r2 = r.to_string_lossy().into_owned();
        let d2 = d.to_string_lossy().into_owned();
        let port_s = port.to_string();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(run(&[
                "process", "start", "t-c", "--id", "w-1", "--cwd", &r2, "--port", &port_s,
                "--dir", &d2, "--", "/bin/sh", "-c", &script,
            ]));
        });

        // The program runs only after start's pre-checks read the sheet, so once its pid file PARSES as a pid
        // the pre-checks are past (they can no longer see the injection) and start is blocked on the lock
        // inside the process-line append. A run that answers the port race instead (refused `already bound`
        // before any spawn) moves to the next attempt; existence of the file alone proves nothing (round-2
        // finding 1: the empty first write panicked the parse 9 runs in 30 under contention).
        let mut refused: Option<Output> = None;
        let started = poll(10_000, || {
            if read_pid(&pid_file).is_some() {
                return true;
            }
            match rx.try_recv() {
                Ok(o) => {
                    refused = Some(o);
                    true
                }
                Err(mpsc::TryRecvError::Empty) => false,
                Err(mpsc::TryRecvError::Disconnected) => true,
            }
        });
        if let Some(o) = refused {
            assert_eq!(code(&o), 2, "an early refusal must be the port race: stdout {:?} stderr {:?}", out(&o), err(&o));
            assert!(err(&o).contains("already bound"), "an early refusal must be the port race: stderr {:?}", err(&o));
            continue;
        }
        assert!(started, "the program never started and the run never answered");
        let pid = read_pid(&pid_file).expect("polled until the pid file parses");

        // Inject a live `w-1.port` line through the locked handle, then release: start appends the process
        // line, then its port append is refused as a duplicate live id.
        let inject = format!(
            "{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-c\",\"id\":\"w-1.port\",\"class\":\"port\",\"port\":{port},\"ts_ms\":1730000000002}}\n"
        );
        lock.write_all(inject.as_bytes()).unwrap();
        lock.sync_all().unwrap();
        drop(lock);

        let s = rx.recv().expect("the run answers once unblocked");
        done = Some((s, pid));
        break;
    }
    let (s, pid) = done.expect("5 attempts in a row hit the already-bound port race");
    guard.add(pid);
    assert_eq!(code(&s), 2, "the refused port append must answer 2: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(err(&s).contains("duplicate live id"), "the port append was refused by the manifest, not by the range check: stderr {:?}", err(&s));
    assert!(poll(10_000, || !alive(pid)), "the group the refused start made is still alive");
    assert!(poll(10_000, || !group_alive(pid)), "the group the refused start made is still alive");

    let l = run(&["manifest", "list", "t-c", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    let text = out(&l);
    assert!(!text.contains("process"), "the refused start left a process line listed: {}", text);
    assert!(text.contains("w-1.port"), "only the test's own injected port line stays live: {}", text);
}

// ---------------------------------------------------------------- fix round 2: the bind error kind is named

/// Fix round 2, finding 3: a bind error that is NOT `AddrInUse` is not reported as "already bound". On macOS an
/// unprivileged bind on 127.0.0.1 below 1024 is EACCES (and EPERM on Linux), so `--port 1` on a FREE port must
/// name the error kind (`PermissionDenied`, ports below 1024 need privilege) — or, under privilege, start
/// cleanly. Either way it must never claim the free port is "already bound".
#[test]
fn low_port_names_the_bind_error_kind() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-p", &r, &d);
    let mut guard = Guard::default();

    let marker = r.join("marker");
    let script = format!("echo ran > {}", marker.to_string_lossy());
    let (s, pid) = start("t-p", "l-1", &r, Some(1), &d, &["/bin/sh", "-c", &script]);
    if code(&s) == 0 {
        // A privileged environment lets the bind through; that is the other documented outcome.
        assert!(pid > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
        guard.add(pid);
    } else {
        assert_eq!(code(&s), 2, "port 1 must start or refuse 2: stdout {:?} stderr {:?}", out(&s), err(&s));
        assert!(err(&s).contains("permission denied"), "a privilege refusal names the kind: stderr {:?}", err(&s));
        assert!(err(&s).contains("below 1024"), "a privilege refusal names the low-port rule: stderr {:?}", err(&s));
        assert!(!err(&s).contains("already bound"), "a free low port is not 'already bound': stderr {:?}", err(&s));
        assert!(!marker.exists(), "the refused start ran the program");
    }
}

// ---------------------------------------------------------------- fix round 3: the rollback is PATH-free and leaves nothing running

/// Fix round 3, item 2: a refused start whose rollback cannot reach its helpers must return FAST, answer 2, and
/// leave nothing running and nothing listed. Two cases:
///
/// (A) PATH points at an EMPTY folder after the spawn. Since #183 the start-time read runs `/bin/ps` by
///     ABSOLUTE path (manifest.rs, one line), so the empty PATH refuses NOTHING any more — the start
///     SUCCEEDS on every platform and PATH never decides identity (the round-4 H7 hostile: a PATH-shadowed
///     `ps` could move a recorded start time). The started pid joins the cleanup guard BEFORE any assert,
///     so a wrong assert can never leave the sleep 30 running. The rollback still ends the child —
///     `/bin/kill` is called by absolute path and the std `Child::kill()` needs no PATH — and never blocks
///     in `wait()` on a child it could not signal (r3 H1d: the round-2 code answered only after 8033 ms,
///     and a server would never).
/// (A2) the post-spawn refusal is ALSO proved platform-neutrally, the lock-held torn-tail way (B) uses: the
///     manifest lock is held across the spawn so the start blocks inside its process-line append, a
///     duplicate live id is injected through the lock, and on release the append is refused AFTER the
///     spawn — on Linux this is the refusal (A) cannot make there; everywhere it also re-proves the
///     bounded rollback with nothing appended and nothing left running.
/// (B) the std fallback is load-bearing on its own: a child that MOVES OUT of the group the rollback
///     signals (`setpgid` back into the parent's group — a spawned leader cannot `setsid`, but it may join
///     another group of its session) is out of reach of every group signal — `/bin/kill -TERM -- -pid`
///     answers "no such process" — so only the std kill of the direct pid ends it. The refusal is engineered
///     to land only AFTER the escape is provable: the test holds the manifest lock so the start blocks inside
///     its process-line append, waits until the child provably left the group, then injects a duplicate live
///     id so the append is refused and the rollback runs against an already-escaped child.
#[test]
fn refused_start_rollback_leaves_nothing_running() {
    // (A) empty PATH after the spawn: the start-time read runs `/bin/ps` by ABSOLUTE path (the #183
    // one-line manifest change), so the empty PATH refuses nothing — the start SUCCEEDS on every
    // platform and PATH never decides identity. The started pid is registered with the guard BEFORE any
    // assert (the pid file is watched from before the spawn), so a failing assert can never leave the
    // sleep 30 running.
    {
        let t = Tmp::new();
        let (r, _o, d) = t.fence();
        init_manifest("t-r1", &r, &d);
        let mut guard = Guard::default();
        let pid_file = r.join("pid");
        guard.watch_pid_file(pid_file.clone());
        let empty_bin = t.0.join("emptybin");
        fs::create_dir_all(&empty_bin).unwrap();
        let file = d.join("t-r1.jsonl");
        let before = fs::read(&file).unwrap();
        // The shell writes its own pid before exec'ing sleep, so the guard can always find it.
        let script = format!("echo $$ > {}; exec /bin/sleep 30", pid_file.to_string_lossy());
        let s = Command::new(BIN)
            .args([
                "process", "start", "t-r1", "--id", "a-1", "--cwd", r.to_str().unwrap(),
                "--dir", d.to_str().unwrap(), "--", "/bin/sh", "-c", &script,
            ])
            .env("PATH", empty_bin.to_str().unwrap())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        // #183: the /bin/ps start-time read needs no PATH on ANY platform, so the start succeeds —
        // PATH never decides identity. Register the started pid BEFORE any assert.
        let pid_stdout = out(&s).split('\t').nth(1).and_then(|f| f.trim().parse::<u32>().ok());
        if let Some(pid) = pid_stdout {
            guard.add(pid);
        }
        assert_eq!(
            code(&s), 0,
            "the /bin/ps start-time read needs no PATH, so the start succeeds: stdout {:?} stderr {:?}",
            out(&s), err(&s)
        );
        let pid = pid_stdout.expect("start printed the pid it started");
        assert!(alive(pid), "the started pid is running: the empty PATH must not have refused the start");
        let after = fs::read(&file).unwrap();
        assert!(after.starts_with(&before), "the successful start rewrote the head of the manifest");
        assert_eq!(
            String::from_utf8_lossy(&after[before.len()..]).matches('\n').count(),
            1,
            "the successful start appended exactly one line: {}",
            String::from_utf8_lossy(&after)
        );
    }

    // (A2) the post-spawn refusal, platform-neutral (on Linux this is the refusal the empty-PATH arm of
    // (A) cannot make there): hold the manifest lock across the spawn — the pre-check read is a plain
    // read and passes, the first append blocks — then inject a duplicate live id through the lock and
    // release, so the append is refused AFTER the spawn and the rollback must answer 2, FAST, append
    // nothing, and leave nothing running. The same torn-tail shape (B) uses, without the group escape.
    {
        let t = Tmp::new();
        let (r, _o, d) = t.fence();
        init_manifest("t-r4", &r, &d);
        let mut guard = Guard::default();
        let pid_file = r.join("pid");
        guard.watch_pid_file(pid_file.clone());
        let file = d.join("t-r4.jsonl");
        let before = fs::read(&file).unwrap();
        let script = format!("echo $$ > {}; exec /bin/sleep 30", pid_file.to_string_lossy());
        let mut lock = OpenOptions::new().read(true).append(true).open(&file).unwrap();
        lock.lock().unwrap();
        let r4 = r.to_string_lossy().into_owned();
        let d4 = d.to_string_lossy().into_owned();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let _ = tx.send(
                Command::new(BIN)
                    .args(["process", "start", "t-r4", "--id", "a-4", "--cwd", &r4, "--dir", &d4, "--", "/bin/sh", "-c", &script])
                    .stdin(Stdio::null())
                    .output()
                    .unwrap(),
            );
        });
        assert!(poll(10_000, || read_pid(&pid_file).is_some()), "the program never started");
        let pid = read_pid(&pid_file).unwrap();
        guard.add(pid);
        let inject = "{\"v\":1,\"op\":\"add\",\"ticket\":\"t-r4\",\"id\":\"a-4\",\"class\":\"process\",\"pid\":999999,\"pid_start_ms\":1730000000000,\"ts_ms\":1730000000002}\n";
        lock.write_all(inject.as_bytes()).unwrap();
        lock.sync_all().unwrap();
        drop(lock);
        let s = rx.recv().expect("the run answers once unblocked");
        assert_eq!(code(&s), 2, "the duplicate live id refuses the start after the spawn: stdout {:?} stderr {:?}", out(&s), err(&s));
        assert!(err(&s).contains("duplicate live id"), "the refusal names the append failure: stderr {:?}", err(&s));
        assert!(poll(5_000, || !alive(pid)), "the child of the refused start still lives");
        let after = fs::read(&file).unwrap();
        assert!(after.starts_with(&before), "the injected tail moved the head of the manifest");
        assert_eq!(&after[before.len()..], inject.as_bytes(), "the refused start appended beyond the injected line");
    }

    // (B) the std fallback: a child that moved out of the signalled group can only be ended by the
    // direct (std) kill. Skipped when the host has no python3 or refuses the move.
    if !Path::new("/usr/bin/python3").exists() {
        println!("SKIP: no /usr/bin/python3 on this host, so the group-escape cannot be spawned");
        return;
    }
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-r2", &r, &d);
    let mut guard = Guard::default();
    let pid_file = r.join("pid");
    guard.watch_pid_file(pid_file.clone());
    let moved = r.join("moved");
    // The child is its own group leader (start spawned it with process_group(0)), so setsid() would fail;
    // moving back into the PARENT's group (same session) puts it out of reach of a signal to -<its pid>
    // while the std kill of the direct pid still reaches it. The marker names which of the two happened.
    let script = format!(
        "echo $$ > {}; exec /usr/bin/python3 -c 'import os, time\ntry:\n    os.setpgid(0, os.getpgid(os.getppid())); open(\"{}\", \"w\").write(\"moved\")\nexcept OSError:\n    open(\"{}\", \"w\").write(\"fail\")\ntime.sleep(30)'",
        pid_file.to_string_lossy(),
        moved.to_string_lossy(),
        moved.to_string_lossy()
    );

    // Hold the manifest lock: the start's pre-check read is a plain read, so it passes, and its first append
    // blocks here until the escape below is provable.
    let mut lock = OpenOptions::new().read(true).append(true).open(d.join("t-r2.jsonl")).unwrap();
    lock.lock().unwrap();
    let r2 = r.to_string_lossy().into_owned();
    let d2 = d.to_string_lossy().into_owned();
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let _ = tx.send(
            Command::new(BIN)
                .args(["process", "start", "t-r2", "--id", "b-1", "--cwd", &r2, "--dir", &d2, "--", "/bin/sh", "-c", &script])
                .stdin(Stdio::null())
                .output()
                .unwrap(),
        );
    });
    assert!(poll(10_000, || read_pid(&pid_file).is_some()), "the program never started");
    let pid = read_pid(&pid_file).unwrap();
    assert!(
        poll(10_000, || fs::read_to_string(&moved).map(|m| m.trim() == "moved").unwrap_or(false)),
        "the child never reported the group move"
    );
    if fs::read_to_string(&moved).unwrap().trim() == "fail" {
        println!("SKIP: this host refuses setpgid into the parent group, so the escape cannot be exercised");
        return;
    }
    // Provably out of the signalled group BEFORE the refusal: the group probe (kill -0 -- -pid) says
    // no such group exists any more.
    assert!(poll(10_000, || !group_alive(pid)), "the child never left its process group");
    // Inject a duplicate live id through the locked handle, then release: the process-line append is
    // refused as a duplicate and the rollback runs with the group already unreachable.
    let inject = "{\"v\":1,\"op\":\"add\",\"ticket\":\"t-r2\",\"id\":\"b-1\",\"class\":\"process\",\"pid\":999999,\"pid_start_ms\":1730000000000,\"ts_ms\":1730000000002}\n";
    lock.write_all(inject.as_bytes()).unwrap();
    lock.sync_all().unwrap();
    drop(lock);
    let s = rx.recv().expect("the run answers once unblocked");
    assert_eq!(code(&s), 2, "the duplicate live id refuses the start: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(err(&s).contains("duplicate live id"), "the refusal names the append failure: stderr {:?}", err(&s));
    assert!(poll(5_000, || !alive(pid)), "the child that left the signalled group still lives: only the std kill reaches it");
}

// ---------------------------------------------------------------- #183: process stop helpers

/// Which entry a stop addresses: `--id <id>` or `--pid <n>`.
enum StopSel<'a> {
    Id(&'a str),
    Pid(u32),
}

/// Run `process stop`, by id or by pid, with an optional `--grace-ms`.
fn stop(ticket: &str, sel: StopSel, grace: Option<u64>, d: &Path) -> Output {
    let mut v: Vec<String> = vec!["process".into(), "stop".into(), ticket.into()];
    match sel {
        StopSel::Id(i) => {
            v.push("--id".into());
            v.push(i.into());
        }
        StopSel::Pid(p) => {
            v.push("--pid".into());
            v.push(p.to_string());
        }
    }
    if let Some(g) = grace {
        v.push("--grace-ms".into());
        v.push(g.to_string());
    }
    v.push("--dir".into());
    v.push(d.to_string_lossy().into_owned());
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    run(&args)
}

/// The process-group id of one pid, asked through the system `ps` (a TEST-side precondition check, not the
/// code under test).
fn pgid_of(pid: u32) -> u32 {
    let o = Command::new("ps")
        .arg("-o")
        .arg("pgid=")
        .arg("-p")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).trim().parse().unwrap()
}

/// The REAL OS start time (ms) of a live pid, read through the public `manifest add --class process` verb
/// into a throwaway manifest (the same read the supervisor itself uses), so a planted fixture can be offset
/// from a value the harness measured, not from one the test re-derives.
fn real_start_ms(pid: u32) -> u64 {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("probe", &r, &d);
    let add = run(&["manifest", "add", "probe", "--class", "process", "--id", "probe-1", "--pid", &pid.to_string(), "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&add), 0, "probe add of pid {pid}: {}", err(&add));
    let l = run(&["manifest", "list", "probe", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    let listing = out(&l);
    let line = listing.lines().find(|x| x.starts_with("probe-1\tprocess\t")).expect("the probe manifest lists the process line");
    line.split('\t').nth(3).expect("the process line carries pid_start_ms").parse().expect("pid_start_ms is a number")
}

/// Plant a schema-valid `add` process line with a CHOSEN `pid_start_ms` (the fixture carrier for the
/// re-used-pid cases: the recorded time is what the test wants it to be, not what the writer measured).
fn plant_process(d: &Path, ticket: &str, id: &str, pid: u32, pid_start_ms: u64) {
    let line = format!(
        "{{\"v\":1,\"op\":\"add\",\"ticket\":\"{ticket}\",\"id\":\"{id}\",\"class\":\"process\",\"pid\":{pid},\"pid_start_ms\":{pid_start_ms},\"ts_ms\":1730000000002}}\n"
    );
    let mut f = OpenOptions::new().append(true).open(d.join(format!("{ticket}.jsonl"))).unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

/// Sign a live pid out through the PUBLIC `manifest add --class process` verb (records the real start time).
fn manifest_add_process(ticket: &str, id: &str, pid: u32, d: &Path) -> Output {
    run(&["manifest", "add", ticket, "--class", "process", "--id", id, "--pid", &pid.to_string(), "--dir", d.to_str().unwrap()])
}

/// The pids a fixture program wrote to its pid file, once each line parses as one whole number.
fn read_pids(f: &Path) -> Vec<u32> {
    fs::read_to_string(f)
        .map(|t| t.lines().filter_map(|l| l.trim().parse().ok()).collect())
        .unwrap_or_default()
}

// ---------------------------------------------------------------- #183 AC 1

/// #183 AC 1: a started program that forks a grandchild (both pids written to a file in the root) is stopped
/// with exit 0; NEITHER pid exists afterwards; both entries (the process line and the `<id>.port` line) are
/// removed. A stop that signalled only the pid (mutant: signal the pid, not its group) leaves the grandchild
/// alive, so the "neither pid exists afterwards" poll fails.
#[test]
fn stop_ends_process_and_its_children() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-1", &r, &d);
    let mut guard = Guard::default();
    let pids_file = r.join("pids");
    let script = format!("sleep 300 & echo $! > {}; echo $$ >> {}; wait", pids_file.to_string_lossy(), pids_file.to_string_lossy());
    let (s, leader, _port) = start_free_port("ts-1", "p-1", &r, &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(leader > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    guard.add(leader);

    assert!(poll(10_000, || read_pids(&pids_file).len() == 2), "the program never wrote both pids");
    let pids = read_pids(&pids_file);
    let (grand, lead) = (pids[0], pids[1]);
    assert_eq!(lead, leader, "the started pid $$ is the group leader the sheet records");
    assert_ne!(grand, lead, "the grandchild must be a second, different pid");
    assert!(alive(grand) && alive(lead), "both pids must be live before the stop");

    let st = stop("ts-1", StopSel::Id("p-1"), None, &d);
    assert_eq!(code(&st), 0, "stop: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert!(poll(10_000, || !alive(lead) && !alive(grand)), "a pid of the stopped group still exists");
    let l = run(&["manifest", "list", "ts-1", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    let text = out(&l);
    assert!(!text.contains("p-1"), "the stopped id (or its port line) is still listed: {}", text);
}

// ---------------------------------------------------------------- #183 AC 2

/// #183 AC 2: `stop --pid` of a live process the ticket does not list, and of a pid listed only on ANOTHER
/// ticket, each exit 2; both processes are still alive (each is a test-owned Child checked not to have
/// exited, so a zombie of ours cannot read as alive); both manifests are byte-identical.
#[test]
fn unlisted_pid_is_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-2a", &r, &d);
    init_manifest("ts-2b", &r, &d);
    let mut guard = Guard::default();

    let mut own = spawn_own_group("sleep", "60");
    let own_pid = own.id();
    guard.add(own_pid);
    let mut other = spawn_own_group("sleep", "60");
    let other_pid = other.id();
    guard.add(other_pid);
    // `other_pid` is listed only on ANOTHER ticket (ts-2b), through the public verb.
    let add = manifest_add_process("ts-2b", "q-1", other_pid, &d);
    assert_eq!(code(&add), 0, "add on the other ticket: {}", err(&add));

    let fa = d.join("ts-2a.jsonl");
    let fb = d.join("ts-2b.jsonl");
    let (ba, bb) = (fs::read(&fa).unwrap(), fs::read(&fb).unwrap());
    for pid in [own_pid, other_pid] {
        assert!(alive(pid), "pid {pid} must be live before the stop");
    }
    assert!(own.try_wait().unwrap().is_none() && other.try_wait().unwrap().is_none(), "a fixture child exited early");

    let s1 = stop("ts-2a", StopSel::Pid(own_pid), None, &d);
    assert_eq!(code(&s1), 2, "an unlisted pid refuses: stdout {:?} stderr {:?}", out(&s1), err(&s1));
    assert!(err(&s1).contains("is not a live entry"), "the refusal names the ownership rule: stderr {:?}", err(&s1));
    assert!(alive(own_pid), "the refused stop signalled the unlisted pid");

    let s2 = stop("ts-2a", StopSel::Pid(other_pid), None, &d);
    assert_eq!(code(&s2), 2, "a pid of another ticket refuses: stdout {:?} stderr {:?}", out(&s2), err(&s2));
    assert!(err(&s2).contains("is not a live entry"), "the refusal names the ownership rule: stderr {:?}", err(&s2));
    assert!(alive(other_pid), "the refused stop signalled another ticket's pid");

    assert!(own.try_wait().unwrap().is_none() && other.try_wait().unwrap().is_none(), "a refused stop ended a fixture child");
    assert_eq!(fs::read(&fa).unwrap(), ba, "the refused stop changed this ticket's manifest");
    assert_eq!(fs::read(&fb).unwrap(), bb, "the refused stop changed the other ticket's manifest");
    let _ = own.kill();
    let _ = own.wait();
    let _ = other.kill();
    let _ = other.wait();
}

// ---------------------------------------------------------------- #183 AC 3

/// #183 AC 3: a re-used pid is never signalled. Planted fixture: a schema-valid manifest lists a LIVE
/// foreign process (its own group leader) with `pid_start_ms` one hour off the measured real start time;
/// the test first asserts the real start time differs from the recorded one by more than 2000 ms. `stop
/// --id` exits 3, prints `pid-reused`, the process is still alive, and the entry is removed.
#[test]
fn reused_pid_is_not_signalled() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-3", &r, &d);
    let mut guard = Guard::default();

    let mut own = spawn_own_group("sleep", "60");
    let pid = own.id();
    guard.add(pid);
    let real = real_start_ms(pid);
    let recorded = real.saturating_sub(3_600_000);
    assert!(real.abs_diff(recorded) > 2000, "the fixture must carry a recorded start time far off the real one");
    plant_process(&d, "ts-3", "p-3", pid, recorded);
    let before = fs::read(d.join("ts-3.jsonl")).unwrap();

    let s = stop("ts-3", StopSel::Id("p-3"), None, &d);
    assert_eq!(code(&s), 3, "a re-used pid answers 3: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(err(&s).contains("pid-reused"), "the answer names pid-reused: stderr {:?}", err(&s));
    assert!(alive(pid), "the re-used pid was signalled");
    assert!(own.try_wait().unwrap().is_none(), "the re-used pid is dead");
    let l = run(&["manifest", "list", "ts-3", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).contains("p-3"), "the re-used entry is still listed: {}", out(&l));
    assert!(fs::read(d.join("ts-3.jsonl")).unwrap().len() > before.len(), "the entry was not removed");
    let _ = own.kill();
    let _ = own.wait();
}

// ---------------------------------------------------------------- #183 AC 4

/// #183 AC 4: a live pid that is NOT its own process-group leader (a background child of a shell the test
/// spawned as the leader; the test asserts pgid != pid before stopping), signed out through the public
/// `manifest add --class process` verb, refuses the stop: exit 2, the process alive, the manifest
/// byte-identical.
#[test]
fn non_leader_pid_is_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-4", &r, &d);
    let mut guard = Guard::default();

    let pid_file = r.join("bg");
    let script = format!("sleep 300 & echo $! > {}; wait", pid_file.to_string_lossy());
    let mut sh = spawn_sh(&script);
    let sh_pid = sh.id();
    guard.add(sh_pid);
    assert!(poll(10_000, || read_pid(&pid_file).is_some()), "the shell never wrote the background pid");
    let bg = read_pid(&pid_file).unwrap();
    assert_ne!(bg, sh_pid, "the background pid must differ from the leader's");
    assert_eq!(pgid_of(bg), sh_pid, "precondition: the background pid is NOT its own group leader");
    assert!(alive(bg), "the background pid must be live before the stop");

    let add = manifest_add_process("ts-4", "p-4", bg, &d);
    assert_eq!(code(&add), 0, "add of the non-leader: {}", err(&add));
    let file = d.join("ts-4.jsonl");
    let before = fs::read(&file).unwrap();

    let s = stop("ts-4", StopSel::Id("p-4"), None, &d);
    assert_eq!(code(&s), 2, "a non-leader pid refuses: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(alive(bg), "the refused stop signalled the non-leader pid");
    assert!(err(&s).to_lowercase().contains("group"), "the refusal says a group signal could not prove it: stderr {:?}", err(&s));
    assert_eq!(fs::read(&file).unwrap(), before, "the refused stop changed the manifest");
    let _ = sh.kill();
    let _ = sh.wait();
}

// ---------------------------------------------------------------- #183 AC 5

/// #183 AC 5: stop confirms death before answering. Fixture: a test-spawned group leader the test NEVER
/// reaps, signed out through the public verb — after the stop's KILL it is a zombie that still answers the
/// liveness probe. `stop --id --grace-ms 200` exits 4 and the entry is still listed; once the test reaps
/// it, a second stop exits 3 `already-gone` and the entry is removed.
#[test]
fn stop_confirms_death_before_answering() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-5", &r, &d);
    let mut guard = Guard::default();

    let mut own = spawn_own_group("sleep", "300");
    let pid = own.id();
    guard.add(pid);
    let add = manifest_add_process("ts-5", "p-5", pid, &d);
    assert_eq!(code(&add), 0, "add of the leader: {}", err(&add));
    assert!(alive(pid), "the leader must be live before the stop");
    assert!(own.try_wait().unwrap().is_none(), "precondition: the child has not exited, so its exit leaves a zombie the test never reaps");

    let s1 = stop("ts-5", StopSel::Id("p-5"), Some(200), &d);
    assert_eq!(code(&s1), 4, "an unconfirmable death answers 4: stdout {:?} stderr {:?}", out(&s1), err(&s1));
    assert!(err(&s1).contains("not confirmed"), "the answer names the unconfirmed death: stderr {:?}", err(&s1));
    // The unreaped zombie still answers the probe — the precondition the fixture carries.
    assert!(alive(pid), "the killed pid must still answer while it is an unreaped zombie");
    let l = run(&["manifest", "list", "ts-5", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(out(&l).contains("p-5"), "the unconfirmed entry must stay listed: {}", out(&l));

    // Reap, then the retry: the pid is provably gone, so the entry is removed with already-gone.
    let _ = own.wait();
    assert!(poll(10_000, || !alive(pid)), "the reaped pid still answers the probe");
    let s2 = stop("ts-5", StopSel::Id("p-5"), None, &d);
    assert_eq!(code(&s2), 3, "the retry after the reap answers 3: stdout {:?} stderr {:?}", out(&s2), err(&s2));
    assert!(err(&s2).contains("already-gone"), "the retry names already-gone: stderr {:?}", err(&s2));
    let l2 = run(&["manifest", "list", "ts-5", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l2), 0);
    assert!(!out(&l2).contains("p-5"), "the gone entry is still listed: {}", out(&l2));
}

// ---------------------------------------------------------------- #183 AC 7

/// #183 AC 7: an entry whose process exited on its own answers 3 `already-gone` with the entry removed; a
/// stop with NO manifest answers 2; a manifest with a torn tail answers 2 and the file is byte-identical.
#[test]
fn dead_and_missing_cases() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-7", &r, &d);
    init_manifest("ts-7t", &r, &d);
    let mut guard = Guard::default();

    // (a) an entry whose process exited on its own (spawned, signed out while alive, then reaped). The
    // fixture's lifetime is one the TEST ends (fix r2, F4): it waits for the `go` file instead of a
    // wall-clock `sleep 2` a slow CI host could run out of during the `manifest add`.
    let pid_file = r.join("short");
    let go_file = r.join("short-go");
    let script = format!("echo $$ > {}; while [ ! -f {} ]; do sleep 0.1; done", pid_file.to_string_lossy(), go_file.to_string_lossy());
    let mut own = spawn_sh(&script);
    guard.add(own.id());
    assert!(poll(10_000, || read_pid(&pid_file).is_some()), "the short program never wrote its pid");
    let pid = read_pid(&pid_file).unwrap();
    let add = manifest_add_process("ts-7", "p-7", pid, &d);
    assert_eq!(code(&add), 0, "add of the short-lived pid: {}", err(&add));
    fs::write(&go_file, b"").unwrap();
    assert!(poll(10_000, || own.try_wait().unwrap().is_some()), "the short program never exited after go");
    assert!(!alive(pid), "the reaped pid must be gone before the stop");
    let s = stop("ts-7", StopSel::Id("p-7"), None, &d);
    assert_eq!(code(&s), 3, "a dead entry answers 3: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(err(&s).contains("already-gone"), "the answer names already-gone: stderr {:?}", err(&s));
    let l = run(&["manifest", "list", "ts-7", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).contains("p-7"), "the gone entry is still listed: {}", out(&l));

    // (b) no manifest for the ticket.
    let s2 = stop("ts-7none", StopSel::Id("x-1"), None, &d);
    assert_eq!(code(&s2), 2, "a missing manifest refuses: stdout {:?} stderr {:?}", out(&s2), err(&s2));

    // (c) a torn tail: bytes with no closing newline refuse the stop and change nothing.
    let file = d.join("ts-7t.jsonl");
    let mut f = OpenOptions::new().append(true).open(&file).unwrap();
    f.write_all(b"{\"v\":1,\"op\":\"add\"").unwrap();
    drop(f);
    let before = fs::read(&file).unwrap();
    let s3 = stop("ts-7t", StopSel::Id("x-1"), None, &d);
    assert_eq!(code(&s3), 2, "a torn tail refuses: stdout {:?} stderr {:?}", out(&s3), err(&s3));
    assert!(err(&s3).contains("torn tail"), "the refusal names the torn tail: stderr {:?}", err(&s3));
    assert_eq!(fs::read(&file).unwrap(), before, "the refused stop changed the torn manifest");
}

// ---------------------------------------------------------------- #183 AC 8

/// #183 AC 8: two CONCURRENT stops of one id give {0, 2}; the loser's stderr is the pre-signal refusal
/// (`is not a live entry`), never a manifest refusal; the group is gone; exactly one `remove` line exists
/// for that id. Without the per-ticket supervisor lock both stops would pass the live-entry check and the
/// loser's remove would be refused by the manifest instead (mutant: the lock removed).
#[test]
fn two_stops_at_once() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-8", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start("ts-8", "p-8", &r, None, &d, &["sleep", "60"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    guard.add(pid);

    let d1 = d.to_string_lossy().into_owned();
    let (tx, rx) = mpsc::channel();
    for _ in 0..2 {
        let tx = tx.clone();
        let dd = d1.clone();
        thread::spawn(move || {
            let _ = tx.send(stop("ts-8", StopSel::Id("p-8"), None, Path::new(&dd)));
        });
    }
    drop(tx);
    let a = rx.recv().unwrap();
    let b = rx.recv().unwrap();
    let mut codes = [code(&a), code(&b)];
    codes.sort();
    assert_eq!(codes, [0, 2], "one stop wins, one refuses: {:?} / {:?}", (out(&a), err(&a)), (out(&b), err(&b)));
    let loser = if code(&a) == 2 { &a } else { &b };
    assert!(err(loser).contains("is not a live entry"), "the loser's refusal is the pre-signal one: stderr {:?}", err(loser));
    assert!(!err(loser).contains("manifest refused"), "the loser's refusal must not be a manifest refusal: stderr {:?}", err(loser));
    assert!(poll(10_000, || !group_alive(pid)), "the group is still alive");
    let text = fs::read_to_string(d.join("ts-8.jsonl")).unwrap();
    let removes = text.lines().filter(|l| l.contains("\"op\":\"remove\"") && l.contains("\"id\":\"p-8\"")).count();
    assert_eq!(removes, 1, "exactly one remove line for the id: {}", text);
}

// ---------------------------------------------------------------- #183 AC 9 (non-Linux)

/// #183 AC 9 (non-Linux, where the start-time read runs `ps`): with the harness child's PATH pointing at a
/// fake `ps` that prints an elapsed time MATCHING the recorded (wrong) start time, a re-used-pid fixture
/// still exits 3 `pid-reused` and is not signalled — the identity read goes through `/bin/ps` by absolute
/// path, so a PATH-shadowed `ps` cannot bless a re-used pid. (On Linux the read is /proc and PATH is
/// irrelevant, so this test is compiled out there.)
#[cfg(not(target_os = "linux"))]
#[test]
fn path_ps_liar_cannot_bless_a_reused_pid() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-9", &r, &d);
    let mut guard = Guard::default();

    let mut own = spawn_own_group("sleep", "60");
    let pid = own.id();
    guard.add(pid);
    let real = real_start_ms(pid);
    let recorded = real.saturating_sub(3_600_000);
    assert!(real.abs_diff(recorded) > 2000, "the fixture must carry a recorded start time far off the real one");
    plant_process(&d, "ts-9", "p-9", pid, recorded);

    // The fake ps prints the elapsed time the RECORDED start time implies — exactly what would bless the
    // re-used pid if the identity read consulted PATH.
    let fakebin = t.0.join("fakebin");
    fs::create_dir_all(&fakebin).unwrap();
    let fake = fakebin.join("ps");
    let recorded_s = recorded / 1000;
    fs::write(&fake, format!("#!/bin/sh\ns=$(( $(date +%s) - {recorded_s} ))\nprintf '%d:%02d\\n' $(( s / 60 )) $(( s % 60 ))\n")).unwrap();
    let mut perm = fs::metadata(&fake).unwrap().permissions();
    use std::os::unix::fs::PermissionsExt;
    perm.set_mode(0o755);
    fs::set_permissions(&fake, perm).unwrap();

    let dstr = d.to_string_lossy().into_owned();
    let v: Vec<String> = vec!["process".into(), "stop".into(), "ts-9".into(), "--id".into(), "p-9".into(), "--dir".into(), dstr];
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    let s = Command::new(BIN)
        .args(&args)
        .env("PATH", fakebin.to_str().unwrap())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(code(&s), 3, "the liar PATH cannot bless the re-used pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(err(&s).contains("pid-reused"), "the answer names pid-reused: stderr {:?}", err(&s));
    assert!(alive(pid), "the re-used pid was signalled");
    assert!(own.try_wait().unwrap().is_none(), "the re-used pid is dead");
    let _ = own.kill();
    let _ = own.wait();
}

// ---------------------------------------------------------------- #183 fix r2 (F2): members that ignore TERM

/// #183 fix r2, F2 (review repro H1): the started leader obeys TERM, but a group member TRAPS it. The head
/// code gated the KILL on the LEADER's identity re-check — a leader the stop's own TERM just killed has no
/// identity to re-read — so no KILL went, and the answer was 0 "stopped" with the entry removed while the
/// member still ran. The stop must end the member (or answer 4 and KEEP the entry); it may never answer 0
/// while any member of the group lives.
#[test]
fn stop_ends_a_group_member_that_ignores_term() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-f2a", &r, &d);
    let mut guard = Guard::default();
    let mfile = r.join("member");
    let script = format!("sh -c 'trap \"\" TERM; echo $$ > {}; while :; do sleep 1; done' & wait", mfile.to_string_lossy());
    let (s, leader) = start("ts-f2a", "p-1", &r, None, &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(leader > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    guard.add(leader);
    assert!(poll(10_000, || read_pid(&mfile).is_some()), "the member never wrote its pid");
    let member = read_pid(&mfile).unwrap();
    guard.add(member);
    assert!(alive(member) && alive(leader), "both must be live before the stop");
    assert_eq!(pgid_of(member), leader, "precondition: the member is in the started leader's group");

    let st = stop("ts-f2a", StopSel::Id("p-1"), Some(500), &d);
    assert!(
        code(&st) == 0 || code(&st) == 4,
        "a surviving member must mean a KILL or a 4, never a silent leader-only success: stdout {:?} stderr {:?}",
        out(&st),
        err(&st)
    );
    if code(&st) == 0 {
        assert!(poll(10_000, || !alive(member) && !alive(leader)), "the stop answered 0 but the group still has a member");
        let l = run(&["manifest", "list", "ts-f2a", "--dir", d.to_str().unwrap()]);
        assert_eq!(code(&l), 0);
        assert!(!out(&l).contains("p-1"), "the stopped id is still listed: {}", out(&l));
    } else {
        assert!(err(&st).contains("not confirmed"), "a 4 names the unconfirmed death: stderr {:?}", err(&st));
        let l = run(&["manifest", "list", "ts-f2a", "--dir", d.to_str().unwrap()]);
        assert_eq!(code(&l), 0);
        assert!(out(&l).contains("p-1"), "the unconfirmed entry must stay listed: {}", out(&l));
    }
}

/// #183 fix r2, F2 (held-out must-fire): TWO members of the group both ignore TERM — the group KILL must
/// end both (or the answer is 4 with the entry kept); never 0 with a survivor.
#[test]
fn stop_ends_two_members_that_ignore_term() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-f2b", &r, &d);
    let mut guard = Guard::default();
    let mfile = r.join("members");
    let script = format!(
        "sh -c 'trap \"\" TERM; echo $$ >> {}; while :; do sleep 1; done' & sh -c 'trap \"\" TERM; echo $$ >> {}; while :; do sleep 1; done' & wait",
        mfile.to_string_lossy(),
        mfile.to_string_lossy()
    );
    let (s, leader) = start("ts-f2b", "p-1", &r, None, &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(leader);
    assert!(poll(10_000, || read_pids(&mfile).len() == 2), "the members never wrote both pids");
    let members = read_pids(&mfile);
    for m in &members {
        guard.add(*m);
        assert!(alive(*m), "member {m} must be live before the stop");
        assert_eq!(pgid_of(*m), leader, "precondition: both members are in the started leader's group");
    }

    let st = stop("ts-f2b", StopSel::Id("p-1"), Some(500), &d);
    assert!(
        code(&st) == 0 || code(&st) == 4,
        "two TERM-ignoring members must mean a group KILL or a 4: stdout {:?} stderr {:?}",
        out(&st),
        err(&st)
    );
    if code(&st) == 0 {
        assert!(poll(10_000, || members.iter().all(|m| !alive(*m)) && !alive(leader)), "the stop answered 0 but a member still lives");
        let l = run(&["manifest", "list", "ts-f2b", "--dir", d.to_str().unwrap()]);
        assert_eq!(code(&l), 0);
        assert!(!out(&l).contains("p-1"), "the stopped id is still listed: {}", out(&l));
    } else {
        assert!(err(&st).contains("not confirmed"), "a 4 names the unconfirmed death: stderr {:?}", err(&st));
    }
}

/// #183 fix r2 (F2 wiring, the Alive+Same arm): the LEADER itself ignores TERM. The identity re-check must
/// still pass (the live leader IS still the recorded process), so the KILL goes and the stop answers 0
/// with the leader gone. A gate that KILLed only dead leaders (or never re-read identity) leaves this red.
#[test]
fn stop_ends_a_leader_that_ignores_term() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-f2c", &r, &d);
    let mut guard = Guard::default();
    let mfile = r.join("leader");
    let script = format!("trap \"\" TERM; echo $$ > {}; while :; do sleep 1; done", mfile.to_string_lossy());
    let (s, leader) = start("ts-f2c", "p-1", &r, None, &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(leader > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    guard.add(leader);
    assert!(poll(10_000, || read_pid(&mfile).is_some()), "the leader never wrote its pid");
    assert_eq!(read_pid(&mfile).unwrap(), leader, "the started pid is the pid the script reports");
    assert!(alive(leader), "the leader must be live before the stop");

    let st = stop("ts-f2c", StopSel::Id("p-1"), Some(500), &d);
    assert_eq!(code(&st), 0, "a live, still-ours leader that ignored TERM must be KILLed and confirmed: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert!(poll(10_000, || !alive(leader)), "the leader survived its own stop");
    let l = run(&["manifest", "list", "ts-f2c", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).contains("p-1"), "the stopped id is still listed: {}", out(&l));
}

/// #183 fix r2 (held-out must-fire): the recorded leader is ALREADY dead when the stop begins, while a
/// group member still lives. The stop must not answer 0 for a non-empty group: the sanctioned answer is
/// exit 3 `already-gone` (nothing signalled — the doc's "does not cover" line; the orphan reaper P2 owns
/// leaderless groups), and this pins that it is never 0 while the member lives.
#[test]
fn dead_leader_with_live_member_never_answers_stopped() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("ts-f2d", &r, &d);
    let mut guard = Guard::default();
    let mfile = r.join("member");
    let gofile = r.join("go");
    // The leader forks a TERM-trapping member, then waits until the TEST lets it exit (so the entry can
    // be recorded with the leader's REAL start time while it still lives): after `go`, the group
    // outlives its leader.
    let script = format!(
        "sh -c 'trap \"\" TERM; echo $$ > {}; while :; do sleep 1; done' & while [ ! -f {} ]; do sleep 0.1; done",
        mfile.to_string_lossy(),
        gofile.to_string_lossy()
    );
    let mut lead = spawn_sh(&script);
    let lead_pid = lead.id();
    guard.add(lead_pid);
    assert!(poll(10_000, || read_pid(&mfile).is_some()), "the member never wrote its pid");
    let member = read_pid(&mfile).unwrap();
    guard.add(member);
    assert!(alive(member) && alive(lead_pid), "both must be live before the recording");
    assert_eq!(pgid_of(member), lead_pid, "precondition: the member is in the leader's group");
    // Record the entry with the leader's REAL start time while it lives, then let it die and reap it.
    let recorded = real_start_ms(lead_pid);
    plant_process(&d, "ts-f2d", "p-1", lead_pid, recorded);
    fs::write(&gofile, b"").unwrap();
    assert!(poll(10_000, || lead.try_wait().unwrap().is_some()), "the leader never exited");
    assert!(!alive(lead_pid), "the leader must be gone before the stop");

    let st = stop("ts-f2d", StopSel::Id("p-1"), None, &d);
    assert_ne!(code(&st), 0, "a non-empty group must never take the 0 answer: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert_eq!(code(&st), 3, "a leader already gone answers already-gone: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert!(err(&st).contains("already-gone"), "the answer names already-gone: stderr {:?}", err(&st));
    assert!(alive(member), "the already-gone path signalled anything (it must not)");
    let l = run(&["manifest", "list", "ts-f2d", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).contains("p-1"), "the gone entry is still listed: {}", out(&l));
}

// ---------------------------------------------------------------- #207 helpers

/// Plant a schema-valid `limit` line with a CHOSEN `timeout_ms` / `ts_ms` (the fixture carrier for the
/// expired / backward-clock cases).
fn plant_limit(d: &Path, ticket: &str, id: &str, timeout_ms: u64, ts_ms: u64) {
    let line = format!(
        "{{\"v\":1,\"op\":\"limit\",\"ticket\":\"{ticket}\",\"id\":\"{id}\",\"timeout_ms\":{timeout_ms},\"ts_ms\":{ts_ms}}}\n"
    );
    let mut f = OpenOptions::new().append(true).open(d.join(format!("{ticket}.jsonl"))).unwrap();
    f.write_all(line.as_bytes()).unwrap();
}

/// `process start` with `--timeout-ms` (the #207 flag), optional `--port`.
fn start_to(ticket: &str, id: &str, cwd: &Path, port: Option<u64>, d: &Path, timeout_ms: u64, prog: &[&str]) -> (Output, u32) {
    let mut v: Vec<String> = vec![
        "process".into(), "start".into(), ticket.into(), "--id".into(), id.into(),
        "--cwd".into(), cwd.to_string_lossy().into_owned(),
        "--timeout-ms".into(), timeout_ms.to_string(),
    ];
    if let Some(p) = port {
        v.push("--port".into());
        v.push(p.to_string());
    }
    v.push("--dir".into());
    v.push(d.to_string_lossy().into_owned());
    v.push("--".into());
    for a in prog {
        v.push((*a).into());
    }
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    let o = run(&args);
    let pid = out(&o).split('\t').nth(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(0);
    (o, pid)
}

/// `process check <ticket> [--grace-ms <n>]`.
fn check(ticket: &str, grace: Option<u64>, d: &Path) -> Output {
    let mut v: Vec<String> = vec!["process".into(), "check".into(), ticket.into()];
    if let Some(g) = grace {
        v.push("--grace-ms".into());
        v.push(g.to_string());
    }
    v.push("--dir".into());
    v.push(d.to_string_lossy().into_owned());
    let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
    run(&args)
}

/// The whole seconds of a pid's `ps etime` (the same granularity the supervisor's off-Linux age read uses),
/// or None when the pid is gone or ps cannot parse.
fn etime_secs(pid: u32) -> Option<u64> {
    let o = Command::new("ps").arg("-o").arg("etime=").arg("-p").arg(pid.to_string()).stdin(Stdio::null()).output().ok()?;
    if !o.status.success() {
        return None;
    }
    let et = String::from_utf8_lossy(&o.stdout).trim().to_string();
    let main = et.split('.').next().unwrap_or(&et).to_string();
    let (days, time) = match main.split_once('-') {
        Some((d, r)) => (d.trim().parse().ok()?, r),
        None => (0, main.as_str()),
    };
    let parts: Vec<u64> = time.split(':').map(|p| p.trim().parse().ok()).collect::<Option<Vec<u64>>>()?;
    match parts.as_slice() {
        [m, s] => Some(days * 86400 + m * 60 + s),
        [h, m, s] => Some(days * 86400 + h * 3600 + m * 60 + s),
        _ => None,
    }
}

/// Poll until the pid's whole-second age reaches `secs` (2 s clears the whole-second truncation of a
/// 1000 ms timeout with margin); a state poll, never a wall-clock assert.
fn wait_past_age(pid: u32, secs: u64) {
    assert!(poll(30_000, || etime_secs(pid).is_some_and(|s| s >= secs)), "pid {pid} never reached an etime of {secs} s");
}

// ---------------------------------------------------------------- #207 AC 1

/// #207 AC 1: `start --timeout-ms` records exactly one `limit` line after the `add` line for the id; a
/// start WITHOUT the flag writes no `limit` line (the planted case).
#[test]
fn timeout_ms_records_one_limit_line() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-1", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start_to("tl-1", "a", &r, None, &d, 60_000, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1, "start printed no pid: stdout {:?} stderr {:?}", out(&s), err(&s));
    guard.add(pid);

    let (s2, pid2) = start_to("tl-1", "b", &r, None, &d, 60_000, &["sleep", "30"]);
    assert_eq!(code(&s2), 0, "second start: {}", err(&s2));
    guard.add(pid2);
    // The planted case: no flag, no limit line for that id.
    let (s3, pid3) = start("tl-1", "c", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s3), 0, "start without the flag: {}", err(&s3));
    guard.add(pid3);

    let text = fs::read_to_string(d.join("tl-1.jsonl")).unwrap();
    let limits: Vec<&str> = text.lines().filter(|l| l.contains("\"op\":\"limit\"")).collect();
    assert_eq!(limits.len(), 2, "exactly one limit line per flagged start: {}", text);
    for l in &limits {
        assert!(l.contains("\"id\":\"a\"") || l.contains("\"id\":\"b\""), "the limit line names its id: {}", l);
        assert!(l.contains("\"timeout_ms\":60000"), "the limit line carries the given timeout: {}", l);
    }
    let pos_add_a = text.lines().position(|l| l.contains("\"op\":\"add\"") && l.contains("\"id\":\"a\"")).expect("the add line for a");
    let pos_limit_a = text.lines().position(|l| l.contains("\"op\":\"limit\"") && l.contains("\"id\":\"a\"")).expect("the limit line for a");
    assert!(pos_add_a < pos_limit_a, "the limit line follows the add line for a: {}", text);
    assert!(!text.lines().any(|l| l.contains("\"id\":\"c\"") && l.contains("\"op\":\"limit\"")), "an unflagged start writes no limit line: {}", text);
}

// ---------------------------------------------------------------- #207 AC 2

/// #207 AC 2: a bad `--timeout-ms` (0, past 7 days, non-numeric, negative, `1e3`, `+5`, past u64) is a
/// parser usage error: exit 2, no manifest change, no lock file, nothing spawned (the marker never exists).
#[test]
fn bad_timeout_ms_refuses_before_anything() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-2", &r, &d);

    let marker = r.join("marker");
    let script = format!("touch {}", marker.to_string_lossy());
    let file = d.join("tl-2.jsonl");
    let before = fs::read(&file).unwrap();
    for bad in ["0", "604800001", "abc", "-5", "1e3", "+5", "99999999999999999999999"] {
        let s = run(&[
            "process", "start", "tl-2", "--id", "w-1", "--cwd", r.to_str().unwrap(),
            "--timeout-ms", bad, "--dir", d.to_str().unwrap(), "--", "/bin/sh", "-c", &script,
        ]);
        assert_eq!(code(&s), 2, "--timeout-ms {bad} must refuse: stdout {:?} stderr {:?}", out(&s), err(&s));
        assert!(err(&s).contains("timeout-ms"), "--timeout-ms {bad} must name the flag: stderr {:?}", err(&s));
        assert!(!marker.exists(), "--timeout-ms {bad}: the refused start ran the program");
        assert_eq!(fs::read(&file).unwrap(), before, "--timeout-ms {bad}: the refused start changed the manifest");
        assert!(!d.join("tl-2.proc.lock").exists(), "--timeout-ms {bad}: the refused start made a lock file");
    }
}

// ---------------------------------------------------------------- #207 AC 3

/// #207 AC 3: an entry inside its limit answers 0 with NO stdout, the manifest byte-identical, the
/// process alive.
#[test]
fn check_within_limit_is_silent() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-3", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start_to("tl-3", "a", &r, None, &d, 600_000, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let file = d.join("tl-3.jsonl");
    let before = fs::read(&file).unwrap();

    let c = check("tl-3", None, &d);
    assert_eq!(code(&c), 0, "a within-limit entry answers 0: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert_eq!(out(&c), "", "a within-limit entry prints nothing: stdout {:?}", out(&c));
    assert_eq!(fs::read(&file).unwrap(), before, "a within-limit check changed the manifest");
    assert!(alive(pid), "a within-limit check stopped the process");
}

// ---------------------------------------------------------------- #207 AC 4

/// #207 AC 4: an expired entry is stopped through stop's checks: exit 5, stdout exactly
/// `a<TAB>pid<TAB>timed_out`, the pid dead, the group empty, the manifest no longer lists the id (nor its
/// port line when a port was given).
#[test]
fn expired_entry_is_stopped() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-4", &r, &d);
    let mut guard = Guard::default();

    // A free port, retried the same way start_free_port does (the snapshot port can be taken between
    // free_port() and start's own bind check; only the already-bound refusal is retried).
    let mut done: Option<(Output, u32, u64)> = None;
    for _ in 0..5 {
        let port = free_port();
        let (s, pid) = start_to("tl-4", "a", &r, Some(port), &d, 1_000, &["sleep", "30"]);
        if !(code(&s) == 2 && err(&s).contains("already bound")) {
            done = Some((s, pid, port));
            break;
        }
    }
    let (s, pid, _port) = done.expect("5 free ports in a row were taken before the start");
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(pid > 1, "start printed no pid");
    guard.add(pid);
    wait_past_age(pid, 2);

    let c = check("tl-4", None, &d);
    assert_eq!(code(&c), 5, "an expired entry answers 5: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert_eq!(out(&c), format!("a\t{pid}\ttimed_out\n"), "the timed_out line is exact: stdout {:?}", out(&c));
    assert!(poll(10_000, || !alive(pid)), "the expired entry's pid still lives");
    assert!(poll(10_000, || !group_alive(pid)), "the expired entry's group still has a member");
    let l = run(&["manifest", "list", "tl-4", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(
        !out(&l).lines().any(|x| x.starts_with("a\t") || x.starts_with("a.port\t")),
        "the stopped id (or its port line) is still listed: {}",
        out(&l)
    );
}

// ---------------------------------------------------------------- #207 AC 5

/// #207 AC 5: a TERM-ignoring process at its limit still dies: check `--grace-ms 300` ends in the KILL leg
/// (exit 5, the group provably empty).
#[test]
fn term_ignoring_entry_at_limit_is_killed() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-5", &r, &d);
    let mut guard = Guard::default();

    let script = "trap \"\" TERM; while :; do sleep 1; done";
    let (s, pid) = start_to("tl-5", "a", &r, None, &d, 1_000, &["/bin/sh", "-c", script]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    wait_past_age(pid, 2);

    let c = check("tl-5", Some(300), &d);
    assert_eq!(code(&c), 5, "a TERM-ignoring expired entry is still resolved by the KILL leg: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert_eq!(out(&c), format!("a\t{pid}\ttimed_out\n"), "stdout {:?}", out(&c));
    assert!(poll(10_000, || !group_alive(pid)), "the KILL leg left a group member behind");
    assert!(poll(10_000, || !alive(pid)), "the KILL leg left the pid behind");
}

// ---------------------------------------------------------------- #207 AC 6

/// #207 AC 6: a recycled pid at the limit is never signalled. The fixture carries the bad condition: a live
/// stranger (its own group leader) recorded with a `pid_start_ms` an hour off the real one, plus an expired
/// limit. check answers 5 with stop's pid-reused stderr line, the stranger is alive, the entries removed.
#[test]
fn reused_pid_at_limit_is_not_signalled() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-6", &r, &d);
    let mut guard = Guard::default();

    let mut own = spawn_own_group("sleep", "60");
    let pid = own.id();
    guard.add(pid);
    let real = real_start_ms(pid);
    let recorded = real.saturating_sub(3_600_000);
    assert!(real.abs_diff(recorded) > 2000, "the fixture must carry a recorded start time far off the real one");
    plant_process(&d, "tl-6", "a", pid, recorded);
    plant_limit(&d, "tl-6", "a", 1, 1);
    // The planted timeout is 1 ms, but the off-Linux age is whole seconds: wait until the OS itself
    // reports the stranger as at least 1 s old, so the limit is provably past (a state poll).
    wait_past_age(pid, 1);
    let before = fs::read(d.join("tl-6.jsonl")).unwrap();

    let c = check("tl-6", None, &d);
    assert_eq!(code(&c), 5, "a recycled pid resolves (nothing signalled, entries removed): stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains("pid-reused"), "the answer reuses stop's pid-reused line: stderr {:?}", err(&c));
    assert!(alive(pid), "the stranger was signalled");
    assert!(own.try_wait().unwrap().is_none(), "the stranger is dead");
    assert!(fs::read(d.join("tl-6.jsonl")).unwrap().len() > before.len(), "the stale entry was not removed");
    let l = run(&["manifest", "list", "tl-6", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).contains("a\t"), "the reused entry is still listed: {}", out(&l));
    let _ = own.kill();
    let _ = own.wait();
}

// ---------------------------------------------------------------- #207 AC 7 (integration half)

/// #207 AC 7 after r1 F1: the unreadable-age refusal is reserved for a LIVE pid. The fixture is a REAL
/// started entry (a live pid and its own limit line); `check` runs under a seatbelt profile that denies
/// exec of /bin/ps ONLY — /bin/kill still runs, so liveness stays provable — so the AGE read of a
/// proven-live pid fails and check must REFUSE: exit 2, the age refusal naming the pid, nothing
/// signalled, the manifest byte-identical, the process alive. (A dead pid is NOT this case: a gone pid
/// resolves as already-gone, see `naturally_exited_entry_resolves_not_refuses`. The Linux leg reads /proc
/// instead of /bin/ps and cannot be denied this way: the pure `boot_age_ms` table and
/// `supervisor_age_reader_unreadable_is_err` carry the same rule there.)
#[test]
#[cfg(target_os = "macos")]
fn live_pid_unreadable_age_refuses_check() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-7", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start_to("tl-7", "a", &r, None, &d, 600_000, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let file = d.join("tl-7.jsonl");
    let before = fs::read(&file).unwrap();

    // The planted unrunnable helper: a profile denying exec of /bin/ps only.
    let profile = r.join("no-ps.sb");
    fs::write(&profile, "(version 1)(allow default)(deny process-exec (literal \"/bin/ps\"))\n").unwrap();
    let c = Command::new("/usr/bin/sandbox-exec")
        .arg("-f")
        .arg(profile.to_str().unwrap())
        .arg(BIN)
        .args(["process", "check", "tl-7", "--dir", d.to_str().unwrap()])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    let cs = format!("{}", c.status.code().unwrap_or(-1));
    assert_eq!(c.status.code(), Some(2), "a live pid whose age cannot be read refuses (rc {cs}): stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains(&format!("cannot read the age of pid {pid}")), "the refusal names the unreadable age of the LIVE pid: stderr {:?}", err(&c));
    assert!(err(&c).contains("cannot run ps"), "the refusal names the unrunnable helper: stderr {:?}", err(&c));
    assert!(err(&c).contains("nothing signalled"), "the refusal says nothing was signalled: stderr {:?}", err(&c));
    assert_eq!(fs::read(&file).unwrap(), before, "the refused check changed the manifest");
    assert!(alive(pid), "the refused check signalled the live process");
}

/// #207 r1 F1: a limited process that EXITS ON ITS OWN is the already-gone path, never a refusal. check
/// resolves the entry (exit 5, stop's already-gone stderr line verbatim, entries removed), and the NEXT
/// check answers 0 — the head read the age before any liveness check and answered exit 2 forever.
#[test]
fn naturally_exited_entry_resolves_not_refuses() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-7b", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start_to("tl-7b", "a", &r, None, &d, 600_000, &["sleep", "1"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    // The program finished well inside its limit on its own; wait until the OS itself reports it gone
    // (a state poll, never a wall-clock assert).
    assert!(poll(30_000, || !alive(pid)), "the program never exited on its own");

    let c = check("tl-7b", None, &d);
    assert_eq!(code(&c), 5, "a naturally-exited entry resolves as already-gone, never a refusal: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains("already-gone"), "the answer reuses stop's already-gone line: stderr {:?}", err(&c));
    assert!(err(&c).contains("nothing signalled"), "the answer says nothing was signalled: stderr {:?}", err(&c));
    let l = run(&["manifest", "list", "tl-7b", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).lines().any(|x| x.starts_with("a\t")), "the resolved entry is still listed: {}", out(&l));

    let c2 = check("tl-7b", None, &d);
    assert_eq!(code(&c2), 0, "the second check answers 0, not exit 2 again: stdout {:?} stderr {:?}", out(&c2), err(&c2));
    assert_eq!(out(&c2), "", "the second check prints nothing: stdout {:?}", out(&c2));
}

/// #207 r1 F3: check's exit 4 reached end to end, then the r1 F1 resolution. Fixture (the #183 AC 5
/// technique): a test-spawned leader the test NEVER reaps, signed out through the public verb, carrying
/// an expired planted limit. The stop inside check signals it, but the unreaped zombie still answers the
/// liveness probe, so its death is not confirmed: exit 4, the entry KEPT (still listed). Once the test
/// reaps the zombie, the next check resolves the entry as already-gone (exit 5, entries removed).
#[test]
fn expired_entry_unproven_death_answers_4_then_resolves() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-7c", &r, &d);
    let mut guard = Guard::default();

    let mut own = spawn_own_group("sleep", "300");
    let pid = own.id();
    guard.add(pid);
    let add = manifest_add_process("tl-7c", "z", pid, &d);
    assert_eq!(code(&add), 0, "add of the leader: {}", err(&add));
    plant_limit(&d, "tl-7c", "z", 1, 1);
    // The planted timeout is 1 ms, but the off-Linux age is whole seconds: wait until the OS itself
    // reports the process as at least 1 s old, so the limit is provably past (a state poll).
    wait_past_age(pid, 1);

    let c1 = check("tl-7c", Some(200), &d);
    assert_eq!(code(&c1), 4, "an expired entry whose death cannot be proven answers 4: stdout {:?} stderr {:?}", out(&c1), err(&c1));
    assert!(err(&c1).contains("not confirmed"), "the answer names the unconfirmed death: stderr {:?}", err(&c1));
    let l = run(&["manifest", "list", "tl-7c", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(out(&l).lines().any(|x| x.starts_with("z\t")), "the unconfirmed entry must stay listed: {}", out(&l));

    // Reap the zombie, then the retry: the pid is provably gone, so check resolves it as already-gone.
    let _ = own.wait();
    assert!(poll(10_000, || !alive(pid)), "the reaped pid still answers the probe");
    let c2 = check("tl-7c", None, &d);
    assert_eq!(code(&c2), 5, "the retry after the reap resolves the entry: stdout {:?} stderr {:?}", out(&c2), err(&c2));
    assert!(err(&c2).contains("already-gone"), "the retry names already-gone: stderr {:?}", err(&c2));
    let l2 = run(&["manifest", "list", "tl-7c", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l2), 0);
    assert!(!out(&l2).lines().any(|x| x.starts_with("z\t")), "the resolved entry is still listed: {}", out(&l2));
}

// ---------------------------------------------------------------- #207 AC 9 (integration half)

/// #207 AC 9: a manifest whose `limit` lines break a reader rule (here: a second limit for one id) refuses
/// BOTH check and start with exit 2 and the file byte-identical; the planted decoder case (a process `add`
/// line with no `pid_start_ms`) refuses check the same way.
#[test]
fn limit_reader_rules_refuse_check_and_start() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-9", &r, &d);

    plant_process(&d, "tl-9", "a", 999999, 1730000000000);
    plant_limit(&d, "tl-9", "a", 60_000, 1730000000001);
    plant_limit(&d, "tl-9", "a", 60_000, 1730000000002);
    let file = d.join("tl-9.jsonl");
    let before = fs::read(&file).unwrap();

    let c = check("tl-9", None, &d);
    assert_eq!(code(&c), 2, "a second limit line is corruption: stdout {:?} stderr {:?}", out(&c), err(&c));
    assert!(err(&c).contains("corrupt"), "the refusal names corruption: stderr {:?}", err(&c));
    assert_eq!(fs::read(&file).unwrap(), before, "the refused check changed the manifest");

    let s = run(&["process", "start", "tl-9", "--id", "b", "--cwd", r.to_str().unwrap(), "--timeout-ms", "60000", "--dir", d.to_str().unwrap(), "--", "sleep", "30"]);
    assert_eq!(code(&s), 2, "a corrupt manifest refuses the start: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert_eq!(fs::read(&file).unwrap(), before, "the refused start changed the manifest");

    // The planted decoder case: a process add line with no pid_start_ms key is corruption already.
    let t2 = Tmp::new();
    let (r2, _o2, d2) = t2.fence();
    init_manifest("tl-9b", &r2, &d2);
    let mut f = OpenOptions::new().append(true).open(d2.join("tl-9b.jsonl")).unwrap();
    f.write_all(b"{\"v\":1,\"op\":\"add\",\"ticket\":\"tl-9b\",\"id\":\"a\",\"class\":\"process\",\"pid\":999999,\"ts_ms\":1730000000002}\n").unwrap();
    drop(f);
    let before2 = fs::read(d2.join("tl-9b.jsonl")).unwrap();
    let c2 = check("tl-9b", None, &d2);
    assert_eq!(code(&c2), 2, "an add line without pid_start_ms is corruption: stdout {:?} stderr {:?}", out(&c2), err(&c2));
    assert_eq!(fs::read(d2.join("tl-9b.jsonl")).unwrap(), before2, "the refused check changed the manifest");
}

// ---------------------------------------------------------------- #207 AC 10

/// #207 AC 10: past the soft line check writes ONE `check_in` event and says carry on; below it nothing is
/// written. `events verify` accepts the log; a live ticket process stays alive.
#[test]
fn soft_limit_writes_check_in_and_continues() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-10", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start("tl-10", "a", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let log = r.join("events.jsonl");

    let st = run(&[
        "process", "step", "tl-10", "--repo", "r", "--steps", "5", "--soft", "5", "--hard", "9",
        "--dir", d.to_str().unwrap(), "--log", log.to_str().unwrap(), "--root", r.to_str().unwrap(),
    ]);
    assert_eq!(code(&st), 0, "the soft line says carry on: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert_eq!(out(&st), "check_in\t5\t5\n", "stdout {:?}", out(&st));
    let text = fs::read_to_string(&log).unwrap();
    let check_ins: Vec<&str> = text.lines().filter(|l| l.contains("\"kind\":\"check_in\"")).collect();
    assert_eq!(check_ins.len(), 1, "exactly one check_in line: {}", text);
    assert!(check_ins[0].contains("\"steps\":5"), "the event carries steps: {}", check_ins[0]);
    assert!(check_ins[0].contains("\"soft_limit\":5"), "the event carries soft_limit: {}", check_ins[0]);
    assert!(check_ins[0].contains("\"ticket\":\"tl-10\""), "the event carries the ticket: {}", check_ins[0]);
    assert!(check_ins[0].contains("\"repo\":\"r\""), "the event carries the repo: {}", check_ins[0]);
    let v = run(&["events", "verify", "--log", log.to_str().unwrap()]);
    assert_eq!(code(&v), 0, "events verify accepts the log: {}", err(&v));
    assert!(alive(pid), "the soft line stopped the live process");

    // r1 F4: steps != soft. The event must carry the two counts as DIFFERENT numbers (steps 7, soft 5),
    // and stdout's second column is the soft line — so a swap of the two (the review's mutant O4,
    // writing `steps` where `soft_limit` belongs) goes red here.
    let st15 = run(&[
        "process", "step", "tl-10", "--repo", "r", "--steps", "7", "--soft", "5", "--hard", "9",
        "--dir", d.to_str().unwrap(), "--log", log.to_str().unwrap(), "--root", r.to_str().unwrap(),
    ]);
    assert_eq!(code(&st15), 0, "steps 7 soft 5 says carry on: stdout {:?} stderr {:?}", out(&st15), err(&st15));
    assert_eq!(out(&st15), "check_in\t7\t5\n", "stdout carries the step then the soft line: stdout {:?}", out(&st15));
    let text15 = fs::read_to_string(&log).unwrap();
    let ci7: Vec<&str> = text15.lines().filter(|l| l.contains("\"kind\":\"check_in\"") && l.contains("\"steps\":7")).collect();
    assert_eq!(ci7.len(), 1, "exactly one check_in for steps 7: {}", text15);
    assert!(ci7[0].contains("\"soft_limit\":5"), "the event carries soft_limit 5, not steps 7: {}", ci7[0]);

    let before = fs::read(&log).unwrap();
    let st2 = run(&[
        "process", "step", "tl-10", "--repo", "r", "--steps", "4", "--soft", "5", "--hard", "9",
        "--dir", d.to_str().unwrap(), "--log", log.to_str().unwrap(), "--root", r.to_str().unwrap(),
    ]);
    assert_eq!(code(&st2), 0, "below the soft line answers 0: stdout {:?} stderr {:?}", out(&st2), err(&st2));
    assert_eq!(out(&st2), "", "below the soft line prints nothing: stdout {:?}", out(&st2));
    assert_eq!(fs::read(&log).unwrap(), before, "below the soft line changed the log");
}

// ---------------------------------------------------------------- #207 AC 11

/// #207 AC 11: at the hard line the event is written FIRST, then every live process of the ticket is
/// stopped: exit 6, stdout starts `step_limit_stop<TAB>9<TAB>9` and holds one stopped line per process, the
/// log gains exactly one step_limit_stop line, both groups empty, no process left listed. With ZERO live
/// processes the answer is still 6 and the event is written.
#[test]
fn hard_limit_writes_step_limit_stop_then_stops_all() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-11", &r, &d);
    let mut guard = Guard::default();

    let (s1, p1) = start("tl-11", "a", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s1), 0, "start a: {}", err(&s1));
    guard.add(p1);
    let (s2, p2) = start("tl-11", "b", &r, None, &d, &["/bin/sh", "-c", "trap \"\" TERM; while :; do sleep 1; done"]);
    assert_eq!(code(&s2), 0, "start b: {}", err(&s2));
    guard.add(p2);
    let log = r.join("events.jsonl");

    let st = run(&[
        "process", "step", "tl-11", "--repo", "r", "--steps", "9", "--soft", "5", "--hard", "9",
        "--grace-ms", "500", "--dir", d.to_str().unwrap(), "--log", log.to_str().unwrap(), "--root", r.to_str().unwrap(),
    ]);
    assert_eq!(code(&st), 6, "the hard line with every death proven answers 6: stdout {:?} stderr {:?}", out(&st), err(&st));
    let text_out = out(&st);
    let lines: Vec<&str> = text_out.lines().collect();
    assert_eq!(lines.first(), Some(&"step_limit_stop\t9\t9"), "stdout starts with the header: stdout {:?}", text_out);
    assert_eq!(lines.len(), 3, "the header plus one stopped line per process: stdout {:?}", out(&st));
    for (id, pid) in [("a", p1), ("b", p2)] {
        assert!(lines.contains(&format!("{id}\t{pid}\tstopped").as_str()), "stdout holds the stopped line for {id}: stdout {:?}", out(&st));
        assert!(poll(10_000, || !group_alive(pid)), "the group of {id} still has a member");
    }
    let text = fs::read_to_string(&log).unwrap();
    let stops: Vec<&str> = text.lines().filter(|l| l.contains("\"kind\":\"step_limit_stop\"")).collect();
    assert_eq!(stops.len(), 1, "exactly one step_limit_stop line: {}", text);
    assert!(stops[0].contains("\"steps\":9") && stops[0].contains("\"hard_limit\":9"), "the event carries the counts: {}", stops[0]);
    let l = run(&["manifest", "list", "tl-11", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert!(!out(&l).contains("process"), "a process is still listed: {}", out(&l));

    // Zero live processes: still 6, the event still written.
    let log2 = r.join("events2.jsonl");
    let st2 = run(&[
        "process", "step", "tl-11", "--repo", "r", "--steps", "9", "--soft", "5", "--hard", "9",
        "--dir", d.to_str().unwrap(), "--log", log2.to_str().unwrap(), "--root", r.to_str().unwrap(),
    ]);
    assert_eq!(code(&st2), 6, "the hard line with nothing live answers 6: stdout {:?} stderr {:?}", out(&st2), err(&st2));
    assert!(fs::read_to_string(&log2).unwrap().contains("\"kind\":\"step_limit_stop\""), "the event was written");
}

// ---------------------------------------------------------------- #207 AC 13

/// #207 AC 13: when the event CANNOT be written (a `--log` outside `--root`, or a torn tail), step answers 2
/// with `harness: EVENT NOT RECORDED`, at BOTH the soft and the hard level; at the hard level nothing is
/// signalled and the manifest is byte-identical.
#[test]
fn unwritable_event_signals_nothing() {
    let t = Tmp::new();
    let (r, o, d) = t.fence();
    init_manifest("tl-13", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start("tl-13", "a", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    let outside = o.join("events.jsonl");
    let file = d.join("tl-13.jsonl");
    let before = fs::read(&file).unwrap();

    for steps in ["5", "9"] {
        let st = run(&[
            "process", "step", "tl-13", "--repo", "r", "--steps", steps, "--soft", "5", "--hard", "9",
            "--dir", d.to_str().unwrap(), "--log", outside.to_str().unwrap(), "--root", r.to_str().unwrap(),
        ]);
        assert_eq!(code(&st), 2, "steps {steps}: an unwritable event refuses: stdout {:?} stderr {:?}", out(&st), err(&st));
        assert!(err(&st).contains("harness: EVENT NOT RECORDED"), "steps {steps}: the refusal says the event was not recorded: stderr {:?}", err(&st));
        assert!(alive(pid), "steps {steps}: nothing was signalled");
        assert_eq!(fs::read(&file).unwrap(), before, "steps {steps}: the manifest changed");
    }

    // A torn tail on the log refuses the same way (the log exists, inside the root, without a final newline).
    let torn = r.join("torn.jsonl");
    let mut f = OpenOptions::new().append(true).create(true).open(&torn).unwrap();
    f.write_all(b"{\"v\":1,\"kind\":\"check_in\"").unwrap();
    drop(f);
    let st = run(&[
        "process", "step", "tl-13", "--repo", "r", "--steps", "5", "--soft", "5", "--hard", "9",
        "--dir", d.to_str().unwrap(), "--log", torn.to_str().unwrap(), "--root", r.to_str().unwrap(),
    ]);
    assert_eq!(code(&st), 2, "a torn tail refuses: stdout {:?} stderr {:?}", out(&st), err(&st));
    assert!(err(&st).contains("harness: EVENT NOT RECORDED"), "the torn tail says the event was not recorded: stderr {:?}", err(&st));
    assert!(alive(pid), "the torn-tail refusal signalled something");
    assert_eq!(fs::read(&file).unwrap(), before, "the torn-tail refusal changed the manifest");
}

// ---------------------------------------------------------------- #207 AC 14

/// #207 AC 14: bad step limits and inputs refuse with 2, the log and the manifest unchanged: `--soft 0`,
/// `--soft == --hard`, `--soft > --hard`, `--hard` past 1000000, a non-numeric `--steps`, a missing
/// `--repo`, a non-token `--repo`.
#[test]
fn bad_step_limits_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-14", &r, &d);

    let log = r.join("events.jsonl");
    let file = d.join("tl-14.jsonl");
    let before = fs::read(&file).unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("soft 0", vec!["--steps", "5", "--soft", "0", "--hard", "9", "--repo", "r"]),
        ("soft == hard", vec!["--steps", "9", "--soft", "9", "--hard", "9", "--repo", "r"]),
        ("soft > hard", vec!["--steps", "9", "--soft", "10", "--hard", "9", "--repo", "r"]),
        ("hard past 1000000", vec!["--steps", "9", "--soft", "5", "--hard", "1000001", "--repo", "r"]),
        ("steps abc", vec!["--steps", "abc", "--soft", "5", "--hard", "9", "--repo", "r"]),
        ("missing --repo", vec!["--steps", "5", "--soft", "5", "--hard", "9"]),
        ("non-token repo", vec!["--steps", "5", "--soft", "5", "--hard", "9", "--repo", "bad repo"]),
    ];
    for (name, extra) in cases {
        let mut v: Vec<String> = vec!["process".into(), "step".into(), "tl-14".into()];
        for a in &extra {
            v.push((*a).into());
        }
        v.push("--dir".into());
        v.push(d.to_string_lossy().into_owned());
        v.push("--log".into());
        v.push(log.to_string_lossy().into_owned());
        v.push("--root".into());
        v.push(r.to_string_lossy().into_owned());
        let args: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
        let st = run(&args);
        assert_eq!(code(&st), 2, "{name} must refuse: stdout {:?} stderr {:?}", out(&st), err(&st));
        assert_eq!(fs::read(&file).unwrap(), before, "{name}: the manifest changed");
        assert!(!log.exists(), "{name}: the refused step made a log");
    }
}

// ---------------------------------------------------------------- #207 AC 15

/// #207 AC 15: 20 rounds of check racing stop on one expired entry. Every round: exactly ONE of them
/// reports the stop (check 5 with the timed_out line, or stop 0 with the stopped line), the other answers
/// its not-live outcome (check 0, or stop 2), and the group ends empty. Without the per-ticket lock around
/// check's read BOTH would signal (a double answer or a 4).
#[test]
fn concurrent_check_and_stop() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("tl-15", &r, &d);

    for round in 0..20 {
        let mut guard = Guard::default();
        let (s, pid) = start_to("tl-15", "a", &r, None, &d, 1, &["sleep", "30"]);
        assert_eq!(code(&s), 0, "round {round} start: {}", err(&s));
        assert!(pid > 1, "round {round}: start printed no pid");
        guard.add(pid);
        wait_past_age(pid, 1);

        let dd = d.to_string_lossy().into_owned();
        let (tx, rx) = mpsc::channel();
        {
            let tx = tx.clone();
            let dd = dd.clone();
            thread::spawn(move || {
                let _ = tx.send(check("tl-15", None, Path::new(&dd)));
            });
        }
        {
            let tx = tx.clone();
            let dd = dd.clone();
            thread::spawn(move || {
                let _ = tx.send(stop("tl-15", StopSel::Id("a"), None, Path::new(&dd)));
            });
        }
        drop(tx);
        let a = rx.recv().unwrap();
        let b = rx.recv().unwrap();
        // Which output is which: the stop side is the one that printed a stopped line or the not-live
        // refusal; the check side is the other.
        let is_stop = |o: &Output| out(o).contains("\tstopped\n") || err(o).contains("is not a live entry");
        let (co, so) = if is_stop(&a) { (&b, &a) } else { (&a, &b) };
        // Exactly ONE side reported the stop: check 5 with the timed_out line, or stop 0 with the stopped line.
        let winner = (code(co) == 5 && out(co) == format!("a\t{pid}\ttimed_out\n"))
            || (code(so) == 0 && out(so) == format!("a\t{pid}\tstopped\n"));
        assert!(winner, "round {round}: exactly one side reports the stop: check {:?}/{:?} stop {:?}/{:?}", out(co), err(co), out(so), err(so));
        // The loser answers its not-live outcome: check 0 with no output, or stop 2 with the not-live refusal.
        let loser = (code(co) == 0 && out(co).is_empty()) || (code(so) == 2 && err(so).contains("is not a live entry"));
        assert!(loser, "round {round}: the loser answers its not-live outcome: check {:?}/{:?} stop {:?}/{:?}", out(co), err(co), out(so), err(so));
        assert!(poll(10_000, || !group_alive(pid)), "round {round}: the group is still alive");
    }
}
