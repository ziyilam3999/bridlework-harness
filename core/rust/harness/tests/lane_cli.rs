//! `harness lane claim|release|list`: real binary, synthetic fixtures only.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lane-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

fn list(lanes: &Path) -> String {
    let o = run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
    assert_eq!(o.status.code(), Some(0));
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn claim_list_release_cycle() {
    let d = tmp("cycle");
    let lanes = d.join("lanes");
    let sf = scope(&d, "src/a.rs\ndocs/b.md\n");
    let t1 = ticket(&d, "t-one");
    let t2 = ticket(&d, "t-two");

    let o = claim(&t1, &lanes, &sf);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("ticket=t-one lane=1"));
    let l = list(&lanes);
    let lines: Vec<&str> = l.lines().collect();
    assert_eq!(lines.len(), 2, "{l}");
    assert!(lines[0].contains("ticket=t-one") && lines[0].contains("lane=1") && lines[0].contains("repo_id=synthetic-repo") && lines[0].contains("scope=2"));
    assert!(lines[1].contains("live=1 cap=1"));

    let o = claim(&t2, &lanes, &sf);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o).trim(), "lane: refused reason=cap-full");
    assert!(list(&lanes).contains("live=1 cap=1"));

    let o = claim(&t1, &lanes, &sf);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o).trim(), "lane: refused reason=already-live");

    let rel = |id: &str| run(&["lane", "release", id, "--lanes-dir", lanes.to_str().unwrap()]);
    assert_eq!(rel("t-one").status.code(), Some(0));
    assert!(list(&lanes).contains("live=0 cap=1"));
    let o = rel("t-one");
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o).trim(), "lane: refused reason=not-live");

    let o = claim(&t2, &lanes, &sf);
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("ticket=t-two lane=1"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn bad_input_exits_2_and_records_nothing() {
    let d = tmp("bad");
    let lanes = d.join("lanes");
    let t1 = ticket(&d, "t-one");
    for body in ["", "\n", "/abs/path\n", "a/../b\n", "..\n"] {
        let sf = scope(&d, body);
        assert_eq!(claim(&t1, &lanes, &sf).status.code(), Some(2), "{body:?}");
    }
    let missing = d.join("nope.txt");
    assert_eq!(claim(&t1, &lanes, &missing).status.code(), Some(2));
    let sf = scope(&d, "a\n");
    let bad_ticket = d.join("bad.json");
    fs::write(&bad_ticket, "{\"id\":\"x\"}").unwrap();
    assert_eq!(claim(&bad_ticket, &lanes, &sf).status.code(), Some(2));
    assert!(list(&lanes).contains("live=0 cap=1"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn racing_claims_never_pass_the_cap() {
    let d = tmp("race");
    let lanes = d.join("lanes");
    let sf = scope(&d, "src/a.rs\n");
    let n = 12;
    let tickets: Vec<PathBuf> = (0..n).map(|i| ticket(&d, &format!("t-{i}"))).collect();
    let kids: Vec<_> = tickets
        .iter()
        .map(|t| {
            Command::new(BIN)
                .args(["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--scope-file", sf.to_str().unwrap()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap()
        })
        .collect();
    let codes: Vec<Option<i32>> = kids.into_iter().map(|mut k| k.wait().unwrap().code()).collect();
    assert_eq!(codes.iter().filter(|c| **c == Some(0)).count(), 1, "{codes:?}");
    assert_eq!(codes.iter().filter(|c| **c == Some(1)).count(), n - 1, "{codes:?}");
    let l = list(&lanes);
    assert!(l.contains("live=1 cap=1"), "{l}");
    assert_eq!(l.lines().count(), 2);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_usage_line_lists_the_lane_verbs() {
    let o = run(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("lane claim|release|list"));
}
