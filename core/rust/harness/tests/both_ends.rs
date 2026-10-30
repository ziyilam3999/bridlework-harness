//! Both-ends judgement smoke. The "trees" are plain temp folders; no git is involved.
#![cfg(unix)]
use harness::both_ends::{judge, Spec};
use harness::gate_result::Status;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
const HEAD: &str = "89abcdef0123456789abcdef0123456789abcdef";

fn scratch(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hl-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn put(dir: &Path, rel: &str, body: &str) {
    let f = dir.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, body).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Two trees, each holding ci/check.sh that exits with the given code.
fn pair(name: &str, base_exit: &str, head_exit: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = scratch(name);
    let (b, h) = (root.join("b"), root.join("h"));
    put(&b, "ci/check.sh", &format!("#!/bin/sh\n{}\n", base_exit));
    put(&h, "ci/check.sh", &format!("#!/bin/sh\n{}\n", head_exit));
    (root, b, h)
}

fn spec() -> Spec<'static> {
    Spec { id: "ticket-test", program: "ci/check.sh", args: &[], budget: Duration::from_secs(10), max_output: 1000 }
}

#[test]
fn red_at_base_and_green_at_head_passes_and_names_the_head_sha() {
    let (root, b, h) = pair("ok", "exit 1", "exit 0");
    let r = judge(&b, &h, BASE, HEAD, HEAD, &spec()).unwrap();
    assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
    assert_eq!(r.failure_class, None);
    assert_eq!(r.head_sha, HEAD);
    assert!(r.validate().is_ok());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_check_that_passes_at_base_is_fail() {
    let (root, b, h) = pair("green", "exit 0", "exit 0");
    let r = judge(&b, &h, BASE, HEAD, HEAD, &spec()).unwrap();
    assert_eq!(r.status, Status::Fail);
    assert_eq!(r.failure_class.as_deref(), Some("passes-at-base"));
    assert_eq!(r.head_sha, HEAD);
    assert!(r.validate().is_ok());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_check_still_red_at_head_is_fail() {
    let (root, b, h) = pair("red", "exit 1", "echo nope; exit 3");
    let r = judge(&b, &h, BASE, HEAD, HEAD, &spec()).unwrap();
    assert_eq!(r.status, Status::Fail);
    assert_eq!(r.failure_class.as_deref(), Some("check-failed"));
    assert_eq!(r.head_sha, HEAD);
    assert!(r.message.contains("nope"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_base_that_did_not_run_cleanly_is_not_a_red_result() {
    // 127 (something missing), a signal, and a timeout at base must never read as red.
    for (name, base_body) in [("missing", "exec ./ci/helper.sh"), ("signal", "kill -9 $$"), ("slow", "exec sleep 5")] {
        let (root, b, h) = pair(name, base_body, "exit 0");
        let mut s = spec();
        if name == "slow" {
            s.budget = Duration::from_millis(300);
        }
        let r = judge(&b, &h, BASE, HEAD, HEAD, &s).unwrap();
        assert_eq!((r.status, r.failure_class.as_deref()), (Status::Fail, Some("base-error")), "case {}", name);
        let _ = std::fs::remove_dir_all(root);
    }
}

#[test]
fn bad_shas_and_mismatches_are_refused() {
    let (root, b, h) = pair("shas", "exit 1", "exit 0");
    assert!(judge(&b, &h, BASE, HEAD, BASE, &spec()).is_err(), "recorded sha must match head");
    assert!(judge(&b, &h, HEAD, HEAD, HEAD, &spec()).is_err(), "base equal to head");
    assert!(judge(&b, &h, BASE, "89ABCDEF0123456789ABCDEF0123456789ABCDEF", "89ABCDEF0123456789ABCDEF0123456789ABCDEF", &spec()).is_err(), "uppercase");
    assert!(judge(&b, &h, BASE, "89abcdef", "89abcdef", &spec()).is_err(), "abbreviated");
    assert!(judge(&b, &h, "", HEAD, HEAD, &spec()).is_err(), "empty base");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_same_folder_for_both_ends_is_refused() {
    let (root, b, _h) = pair("same", "exit 1", "exit 0");
    assert!(judge(&b, &b, BASE, HEAD, HEAD, &spec()).is_err());
    let link = root.join("alias");
    symlink(&b, &link).unwrap();
    assert!(judge(&b, &link, BASE, HEAD, HEAD, &spec()).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn bad_check_paths_are_refused() {
    let (root, b, h) = pair("paths", "exit 1", "exit 0");
    put(&root, "outside.sh", "#!/bin/sh\nexit 0\n");
    symlink(root.join("outside.sh"), b.join("ci/link.sh")).unwrap();
    symlink(root.join("outside.sh"), h.join("ci/link.sh")).unwrap();
    put(&b, ".git", "gitdir: x\n");
    put(&h, ".git", "gitdir: x\n");
    let abs = root.join("outside.sh");
    for bad in ["", "../outside.sh", "ci/../../outside.sh", "./ci/check.sh", "ci//check.sh/..", abs.to_str().unwrap(), "ci/link.sh", "ci/missing.sh", "ci", ".git", ".GIT", ".Git"] {
        let mut s = spec();
        s.program = bad;
        assert!(judge(&b, &h, BASE, HEAD, HEAD, &s).is_err(), "should refuse {:?}", bad);
    }
    // the check missing from only one tree is refused too
    let (root2, b2, h2) = pair("onesided", "exit 1", "exit 0");
    std::fs::remove_file(h2.join("ci/check.sh")).unwrap();
    assert!(judge(&b2, &h2, BASE, HEAD, HEAD, &spec()).is_err());
    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(root2);
}

#[test]
fn a_malformed_check_id_is_refused() {
    let (root, b, h) = pair("id", "exit 1", "exit 0");
    let mut s = spec();
    s.id = "bad id!";
    assert!(judge(&b, &h, BASE, HEAD, HEAD, &s).is_err());
    let _ = std::fs::remove_dir_all(root);
}
