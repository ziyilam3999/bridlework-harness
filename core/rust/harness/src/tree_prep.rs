//! Tree preparation for both-ends mode (std only, Unix). Makes a temporary tree at the base commit and one at the head commit, runs the declared check once
//! in each, removes the trees, and returns one end report per end. It does no pass/fail mapping; that is the judgement ticket's job.
//! One rule runs through it: every input is resolved once, under its one root, and only the resolved value is used afterwards; every write is re-checked
//! against the root it lands in. Non-goals: the check is not sandboxed (a hostile check can still find the main repo or tell the ends apart by commit or
//! content); grandchildren are not killed; a stateful check may differ between runs. Removing `GIT_*` stops accidental redirection only.
//! The check at BOTH ends is the head commit's blob bytes at mode 0755, so a content filter or a file mode in either checkout cannot make the ends differ.

use crate::runner::{run_ended, Check, Ended};
use std::ffi::OsString;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The declared check: a path in the head commit plus its limits.
pub struct Spec<'a> {
    pub id: &'a str,
    pub program: &'a str,
    pub args: &'a [&'a str],
    pub budget: Duration,
    pub max_output: usize,
}

/// What happened at one end: the full commit sha, how the process ended, and the capped output (with its cut marker).
#[derive(Debug, Clone)]
pub struct EndReport {
    pub sha: String,
    pub ended: Ended,
    pub output: String,
}

#[derive(Debug, Clone)]
pub struct Reports {
    pub base: EndReport,
    pub head: EndReport,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);
const MAX_CHECK_BYTES: usize = 4 * 1024 * 1024;

/// One git call: no hooks, no inherited `GIT_*` name, no stdin.
fn git_bytes(repo: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(["-c", "core.hooksPath=/dev/null"]).args(args);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    let out = c.stdin(Stdio::null()).output().map_err(|_| "git could not be started".to_string())?;
    if !out.status.success() {
        return Err("git refused the request".to_string());
    }
    Ok(out.stdout)
}

fn git(repo: &Path, args: &[&str]) -> Result<String, String> {
    Ok(String::from_utf8_lossy(&git_bytes(repo, args)?).trim().to_string())
}

/// The repo path, resolved once: canonical, equal to its own top level, and a main work tree (not a linked one).
struct Repo {
    top: PathBuf,
    common: PathBuf,
}

fn resolve_repo(path: &Path) -> Result<Repo, String> {
    let canon = path.canonicalize().map_err(|_| "repo path unreadable".to_string())?;
    let out = git(&canon, &["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-dir", "--git-common-dir"]).map_err(|_| "not a git work tree".to_string())?;
    let lines: Vec<&str> = out.lines().collect();
    if lines.len() != 3 {
        return Err("not a git work tree".to_string());
    }
    let top = Path::new(lines[0]).canonicalize().map_err(|_| "repo top level unreadable".to_string())?;
    let gitdir = Path::new(lines[1]).canonicalize().map_err(|_| "git dir unreadable".to_string())?;
    let common = Path::new(lines[2]).canonicalize().map_err(|_| "git common dir unreadable".to_string())?;
    if top != canon {
        return Err("repo path is not the top level of its work tree".to_string());
    }
    if gitdir != common {
        return Err("repo path is a linked work tree".to_string());
    }
    Ok(Repo { top, common })
}

/// A ref, resolved once to a full commit sha. A leading dash is never passed to git; it must peel to a commit.
fn commit_sha(repo: &Path, r: &str) -> Result<String, String> {
    if r.is_empty() || r.starts_with('-') || r.contains('\n') || r.contains('\0') {
        return Err("ref malformed".to_string());
    }
    let sha = git(repo, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", r)]).map_err(|_| "ref does not resolve to a commit".to_string())?;
    if (sha.len() == 40 || sha.len() == 64) && sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) {
        Ok(sha)
    } else {
        Err("ref does not resolve to a commit".to_string())
    }
}

/// The scratch root, from the real process environment (TMPDIR, else /tmp): absolute, canonical, outside the repo work tree and git dir.
fn scratch_root(repo: &Repo) -> Result<PathBuf, String> {
    let raw = std::env::var_os("TMPDIR").filter(|v| !v.is_empty()).unwrap_or_else(|| OsString::from("/tmp"));
    let p = PathBuf::from(raw);
    if !p.is_absolute() {
        return Err("scratch root must be an absolute path".to_string());
    }
    let root = p.canonicalize().map_err(|_| "scratch root unreadable".to_string())?;
    if !root.is_dir() {
        return Err("scratch root is not a folder".to_string());
    }
    if root.starts_with(&repo.top) || root.starts_with(&repo.common) {
        return Err("scratch root is inside the repo".to_string());
    }
    Ok(root)
}

