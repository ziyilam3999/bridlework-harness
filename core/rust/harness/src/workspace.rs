//! `harness workspace create` (std only, Unix): the ticket workspace maker (P1B-03). One ticket gets ONE fenced
//! folder — a fresh long-lived worktree of the ticket's own repo, on its own branch, plus one scratch folder — and
//! every one of those things is written on the ticket's manifest BEFORE it is made, so nothing this command creates
//! is ever unlisted (a crash can leave listed things half-made, never an orphan). It only makes workspaces;
//! clearing one away is teardown's job (P1B-14) or the P2 orphan reaper's, both working from the manifest.
//! Reuses the merged rules rather than inventing new ones: intake for the repo lookup, `tree_prep.rs`'s
//! main-work-tree and git-call rules, `events.rs`'s token rule for the ticket id, and the manifest library for
//! every manifest write. No error or stdout line ever carries a folder (given or canonicalized): a folder may be
//! private, and the manifest — which lives in a caller-chosen private folder — is the record, not the terminal.
//! The command never cleans up after itself and never retries in place: a re-run is refused as a second workspace.

use crate::events::token_ok;
use crate::intake::{intake, RepoMap};
use crate::manifest::{self, Class, Op, Target};
use crate::ticket::Ticket;
use std::io::Read;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness workspace create <ticket-file> --repo-map <repo_id>=<absolute-dir> [--repo-map ...] --base <full-sha> --workspace-root <absolute-dir> --manifest-dir <absolute-dir> [--ts-ms <n>]";
/// One byte over the 1 MiB ticket cap: an oversize file is read no further than this, then refused by the strict reader.
const TICKET_READ_LIMIT: u64 = 1024 * 1024 + 1;

/// A stop: the exit code and the full stderr line. Code 1 is a ticket refusal at intake (the same meaning as
/// `harness ticket intake`); 2 is every other refusal or error. Stdout stays empty.
struct Stop(i32, String);

fn err2(m: &str) -> Stop {
    Stop(2, format!("harness: error: {}", m))
}

fn usage_err(m: &str) -> Stop {
    Stop(2, format!("harness: error: {}\n{}", m, USAGE))
}

/// A successful create: fixed fields only, no folder.
struct Success {
    ticket: String,
    repo_id: String,
    base: String,
    branch: String,
}

struct Args {
    ticket_file: String,
    map: RepoMap,
    base: String,
    workspace_root: String,
    manifest_dir: String,
    ts_ms: Option<String>,
}

/// A full lowercase hex commit sha (40 or 64 chars), the same shape `harness ticket gate` requires.
fn base_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Absolute, and no `..` component (the same shape `harness ticket gate` requires of its dirs).
fn dir_arg_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// A numeric flag value: digits only, no sign, no empty string.
fn num_ok(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_args(a: &[String]) -> Result<Args, Stop> {
    if a.first().map(|s| s.as_str()) != Some("create") {
        return Err(Stop(2, format!("usage: harness workspace create <ticket-file> ...\n{}", USAGE)));
    }
    let mut map = RepoMap::new();
    let (mut file, mut base, mut ws, mut md, mut ts): (Option<&str>, Option<&str>, Option<&str>, Option<&str>, Option<&str>) =
        (None, None, None, None, None);
    let mut it = a[1..].iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--repo-map" => {
                let Some(spec) = it.next() else { return Err(usage_err("--repo-map is missing its value")) };
                map.add(spec).map_err(|e| usage_err(&e))?;
            }
            "--base" | "--workspace-root" | "--manifest-dir" | "--ts-ms" => {
                let val = it.next().ok_or_else(|| usage_err("an option is missing its value"))?;
                let slot = match arg.as_str() {
                    "--base" => &mut base,
                    "--workspace-root" => &mut ws,
                    "--manifest-dir" => &mut md,
                    _ => &mut ts,
                };
                if slot.replace(val.as_str()).is_some() {
                    return Err(usage_err("an option was given twice"));
                }
            }
            s if s.starts_with("--") => return Err(usage_err("unknown option")),
            s => {
                if file.replace(s).is_some() {
                    return Err(usage_err("more than one ticket file given"));
                }
            }
        }
    }
    let file = file.ok_or_else(|| usage_err("no ticket file given"))?;
    let base = base.ok_or_else(|| usage_err("--base is required"))?;
    let ws = ws.ok_or_else(|| usage_err("--workspace-root is required"))?;
    let md = md.ok_or_else(|| usage_err("--manifest-dir is required"))?;
    if !base_ok(base) {
        return Err(usage_err("--base must be a full lowercase hex commit sha (a ref name is refused)"));
    }
    for (flag, val) in [("--workspace-root", ws), ("--manifest-dir", md)] {
        if !dir_arg_ok(val) {
            return Err(usage_err(&format!("{} must be an absolute path with no `..` component", flag)));
        }
    }
    if let Some(t) = ts {
        if !num_ok(t) {
            return Err(usage_err("--ts-ms must be a non-negative integer"));
        }
    }
    Ok(Args {
        ticket_file: file.to_string(),
        map,
        base: base.to_string(),
        workspace_root: ws.to_string(),
        manifest_dir: md.to_string(),
        ts_ms: ts.map(|t| t.to_string()),
    })
}

