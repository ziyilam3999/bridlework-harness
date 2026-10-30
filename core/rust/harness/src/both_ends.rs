//! Both-ends mode (std only), keyed by head sha. A ticket's declared check must FAIL at base and PASS at head; the result names the head sha it ran on.
//! A check that already passes at base catches nothing and is FAIL. A head that does not match the recorded sha is refused.
//! The check file is always taken from head and run in both trees, so a test added by the fix itself still runs against base.
//! Known limits: only the one declared check file is carried to base (helper files must already exist there); the trees are git worktrees of the repo.

use crate::gate_result::{GateResult, Status};
use crate::runner::{run, Check};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// The declared check: a path inside the repo plus its limits.
pub struct Spec<'a> {
    pub id: &'a str,
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub budget: Duration,
    pub max_output: usize,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| "git could not be started".to_string())?;
    if !out.status.success() {
        return Err("git refused the request".to_string());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Resolve a ref to a full commit sha. A leading dash is never passed to git.
fn commit_sha(repo: &Path, r: &str) -> Result<String, String> {
    if r.is_empty() || r.starts_with('-') {
        return Err("ref malformed".to_string());
    }
    let sha = git(repo, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", r)]).map_err(|_| "ref does not resolve to a commit".to_string())?;
    if sha.len() == 40 || sha.len() == 64 {
        Ok(sha)
    } else {
        Err("ref does not resolve to a commit".to_string())
    }
}

/// A detached worktree that is removed when dropped.
struct Tree {
    repo: PathBuf,
    path: PathBuf,
}

impl Tree {
    fn add(repo: &Path, sha: &str, tag: &str) -> Result<Tree, String> {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("hbe-{}-{}-{}", std::process::id(), n, tag));
        let _ = std::fs::remove_dir_all(&path);
        let p = path.to_str().ok_or("temp path not utf-8")?;
        git(repo, &["worktree", "add", "--detach", p, sha]).map_err(|_| "could not create a worktree".to_string())?;
        Ok(Tree { repo: repo.to_path_buf(), path })
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(p) = self.path.to_str() {
            let _ = git(&self.repo, &["worktree", "remove", "--force", p]);
        }
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = git(&self.repo, &["worktree", "prune"]);
    }
}

fn run_in(tree: &Tree, spec: &Spec, head_sha: &str) -> Result<GateResult, String> {
    let c = Check::load(&tree.path, spec.id, spec.program, spec.args, spec.budget, spec.max_output)?;
    Ok(run(&tree.path, &c, head_sha))
}

fn verdict(spec: &Spec, head_sha: &str, status: Status, class: Option<&str>, message: &str) -> GateResult {
    GateResult { status, gate_id: spec.id.to_string(), head_sha: head_sha.to_string(), failure_class: class.map(|c| c.to_string()), message: message.to_string(), file: None, line: None }
}

/// Run the declared check at `base` and at `head`. Err means the request is refused (bad ref, head not the recorded sha, check path invalid); Ok carries a normalized result.
pub fn both_ends(repo: &Path, base: &str, head: &str, recorded_head: &str, spec: &Spec) -> Result<GateResult, String> {
    let base_sha = commit_sha(repo, base)?;
    let head_sha = commit_sha(repo, head)?;
    if head_sha != recorded_head {
        return Err("head does not match the recorded sha".to_string());
    }
    let head_tree = Tree::add(repo, &head_sha, "head")?;
    let base_tree = Tree::add(repo, &base_sha, "base")?;
    // The check file comes from head; carry it into the base tree.
    let head_real = Check::load(&head_tree.path, spec.id, spec.program, spec.args, spec.budget, spec.max_output)?;
    drop(head_real);
    let src = head_tree.path.join(spec.program);
    let dst = base_tree.path.join(spec.program);
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|_| "could not prepare the base tree".to_string())?;
    }
    let _ = std::fs::remove_file(&dst);
    std::fs::copy(&src, &dst).map_err(|_| "could not carry the check to the base tree".to_string())?;

    let at_base = run_in(&base_tree, spec, &head_sha)?;
    if at_base.status == Status::Pass {
        return Ok(verdict(spec, &head_sha, Status::Fail, Some("passes-at-base"), "the check passes at base, so it catches nothing"));
    }
    if at_base.failure_class.as_deref() != Some("check-failed") {
        return Ok(verdict(spec, &head_sha, Status::Fail, Some("base-error"), "the check did not run cleanly at base, so it is not a red result"));
    }
    let at_head = run_in(&head_tree, spec, &head_sha)?;
    if at_head.status == Status::Pass {
        return Ok(verdict(spec, &head_sha, Status::Pass, None, "fails at base and passes at head"));
    }
    Ok(at_head)
}
