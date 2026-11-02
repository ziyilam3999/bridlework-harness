//! Process-level tests for `harness process start` (P1B-05). Every check runs the real binary as a child
//! process, spawns REAL programs, and reads the real manifest file, so the exit-code contract (0 started,
//! 2 usage error or any refusal with nothing started, signalled or written) and "a refused start leaves
//! nothing running that the sheet does not list" are exercised end to end. `process stop` is NOT a verb of
//! this PR (follow-up ticket #183, stop-by-pid): one test pins the usage refusal. Every wait is a bounded
//! poll, every port a listener the test itself holds or a fresh free port it retries, and every process a
//! test spawns itself is registered in a guard that kills its whole group on drop, so a failed run leaves
//! nothing behind.
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

// ---------------------------------------------------------------- fix round 3: stop is split out

/// Fix round 3, item 1: `process stop` is not a verb of this PR (the operator split stop-by-pid into its own
/// follow-up ticket #183), so ANY stop invocation is the usage refusal — exit 2, the usage line on stderr
/// (naming `start` only), nothing appended to any manifest, nothing signalled. RED on the round-2 code, where
/// `stop --pid` of a live process signalled it and answered 0.
#[test]
fn process_stop_is_not_a_verb() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-sv", &r, &d);
    let mut guard = Guard::default();

    // A live process no sheet lists: a stop of it must not signal it.
    let own = spawn_own_group("sleep", "60");
    let own_pid = own.id();
    guard.add(own_pid);
    let file = d.join("t-sv.jsonl");
    let before = fs::read(&file).unwrap();
    let dstr = d.to_str().unwrap();
    for args in [
        vec!["process", "stop"],
        vec!["process", "stop", "t-sv", "--id", "x-1"],
        vec!["process", "stop", "t-sv", "--id", "x-1", "--dir", dstr],
        vec!["process", "stop", "t-sv", "--pid", &own_pid.to_string(), "--dir", dstr, "--grace-ms", "10"],
    ] {
        let s = run(&args);
        assert_eq!(code(&s), 2, "{:?} must be the usage refusal: stdout {:?} stderr {:?}", args, out(&s), err(&s));
        assert!(err(&s).contains("usage:"), "{:?} must print the usage line: stderr {:?}", args, err(&s));
        assert!(err(&s).contains("process start"), "{:?} usage names the start verb: stderr {:?}", args, err(&s));
        assert!(!err(&s).contains("process stop"), "{:?} usage must not name a stop verb: stderr {:?}", args, err(&s));
        assert_eq!(fs::read(&file).unwrap(), before, "{:?} the refused stop changed the manifest", args);
    }
    assert!(alive(own_pid), "stop signalled a process");
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

// ---------------------------------------------------------------- fix round 3: the rollback is bounded and PATH-free

/// Fix round 3, item 2: a refused start whose rollback cannot reach its helpers must return FAST, answer 2, and
/// leave nothing running and nothing listed. Two cases:
///
/// (A) PATH points at an EMPTY folder, so the start-time read (`ps`, looked up through PATH) cannot run and the
///     start refuses right after the spawn. The rollback still ends the child — `/bin/kill` is called by
///     absolute path and the std `Child::kill()` needs no PATH — and never blocks in `wait()` on a child it
///     could not signal (r3 H1d: the round-2 code answered only after 8033 ms, and a server would never).
/// (B) the std fallback is load-bearing on its own: a child that MOVES OUT of the group the rollback
///     signals (`setpgid` back into the parent's group — a spawned leader cannot `setsid`, but it may join
///     another group of its session) is out of reach of every group signal — `/bin/kill -TERM -- -pid`
///     answers "no such process" — so only the std kill of the direct pid ends it. The refusal is engineered
///     to land only AFTER the escape is provable: the test holds the manifest lock so the start blocks inside
///     its process-line append, waits until the child provably left the group, then injects a duplicate live
///     id so the append is refused and the rollback runs against an already-escaped child.
#[test]
fn refused_start_rollback_is_bounded_and_leaves_nothing_running() {
    // (A) empty PATH: the refusal comes from the start-time read, after the spawn. The pid is parsed
    // from the refusal (it names "the started pid <n>"): the rollback is now fast enough to end the
    // child before any pid file the child might write could land, so the file proves nothing here.
    {
        let t = Tmp::new();
        let (r, _o, d) = t.fence();
        init_manifest("t-r1", &r, &d);
        let mut guard = Guard::default();
        let empty_bin = t.0.join("emptybin");
        fs::create_dir_all(&empty_bin).unwrap();
        let file = d.join("t-r1.jsonl");
        let before = fs::read(&file).unwrap();
        let t0 = Instant::now();
        let s = Command::new(BIN)
            .args([
                "process", "start", "t-r1", "--id", "a-1", "--cwd", r.to_str().unwrap(),
                "--dir", d.to_str().unwrap(), "--", "/bin/sleep", "30",
            ])
            .env("PATH", empty_bin.to_str().unwrap())
            .stdin(Stdio::null())
            .output()
            .unwrap();
        let elapsed = t0.elapsed();
        assert_eq!(code(&s), 2, "an unreadable start time refuses the start: stdout {:?} stderr {:?}", out(&s), err(&s));
        assert!(err(&s).contains("cannot read the start time"), "the refusal names the start-time read: stderr {:?}", err(&s));
        assert!(
            elapsed < Duration::from_secs(3),
            "the refused start answered in {:?}; the rollback must not block on a child it could not signal",
            elapsed
        );
        let needle = "the started pid ";
        let at = err(&s).find(needle).expect("the refusal names the started pid");
        let digits: String = err(&s)[at + needle.len()..].chars().take_while(|c| c.is_ascii_digit()).collect();
        let pid: u32 = digits.parse().expect("the named pid is a number");
        guard.add(pid);
        assert!(poll(5_000, || !alive(pid)), "the child the refused start spawned is still alive");
        assert_eq!(fs::read(&file).unwrap(), before, "the refused start wrote the manifest");
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
    let inject = format!(
        "{{\"v\":1,\"op\":\"add\",\"ticket\":\"t-r2\",\"id\":\"b-1\",\"class\":\"process\",\"pid\":999999,\"pid_start_ms\":1730000000000,\"ts_ms\":1730000000002}}\n"
    );
    lock.write_all(inject.as_bytes()).unwrap();
    lock.sync_all().unwrap();
    drop(lock);
    let t0 = Instant::now();
    let s = rx.recv().expect("the run answers once unblocked");
    let elapsed = t0.elapsed();
    assert_eq!(code(&s), 2, "the duplicate live id refuses the start: stdout {:?} stderr {:?}", out(&s), err(&s));
    assert!(err(&s).contains("duplicate live id"), "the refusal names the append failure: stderr {:?}", err(&s));
    assert!(
        elapsed < Duration::from_secs(3),
        "the rollback answered in {:?}; it must not wait on a child the group signal cannot reach",
        elapsed
    );
    assert!(poll(5_000, || !alive(pid)), "the child that left the signalled group still lives: only the std kill reaches it");
}