/// One git call: no hooks, no inherited `GIT_*` name (so an inherited `GIT_DIR` cannot redirect it), no stdin, and
/// stderr dropped (git's own message may name a folder). The `tree_prep.rs` git-call rule.
fn git(repo: &Path, args: &[&str]) -> Option<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(["-c", "core.hooksPath=/dev/null"]).args(args);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    let out = c.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The repo the ticket names, resolved once the `tree_prep.rs` way: canonical, the top level of its work tree, and
/// a MAIN work tree (git dir == common dir), so a workspace is never made from another ticket's workspace.
struct Repo {
    top: PathBuf,
    common: PathBuf,
}

fn resolve_main_work_tree(path: &Path) -> Result<Repo, Stop> {
    let canon = path.canonicalize().map_err(|_| err2("the mapped repo folder is unreadable"))?;
    let out = git(&canon, &["rev-parse", "--path-format=absolute", "--show-toplevel", "--git-dir", "--git-common-dir"])
        .ok_or_else(|| err2("the mapped repo folder is not a readable git work tree"))?;
    let lines: Vec<&str> = out.lines().collect();
    if lines.len() != 3 {
        return Err(err2("the mapped repo folder is not a readable git work tree"));
    }
    let top = Path::new(lines[0]).canonicalize().map_err(|_| err2("the repo top level is unreadable"))?;
    let gitdir = Path::new(lines[1]).canonicalize().map_err(|_| err2("the repo git dir is unreadable"))?;
    let common = Path::new(lines[2]).canonicalize().map_err(|_| err2("the repo git dir is unreadable"))?;
    if top != canon {
        return Err(err2("the mapped repo folder is not the top level of its work tree"));
    }
    if gitdir != common {
        return Err(err2("the mapped repo folder is a linked work tree, so it may be another ticket's workspace"));
    }
    Ok(Repo { top, common })
}

/// The base: already shape-checked, it must also name a commit that exists in the ticket's repo, and resolving it
/// must return the same full sha (an abbreviated sha never reaches here).
fn resolve_base(repo: &Repo, base: &str) -> Result<String, Stop> {
    let resolved = git(&repo.top, &["rev-parse", "--verify", "--quiet", &format!("{}^{{commit}}", base)])
        .ok_or_else(|| err2("the base commit is not in the ticket's repo"))?;
    if resolved != base {
        return Err(err2("the base commit is not in the ticket's repo"));
    }
    Ok(resolved)
}

/// A branch (or any ref) exists in the repo. The name is always passed behind the `refs/heads/` prefix, never bare.
fn ref_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{}", branch)]).is_some()
}

