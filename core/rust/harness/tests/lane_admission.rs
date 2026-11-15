//! `harness lane admission-check` and the admission-gated cap of `claim`/`list`: real binary, synthetic fixtures only.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lane-adm-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

fn scope(dir: &Path) -> PathBuf {
    let p = dir.join("scope.txt");
    fs::write(&p, "src/a.rs\n").unwrap();
    p
}

/// A full valid config whose `lanes.count` is `count` (placeholders only, like the example config).
fn config(dir: &Path, count: u32) -> PathBuf {
    let text = format!(
        "repos:\nbudgets:\n  s_wall_seconds: 1\n  s_tokens: 1\n  m_wall_seconds: 1\n  m_tokens: 1\n  l_wall_seconds: 1\n  l_tokens: 1\nshadow:\n  p1a_cap: 1\n  p4a_cap: 1\n  d2a_cap: 1\n  p7_cap: 1\n  p1a_token_budget: 1\n  p4a_token_budget: 1\n  d2a_token_budget: 1\n  p7_token_budget: 1\nlanes:\n  count: {count}\nfriction:\n  inbox: /placeholder/friction-inbox.jsonl\nfactory:\n  design_tier: T3\n"
    );
    let p = dir.join(format!("config-{count}-{}.yaml", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&p, text).unwrap();
    p
}

/// A valid admission record admitting `lanes` lanes.
fn admission_text(lanes: u32) -> String {
    format!("lane-admission v1\nlanes={lanes}\nbaseline=main@0f1e2d3\nstep2=p1a 5 p4a 10 d2a 20\nreviewer=rev-one\nverdict=approve\noperator=go-2026-11-14b\nl3=pass\nsigned=2026-11-14\n")
}

fn admission(dir: &Path, lanes: u32) -> PathBuf {
    let p = dir.join(format!("admission-{lanes}-{}.txt", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&p, admission_text(lanes)).unwrap();
    p
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn claim(t: &Path, lanes: &Path, cfg: &Path, sf: &Path, adm: Option<&Path>) -> Output {
    let mut args = vec!["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", sf.to_str().unwrap()];
    if let Some(a) = adm {
        args.push("--admission");
        args.push(a.to_str().unwrap());
    }
    run(&args)
}

fn list(lanes: &Path, cfg: &Path, adm: Option<&Path>) -> Output {
    let mut args = vec!["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap()];
    if let Some(a) = adm {
        args.push("--admission");
        args.push(a.to_str().unwrap());
    }
    run(&args)
}

fn release(id: &str, lanes: &Path, cfg: &Path) -> Output {
    run(&["lane", "release", id, "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap()])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
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

fn refused_once(o: &Output, token: &str) -> bool {
    o.status.code() == Some(1) && err(o).trim() == format!("lane: refused reason={token}")
}

/// Exit 1 with exactly one stderr line `lane: refused reason=bad-admission` followed by what is wrong (the field, when it is a field).
fn bad_admission_once(o: &Output) -> bool {
    let e = err(o);
    o.status.code() == Some(1) && e.trim().lines().count() == 1 && e.starts_with("lane: refused reason=bad-admission ")
}

#[test]
fn admission_check_prints_the_cap_and_a_missing_path_exits_2() {
    let d = tmp("check");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let o = run(&["lane", "admission-check", adm.to_str().unwrap(), "--config", cfg.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "admission ok lanes=2 cap=2\n");
    // the config count bounds the same record
    let cfg3 = config(&d, 3);
    let adm4 = admission(&d, 4);
    let o = run(&["lane", "admission-check", adm4.to_str().unwrap(), "--config", cfg3.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "admission ok lanes=4 cap=3\n");
    // a path that does not exist exits 2
    let missing = d.join("nope.txt");
    let o = run(&["lane", "admission-check", missing.to_str().unwrap(), "--config", cfg.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    // a wrong header, an unknown line, a symbolic link and a file over the size limit are bad-admission
    for (what, body) in [
        ("wrong header", admission_text(2).replacen("lane-admission v1", "lane-admission v2", 1)),
        ("unknown line", format!("{}extra=1\n", admission_text(2).trim_end_matches('\n'))),
        ("cut short", admission_text(2).trim_end_matches('\n').to_string()),
    ] {
        let p = d.join(format!("{what}.txt"));
        fs::write(&p, body).unwrap();
        let o = run(&["lane", "admission-check", p.to_str().unwrap(), "--config", cfg.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{what}: {}", err(&o));
        assert!(err(&o).trim().starts_with("lane: refused reason=bad-admission"), "{what}: {}", err(&o));
        assert_eq!(err(&o).trim().lines().count(), 1, "{what}");
    }
    let link = d.join("link.txt");
    std::os::unix::fs::symlink(&adm, &link).unwrap();
    let o = run(&["lane", "admission-check", link.to_str().unwrap(), "--config", cfg.to_str().unwrap()]);
    assert!(bad_admission_once(&o), "symlink: {}", err(&o));
    assert!(err(&o).contains("not a regular file"), "{}", err(&o));
    let big = d.join("big.txt");
    fs::write(&big, format!("{}#{}\n", admission_text(2).trim_end_matches('\n'), "x".repeat(64 * 1024))).unwrap();
    let o = run(&["lane", "admission-check", big.to_str().unwrap(), "--config", cfg.to_str().unwrap()]);
    assert!(bad_admission_once(&o), "big: {}", err(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn every_missing_or_empty_field_is_refused_by_name() {
    let d = tmp("fields");
    let cfg = config(&d, 4);
    for f in ["lanes", "baseline", "step2", "reviewer", "verdict", "operator", "l3", "signed"] {
        let prefix = format!("{f}=");
        let good = admission_text(2);
        // the record without that field
        let missing: Vec<&str> = good.lines().filter(|l| !l.starts_with(&prefix)).collect();
        let p = d.join(format!("missing-{f}.txt"));
        fs::write(&p, format!("{}\n", missing.join("\n"))).unwrap();
        // the record with that field empty
        let empty: Vec<String> = good.lines().map(|l| if l.starts_with(&prefix) { prefix.clone() } else { l.to_string() }).collect();
        let q = d.join(format!("empty-{f}.txt"));
        fs::write(&q, format!("{}\n", empty.join("\n"))).unwrap();
        for (what, path) in [("missing", &p), ("empty", &q)] {
            let o = run(&["lane", "admission-check", path.to_str().unwrap(), "--config", cfg.to_str().unwrap()]);
            assert_eq!(o.status.code(), Some(1), "{f} {what}: {}", err(&o));
            let e = err(&o);
            assert!(e.trim().starts_with("lane: refused reason=bad-admission"), "{f} {what}: {e}");
            assert!(e.contains(f), "{f} {what}: {e}");
            assert_eq!(e.trim().lines().count(), 1, "{f} {what}");
        }
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn an_admission_of_2_of_4_caps_at_2() {
    let d = tmp("cap24");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    for (id, want) in [("t-one", "lane=1"), ("t-two", "lane=2")] {
        let o = claim(&ticket(&d, id), &lanes, &cfg, &sf, Some(&adm));
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert!(out(&o).contains(want), "{}", out(&o));
    }
    let o = claim(&ticket(&d, "t-three"), &lanes, &cfg, &sf, Some(&adm));
    assert!(refused_once(&o, "cap-full"), "{}", err(&o));
    let o = list(&lanes, &cfg, Some(&adm));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.last().unwrap(), &"live=2 cap=2", "{text}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn an_admission_of_4_of_4_lets_four_claims_in() {
    let d = tmp("cap44");
    let cfg = config(&d, 4);
    let adm = admission(&d, 4);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    for i in 1..=4 {
        let o = claim(&ticket(&d, &format!("t-{i}")), &lanes, &cfg, &sf, Some(&adm));
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert!(out(&o).contains(&format!("lane={i}")), "{}", out(&o));
    }
    let o = claim(&ticket(&d, "t-five"), &lanes, &cfg, &sf, Some(&adm));
    assert!(refused_once(&o, "cap-full"), "{}", err(&o));
    let o = list(&lanes, &cfg, Some(&adm));
    assert!(out(&o).contains("live=4 cap=4"), "{}", out(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_config_count_of_3_bounds_an_admission_of_4() {
    let d = tmp("cap43");
    let cfg = config(&d, 3);
    let adm = admission(&d, 4);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    for i in 1..=3 {
        let o = claim(&ticket(&d, &format!("t-{i}")), &lanes, &cfg, &sf, Some(&adm));
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    }
    let o = claim(&ticket(&d, "t-four"), &lanes, &cfg, &sf, Some(&adm));
    assert!(refused_once(&o, "cap-full"), "{}", err(&o));
    let o = list(&lanes, &cfg, Some(&adm));
    assert!(out(&o).contains("live=3 cap=3"), "{}", out(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn without_admission_no_second_lane_and_the_dir_is_unchanged() {
    let d = tmp("noadm");
    let cfg = config(&d, 4);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    // the first claim with no live ticket still succeeds and gets lane 1
    let o = claim(&ticket(&d, "t-one"), &lanes, &cfg, &sf, None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("lane=1"), "{}", out(&o));
    let before = snapshot(&lanes);
    // no --admission: a claim for another ticket is refused and the dir is unchanged
    let o = claim(&ticket(&d, "t-two"), &lanes, &cfg, &sf, None);
    assert!(refused_once(&o, "no-admission"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    // --admission at a path where nothing exists is the same refusal
    let missing = d.join("no-admission.txt");
    let o = claim(&ticket(&d, "t-two"), &lanes, &cfg, &sf, Some(&missing));
    assert!(refused_once(&o, "no-admission"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_ticket_live_in_lane_2_still_fills_the_cap_of_1_without_admission() {
    let d = tmp("lane2");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    for id in ["t-one", "t-two"] {
        assert_eq!(claim(&ticket(&d, id), &lanes, &cfg, &sf, Some(&adm)).status.code(), Some(0));
    }
    assert_eq!(release("t-one", &lanes, &cfg).status.code(), Some(0));
    // lane 1 is free but one ticket is live, so with no admission the cap of 1 is full
    let before = snapshot(&lanes);
    let o = claim(&ticket(&d, "t-three"), &lanes, &cfg, &sf, None);
    assert!(refused_once(&o, "no-admission"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_bad_admission_file_refuses_a_claim_and_records_nothing() {
    let d = tmp("badadm");
    let cfg = config(&d, 4);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    // a record that exists and is not valid refuses the claim even when no ticket is live
    for (what, body) in [
        ("a missing field", admission_text(2).replacen("reviewer=rev-one\n", "", 1)),
        ("one lane", admission_text(1)),
        ("a wrong header", admission_text(2).replacen("lane-admission v1", "x", 1)),
    ] {
        let p = d.join(format!("{}.txt", what.replace(' ', "-")));
        fs::write(&p, body).unwrap();
        let o = claim(&ticket(&d, "t-one"), &lanes, &cfg, &sf, Some(&p));
        assert_eq!(o.status.code(), Some(1), "{what}: {}", err(&o));
        assert!(err(&o).trim().starts_with("lane: refused reason=bad-admission"), "{what}: {}", err(&o));
        assert!(!lanes.exists(), "{what}: the lanes dir was created");
    }
    // the same record with one ticket live refuses and records nothing
    let good = admission(&d, 2);
    let o = claim(&ticket(&d, "t-one"), &lanes, &cfg, &sf, Some(&good));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let before = snapshot(&lanes);
    let p = d.join("still-one-lane.txt");
    fs::write(&p, admission_text(1)).unwrap();
    let o = claim(&ticket(&d, "t-two"), &lanes, &cfg, &sf, Some(&p));
    assert!(err(&o).trim().starts_with("lane: refused reason=bad-admission"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_same_ticket_second_claim_under_a_cap_of_2_is_already_live() {
    let d = tmp("same");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    let t = ticket(&d, "t-one");
    let o = claim(&t, &lanes, &cfg, &sf, Some(&adm));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let o = claim(&t, &lanes, &cfg, &sf, Some(&adm));
    assert!(refused_once(&o, "already-live"), "{}", err(&o));
    let o = list(&lanes, &cfg, Some(&adm));
    assert!(out(&o).contains("live=1 cap=2"), "{}", out(&o));
    let names: Vec<String> = fs::read_dir(&lanes).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    assert_eq!(names.len(), 1, "{names:?}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn names_follow_the_config_and_a_release_needs_no_admission() {
    let d = tmp("names");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let sf = scope(&d);
    let lanes = d.join("lanes");
    for id in ["t-one", "t-two"] {
        assert_eq!(claim(&ticket(&d, id), &lanes, &cfg, &sf, Some(&adm)).status.code(), Some(0));
    }
    // list without --admission prints cap=1, still lists every live record, and exits 0 (a claim is what gets refused)
    let o = list(&lanes, &cfg, None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("ticket=t-one") && text.contains("ticket=t-two"), "{text}");
    assert!(text.ends_with("live=2 cap=1\n"), "{text}");
    // a claim for a third ticket is what gets refused
    let o = claim(&ticket(&d, "t-three"), &lanes, &cfg, &sf, None);
    assert!(refused_once(&o, "no-admission"), "{}", err(&o));
    // a release works for a ticket in lane 2 (release takes --config and no admission)
    let o = release("t-two", &lanes, &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-two released\n");
    let o = list(&lanes, &cfg, None);
    assert!(out(&o).contains("live=1 cap=1"), "{}", out(&o));
    // a record named above the config count still fails closed for every verb
    fs::write(lanes.join("lane-5"), "lane-record v1\nticket=t-x\nrepo_id=r\nscope=a\n").unwrap();
    let o = list(&lanes, &cfg, None);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    let _ = fs::remove_dir_all(&d);
}

/// Start every claim at once, then wait for all: (exit code, stdout, stderr) per claim.
fn race(lanes: &Path, cfg: &Path, sf: &Path, adm: Option<&Path>, tickets: &[PathBuf]) -> Vec<(Option<i32>, String, String)> {
    let kids: Vec<_> = tickets
        .iter()
        .map(|t| {
            let mut args = vec!["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", sf.to_str().unwrap()];
            if let Some(a) = adm {
                args.push("--admission");
                args.push(a.to_str().unwrap());
            }
            Command::new(BIN)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    kids.into_iter()
        .map(|k| {
            let o = k.wait_with_output().unwrap();
            (o.status.code(), String::from_utf8_lossy(&o.stdout).to_string(), String::from_utf8_lossy(&o.stderr).to_string())
        })
        .collect()
}

fn assert_race_leaves_exactly_cap(lanes: &Path, res: &[(Option<i32>, String, String)], cap: usize) {
    assert_eq!(res.iter().filter(|r| r.0 == Some(0)).count(), cap, "{res:?}");
    let winners: Vec<&String> = res.iter().filter(|r| r.0 == Some(0)).map(|r| &r.1).collect();
    // no lane number twice
    let mut seen: Vec<&str> = Vec::new();
    for w in &winners {
        let lane = w.split(' ').find(|p| p.starts_with("lane=")).unwrap_or_else(|| panic!("no lane in {w:?}"));
        assert!(!seen.contains(&lane), "{w:?} after {seen:?}");
        seen.push(lane);
    }
    assert!(res.iter().filter(|r| r.0 != Some(0)).all(|r| r.0 == Some(1) && r.2.trim() == "lane: refused reason=cap-full"), "{res:?}");
    let names: Vec<String> = fs::read_dir(lanes).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).filter(|n| n.starts_with("lane-")).collect();
    assert_eq!(names.len(), cap, "{names:?}");
}

#[test]
fn racing_claims_under_a_cap_of_2_leave_two_live() {
    let d = tmp("race2");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let sf = scope(&d);
    let tickets: Vec<PathBuf> = (0..32).map(|i| ticket(&d, &format!("t-{i}"))).collect();
    for round in 0..2 {
        let lanes = d.join(format!("lanes-{round}"));
        let res = race(&lanes, &cfg, &sf, Some(&adm), &tickets);
        assert_race_leaves_exactly_cap(&lanes, &res, 2);
        let o = list(&lanes, &cfg, Some(&adm));
        assert!(out(&o).contains("live=2 cap=2"), "{}", out(&o));
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn racing_claims_under_a_cap_of_4_leave_four_live() {
    let d = tmp("race4");
    let cfg = config(&d, 4);
    let adm = admission(&d, 4);
    let sf = scope(&d);
    let tickets: Vec<PathBuf> = (0..32).map(|i| ticket(&d, &format!("t-{i}"))).collect();
    for round in 0..2 {
        let lanes = d.join(format!("lanes-{round}"));
        let res = race(&lanes, &cfg, &sf, Some(&adm), &tickets);
        assert_race_leaves_exactly_cap(&lanes, &res, 4);
        let o = list(&lanes, &cfg, Some(&adm));
        assert!(out(&o).contains("live=4 cap=4"), "{}", out(&o));
    }
    let _ = fs::remove_dir_all(&d);
}
