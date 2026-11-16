//! `harness lane release` without the record options: its old output, pinned before the record lands.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lane-rel-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn ticket(dir: &Path, id: &str) -> PathBuf {
    let body = format!(
        "{{\"id\":\"{id}\",\"repo_id\":\"synthetic-repo\",\"title\":\"t\",\"group\":\"g\",\"size\":\"S\",\"objective\":\"o\",\"acceptance_criteria\":[\"a\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}"
    );
    let p = dir.join(format!("{id}.json"));
    fs::write(&p, body).unwrap();
    p
}

fn scope(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("scope.txt");
    fs::write(&p, body).unwrap();
    p
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn claim(t: &Path, lanes: &Path, sf: &Path) -> Output {
    run(&["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--scope-file", sf.to_str().unwrap()])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// Is there a file whose name ends in `.release` anywhere under `d`?
fn any_release_file(d: &Path) -> bool {
    let Ok(rd) = fs::read_dir(d) else { return false };
    for e in rd {
        let e = e.unwrap();
        if e.file_name().to_string_lossy().ends_with(".release") {
            return true;
        }
        let p = e.path();
        if fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false) && any_release_file(&p) {
            return true;
        }
    }
    false
}

#[test]
fn release_without_reason_is_unchanged() {
    let d = tmp("plain");
    let lanes = d.join("lanes");
    let sf = scope(&d, "src/a.rs\n");
    let t = ticket(&d, "t-one");
    assert_eq!(claim(&t, &lanes, &sf).status.code(), Some(0));
    let o = run(&["lane", "release", "t-one", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one released\n");
    assert_eq!(err(&o), "");
    assert!(!any_release_file(&d));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn not_live_without_reason_is_unchanged() {
    let d = tmp("plain-dead");
    let lanes = d.join("lanes");
    let o = run(&["lane", "release", "t-one", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "lane: refused reason=not-live\n");
    let _ = fs::remove_dir_all(&d);
}
