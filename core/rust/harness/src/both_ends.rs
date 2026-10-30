//! Both-ends mode (std only), keyed by head sha. A ticket's declared check must FAIL at base and PASS at head; the result names the head sha it ran on.
//! A check that already passes at base catches nothing and is FAIL. A head that does not match the recorded sha, or equals base, is refused.
//! The check file is always taken from head and run in both trees, so a test added by the fix itself still runs against base.
//! Known limits: only the one declared check file is carried to base, so a helper file the check needs must already exist there (a missing
//! helper that shows as exit 126 or 127 is a base-error, but a runner that reports a missing target with another code cannot be told apart
//! from a real red); the two ends run one after the other, so a stateful check can differ between them; the trees are git worktrees.

use crate::gate_result::{GateResult, Status};
use crate::runner::{run_code, Check};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    let mut c = Command::new("git");
    // No hooks (worktree add would run post-checkout), and no inherited GIT_* variable may redirect git to another repo.
    c.arg("-C").arg(repo).args(["-c", "core.hooksPath=/dev/null"]).args(args);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    let out = c.stdin(std::process::Stdio::null()).output().map_err(|_| "git could not be started".to_string())?;
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

/// A detached worktree in its own fresh folder, removed when dropped. Both ends use the same inner name, so the path does not say which end it is.
struct Tree {
    repo: PathBuf,
    holder: PathBuf,
    path: PathBuf,
}

impl Tree {
    fn add(repo: &Path, sha: &str) -> Result<Tree, String> {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let holder = std::env::temp_dir().join(format!("hbe-{}-{}-{}", std::process::id(), n, ns));
        // create_dir fails if the folder exists, so another run's live tree is never touched.
        std::fs::create_dir(&holder).map_err(|_| "could not create a scratch folder".to_string())?;
        let path = holder.join("t");
        let tree = Tree { repo: repo.to_path_buf(), holder, path };
        let p = tree.path.to_str().ok_or("temp path not utf-8")?.to_string();
        git(repo, &["worktree", "add", "--detach", &p, sha]).map_err(|_| "could not create a worktree".to_string())?;
        Ok(tree)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(p) = self.path.to_str() {
            let _ = git(&self.repo, &["worktree", "remove", "--force", p]);
        }
        let _ = std::fs::remove_dir_all(&self.holder);
    }
}

fn verdict(spec: &Spec, head_sha: &str, status: Status, class: Option<&str>, message: &str) -> GateResult {
    GateResult { status, gate_id: spec.id.to_string(), head_sha: head_sha.to_string(), failure_class: class.map(|c| c.to_string()), message: message.to_string(), file: None, line: None }
}

/// Refuse a base tree where any part of the check path is a symlink, so the copy below can never write or delete outside the tree.
fn no_symlink_on_path(root: &Path, program: &str) -> Result<(), String> {
    let mut p = root.to_path_buf();
    for part in Path::new(program).components() {
        p.push(part);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => return Err("base tree has a symlink on the check path".to_string()),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// Run the declared check at `base` and at `head`. Err means the request is refused (bad ref, head not the recorded sha, head equal to base, check path invalid);
/// Ok carries a normalized result.
pub fn both_ends(repo: &Path, base: &str, head: &str, recorded_head: &str, spec: &Spec) -> Result<GateResult, String> {
    let base_sha = commit_sha(repo, base)?;
    let head_sha = commit_sha(repo, head)?;
    if head_sha != recorded_head {
        return Err("head does not match the recorded sha".to_string());
    }
    if base_sha == head_sha {
        return Err("base and head are the same commit".to_string());
    }
    let head_tree = Tree::add(repo, &head_sha)?;
    let base_tree = Tree::add(repo, &base_sha)?;
    // The check file comes from head (validated inside the head tree); carry it into the base tree.
    Check::load(&head_tree.path, spec.id, spec.program, spec.args, spec.budget, spec.max_output)?;
    no_symlink_on_path(&base_tree.path, spec.program)?;
    let src = head_tree.path.join(spec.program);
    let dst = base_tree.path.join(spec.program);
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|_| "could not prepare the base tree".to_string())?;
    }
    let _ = std::fs::remove_file(&dst);
    std::fs::copy(&src, &dst).map_err(|_| "could not carry the check to the base tree".to_string())?;

    let base_check = Check::load(&base_tree.path, spec.id, spec.program, spec.args, spec.budget, spec.max_output)?;
    let (at_base, base_code) = run_code(&base_tree.path, &base_check, &head_sha);
    if at_base.status == Status::Pass {
        return Ok(verdict(spec, &head_sha, Status::Fail, Some("passes-at-base"), "the check passes at base, so it catches nothing"));
    }
    // 126 and 127 mean the shell could not run something the check needs: that is a missing piece, not a red result.
    if at_base.failure_class.as_deref() != Some("check-failed") || matches!(base_code, Some(126) | Some(127)) {
        return Ok(verdict(spec, &head_sha, Status::Fail, Some("base-error"), "the check did not run cleanly at base, so it is not a red result"));
    }
    let head_check = Check::load(&head_tree.path, spec.id, spec.program, spec.args, spec.budget, spec.max_output)?;
    let (at_head, _) = run_code(&head_tree.path, &head_check, &head_sha);
    if at_head.status == Status::Pass {
        return Ok(verdict(spec, &head_sha, Status::Pass, None, "fails at base and passes at head"));
    }
    Ok(at_head)
}
