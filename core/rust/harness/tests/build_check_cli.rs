//! Process-level tests for `harness build check` (docs/build-check.md is the contract). The real
//! binary runs as a child against a temp git repo built with local git only (no network). The stamp
//! under test is given with `--stamp-for-test`, which the verb reads only after the test mode flag;
//! the compiled test binary sets no build stamp while HARNESS_BUILD_COMMIT is not exported in the
//! environment of cargo test, and one test proves that arm. Each refusal
//! test checks the exact stderr line and an empty stdout.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const USAGE: &str = "usage: harness build check --repo-dir <absolute-dir> --ref <ref>";
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("bchk-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn gitcmd(dir: &Path, args: &[&str]) -> Command {
    let contact = format!("user.email=t{}scratch", '@');
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str(), "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        c.env_remove(k);
    }
    c.stdin(Stdio::null());
    c
}

fn g(dir: &Path, args: &[&str]) -> String {
    let out = gitcmd(dir, args).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init() -> (Tmp, PathBuf) {
    let t = Tmp::new();
    let dir = t.0.join("repo");
    fs::create_dir_all(&dir).unwrap();
    g(&dir, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&dir, &["config", k, v]);
    }
    (t, dir)
}

fn commit_all(dir: &Path, files: &[(&str, &[u8])]) -> String {
    for (p, b) in files {
        let path = dir.join(p);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, b).unwrap();
    }
    g(dir, &["add", "-A"]);
    g(dir, &["commit", "-q", "-m", "m"]);
    g(dir, &["rev-parse", "HEAD"])
}

/// A scratch repo holding `n` commits, one file changed by each.
fn repo_n(n: usize) -> (Tmp, PathBuf, Vec<String>) {
    let (t, dir) = init();
    let mut shas = Vec::new();
    for i in 0..n {
        shas.push(commit_all(&dir, &[(&format!("f{i}.txt"), format!("v{i}\n").as_bytes())]));
    }
    (t, dir, shas)
}

