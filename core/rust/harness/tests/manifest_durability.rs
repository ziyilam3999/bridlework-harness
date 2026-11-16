//! Durability and lock tests for the manifest reader/writer (ticket manifest-writer-fix). A reader waits
//! for a writer holding the exclusive lock instead of reading a half-written line; a locked read fails
//! closed (a missing file stays missing, a folder at the manifest path is an Err, a 0444 file still reads);
//! a torn tail left by an older binary refuses every write through the CLI; a busy manifest refuses
//! verify/owns/stop through the CLI, and a stop whose read was busy signals nothing.
#![cfg(unix)]
use harness::manifest::{apply, read_manifest, Line, Op};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("mdur-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch layout: `R` is the root, `D` holds the manifest.
    fn fence(&self) -> (PathBuf, PathBuf) {
        let r = self.0.join("R");
        let d = self.0.join("D");
        fs::create_dir_all(&r).unwrap();
        fs::create_dir_all(&d).unwrap();
        (r, d)
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

fn run_s(args: &[String]) -> Output {
    run(&args.iter().map(|s| s.as_str()).collect::<Vec<_>>())
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// The reader blocks on the shared lock while a writer holds the exclusive one and a line is half on
/// disk; once the line is whole and the lock freed, the read is clean and holds the new line.
#[test]
fn a_reader_waits_for_a_writer_holding_the_lock() {
    let t = Tmp::new();
    let (r, d) = t.fence();
    apply(&d, "t-1", &Op::Init { roots: std::slice::from_ref(&r) }, 1000).unwrap();
    let file = d.join("t-1.jsonl");
    let before = fs::read(&file).unwrap();
    let line = Line::AddPort { ticket: "t-1".into(), id: "n-1".into(), port: 8080, ts_ms: 1001 }.encode().unwrap();
    let mut lock = OpenOptions::new().read(true).append(true).open(&file).unwrap();
    lock.lock().unwrap();
    lock.write_all(&line.as_bytes()[..line.len() / 2]).unwrap();
    let (tx, rx) = mpsc::channel();
    let d2 = d.clone();
    std::thread::spawn(move || {
        let _ = tx.send(read_manifest(&d2, "t-1"));
    });
    std::thread::sleep(Duration::from_millis(300));
    assert!(rx.try_recv().is_err(), "the reader returned while the lock was held and the line half written");
    lock.write_all(&line.as_bytes()[line.len() / 2..]).unwrap();
    drop(lock);
    let got = rx.recv_timeout(Duration::from_secs(10)).expect("the reader answers within 10 s, not a hang");
    let read = got.expect("a clean read once the lock frees");
    assert!(read.is_clean());
    assert!(read.live.iter().any(|e| e.id == "n-1"), "the new line is live: {:?}", read.live);
    assert_eq!(fs::read(&file).unwrap().len(), before.len() + line.len());
}

/// The locked read fails closed: a missing manifest is an Err that creates no file, a folder at the
/// manifest path is an Err, and a manifest the user can read but not write (0444) still reads clean.
#[test]
fn a_locked_read_fails_closed() {
    let t = Tmp::new();
    let (r, d) = t.fence();
    assert!(read_manifest(&d, "nope").is_err(), "a missing manifest is an Err");
    assert!(!d.join("nope.jsonl").exists(), "the read created no file");
    fs::create_dir_all(d.join("t-1.jsonl")).unwrap();
    assert!(read_manifest(&d, "t-1").is_err(), "a folder at the manifest path is an Err");
    let _ = fs::remove_dir_all(d.join("t-1.jsonl"));
    apply(&d, "t-2", &Op::Init { roots: std::slice::from_ref(&r) }, 1000).unwrap();
    apply(&d, "t-2", &Op::AddPort { id: "n-1", port: 8080 }, 1001).unwrap();
    let f2 = d.join("t-2.jsonl");
    fs::set_permissions(&f2, std::os::unix::fs::PermissionsExt::from_mode(0o444)).unwrap();
    let read = read_manifest(&d, "t-2").unwrap();
    assert!(read.is_clean());
    assert_eq!(read.live.len(), 1, "the read-only manifest still reads");
    fs::set_permissions(&f2, std::os::unix::fs::PermissionsExt::from_mode(0o644)).unwrap();
}

/// A manifest whose tail is a prefix of a valid line with no newline — what an older binary leaves
/// after a short write — refuses add (path, pid, port), remove and a second init through the CLI, each
/// leaving the file byte-identical; verify exits 1.
#[test]
fn an_old_torn_tail_refuses_every_write_through_the_cli() {
    let t = Tmp::new();
    let (r, d) = t.fence();
    let dir = d.to_str().unwrap();
    assert_eq!(code(&run(&["manifest", "init", "t-1", "--dir", dir, "--root", r.to_str().unwrap(), "--ts-ms", "1000"])), 0);
    assert_eq!(
        code(&run(&["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "a", "--path", &r.join("x").to_string_lossy(), "--ts-ms", "1001"])),
        0
    );
    let file = d.join("t-1.jsonl");
    let mut torn = fs::read(&file).unwrap();
    torn.extend_from_slice(b"{\"v\":1,\"op\":\"rem"); // a prefix of a valid line, no newline
    fs::write(&file, &torn).unwrap();
    let pid = std::process::id().to_string();
    for args in [
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "z", "--path", &r.join("z").to_string_lossy()],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--pid", &pid],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--port", "9091"],
        vec!["manifest", "remove", "t-1", "--dir", dir, "--id", "a"],
        vec!["manifest", "init", "t-1", "--dir", dir, "--root", r.to_str().unwrap()],
    ] {
        let want = if args[1] == "init" { "second init" } else { "torn tail" };
        let before = fs::read(&file).unwrap();
        let o = run(&args);
        assert_eq!(code(&o), 2, "{args:?}: stdout {:?} stderr {:?}", o.stdout, err(&o));
        assert!(err(&o).contains(want), "{args:?} names {want}: {}", err(&o));
        assert_eq!(fs::read(&file).unwrap(), before, "{args:?}: the file is byte-identical");
    }
    let v = run(&["manifest", "verify", "t-1", "--dir", dir]);
    assert_eq!(code(&v), 1, "verify of a torn tail exits 1: {}", err(&v));
}

/// While the manifest's exclusive lock is held, `manifest verify`, `manifest owns` and `process stop`
/// each refuse as busy (verify and stop exit 2, owns exits 1 with its not-owned answer), each child
/// waits the real lock bound, the stop signals nothing (the sleep process outlives it) and the file is
/// byte-identical.
#[test]
fn a_busy_manifest_refuses_through_the_cli() {
    let t = Tmp::new();
    let (r, d) = t.fence();
    let dir = d.to_str().unwrap();
    assert_eq!(code(&run(&["manifest", "init", "t-1", "--dir", dir, "--root", r.to_str().unwrap(), "--ts-ms", "1000"])), 0);
    let mut sleeper = Command::new("sleep").arg("60").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let pid = sleeper.id().to_string();
    let a = run(&["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "p-1", "--pid", &pid]);
    assert_eq!(code(&a), 0, "process add: {}", err(&a));
    let n = run(&["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "n-1", "--port", "8080"]);
    assert_eq!(code(&n), 0, "port add: {}", err(&n));
    let file = d.join("t-1.jsonl");
    let before = fs::read(&file).unwrap();
    let lock = OpenOptions::new().read(true).append(true).open(&file).unwrap();
    lock.lock().unwrap();
    let cases: Vec<Vec<String>> = vec![
        ["manifest", "verify", "t-1", "--dir", dir].iter().map(|s| s.to_string()).collect(),
        ["manifest", "owns", "t-1", "--dir", dir, "--port", "8080"].iter().map(|s| s.to_string()).collect(),
        ["process", "stop", "t-1", "--id", "p-1", "--dir", dir].iter().map(|s| s.to_string()).collect(),
    ];
    let (tx, rx) = mpsc::channel();
    for (i, args) in cases.iter().enumerate() {
        let tx = tx.clone();
        let args = args.clone();
        std::thread::spawn(move || {
            let t0 = Instant::now();
            let o = run_s(&args);
            let _ = tx.send((i, o, t0.elapsed()));
        });
    }
    drop(tx);
    let mut results: Vec<(usize, Output, Duration)> = Vec::new();
    while results.len() < cases.len() {
        let x = rx.recv_timeout(Duration::from_secs(20)).expect("each child answers within 20 s, not a hang");
        results.push(x);
    }
    for (i, o, took) in &results {
        assert!(took.as_secs() >= 9, "child {i} ran only {took:?}; the wait is bounded by the 10 s read limit");
        let text = err(o);
        match i {
            0 => {
                assert_eq!(code(o), 2, "verify exits 2 on a busy manifest: {text}");
                assert!(text.contains("busy"), "verify names busy: {text}");
            }
            1 => {
                assert_eq!(code(o), 1, "owns exits 1 (not owned) on a busy manifest: {text}");
                assert!(text.contains("busy"), "owns names busy: {text}");
            }
            _ => {
                assert_eq!(code(o), 2, "stop exits 2 on a busy manifest: {text}");
                assert!(text.contains("busy"), "stop names busy: {text}");
            }
        }
    }
    assert!(sleeper.try_wait().unwrap().is_none(), "the stop signalled nothing: the sleep process is still alive");
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    drop(lock);
    assert_eq!(fs::read(&file).unwrap(), before, "the busy refusals changed nothing");
}
