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

    // (a) an entry whose process exited on its own (spawned, signed out while alive, then reaped).
    let pid_file = r.join("short");
    let script = format!("echo $$ > {}; sleep 2", pid_file.to_string_lossy());
    let mut own = spawn_sh(&script);
    guard.add(own.id());
    assert!(poll(10_000, || read_pid(&pid_file).is_some()), "the short program never wrote its pid");
    let pid = read_pid(&pid_file).unwrap();
    let add = manifest_add_process("ts-7", "p-7", pid, &d);
    assert_eq!(code(&add), 0, "add of the short-lived pid: {}", err(&add));
    assert!(poll(10_000, || own.try_wait().unwrap().is_some()), "the short program never exited");
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