fn bin(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

/// The verb under the test mode flag, with `--stamp-for-test` standing in for the compiled stamp.
fn check_stamp(dir: &Path, head_ref: &str, stamp: &str) -> Output {
    bin(&["--testmode", "synthetic-test", "build", "check", "--repo-dir", dir.to_str().unwrap(), "--ref", head_ref, "--stamp-for-test", stamp])
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

fn check_current(o: &Output, stamp: &str) {
    assert_eq!(code(o), 0, "{}", err(o));
    assert_eq!(err(o), "");
    assert_eq!(out(o), format!("build current commit={}\n", &stamp[..12]));
}

fn check_cannot_tell(o: &Output, reason: &str) {
    assert_eq!(code(o), 2);
    assert_eq!(out(o), "");
    assert_eq!(err(o), format!("harness: build check refused: cannot tell: {reason}\n"));
}

fn check_older(o: &Output, stamp: &str, ahead: u64) {
    assert_eq!(code(o), 1);
    assert_eq!(out(o), "");
    assert_eq!(err(o), format!("harness: build check refused: older: this binary is built from {}, the head is {} commits ahead; rebuild it\n", &stamp[..12], ahead));
}

#[test]
fn a_stamp_equal_to_the_head_is_current() {
    let (_t, dir, shas) = repo_n(1);
    let head = &shas[0];
    check_current(&check_stamp(&dir, "HEAD", head), head);
    check_current(&check_stamp(&dir, head, head), head);
    // the check reads only the repo dir and the arguments: the tree is clean after the call
    assert_eq!(g(&dir, &["status", "--porcelain"]), "");
}

#[test]
fn a_stamp_behind_the_head_is_older_and_counts_the_commits() {
    let (_t, dir, shas) = repo_n(3);
    check_older(&check_stamp(&dir, "HEAD", &shas[0]), &shas[0], 2);
    check_older(&check_stamp(&dir, "HEAD", &shas[1]), &shas[1], 1);
}

#[test]
fn a_stamp_that_cannot_be_judged_is_cannot_tell_exit_2_with_empty_stdout() {
    // a stamp on a side branch: HEAD moved on another line from the stamped commit
    let (_t, dir, shas) = repo_n(2);
    g(&dir, &["checkout", "-q", "--detach", &shas[0]]);
    let _side = commit_all(&dir, &[("side.txt", b"s\n")]);
    check_cannot_tell(&check_stamp(&dir, "HEAD", &shas[1]), "the build stamp is not an ancestor of the head");
    // a stamp ahead of the head: the ref sits at the older commit
    check_cannot_tell(&check_stamp(&dir, &shas[0], &shas[1]), "the build stamp is not an ancestor of the head");
    // an unknown ref
    check_cannot_tell(&check_stamp(&dir, "nosuchref", &shas[0]), "the head ref cannot be resolved in the repo dir");
    // a well-formed id that is no commit of the repo
    let made_up = "b".repeat(40);
    check_cannot_tell(&check_stamp(&dir, "HEAD", &made_up), "the build stamp is not a commit of the repo dir");
    // shapes that are not a 40 character lower-case commit id
    let head = &shas[0];
    let upper = head.to_uppercase();
    for bad in [upper.as_str(), &head[..39], &format!("0{head}"), "", "unknown", &format!(" {head}")] {
        check_cannot_tell(&check_stamp(&dir, "HEAD", bad), "the build stamp is not a 40 character lower-case commit id");
    }
}

#[test]
fn a_binary_built_with_no_stamp_cannot_tell() {
    let (_t, dir, _shas) = repo_n(1);
    // the compiled test binary sets no stamp: the verb must refuse it, not take it as current
    let o = bin(&["--testmode", "synthetic-test", "build", "check", "--repo-dir", dir.to_str().unwrap(), "--ref", "HEAD"]);
    check_cannot_tell(&o, "this binary has no build stamp");
}

#[test]
fn a_tag_ref_is_resolved_to_its_commit_and_a_folder_that_is_no_repo_cannot_tell() {
    let (t, dir, shas) = repo_n(1);
    g(&dir, &["tag", "-a", "v1", "-m", "t"]);
    // the annotated tag is peeled to its commit, so a stamp equal to that commit is current
    check_current(&check_stamp(&dir, "v1", &shas[0]), &shas[0]);
    // a folder that is no repo: the head cannot be resolved there
    let empty = t.0.join("notrepo");
    fs::create_dir_all(&empty).unwrap();
    check_cannot_tell(&check_stamp(&empty, "HEAD", &shas[0]), "the head ref cannot be resolved in the repo dir");
}

#[test]
fn argument_faults_exit_2_and_the_test_flag_needs_the_test_mode() {
    let (_t, dir, shas) = repo_n(1);
    let d = dir.to_str().unwrap();
    let sha = &shas[0];
    let cases: Vec<Vec<&str>> = vec![
        vec!["build"],
        vec!["build", "stamp"],
        vec!["build", "check"],
        vec!["build", "check", "--ref", "HEAD"],
        vec!["build", "check", "--repo-dir", d],
        vec!["build", "check", "--repo-dir", "repo", "--ref", "HEAD"],
        vec!["build", "check", "--repo-dir", d, "--ref", "-x"],
        vec!["build", "check", "--repo-dir", d, "--ref", ""],
        vec!["build", "check", "--repo-dir", d, "--ref", "HEAD", "--ref", "HEAD"],
        vec!["build", "check", "--repo-dir", d, "--ref"],
        vec!["build", "check", "--repo-dir", d, "--ref", "HEAD", "--other", "x"],
        vec!["build", "check", "--repo-dir", d, "--ref", "HEAD", "--stamp-for-test", sha],
    ];
    for a in &cases {
        let o = bin(a);
        assert_eq!(code(&o), 2, "case {a:?}: stdout {:?} stderr {:?}", out(&o), err(&o));
        assert_eq!(out(&o), "", "case {a:?}");
        let e = err(&o);
        let mut lines = e.lines();
        assert!(lines.next().unwrap().starts_with("harness: error: "), "case {a:?}: {e}");
        assert_eq!(lines.next(), Some(USAGE), "case {a:?}: {e}");
        assert_eq!(lines.next(), None, "case {a:?}: {e}");
    }
}
