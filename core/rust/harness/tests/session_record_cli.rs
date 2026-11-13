//! `harness session record-start` (P1B-21): the one CLI door that writes a `session_started` event with `source=hook`.
//! Real binary, synthetic ids, a private folder per case.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);
const ID: &str = "5c1e7a3b-9d24-4f6a-8b0e-2a7d9c4f1e65";
const ID2: &str = "0b8f3d52-61aa-4c07-9e1d-7f3a2b5c8d90";

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("srec-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
    fn log(&self) -> PathBuf {
        self.0.join("events.jsonl")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL"] {
        c.env_remove(k);
    }
    c.output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_string()
}

fn start(id: &str, log: &Path, extra: &[&str]) -> Output {
    let mut a = vec!["session", "record-start", "--session-id", id, "--log", log.to_str().unwrap()];
    a.extend_from_slice(extra);
    run(&a)
}

fn list(log: &Path) -> Output {
    run(&["events", "list", "--log", log.to_str().unwrap()])
}

fn assert_refused(o: &Output, log: &Path) {
    assert_eq!(o.status.code(), Some(2), "stderr: {}", text(&o.stderr));
    assert!(o.stdout.is_empty());
    assert!(text(&o.stderr).trim_end().ends_with("EVENT NOT RECORDED"), "stderr: {}", text(&o.stderr));
    assert!(!log.exists(), "a refused call must write nothing");
}

#[test]
fn valid_id_is_recorded_with_empty_stdout() {
    let t = Tmp::new();
    let o = start(ID, &t.log(), &["--ts-ms", "1731000000123"]);
    assert_eq!(o.status.code(), Some(0), "stderr: {}", text(&o.stderr));
    assert!(o.stdout.is_empty());
    let v = run(&["events", "verify", "--log", t.log().to_str().unwrap()]);
    assert_eq!(v.status.code(), Some(0));
    let l = text(&list(&t.log()).stdout);
    assert_eq!(l.lines().count(), 1, "{l}");
    assert!(l.contains("session_started"), "{l}");
    assert!(l.contains(ID), "{l}");
    assert!(l.contains("hook"), "{l}");
    assert!(l.contains("1731000000123"), "{l}");
}

#[test]
fn second_call_appends_a_second_line() {
    let t = Tmp::new();
    assert_eq!(start(ID, &t.log(), &[]).status.code(), Some(0));
    assert_eq!(start(ID2, &t.log(), &[]).status.code(), Some(0));
    let l = text(&list(&t.log()).stdout);
    assert_eq!(l.lines().count(), 2, "{l}");
    assert!(l.contains(ID) && l.contains(ID2));
}

#[test]
fn missing_session_id_is_refused() {
    let t = Tmp::new();
    let o = run(&["session", "record-start", "--log", t.log().to_str().unwrap()]);
    assert_refused(&o, &t.log());
}

#[test]
fn missing_log_is_refused() {
    let t = Tmp::new();
    let o = run(&["session", "record-start", "--session-id", ID]);
    assert_refused(&o, &t.log());
}

#[test]
fn relative_log_is_refused() {
    let t = Tmp::new();
    let o = run(&["session", "record-start", "--session-id", ID, "--log", "events.jsonl"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(text(&o.stderr).trim_end().ends_with("EVENT NOT RECORDED"));
    assert!(!Path::new("events.jsonl").exists());
    assert!(!t.log().exists());
}

#[test]
fn malformed_ids_are_refused() {
    for bad in ["5C1E7A3B-9D24-4F6A-8B0E-2A7D9C4F1E65", "5c1e7a3b", "5c1e7a3b-9d24-4f6a-8b0e-2a7d9c4f1e6é", "NOT-A-UUID", ""] {
        let t = Tmp::new();
        assert_refused(&start(bad, &t.log(), &[]), &t.log());
    }
}

#[test]
fn unknown_and_duplicate_flags_are_refused() {
    let t = Tmp::new();
    assert_refused(&start(ID, &t.log(), &["--bogus", "x"]), &t.log());
    assert_refused(&start(ID, &t.log(), &["--session-id", ID2]), &t.log());
    assert_refused(&start(ID, &t.log(), &["--ts-ms", "abc"]), &t.log());
}
