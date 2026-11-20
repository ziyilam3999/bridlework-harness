//! `harness build check` (std only; `docs/build-check.md` is the contract). Says whether this binary is
//! built from the head of the engine source as last fetched. The binary carries the commit it was built
//! from as a build-time stamp: the HARNESS_BUILD_COMMIT variable read with option_env! at compile time
//! (no build script, no git call at build time), set by ci/stamped-build.sh, which reads HEAD of the
//! checkout it lives in and refuses a tree that is not that commit. A plain cargo build sets nothing, so
//! such a binary has no stamp and is refused as cannot tell: an unstamped binary is not taken as current.
//! The verb compares commits, not changes: a merge that changes no file a caller uses still makes an
//! older binary older. It reads one binary constant, the arguments and read-only git answers; it writes
//! nothing and fetches nothing, so the head is the head as last fetched and the caller fetches first.
//! Exit codes: 0 current (the stamp equals the head); 1 older (the stamp is a commit of the repo dir and
//! an ancestor of the head); 2 cannot tell (one stderr line with the reason) or a usage fault.

use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};

pub const USAGE: &str = "usage: harness build check --repo-dir <absolute-dir> --ref <ref>";

/// The commit this binary was built from, read at build time. None when the variable was not set (a
/// plain cargo build, a CI build, a release tarball build): such a binary cannot be judged.
const STAMP: Option<&str> = option_env!("HARNESS_BUILD_COMMIT");

fn usage_fault(m: &str) -> i32 {
    eprintln!("harness: error: {m}");
    eprintln!("{USAGE}");
    2
}

fn cannot_tell(reason: &str) -> i32 {
    eprintln!("harness: build check refused: cannot tell: {reason}");
    2
}

fn stamp_ok(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// One read-only git call on the repo dir: GIT_DIR and GIT_WORK_TREE removed from the environment, no
/// stdin, no replace refs, no lazy fetch of a missing object (an unfetched object fails the call, it is
/// not fetched). None: git cannot run.
fn git(repo: &Path, args: &[&str]) -> Option<Output> {
    let mut c = Command::new("git");
    c.arg("--no-replace-objects").arg("--no-lazy-fetch").arg("-C").arg(repo).args(args);
    c.env_remove("GIT_DIR").env_remove("GIT_WORK_TREE");
    c.stdin(Stdio::null()).output().ok()
}

/// `harness build check --repo-dir <absolute-dir> --ref <ref>` (see the module doc for the contract).
/// `--stamp-for-test <sha>` is read only when the caller passed the test mode flag, and stands in for
/// the compiled stamp.
pub fn build_check_cmd(a: &[String], testmode: bool) -> i32 {
    if a.first().map(|s| s.as_str()) != Some("check") {
        return usage_fault("build check is the only build verb");
    }
    let (mut repo, mut head_ref, mut stamp_for_test): (Option<&str>, Option<&str>, Option<&str>) = (None, None, None);
    let mut it = a[1..].iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--repo-dir" => &mut repo,
            "--ref" => &mut head_ref,
            "--stamp-for-test" if testmode => &mut stamp_for_test,
            "--stamp-for-test" => return usage_fault("--stamp-for-test is read only with --testmode synthetic-test"),
            _ => return usage_fault(&format!("unknown option {arg}")),
        };
        let Some(v) = it.next() else { return usage_fault(&format!("{arg} is missing its value")) };
        if slot.replace(v.as_str()).is_some() {
            return usage_fault(&format!("{arg} was given twice"));
        }
    }
    let (Some(repo), Some(head_ref)) = (repo, head_ref) else {
        return usage_fault("--repo-dir and --ref are required");
    };
    let dir = PathBuf::from(repo);
    if !dir.is_absolute() || dir.components().any(|c| matches!(c, Component::ParentDir)) {
        return usage_fault("--repo-dir must be an absolute path");
    }
    if !dir.is_dir() {
        return usage_fault("--repo-dir is not a folder");
    }
    if head_ref.is_empty() {
        return usage_fault("--ref is empty");
    }
    if head_ref.starts_with('-') {
        return usage_fault("--ref must not start with a dash");
    }
    // The stamp under test: the test flag stands in for the compiled one; the compiled one is None in a
    // build that set no variable, and such a binary is refused, not taken as current.
    let stamp = match stamp_for_test {
        Some(s) => s,
        None => match STAMP {
            Some(s) => s,
            None => return cannot_tell("this binary has no build stamp"),
        },
    };
    if !stamp_ok(stamp) {
        return cannot_tell("the build stamp is not a 40 character lower-case commit id");
    }
    // The head: the ref resolved to its commit (a tag is peeled) in the repo dir, as last fetched.
    let head = match git(&dir, &["rev-parse", "--verify", &format!("{head_ref}^{{commit}}")]) {
        None => return cannot_tell("git is not runnable"),
        Some(o) if !o.status.success() => return cannot_tell("the head ref cannot be resolved in the repo dir"),
        Some(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
    };
    if !stamp_ok(&head) {
        return cannot_tell("the head ref cannot be resolved in the repo dir");
    }
    if stamp == head {
        println!("build current commit={}", &stamp[..12]);
        return 0;
    }
    // A stamp the repo dir does not hold (a branch it has not fetched, a made-up id) cannot be judged.
    match git(&dir, &["cat-file", "-t", stamp]) {
        None => return cannot_tell("git is not runnable"),
        Some(o) if o.status.success() && o.stdout == b"commit\n" => {}
        Some(_) => return cannot_tell("the build stamp is not a commit of the repo dir"),
    }
    // A stamp ahead of the head or on a side branch is not called current: the verb cannot show that
    // this binary holds the head, so anything but a strict ancestor is a refusal.
    match git(&dir, &["merge-base", "--is-ancestor", stamp, &head]) {
        None => return cannot_tell("git is not runnable"),
        Some(o) if o.status.success() => {}
        Some(_) => return cannot_tell("the build stamp is not an ancestor of the head"),
    }
    let ahead = match git(&dir, &["rev-list", "--count", &format!("{stamp}..{head}")]) {
        None => return cannot_tell("git is not runnable"),
        Some(o) if !o.status.success() => return cannot_tell("the commits between the stamp and the head cannot be counted"),
        Some(o) => String::from_utf8_lossy(&o.stdout).trim().parse::<u64>().unwrap_or(0),
    };
    if ahead == 0 {
        return cannot_tell("the commits between the stamp and the head cannot be counted");
    }
    eprintln!("harness: build check refused: older: this binary is built from {}, the head is {} commits ahead; rebuild it", &stamp[..12], ahead);
    1
}
