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

/// Start every claim at once, then wait for all: (exit code, stderr) per claim.
fn race(lanes: &Path, sf: &Path, tickets: &[PathBuf]) -> Vec<(Option<i32>, String)> {
    let kids: Vec<_> = tickets
        .iter()
        .map(|t| {
            Command::new(BIN)
                .args(["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--scope-file", sf.to_str().unwrap()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    kids.into_iter()
        .map(|k| {
            let o = k.wait_with_output().unwrap();
            (o.status.code(), String::from_utf8_lossy(&o.stderr).to_string())
        })
        .collect()
}

#[test]
fn racing_claims_never_pass_the_cap() {
    let d = tmp("race");
    let sf = scope(&d, "src/a.rs\n");
    let n = 48;
    let tickets: Vec<PathBuf> = (0..n).map(|i| ticket(&d, &format!("t-{i}"))).collect();
    for round in 0..4 {
        let lanes = d.join(format!("lanes-{round}"));
        let res = race(&lanes, &sf, &tickets);
        assert_eq!(res.iter().filter(|r| r.0 == Some(0)).count(), 1, "{res:?}");
        assert!(res.iter().filter(|r| r.0 != Some(0)).all(|r| r.0 == Some(1) && r.1.trim() == "lane: refused reason=cap-full"), "{res:?}");
        let l = list(&lanes);
        assert!(l.contains("live=1 cap=1"), "{l}");
        assert_eq!(l.lines().count(), 2);
        let names: Vec<String> = fs::read_dir(&lanes).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        assert!(names.iter().all(|n| n == "lane-1"), "{names:?}");
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn racing_claims_of_one_ticket_win_once() {
    let d = tmp("race-same");
    let sf = scope(&d, "src/a.rs\n");
    let t = ticket(&d, "t-same");
    let tickets: Vec<PathBuf> = (0..48).map(|_| t.clone()).collect();
    for round in 0..4 {
        let lanes = d.join(format!("lanes-{round}"));
        let res = race(&lanes, &sf, &tickets);
        assert_eq!(res.iter().filter(|r| r.0 == Some(0)).count(), 1, "{res:?}");
        assert!(res.iter().filter(|r| r.0 != Some(0)).all(|r| r.0 == Some(1) && r.1.trim() == "lane: refused reason=already-live"), "{res:?}");
        assert!(list(&lanes).contains("live=1 cap=1"));
    }
    let _ = fs::remove_dir_all(&d);
}

const GOOD: &str = "lane-record v1\nticket=t-x\nrepo_id=r\nscope=a\n";

fn plant(lanes: &Path, name: &str, body: &str) {
    fs::create_dir_all(lanes).unwrap();
    fs::write(lanes.join(name), body).unwrap();
}

/// Names and sizes of everything in the dir (links not followed).
fn snapshot(lanes: &Path) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = fs::read_dir(lanes)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().to_string(), fs::symlink_metadata(e.path()).unwrap().len())
        })
        .collect();
    v.sort();
    v
}

/// `list`, `claim` and `release` each exit 2 with one `lane: error:` line and leave the dir as it was.
fn all_verbs_refuse(d: &Path, lanes: &Path, what: &str) {
    let sf = scope(d, "a\n");
    let t = ticket(d, "t-new");
    let before = snapshot(lanes);
    let outs = [
        run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]),
        claim(&t, lanes, &sf),
        run(&["lane", "release", "t-x", "--lanes-dir", lanes.to_str().unwrap()]),
    ];
    for o in &outs {
        assert_eq!(o.status.code(), Some(2), "{what}: {}", err(o));
        assert!(err(o).starts_with("lane: error: ") && err(o).trim().lines().count() == 1, "{what}: {}", err(o));
    }
    assert_eq!(snapshot(lanes), before, "{what}");
}

#[test]
fn a_bad_lanes_dir_fails_closed_for_every_verb() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let cases: Vec<(&str, &str, &str)> = vec![
        ("truncated record", "lane-1", "lane-record v1\nticket=t-x\nrepo_id=r\nsco"),
        ("truncated at a line end", "lane-1", "lane-record v1\nticket=t-x\nrepo_id=r\n"),
        ("empty record", "lane-1", ""),
        ("junk record", "lane-1", "hello\n"),
        ("lane 0", "lane-0", GOOD),
        ("above the cap", "lane-2", GOOD),
        ("far above the cap", "lane-99", GOOD),
        ("leading zero", "lane-01", GOOD),
        ("not a number", "lane-x", GOOD),
    ];
    for (what, name, body) in cases {
        let d = tmp("badrec");
        let lanes = d.join("lanes");
        plant(&lanes, name, body);
        all_verbs_refuse(&d, &lanes, what);
        let _ = fs::remove_dir_all(&d);
    }
    // two records for the same lane (the same number under two spellings)
    let d = tmp("samelane");
    let lanes = d.join("lanes");
    plant(&lanes, "lane-1", GOOD);
    plant(&lanes, "lane-01", &GOOD.replace("t-x", "t-y"));
    all_verbs_refuse(&d, &lanes, "two records, one lane");
    let _ = fs::remove_dir_all(&d);
    // two records for one ticket: at cap 1 the second name is already refused (above the cap); the ticket repeat itself is unit-tested in
    // lane.rs with cap 2.
    let d = tmp("sameticket");
    let lanes = d.join("lanes");
    plant(&lanes, "lane-1", GOOD);
    plant(&lanes, "lane-2", GOOD);
    all_verbs_refuse(&d, &lanes, "two records, one ticket");
    let _ = fs::remove_dir_all(&d);
    // a dangling link named lane-1
    let d = tmp("dangling");
    let lanes = d.join("lanes");
    fs::create_dir_all(&lanes).unwrap();
    symlink("/nonexistent-lane-target", lanes.join("lane-1")).unwrap();
    all_verbs_refuse(&d, &lanes, "dangling link");
    let _ = fs::remove_dir_all(&d);
    // a link to a good record
    let d = tmp("goodlink");
    let lanes = d.join("lanes");
    plant(&d, "real", GOOD);
    fs::create_dir_all(&lanes).unwrap();
    symlink(d.join("real"), lanes.join("lane-1")).unwrap();
    all_verbs_refuse(&d, &lanes, "link to a record");
    let _ = fs::remove_dir_all(&d);
    // an unreadable record (skipped when the process can read it anyway, e.g. root)
    let d = tmp("unreadable");
    let lanes = d.join("lanes");
    plant(&lanes, "lane-1", GOOD);
    fs::set_permissions(lanes.join("lane-1"), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(lanes.join("lane-1")).is_err() {
        all_verbs_refuse(&d, &lanes, "unreadable record");
    }
    fs::set_permissions(lanes.join("lane-1"), fs::Permissions::from_mode(0o644)).unwrap();
    let _ = fs::remove_dir_all(&d);
    // a lanes dir that is a link
    let d = tmp("dirlink");
    let real = d.join("real");
    fs::create_dir_all(&real).unwrap();
    let lanes = d.join("lanes");
    symlink(&real, &lanes).unwrap();
    all_verbs_refuse(&d, &lanes, "lanes dir is a link");
    assert!(fs::read_dir(&real).unwrap().next().is_none());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_usage_line_lists_the_lane_verbs() {
    let o = run(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("lane claim|release|list"));
}