/// Resolve `--workspace-root` / `--manifest-dir` once: an existing real directory (never a link), canonical, not
/// `/`, outside the repo's work tree and its git common dir (a workspace inside the repo would show up as an
/// untracked change in the primary clone). Only the resolved value is used afterwards.
fn resolve_dir(flag: &str, arg: &str, repo: &Repo) -> Result<PathBuf, Stop> {
    let meta = std::fs::symlink_metadata(arg).map_err(|_| err2(&format!("{} is missing or unreadable", flag)))?;
    if !meta.file_type().is_dir() {
        return Err(err2(&format!("{} must be a real directory (a file or a link is refused)", flag)));
    }
    let canon = Path::new(arg).canonicalize().map_err(|_| err2(&format!("{} cannot be resolved", flag)))?;
    if canon == Path::new("/") {
        return Err(err2(&format!("{} may not be the filesystem root", flag)));
    }
    if canon.starts_with(&repo.top) || canon.starts_with(&repo.common) {
        return Err(err2(&format!("{} lies inside the ticket's repo", flag)));
    }
    Ok(canon)
}

/// Any manifest error stops with a FIXED, folder-free line: the manifest library's own messages may repeat a path,
/// and no folder is ever printed. The manifest file is the record; the terminal line says only what happened.
fn manifest_stop(e: manifest::ManifestError) -> Stop {
    let m = match e {
        manifest::ManifestError::Invalid(_) => "the manifest refused the workspace record",
        manifest::ManifestError::TooLong(_) => "the manifest refused a line over its cap",
        manifest::ManifestError::Damaged { .. } => "the manifest is damaged; nothing further was written",
        manifest::ManifestError::Io(_) => "the manifest could not be written",
    };
    err2(m)
}

/// `harness workspace create ...` (the arguments after `workspace`). Exit 0 created (one stdout line of fixed
/// fields); 1 the ticket is refused at intake (bad ticket, unknown repo id, repo not found — the same meaning as
/// `harness ticket intake`); 2 every other refusal or error. Any non-zero exit means "no usable workspace".
pub fn workspace_cmd(a: &[String]) -> i32 {
    match create(a) {
        Ok(s) => {
            println!("workspace=created ticket={} repo_id={} base={} branch={}", s.ticket, s.repo_id, s.base, s.branch);
            0
        }
        Err(Stop(code, line)) => {
            eprintln!("{}", line);
            code
        }
    }
}

