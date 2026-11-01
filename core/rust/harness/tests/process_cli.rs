//! Process-level tests for `harness process start|stop` (P1B-05). Every check runs the real binary as a child
//! process, spawns REAL programs, and reads the real manifest file, so the exit-code contract (0 started / stopped,
//! 2 usage error or refusal with nothing started, signalled or written, 3 already-gone / pid-reused) and "a refused
//! start leaves nothing running that the sheet does not list" are exercised end to end. Every wait is a bounded
//! poll, and every process a test spawns itself is registered in a guard that kills its whole group on drop, so a
//! failed run leaves nothing behind.
#![cfg(unix)]
use std::fs;
use std::net::TcpListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
fn poll(ms: u64, f: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        thread::sleep(Duration::from_millis(25));
    }
    f()
}

/// Kills the whole process group of every registered pid when dropped (TERM, then KILL), so a failed test run
/// leaves nothing behind. Only pids spawned in their OWN group (process_group(0)) may be registered.
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

/// The wall clock in ms since the epoch (for hand-written pid_start_ms values).
fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64
}

// ---------------------------------------------------------------- AC 1

/// AC 1: after `start` with `--port`, `manifest list` shows the process line with the live pid and the `<id>.port`
/// line, and `manifest owns --pid` exits 0. A `stop` then ends it and empties the sheet.
#[test]
fn start_records_process_and_port() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-1", &r, &d);
    let port = free_port();
    let mut guard = Guard::default();

    let (s, pid) = start("t-1", "p-1", &r, Some(port), &d, &["sleep", "30"]);
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

    let stop = run(&["process", "stop", "t-1", "--id", "p-1", "--dir", d.to_str().unwrap(), "--grace-ms", "3000"]);
    assert_eq!(code(&stop), 0, "stop: {}", err(&stop));
    let l2 = run(&["manifest", "list", "t-1", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l2), 0);
    assert_eq!(out(&l2), "", "after the stop the sheet lists nothing");
}

// ---------------------------------------------------------------- AC 2

