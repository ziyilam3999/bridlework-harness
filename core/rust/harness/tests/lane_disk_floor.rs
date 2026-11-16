//! `harness lane claim` under the config disk floor `lanes.min_free_gb` (`docs/lanes.md`): a claim is refused
//! with `low-disk` when the free space of the lanes dir file system is under the floor, and nothing is created.
//! Real binary, synthetic fixtures only.
#![cfg(unix)]
use harness::lane::free_bytes;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lane-floor-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

/// The example text with its `min_free_gb` line replaced by `floor`, written under the temp folder.
fn config(dir: &Path, floor: &str) -> PathBuf {
    let text = fs::read_to_string(EXAMPLE).unwrap().replace("  min_free_gb: 50", &format!("  min_free_gb: {floor}"));
    let p = dir.join(format!("floor-{floor}.yaml"));
    fs::write(&p, text).unwrap();
    p
}

fn claim(dir: &Path, cfg: &Path, lanes: &Path, t: &Path) -> Output {
    run(&["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", scope(dir).to_str().unwrap()])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn a_claim_under_the_floor_is_refused_low_disk() {
    // 18446744074 * 1000000000 wraps in u64 to about 0.29 GB, so a u64 compare would let the claim through
    for floor in ["18446744074", "999999999999999"] {
        let d = tmp("under");
        let lanes = d.join("lanes");
        let t = ticket(&d, "t-one");
        let o = claim(&d, &config(&d, floor), &lanes, &t);
        assert_eq!(o.status.code(), Some(1), "{floor}: {}", err(&o));
        assert!(out(&o).is_empty(), "{floor}: {}", out(&o));
        let e = err(&o);
        let lines: Vec<&str> = e.lines().collect();
        assert_eq!(lines.len(), 1, "{floor}: {e}");
        assert!(lines[0].starts_with("lane: refused reason=low-disk free_gb="), "{floor}: {e}");
        assert!(lines[0].ends_with(&format!("min_free_gb={floor}")), "{floor}: {e}");
        assert!(!lanes.exists(), "{floor}: a missing lanes dir must stay missing");
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn a_claim_over_the_floor_claims() {
    let d = tmp("over");
    let lanes = d.join("lanes");
    let t = ticket(&d, "t-one");
    let o = claim(&d, &config(&d, "1"), &lanes, &t);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one lane=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_low_disk_refusal_leaves_a_live_lane_alone() {
    let d = tmp("live");
    let lanes = d.join("lanes");
    let a = ticket(&d, "t-a");
    let b = ticket(&d, "t-b");
    // ticket A claims under a floor of 1
    let o = claim(&d, &config(&d, "1"), &lanes, &a);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let lane_file = lanes.join("lane-1");
    let before = fs::read(&lane_file).unwrap();
    // ticket B is refused under the unreachable floor, and the lane file of A is byte-unchanged
    let huge = config(&d, "999999999999999");
    let o = claim(&d, &huge, &lanes, &b);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).starts_with("lane: refused reason=low-disk"), "{}", err(&o));
    assert_eq!(fs::read(&lane_file).unwrap(), before);
    // ticket A itself, claiming again under the unreachable floor, is refused low-disk, not already-live
    let o = claim(&d, &huge, &lanes, &a);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).starts_with("lane: refused reason=low-disk"), "{}", err(&o));
    // list and release read no disk: both exit 0 under the unreachable floor, and the lane can be freed
    let o = run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", huge.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("ticket=t-a lane=1"), "{}", out(&o));
    let o = run(&["lane", "release", "t-a", "--lanes-dir", lanes.to_str().unwrap(), "--config", huge.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-a released\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_free_space_reader_agrees_with_df() {
    let d = tmp("df");
    let got = free_bytes(&d).expect("the temp folder reads");
    let df = Command::new("df").arg("-Pk").arg(&d).output().unwrap();
    assert!(df.status.success(), "df -Pk must run");
    let text = String::from_utf8_lossy(&df.stdout).into_owned();
    let avail: u64 = text.lines().nth(1).expect("the df row").split_whitespace().nth(3).expect("the Available column").parse().unwrap();
    let avail = avail * 1024;
    // equal within 2 GB or 2 percent, the larger
    let bound = (2 * 1000000000u64).max(avail / 50);
    assert!(got.abs_diff(avail) <= bound, "free_bytes={got} df available={avail}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_reader_returns_none_for_a_missing_path() {
    let d = tmp("missing");
    assert_eq!(free_bytes(&d.join("no-such-folder-7f3a9").join("x")), None);
    let _ = fs::remove_dir_all(&d);
}