/// The check path: plain names only, no part equal to `.git` in any letter case.
fn plain_parts(program: &str) -> Result<Vec<OsString>, String> {
    let mut parts = Vec::new();
    for c in Path::new(program).components() {
        match c {
            Component::Normal(n) if !n.to_string_lossy().eq_ignore_ascii_case(".git") => parts.push(n.to_os_string()),
            _ => return Err("check path must be plain names relative to the repo".to_string()),
        }
    }
    if parts.is_empty() {
        return Err("check path must be plain names relative to the repo".to_string());
    }
    Ok(parts)
}

/// The check, read from the head commit: it must be a regular executable blob at that path.
fn head_blob(repo: &Path, head_sha: &str, program: &str) -> Result<Vec<u8>, String> {
    let out = git_bytes(repo, &["ls-tree", "-z", head_sha, "--", program])?;
    let rec = out.split(|b| *b == 0).next().unwrap_or(&[]);
    let text = String::from_utf8_lossy(rec).into_owned();
    let (meta, path) = text.split_once('\t').ok_or("check path is not in the head commit")?;
    if path != program {
        return Err("check path is not in the head commit".to_string());
    }
    let f: Vec<&str> = meta.split(' ').collect();
    if f.len() != 3 || f[0] != "100755" || f[1] != "blob" {
        return Err("check path is not a regular executable file in the head commit".to_string());
    }
    let size: usize = git(repo, &["cat-file", "-s", f[2]])?.parse().map_err(|_| "check file size unreadable".to_string())?;
    if size > MAX_CHECK_BYTES {
        return Err("check file is too large".to_string());
    }
    git_bytes(repo, &["cat-file", "blob", f[2]])
}

/// The private scratch folder, removed when dropped.
struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn create(root: &Path) -> Result<Scratch, String> {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let path = root.join(format!("hbe-{}-{}-{}", std::process::id(), n, ns));
        // Exclusive, owner-only: fails if the name exists, so another run's folder is never touched.
        std::fs::DirBuilder::new().mode(0o700).create(&path).map_err(|_| "could not create the scratch folder".to_string())?;
        Ok(Scratch { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        force_remove(&self.path);
    }
}

/// Make every real folder under `p` writable again (never following a symlink), so a check that left read-only folders cannot block removal.
fn relax(p: &Path) {
    if let Ok(m) = std::fs::symlink_metadata(p) {
        if m.file_type().is_dir() {
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o700));
            if let Ok(rd) = std::fs::read_dir(p) {
                for e in rd.flatten() {
                    relax(&e.path());
                }
            }
        }
    }
}

fn force_remove(p: &Path) {
    if std::fs::remove_dir_all(p).is_err() {
        relax(p);
        let _ = std::fs::remove_dir_all(p);
    }
}

/// A detached worktree, removed when dropped. Its admin entry is recorded before any check runs, so cleanup never needs a repo-wide prune.
struct Tree {
    repo: PathBuf,
    common: PathBuf,
    path: PathBuf,
    admin: Option<PathBuf>,
}

impl Tree {
    fn add(repo: &Repo, path: &Path, sha: &str) -> Result<Tree, String> {
        let p = path.to_str().ok_or("tree path not utf-8")?;
        let mut tree = Tree { repo: repo.top.clone(), common: repo.common.clone(), path: path.to_path_buf(), admin: None };
        git(&repo.top, &["worktree", "add", "--detach", p, sha]).map_err(|_| "could not create a worktree".to_string())?;
        // Record this tree's own admin entry now, as git itself resolves it (absolute, even when the pointer file is relative), before any check runs.
        let admin = git(path, &["rev-parse", "--path-format=absolute", "--git-dir"]).map_err(|_| "worktree git dir unreadable".to_string())?;
        tree.admin = Some(Path::new(&admin).canonicalize().map_err(|_| "worktree git dir unreadable".to_string())?);
        Ok(tree)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        if let Some(p) = self.path.to_str() {
            let _ = git(&self.repo, &["worktree", "remove", "--force", p]);
        }
        // If git could not remove it (for example the check rewrote `.git`), remove the folder and this tree's own admin entry only.
        force_remove(&self.path);
        if let Some(admin) = &self.admin {
            if admin.starts_with(self.common.join("worktrees")) && admin != &self.common.join("worktrees") {
                force_remove(admin);
            }
        }
    }
}