/// AC 2: a program that forks a grandchild and writes both pids to a file in the root is stopped; afterwards
/// neither pid exists and both entries (the process and its port) are removed.
#[test]
fn stop_ends_process_and_its_children() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-2", &r, &d);
    let port = free_port();
    let mut guard = Guard::default();

    // /bin/sh stays as the group leader, forks `sleep 300` in the SAME group, and writes both pids into the root.
    let pids_file = r.join("pids");
    let script = format!("sleep 300 & echo \"$$ $!\" > {}; wait", pids_file.to_string_lossy());
    let (s, leader) = start("t-2", "c-1", &r, Some(port), &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    assert!(leader > 1);
    guard.add(leader);
    assert!(
        poll(10_000, || pids_file.exists()),
        "the program never wrote its pids file"
    );
    let written = fs::read_to_string(&pids_file).unwrap();
    let pids: Vec<u32> = written.split_whitespace().map(|x| x.parse().unwrap()).collect();
    assert_eq!(pids.len(), 2, "pids file holds leader and grandchild: {:?}", written);
    assert_eq!(pids[0], leader, "the group leader the supervisor recorded is the shell");
    let grandchild = pids[1];
    guard.add(grandchild);
    assert!(alive(leader) && alive(grandchild), "both live before the stop");

    let stop = run(&["process", "stop", "t-2", "--id", "c-1", "--dir", d.to_str().unwrap(), "--grace-ms", "5000"]);
    assert_eq!(code(&stop), 0, "stop: {}", err(&stop));
    assert!(
        poll(5_000, || !alive(leader) && !alive(grandchild)),
        "the stop ended the leader but its child still runs (leader alive: {}, grandchild alive: {})",
        alive(leader),
        alive(grandchild)
    );

    let l = run(&["manifest", "list", "t-2", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert_eq!(out(&l), "", "both entries are removed");
}

// ---------------------------------------------------------------- AC 3

/// AC 3: `stop --pid` of a live process the test spawned itself (not on the sheet), and of a pid listed only on
/// ANOTHER ticket's manifest, each exit 2, and both processes are still alive afterwards.
#[test]
fn unlisted_pid_is_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-a", &r, &d);
    let mut guard = Guard::default();

    // A live process no sheet lists.
    let own = spawn_own_group("sleep", "60");
    let own_pid = own.id();
    guard.add(own_pid);
    let s1 = run(&["process", "stop", "t-a", "--pid", &own_pid.to_string(), "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&s1), 2, "a pid not on the sheet is refused, got {}: stdout {:?} stderr {:?}", code(&s1), out(&s1), err(&s1));
    assert!(alive(own_pid), "the unlisted process was signalled");

    // A live pid listed only on ANOTHER ticket's manifest.
    init_manifest("t-b", &r, &d);
    let (s2, other) = start("t-b", "b-1", &r, None, &d, &["sleep", "60"]);
    assert_eq!(code(&s2), 0, "start on t-b: {}", err(&s2));
    guard.add(other);
    let s3 = run(&["process", "stop", "t-a", "--pid", &other.to_string(), "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&s3), 2, "another ticket's pid is refused: stdout {:?} stderr {:?}", out(&s3), err(&s3));
    assert!(alive(other), "the other ticket's process was signalled");

    // Cleanup: stop it through its own ticket.
    let stop = run(&["process", "stop", "t-b", "--id", "b-1", "--dir", d.to_str().unwrap(), "--grace-ms", "3000"]);
    assert_eq!(code(&stop), 0, "stop on t-b: {}", err(&stop));
}

// ---------------------------------------------------------------- AC 4

/// AC 4: a hand-written, schema-valid manifest lists a live foreign process with a `pid_start_ms` one hour off;
/// `stop --id` exits 3 printing `pid-reused`, the foreign process is still alive, and the entry is removed.
#[test]
fn reused_pid_is_not_signalled() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    let mut guard = Guard::default();

    let foreign = spawn_own_group("sleep", "60");
    let pid = foreign.id();
    guard.add(pid);

    // One hour off the real start time: on Linux (exact) and off Linux (2000 ms tolerance) alike, a different process.
    let hour_off = now_ms() + 3_600_000;
    let text = format!(
        "{{\"v\":1,\"op\":\"root\",\"ticket\":\"t-4\",\"path\":\"{}\",\"ts_ms\":1730000000000}}\n\
         {{\"v\":1,\"op\":\"add\",\"ticket\":\"t-4\",\"id\":\"p-1\",\"class\":\"process\",\"pid\":{},\"pid_start_ms\":{},\"ts_ms\":1730000000001}}\n",
        r.to_string_lossy(),
        pid,
        hour_off
    );
    fs::write(d.join("t-4.jsonl"), text).unwrap();

    let stop = run(&["process", "stop", "t-4", "--id", "p-1", "--dir", d.to_str().unwrap(), "--grace-ms", "2000"]);
    assert_eq!(code(&stop), 3, "a re-used pid answers 3, got {}: stdout {:?} stderr {:?}", code(&stop), out(&stop), err(&stop));
    assert!(out(&stop).contains("pid-reused"), "stop prints pid-reused: stdout {:?}", out(&stop));
    assert!(alive(pid), "the foreign process at the re-used pid was signalled");
    let l = run(&["manifest", "list", "t-4", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert_eq!(out(&l), "", "the stale entry is removed");
}

// ---------------------------------------------------------------- AC 5

/// AC 5: stopping an entry whose process has exited exits 3 (`already-gone`); stop with no manifest file exits 2;
/// stop on a manifest with a torn tail exits 2 and the file is byte-identical.
#[test]
fn dead_and_missing_cases() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    let mut guard = Guard::default();

    // An entry whose process has already exited.
    init_manifest("t-5", &r, &d);
    let (s, pid) = start("t-5", "d-1", &r, None, &d, &["/bin/sh", "-c", "sleep 1"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);
    assert!(poll(10_000, || !alive(pid)), "the short-lived program never exited");
    let stop = run(&["process", "stop", "t-5", "--id", "d-1", "--dir", d.to_str().unwrap(), "--grace-ms", "2000"]);
    assert_eq!(code(&stop), 3, "a dead entry answers 3, got {}: stdout {:?} stderr {:?}", code(&stop), out(&stop), err(&stop));
    assert!(out(&stop).contains("already-gone"), "stop prints already-gone: stdout {:?}", out(&stop));
    let l = run(&["manifest", "list", "t-5", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&l), 0);
    assert_eq!(out(&l), "", "the dead entry is removed");

    // No manifest file at all.
    let s2 = run(&["process", "stop", "t-none", "--id", "x-1", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&s2), 2, "stop without a manifest exits 2: stdout {:?} stderr {:?}", out(&s2), err(&s2));

    // A manifest with a torn tail: refused, nothing signalled, nothing written, byte-identical.
    let torn = format!(
        "{{\"v\":1,\"op\":\"root\",\"ticket\":\"t-6\",\"path\":\"{}\",\"ts_ms\":1730000000000}}\n\
         {{\"v\":1,\"op\":\"add\",\"ticket\":\"t-6\",\"id\":\"p-1\",\"class\":\"process\",\"pid\":999999,\"pid_start_ms\":1730000000000,\"ts_ms\":1730000000001}}\n\
         {{\"v\":1,\"op\":\"add\"",
        r.to_string_lossy()
    );
    let file = d.join("t-6.jsonl");
    fs::write(&file, torn).unwrap();
    let before = fs::read(&file).unwrap();
    let s3 = run(&["process", "stop", "t-6", "--id", "p-1", "--dir", d.to_str().unwrap()]);
    assert_eq!(code(&s3), 2, "stop on a torn manifest exits 2: stdout {:?} stderr {:?}", out(&s3), err(&s3));
    assert_eq!(fs::read(&file).unwrap(), before, "a refused stop changed the manifest");
}

// ---------------------------------------------------------------- AC 6

/// AC 6: two concurrent stops of one id give exit codes {0, 2}, the group is gone, and the manifest has exactly
/// one `remove` line for that id.
#[test]
fn two_stops_at_once() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    init_manifest("t-7", &r, &d);
    let mut guard = Guard::default();

    let (s, pid) = start("t-7", "s-1", &r, None, &d, &["sleep", "30"]);
    assert_eq!(code(&s), 0, "start: {}", err(&s));
    guard.add(pid);

    let d1 = d.to_string_lossy().into_owned();
    let h1 = thread::spawn(move || {
        run(&["process", "stop", "t-7", "--id", "s-1", "--dir", &d1, "--grace-ms", "8000"])
    });
    let d2 = d.to_string_lossy().into_owned();
    let h2 = thread::spawn(move || {
        run(&["process", "stop", "t-7", "--id", "s-1", "--dir", &d2, "--grace-ms", "8000"])
    });
    let (o1, o2) = (h1.join().unwrap(), h2.join().unwrap());
    let mut codes = [code(&o1), code(&o2)];
    codes.sort();
    assert_eq!(codes, [0, 2], "one stop wins, the other is refused: {:?} / {:?}", err(&o1), err(&o2));
    assert!(poll(5_000, || !group_alive(pid)), "the group is gone");

    let text = fs::read_to_string(d.join("t-7.jsonl")).unwrap();
    let removes = text
        .lines()
        .filter(|l| l.contains("\"op\":\"remove\"") && l.contains("\"id\":\"s-1\""))
        .count();
    assert_eq!(removes, 1, "exactly one remove line for s-1: {}", text);
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
    // (c) a port already bound on 127.0.0.1
    let bound = free_port();
    let held = TcpListener::bind(("127.0.0.1", bound as u16)).unwrap();
    let c = start("t-8", "m-2", &r, Some(bound), &d, &["/bin/sh", "-c", &script]);
    assert_eq!(code(&c.0), 2, "a bound port refuses the start: stdout {:?} stderr {:?}", out(&c.0), err(&c.0));
    drop(held);

    assert!(!marker.exists(), "a refused start ran the program");
    assert_eq!(fs::read(&file).unwrap(), before, "a refused start changed the manifest");

    let stop = run(&["process", "stop", "t-8", "--id", "k-1", "--dir", d.to_str().unwrap(), "--grace-ms", "3000"]);
    assert_eq!(code(&stop), 0, "cleanup stop: {}", err(&stop));
}
