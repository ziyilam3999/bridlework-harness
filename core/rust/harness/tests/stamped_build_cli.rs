//! Process-level tests for `ci/stamped-build.sh` (docs/build-check.md is the contract). The script is
//! copied into a scratch git repo and run with `sh`, its PATH holding a fake `cargo` first (a script
//! that records the HARNESS_BUILD_COMMIT value it was handed and exits 0) and the system folders: the
//! recorded value is the stamp the build would run with, and no recording means no build ran. The PATH
//! string is fixed in the test source; no environment value is read.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("sbuild-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

/// The real script text, read from the checkout the tests run in.
fn script_text() -> String {
    fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../ci/stamped-build.sh")).unwrap()
}

/// A scratch repo holding the script at `ci/stamped-build.sh` and one tracked file, plus a bin folder
/// whose `cargo` records the stamp value it sees and the file the recording lands in.
struct Fix {
    _t: Tmp,
    dir: PathBuf,
    bin: PathBuf,
    rec: PathBuf,
}

fn fix() -> Fix {
    let t = Tmp::new();
    let dir = t.0.join("r");
    fs::create_dir_all(dir.join("ci")).unwrap();
    g(&dir, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&dir, &["config", k, v]);
    }
    fs::write(dir.join("ci/stamped-build.sh"), script_text()).unwrap();
    fs::write(dir.join("a.txt"), b"a\n").unwrap();
    g(&dir, &["add", "-A"]);
    g(&dir, &["commit", "-q", "-m", "m"]);
    let bin = t.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let rec = t.0.join("stamp.txt");
    let fake = format!("#!/bin/sh\nprintf '%s\\n' \"$HARNESS_BUILD_COMMIT\" > {}\nexit 0\n", rec.to_str().unwrap());
    fs::write(bin.join("cargo"), fake).unwrap();
    fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    Fix { _t: t, dir, bin, rec }
}

impl Fix {
    /// Run the script with the fake cargo first on the PATH (a fixed string: the bin folder, then the
    /// system folders that hold git).
    fn run(&self) -> Output {
        self.cmd().stdin(Stdio::null()).output().unwrap()
    }

    /// The script command with the git location variables cleared; a test may set them after this.
    fn cmd(&self) -> Command {
        let mut c = Command::new("sh");
        c.arg(self.dir.join("ci/stamped-build.sh"));
        c.env("PATH", format!("{}:/usr/bin:/bin", self.bin.to_str().unwrap()));
        for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
            c.env_remove(k);
        }
        c
    }

    /// The run was refused: exit 1, empty stdout, and the fake cargo did not run.
    fn assert_refused(&self, what: &str) {
        let o = self.run();
        assert_eq!(o.status.code(), Some(1), "{what}: stdout {:?} stderr {:?}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
        assert_eq!(String::from_utf8_lossy(&o.stdout), "", "{what}");
        assert!(!self.rec.exists(), "{what}: a refused tree ran the build");
    }
}

#[test]
fn a_clean_tree_is_built_with_the_head_commit_as_the_stamp() {
    let f = fix();
    let head = g(&f.dir, &["rev-parse", "HEAD"]);
    let o = f.run();
    assert_eq!(o.status.code(), Some(0), "stdout {:?} stderr {:?}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), format!("stamped-build: built commit={head}\n"));
    assert_eq!(fs::read_to_string(&f.rec).unwrap(), format!("{head}\n"), "the build ran with the HEAD commit as the stamp");
}

#[test]
fn a_tracked_change_an_untracked_file_and_no_commit_are_refused_and_cargo_is_not_run() {
    // a tracked change
    let f = fix();
    fs::write(f.dir.join("a.txt"), b"changed\n").unwrap();
    let o = f.run();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert!(!f.rec.exists(), "a refused tree ran the build");
    // an untracked file
    fs::write(f.dir.join("a.txt"), b"a\n").unwrap();
    fs::write(f.dir.join("new.txt"), b"n\n").unwrap();
    let o = f.run();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert!(!f.rec.exists(), "a refused tree ran the build");
    // a repo with no commit
    let t = Tmp::new();
    let dir = t.0.join("r2");
    fs::create_dir_all(dir.join("ci")).unwrap();
    g(&dir, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&dir, &["config", k, v]);
    }
    fs::write(dir.join("ci/stamped-build.sh"), script_text()).unwrap();
    let bin = t.0.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let rec = t.0.join("stamp.txt");
    let fake = format!("#!/bin/sh\nprintf '%s\\n' \"$HARNESS_BUILD_COMMIT\" > {}\nexit 0\n", rec.to_str().unwrap());
    fs::write(bin.join("cargo"), fake).unwrap();
    fs::set_permissions(bin.join("cargo"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut c = Command::new("sh");
    c.arg(dir.join("ci/stamped-build.sh"));
    c.env("PATH", format!("{}:/usr/bin:/bin", bin.to_str().unwrap()));
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        c.env_remove(k);
    }
    let o = c.stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert!(!rec.exists(), "a refused tree ran the build");
}

#[test]
fn a_git_status_that_fails_is_refused_and_cargo_is_not_run() {
    let f = fix();
    // a corrupt index: git status fails while rev-parse HEAD still works
    fs::write(f.dir.join(".git/index"), b"not an index file\n").unwrap();
    assert!(!gitcmd(&f.dir, &["status", "--porcelain"]).output().unwrap().status.success(), "the scratch index is not broken");
    f.assert_refused("a failing git status");
}

#[test]
fn an_untracked_file_is_refused_whatever_the_git_config_says() {
    let f = fix();
    g(&f.dir, &["config", "status.showUntrackedFiles", "no"]);
    fs::write(f.dir.join("new.txt"), b"n\n").unwrap();
    assert_eq!(g(&f.dir, &["status", "--porcelain"]), "", "the config hides the file from a plain status");
    f.assert_refused("an untracked file with showUntrackedFiles=no");
}

#[test]
fn the_stamp_is_head_of_the_checkout_even_when_git_dir_names_another_repo() {
    let f = fix();
    let head = g(&f.dir, &["rev-parse", "HEAD"]);
    // a second valid repo with its own commit, named by GIT_DIR and GIT_WORK_TREE
    let other = f._t.0.join("other");
    fs::create_dir_all(&other).unwrap();
    g(&other, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&other, &["config", k, v]);
    }
    fs::write(other.join("o.txt"), b"o\n").unwrap();
    g(&other, &["add", "-A"]);
    g(&other, &["commit", "-q", "-m", "o"]);
    let other_head = g(&other, &["rev-parse", "HEAD"]);
    assert_ne!(head, other_head);
    let mut c = f.cmd();
    c.env("GIT_DIR", other.join(".git")).env("GIT_WORK_TREE", &other);
    let o = c.stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "stdout {:?} stderr {:?}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
    assert_eq!(String::from_utf8_lossy(&o.stdout), format!("stamped-build: built commit={head}\n"));
    assert_eq!(fs::read_to_string(&f.rec).unwrap(), format!("{head}\n"), "the stamp names HEAD of the script's own checkout");
}

#[test]
fn a_failing_cargo_build_exits_1_with_empty_stdout() {
    let f = fix();
    let fake = format!("#!/bin/sh\nprintf '%s\\n' \"$HARNESS_BUILD_COMMIT\" > {}\nexit 1\n", f.rec.to_str().unwrap());
    fs::write(f.bin.join("cargo"), fake).unwrap();
    let o = f.run();
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert!(f.rec.exists(), "the build was started");
}