/// Place the head's check into a tree (base: carried over; head: rewritten from the blob so both ends run the same bytes). Every existing parent part must be a real directory (never a symlink); missing parts are created one at a time;
/// the destination must be absent or a regular single-link file that is not the tree's `.git` entry; the canonical parent must stay under the canonical tree.
fn place_check(tree: &Path, parts: &[OsString], bytes: &[u8]) -> Result<(), String> {
    let (file, dirs) = parts.split_last().ok_or("check path empty")?;
    let mut p = tree.to_path_buf();
    for d in dirs {
        p.push(d);
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => return Err("base tree has a non-directory or symlink on the check path".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&p).map_err(|_| "could not prepare the base tree".to_string())?,
            Err(_) => return Err("could not inspect the base tree".to_string()),
        }
    }
    let dst = p.join(file);
    let dotgit = std::fs::symlink_metadata(tree.join(".git")).ok();
    match std::fs::symlink_metadata(&dst) {
        Ok(m) if m.file_type().is_file() => {
            if m.nlink() > 1 {
                return Err("base tree has a multiply linked file at the check path".to_string());
            }
            if let Some(g) = &dotgit {
                if g.dev() == m.dev() && g.ino() == m.ino() {
                    return Err("check path is the tree's own git pointer".to_string());
                }
            }
        }
        Ok(_) => return Err("base tree has a non-file or symlink at the check path".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err("could not inspect the base tree".to_string()),
    }
    let real_root = tree.canonicalize().map_err(|_| "base tree unreadable".to_string())?;
    let real_parent = p.canonicalize().map_err(|_| "base tree unreadable".to_string())?;
    if !real_parent.starts_with(&real_root) {
        return Err("check path escapes the base tree".to_string());
    }
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o755).open(&dst).map_err(|_| "could not write the check into the base tree".to_string())?;
    f.write_all(bytes).map_err(|_| "could not write the check into the base tree".to_string())?;
    f.flush().map_err(|_| "could not write the check into the base tree".to_string())?;
    // The mode given at open applies only when the file is created; set it on the open handle so an existing file ends up 0755 too.
    f.set_permissions(std::fs::Permissions::from_mode(0o755)).map_err(|_| "could not write the check into the base tree".to_string())?;
    Ok(())
}

fn run_in(tree: &Path, sha: &str, rel: &str, spec: &Spec) -> Result<EndReport, String> {
    let check = Check::load(tree, spec.id, rel, spec.args, spec.budget, spec.max_output)?;
    let (res, ended) = run_ended(tree, &check, sha);
    Ok(EndReport { sha: sha.to_string(), ended, output: res.message })
}

/// Prepare a base tree and a head tree under a fresh private folder, run the declared check once in each, clean up, and report both ends.
/// Every refusal happens before any tree exists. Both trees use the same absolute path, one after the other.
pub fn prepare_and_run(repo_path: &Path, base: &str, head: &str, recorded_head: &str, spec: &Spec) -> Result<Reports, String> {
    // Resolve every input once, before anything is created.
    let repo = resolve_repo(repo_path)?;
    let base_sha = commit_sha(&repo.top, base)?;
    let head_sha = commit_sha(&repo.top, head)?;
    if head_sha != recorded_head {
        return Err("head does not match the recorded sha".to_string());
    }
    if base_sha == head_sha {
        return Err("base and head are the same commit".to_string());
    }
    let parts = plain_parts(spec.program)?;
    // Resolved once: from here on only the normalized path is used, by git, by the loader and by the placement.
    let rel: String = parts.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
    let blob = head_blob(&repo.top, &head_sha, &rel)?;
    let root = scratch_root(&repo)?;
    // Check the spec's limits and id now too, so a bad one is refused before any tree exists.
    Check::load_limits(spec.id, spec.budget, spec.max_output)?;

    let scratch = Scratch::create(&root)?;
    // The folder is named like the scratch folder, so git's admin entry name is unique to this run and identical at both ends.
    let tree_path = scratch.path.join(scratch.path.file_name().ok_or("scratch folder unnamed")?);

    let base_report = {
        let tree = Tree::add(&repo, &tree_path, &base_sha)?;
        place_check(&tree.path, &parts, &blob)?;
        run_in(&tree.path, &base_sha, &rel, spec)?
    };
    let head_report = {
        let tree = Tree::add(&repo, &tree_path, &head_sha)?;
        place_check(&tree.path, &parts, &blob)?;
        run_in(&tree.path, &head_sha, &rel, spec)?
    };
    drop(scratch);
    Ok(Reports { base: base_report, head: head_report })
}