fn create(a: &[String]) -> Result<Success, Stop> {
    let args = parse_args(a)?;

    // 1. Read the ticket (strict reader) and run intake: the repo comes from the ticket and the repo map only,
    //    never from the working folder. A bad ticket file or an unknown repo stops here with exit 1.
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&args.ticket_file).and_then(|f| f.take(TICKET_READ_LIMIT).read_to_end(&mut bytes));
    if read.is_err() {
        return Err(err2("the ticket file cannot be read"));
    }
    let ticket = Ticket::parse_bytes(&bytes).map_err(|e| Stop(1, format!("harness: ticket refused: {}", e)))?;
    let accepted = intake(&ticket, &args.map).map_err(|r| Stop(1, format!("harness: ticket refused: {}", r)))?;

    // 2. The ticket id must be a plain token exactly as the manifest defines it (lowercase letters, digits, `-`,
    //    `_`, `.`). Never lower-cased or rewritten: that would let `T-1` and `t-1` share one desk.
    if !token_ok(&ticket.id) {
        return Err(err2("the ticket id must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')"));
    }

    // 3. The repo must be a MAIN work tree (the `tree_prep.rs` rule), so a workspace is never made from another
    //    ticket's workspace; the base must be a full sha that exists in it.
    let dir = args
        .map
        .lookup(&accepted.repo_id)
        .ok_or_else(|| err2("the repo map no longer lists the ticket's repo"))?;
    let repo = resolve_main_work_tree(dir)?;
    let base = resolve_base(&repo, &args.base)?;
    let branch = format!("ws/{}", ticket.id);

    // 4. Resolve both folders once. Each lies outside the repo; the manifest dir lies outside the workspace root
    //    (the manifest must survive removal of the roots) and the workspace root outside the manifest dir (a later
    //    manifest-dir sweep must not walk into live workspaces); equality counts as inside in both directions.
    let ws = resolve_dir("--workspace-root", &args.workspace_root, &repo)?;
    let md = resolve_dir("--manifest-dir", &args.manifest_dir, &repo)?;
    if md.starts_with(&ws) {
        return Err(err2("--manifest-dir lies inside the workspace root"));
    }
    if ws.starts_with(&md) {
        return Err(err2("--workspace-root lies inside the manifest dir"));
    }

    // 5. Read-only pre-check: a ticket that already has a manifest file already has a workspace. The race-proof
    //    check is the exclusive create below; this only gives the common case a clear answer.
    if std::fs::symlink_metadata(manifest::manifest_file(&md, &ticket.id)).is_ok() {
        return Err(err2("this ticket already has a workspace"));
    }
    // The branch is never reused and never reset. Git refuses it natively too, but only after the manifest was
    // seeded, so the existence check runs here, still before anything is written.
    if ref_exists(&repo.top, &branch) {
        return Err(err2("the workspace branch already exists in the repo and is never reused"));
    }

    // 6. The ticket root: a new, empty, real, owner-only folder, created exclusively — fails if anything
    //    (including a symlink) already sits at that name. Two racing `create` calls for one ticket: exactly one
    //    wins HERE.
    let ticket_root = ws.join(&ticket.id);
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&ticket_root)
        .map_err(|_| err2("the ticket folder could not be created (the ticket may already have a workspace)"))?;
    let tree = ticket_root.join("tree");
    let tmp = ticket_root.join("tmp");

    let ts_ms = match args.ts_ms.as_deref() {
        Some(t) => t.parse::<u64>().map_err(|_| usage_err("--ts-ms out of range"))?,
        None => SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).map_err(|_| err2("the clock is before the epoch"))?,
    };

    // 7-8. Write-ahead: init the manifest with the ticket root as its ONE root, then list both entries BEFORE the
    // things exist, so a crash between steps can leave only listed things, never an orphan.
    manifest::apply(&md, &ticket.id, &Op::Init { roots: &[ticket_root.clone()] }, ts_ms).map_err(manifest_stop)?;
    manifest::apply(&md, &ticket.id, &Op::AddPath { id: "worktree", class: Class::Worktree, path: &tree }, ts_ms).map_err(manifest_stop)?;
    manifest::apply(&md, &ticket.id, &Op::AddPath { id: "tmp", class: Class::TempDir, path: &tmp }, ts_ms).map_err(manifest_stop)?;

    // 9. Make the scratch folder, then the worktree on its own branch at the base (the git-call rule: no hooks, no
    //    inherited `GIT_*`, git's own stderr never echoed).
    std::fs::DirBuilder::new().mode(0o700).create(&tmp).map_err(|_| err2("the ticket scratch folder could not be created"))?;
    let tree_str = tree.to_str().ok_or_else(|| err2("the worktree path is not utf-8"))?;
    git(&repo.top, &["worktree", "add", "-b", &branch, tree_str, &base])
        .ok_or_else(|| err2("the worktree could not be created"))?;

    // 10. Read the manifest back and confirm both entries are live and both paths exist; only then is it a workspace.
    let read = manifest::read_manifest(&md, &ticket.id).map_err(|_| err2("the manifest could not be read back"))?;
    if !read.is_clean() {
        return Err(err2("the manifest did not read back cleanly"));
    }
    let want = [("worktree", Class::Worktree, &tree), ("tmp", Class::TempDir, &tmp)];
    if read.live.len() != want.len() {
        return Err(err2("the manifest did not read back both entries"));
    }
    for (id, class, path) in want {
        let hit = read.live.iter().any(|e| match &e.target {
            Target::Path(p) => e.id == id && e.class == class && Path::new(p) == path,
            _ => false,
        });
        if !hit || !path.exists() {
            return Err(err2("the manifest did not read back both entries with their paths in place"));
        }
    }
    Ok(Success { ticket: ticket.id, repo_id: accepted.repo_id.as_str().to_string(), base, branch })
}
