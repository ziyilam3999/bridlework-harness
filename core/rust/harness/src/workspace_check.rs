//! `harness workspace check` (std only, Unix): the clean-state check (P1B-11). One read-only look at the ticket's
//! worktree — found ONLY as the live manifest entry id `worktree`, never by name and never from the current
//! folder — asking one question: is anything in that folder that should not be there at finalize time? Expected
//! is (a) everything committed on the branch `ws/<ticket>` (the content of the ticket's own commits is the gate's
//! business, not this check's) and (b) a live manifest path entry INSIDE the worktree that is still what the
//! manifest says it is — today that is the P1B-04 `node_modules` link, so "still what the manifest says" means a
//! no-follow stat finds a symlink at that exact stored path, and the exemption covers that one path itself,
//! never anything under it and never anything matched after following a link. Everything else is STRAY: modified,
//! deleted, renamed, type-changed or staged tracked paths; untracked paths (every file, not just the folder,
//! via untracked=all); ignored paths (an ignore rule must not be able to hide a file); tracked paths hidden from
//! status with the assume-unchanged or skip-worktree bit; a submodule with any change; and a listed inside path
//! that is gone or no longer the recorded kind (kind `missing`).
//!
//! The check never writes, deletes, moves or locks anything in the repo or the manifest (read through the
//! existing reader only), and every git call runs under `--no-optional-locks` with fsmonitor off, so not even the
//! index's stat cache is refreshed; its ONE write anywhere is a private throwaway index file in the system
//! temp folder (ticket #184), built from HEAD for a content compare, never inside any repo — not even its
//! lock: git reaches it through `GIT_INDEX_FILE`, so the only lock ever taken is `<temp file>.lock` next to
//! the throwaway index itself — and removed
//! before the check returns. No repo-controlled setting or index flag is trusted when deciding clean:
//! every git call forces `--no-optional-locks`, `--no-replace-objects` (a replace ref cannot swap what HEAD
//! looks like), no hooks, fsmonitor/trustctime/checkStat/filemode/untrackedCache/sparseCheckout off-or-default
//! plus `core.ignorecase=false` (two spellings of one name stay two paths) and `core.symlinks=true` (a
//! tracked symlink replaced by a regular file stays a type change) on the command line (command-line config
//! outranks repo, worktree and global config), every inherited
//! `GIT_*` name is stripped, ignored paths are listed (`--ignored=matching`), and the same forced reads run
//! recursively INSIDE every checked-out submodule (the submodule's own config, exclude files and index bits
//! hide nothing; a submodule that cannot be inspected is exit 3 naming it). "Modified" is decided by
//! CONTENT, not by the stat cache: on top of the status read, every repo checked gets a second
//! tracked-file read against that throwaway index rebuilt from HEAD with no stat information at all, so
//! git must hash every tracked file and compare bytes — a same-size edit with the clock forged back into
//! the index-write second cannot read clean. User-level config is not the
//! repo's doing and never changes what the check sees, so it is neither read nor refused: every git call
//! runs with the system config off and the global config pointed at the null device, and the user
//! attributes file forced to the null device too — a `filter.lfs.*` block in the user's own global config
//! (the standard git-lfs install) does not make the check refuse, and no command it names can run. Where
//! even that cannot prove the answer, the check REFUSES (exit 3, never a pass): an `info/attributes` file
//! in the worktree or any checked-out submodule's git dir, any `filter.*` or `core.attributesFile` config
//! key in the repo or worktree scope (a clean/smudge/process filter is both a hide and an arbitrary
//! command a "read-only" check must never run; a `core.attributesFile` key that reaches effect only
//! through the repo's `include.path` is not refused — the scope-limited refusal read skips includes —
//! but neutralised the same way, by the forced `core.attributesFile=/dev/null` outranking it), or a
//! `core.worktree` that redirects elsewhere. Entries git never lists (a FIFO, a socket, a device) are
//! caught by a no-follow walk of the tree. Git output is read through a hard 8 MiB cap and stray entries
//! through a 10,000 cap; the submodule pass and the walk carry their own caps; hitting any is exit 3
//! ("too many paths"), never a truncated PASS and never an unbounded read (fail closed).
//!
//! Exit codes: 0 clean (one line `workspace=clean ticket=<t>`); 2 at least one stray path (one `stray <kind>
//! <path>` line per path, sorted by raw path bytes, then `workspace=stray ticket=<t> count=<n>`); 3 cannot check
//! — usage errors, a missing/torn/corrupt manifest, no live `worktree` entry, a worktree path that is missing, a
//! symlink or not a directory, a folder that is not that worktree's own top level, a HEAD that is detached or not
//! `refs/heads/ws/<ticket>`, an in-progress merge/rebase/cherry-pick, a failed git call, or a size cap. Paths are
//! worktree-relative only and escaped (`\\` for a backslash, `\xNN` for any control, space, or non-ASCII byte),
//! so one path is always one line and no folder is ever printed.
//!
//! `--pushed --repo <absolute-dir>` (P1B-12) switches the SAME command into the unpushed check ONLY (the
//! stray scan above does not run in this mode): after steps 1-6 of the clean-state check run unchanged, it
//! asks one more read-only question — is every commit on `ws/<ticket>` also on the branch's upstream
//! remote-tracking ref? — answered without any network: the upstream is resolved from the repo's own config
//! and compared as local refs only, and the exact tracking-ref name rebuilt from that config must EQUAL BOTH
//! of git's own answers — `@{upstream}` (which applies the remote's fetch refspec and the first
//! `branch.<b>.merge` value and FOLLOWS a symbolic ref to its target) and the NON-dereferenced
//! `for-each-ref %(upstream)` name; any disagreement — a non-default fetch refspec, a second merge value, a
//! symref at the tracking ref — is exit 3, never a pass. `<worktree>/.git` must be a linked-worktree pointer FILE, `--repo` a main
//! work tree (a real directory whose `.git` is a real directory) of the SAME repository (the canonical git
//! common dir of the worktree must equal canonical `<repo>/.git`, and `<repo>` must be its own top level),
//! all by no-follow stat before any git call of this mode reads them; `info/grafts` or a `shallow` file in
//! the common dir is refused (either can rewrite the ancestry the answer depends on) and every git call of
//! this mode also forces the commit graph off on top of the global `--no-replace-objects`. A missing
//! upstream, a tracking ref that is a symbolic ref or points at a non-commit, an upstream that is not a
//! `refs/remotes/` name (for example `branch.<b>.remote=.` making a LOCAL branch the "upstream"), or a
//! tracking-ref/branch pair that cannot be resolved all fail CLOSED (exit 3, never a pass). The answer is
//! reachability, not recency: `rev-list --topo-order <branch> ^<upstream-commit>` (full shas only, after
//! `--end-of-options`, capped at 1000 — more is exit 3, never a truncated answer), so a force-moved upstream
//! lists every ticket commit it no longer contains and an upstream AHEAD of the branch that contains it
//! passes. Exit 0 prints exactly one line `workspace=pushed ticket=<t> branch=ws/<t> upstream=<ref>`; exit 4
//! (distinct from the stray exit 2) prints `unpushed <full-sha> ws/<t>` per commit, newest first, then
//! `workspace=unpushed ticket=<t> branch=ws/<t> upstream=<ref> count=<n>`; exit 3 prints nothing on stdout
//! and one fixed folder-free `harness: error:` line on stderr. No fetch, no push, no ref or config write, no
//! index refresh.

use crate::events::token_ok;
use crate::manifest::{self, Target};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

pub const USAGE: &str = "usage: harness workspace check <ticket> --manifest-dir <absolute-dir> [--pushed --repo <absolute-dir>]";

/// The hard cap on any single git output read: an "unbounded read" is impossible past this, and hitting it is
/// exit 3, never a silently truncated answer.
const GIT_CAP: usize = 8 * 1024 * 1024;
/// The hard cap on stray entries: a folder with more strays than this cannot be proven clean, so it is exit 3.
const STRAY_CAP: usize = 10_000;
/// The hard cap on checked-out submodules inspected in one run (recursion included): past it, exit 3.
const SUBMODULE_CAP: usize = 100;
/// The hard cap on submodule nesting: a stack deeper than this cannot be proven clean, so it is exit 3.
const SUBMODULE_DEPTH_CAP: usize = 8;
/// The hard cap on entries visited by the tree walk: past it the tree cannot be proven clean, so it is exit 3.
const WALK_CAP: usize = 200_000;
/// The hard cap on unpushed commits listed in one answer: past it the branch cannot be summarized, exit 3.
const PUSHED_CAP: usize = 1000;

/// The forced status read, identical for the worktree and every checked-out submodule. Submodules are
/// compared by commit pointer only (`--ignore-submodules=dirty`): git's own dirty-content probe would
/// spawn the submodule's own status read, which honours the submodule's own config — so a
/// submodule-local filter could run before this check has refused it. Commit-pointer changes still
/// show, and every dirty-content shape inside a checked-out submodule is named by the check's own
/// pass inside it (see `check_submodule`), so nothing is lost.
const STATUS_ARGS: [&str; 7] = [
    "status",
    "--porcelain=v2",
    "--branch",
    "-z",
    "--untracked-files=all",
    "--ignored=matching",
    "--ignore-submodules=dirty",
];

/// One stray kind. The order is the priority: a path carrying two kinds is reported once, with the stronger.
/// `missing` (a listed thing is gone) outranks everything; a submodule change outranks a plain content change;
/// a hidden bit outranks what status would have said about the same path; `untracked` is the weakest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Untracked,
    Ignored,
    Modified,
    Hidden,
    Submodule,
    Missing,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Untracked => "untracked",
            Kind::Ignored => "ignored",
            Kind::Modified => "modified",
            Kind::Hidden => "hidden",
            Kind::Submodule => "submodule",
            Kind::Missing => "missing",
        }
    }
}

/// Exit 3 with a fixed, folder-free stderr line. Stdout stays empty on every exit-3 path.
fn stop3(m: &str) -> i32 {
    eprintln!("harness: error: {}", m);
    3
}

fn usage3(m: &str) -> i32 {
    eprintln!("harness: error: {}\n{}", m, USAGE);
    3
}

/// Absolute, and no `..` component (the same shape `harness workspace create` requires of its dirs).
fn dir_arg_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// `<ticket> --manifest-dir <absolute-dir> [--pushed --repo <absolute-dir>]`. The ticket must be a plain token
/// exactly as the manifest defines it: refused, never rewritten. No default folder, no config file, no
/// current-folder lookup. `--pushed` (P1B-12) switches the command to the unpushed check ONLY; it carries
/// `--repo <absolute-dir>` (the ticket repo's top folder) and the two are a pair: either one without the
/// other, either one given twice, or a `--repo` that is not absolute or holds `..` is a usage error (exit 3).
fn parse_args(a: &[String]) -> Result<(String, String, bool, Option<String>), i32> {
    let (mut ticket, mut md) = (None, None);
    let (mut pushed, mut repo) = (false, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manifest-dir" => {
                let val = it
                    .next()
                    .ok_or_else(|| usage3("--manifest-dir is missing its value"))?;
                if md.replace(val.as_str()).is_some() {
                    return Err(usage3("--manifest-dir was given twice"));
                }
            }
            "--pushed" => {
                if pushed {
                    return Err(usage3("--pushed was given twice"));
                }
                pushed = true;
            }
            "--repo" => {
                let val = it
                    .next()
                    .ok_or_else(|| usage3("--repo is missing its value"))?;
                if repo.replace(val.as_str()).is_some() {
                    return Err(usage3("--repo was given twice"));
                }
            }
            s if s.starts_with("--") => return Err(usage3("unknown option")),
            s => {
                if ticket.replace(s).is_some() {
                    return Err(usage3("more than one ticket given"));
                }
            }
        }
    }
    let ticket = ticket.ok_or_else(|| usage3("no ticket given"))?;
    let md = md.ok_or_else(|| usage3("--manifest-dir is required"))?;
    if !token_ok(ticket) {
        return Err(usage3("the ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')"));
    }
    if !dir_arg_ok(md) {
        return Err(usage3(
            "--manifest-dir must be an absolute path with no `..` component",
        ));
    }
    let repo = match repo {
        Some(r) if !dir_arg_ok(r) => {
            return Err(usage3(
                "--repo must be an absolute path with no `..` component",
            ))
        }
        Some(r) => Some(r.to_string()),
        None => None,
    };
    match (pushed, repo) {
        (true, Some(r)) => Ok((ticket.to_string(), md.to_string(), true, Some(r))),
        (true, None) => Err(usage3("--pushed requires --repo")),
        (false, Some(_)) => Err(usage3("--repo requires --pushed")),
        (false, None) => Ok((ticket.to_string(), md.to_string(), false, None)),
    }
}

/// One captured git call: read-only (`--no-optional-locks`, so not even the index stat cache is refreshed),
/// replace refs off (`--no-replace-objects`, so a `git replace` cannot swap what HEAD looks like), no hooks,
/// no fsmonitor and no stat levers (command-line config outranks repo, worktree and global config, so
/// `core.trustctime=false`, `core.checkStat=minimal`, `core.filemode=false`, `core.untrackedCache=true`,
/// `core.sparseCheckout=true`, `core.ignorecase=true` (two spellings of one name stay two paths) and
/// `core.symlinks=false` (a tracked symlink replaced by a regular file holding the link text stays a
/// type change) in the repo cannot relax or fold the comparison), no inherited `GIT_*` name (an
/// inherited `GIT_DIR`/`GIT_CONFIG_*` cannot redirect or reconfigure it), and no user-level config at all
/// (`GIT_CONFIG_NOSYSTEM=1`, `GIT_CONFIG_GLOBAL` at the null device, `core.attributesFile` forced to the
/// null device: the system file, the global file, the XDG config and the user attributes file are never
/// read), no stdin, stderr dropped (git's
/// own message may name a folder). The stdout read is hard-capped: past `GIT_CAP` the flag `overflow`
/// is set and the rest is drained and discarded, never buffered.
struct GitOut {
    ok: bool,
    overflow: bool,
    bytes: Vec<u8>,
}

fn git_bytes(tree: &Path, args: &[&str]) -> GitOut {
    git_bytes_env(tree, args, None)
}

/// The same call with one environment pair set AFTER the inherited `GIT_*` strip: the content read
/// points `GIT_INDEX_FILE` at its private throwaway index, and no inherited value can survive to it.
fn git_bytes_env(tree: &Path, args: &[&str], extra_env: Option<(&str, &std::ffi::OsStr)>) -> GitOut {
    let mut c = Command::new("git");
    c.arg("--no-optional-locks")
        .arg("--no-replace-objects")
        .arg("-C")
        .arg(tree)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(["-c", "core.fsmonitor=false"])
        .args(["-c", "core.trustctime=true"])
        .args(["-c", "core.checkStat=default"])
        .args(["-c", "core.filemode=true"])
        .args(["-c", "core.untrackedCache=false"])
        .args(["-c", "core.sparseCheckout=false"])
        .args(["-c", "core.ignorecase=false"])
        .args(["-c", "core.symlinks=true"])
        .args(["-c", "core.attributesFile=/dev/null"])
        .args(args);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    // After the strip: user-level config is neutralised for EVERY git call. Neither the system config file
    // (`GIT_CONFIG_NOSYSTEM=1`) nor the user's global one (`GIT_CONFIG_GLOBAL` at the null device, which also
    // covers the XDG config path) is read, and the user attributes file (`core.attributesFile` forced to the
    // null device, which also covers the XDG default attributes file) is not read either. A key that lives
    // outside the repo can never run on the checked tree — a configured filter only runs when a path's
    // attributes name it, and every repo/worktree route to one is refused by the scan below — so this hides
    // nothing and refuses nothing; it only stops a git-lfs host's user-level `filter.lfs.*` block from
    // failing every check (the CI-red shape).
    c.env("GIT_CONFIG_NOSYSTEM", "1");
    c.env("GIT_CONFIG_GLOBAL", "/dev/null");
    if let Some((k, v)) = extra_env {
        c.env(k, v);
    }
    let mut child = match c
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(ch) => ch,
        Err(_) => {
            return GitOut {
                ok: false,
                overflow: false,
                bytes: Vec::new(),
            }
        }
    };
    let mut bytes = Vec::new();
    let mut overflow = false;
    if let Some(out) = child.stdout.take() {
        let mut r = out;
        let mut buf = [0u8; 65536];
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if bytes.len() + n <= GIT_CAP {
                        bytes.extend_from_slice(&buf[..n]);
                    } else {
                        // Past the cap: keep draining so git can finish, but buffer nothing more.
                        overflow = true;
                    }
                }
                Err(_) => break,
            }
        }
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false);
    GitOut {
        ok,
        overflow,
        bytes,
    }
}

/// A git call that must succeed as UTF-8 text under the cap (rev-parse-style queries). Anything else is None.
fn git_text(tree: &Path, args: &[&str]) -> Option<Result<String, i32>> {
    let g = git_bytes(tree, args);
    if g.overflow {
        return Some(Err(stop3(
            "too many paths: a git answer went over the read cap",
        )));
    }
    if !g.ok {
        return None;
    }
    Some(Ok(String::from_utf8_lossy(&g.bytes).trim().to_string()))
}

/// The repo-settings refusal scan, run once for the worktree and once for every checked-out submodule:
/// anything that could convert, filter or redirect git's view of the tree is exit 3, never a pass. `label`
/// names the repo being scanned ("the worktree" or "submodule <rel>") — folder-free either way.
///
/// - `$GIT_DIR/info/attributes` (and the common git dir's copy, which a linked worktree reads): attributes
///   can change what status compares, and a `filter=` attribute would run a repo-configured command;
/// - any `filter.*` or `core.attributesFile` key in the repo or worktree scope: a clean/smudge/process
///   filter is both a hide and an arbitrary command a "read-only" check must never execute (only the KEY
///   NAME is ever printed, never the value — a value is a command line). Keys from system or global scope
///   are simply not seen: those files are neutralised for every git call (see `git_bytes`), because a key
///   outside the repo can never run on the checked tree unless a repo-scope route names it — and those
///   routes are exactly what this scan refuses;
/// - `core.worktree` pointing anywhere but the tree being scanned (git's own plumbing value for an
///   absorbed submodule points at that submodule and passes; anything else redirects the work tree).
fn refuse_scan(tree: &Path, label: &str) -> Result<(), i32> {
    let gitdir = git_text(
        tree,
        &["rev-parse", "--path-format=absolute", "--absolute-git-dir"],
    );
    let gitdir = match gitdir {
        Some(Ok(g)) => PathBuf::from(g),
        Some(Err(code)) => return Err(code),
        None => {
            return Err(stop3(&format!(
                "{label} cannot be inspected: a git call inside it failed"
            )))
        }
    };
    let common = git_text(
        tree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let common = match common {
        Some(Ok(g)) => Some(PathBuf::from(g)),
        Some(Err(code)) => return Err(code),
        None => {
            return Err(stop3(&format!(
                "{label} cannot be inspected: a git call inside it failed"
            )))
        }
    };
    for d in std::iter::once(&gitdir).chain(common.iter()) {
        if d.join("info").join("attributes").exists() {
            return Err(stop3(&format!(
                "an info/attributes file is present in {label}; cannot check (attributes can change what git compares, and a filter would run a command)"
            )));
        }
    }
    let g = git_bytes(tree, &["config", "-z", "--get-regexp", "^filter\\."]);
    if g.overflow {
        return Err(stop3(
            "too many paths: a config answer went over the read cap",
        ));
    }
    if g.ok && !g.bytes.is_empty() {
        // `-z --get-regexp` records are `key\nvalue\0`: the key ends at the newline; only the key is printed.
        // The read is deliberately unscoped: with the system and global files neutralised for every git call
        // (see `git_bytes`), an unscoped read resolves exactly the repo and worktree scopes (and anything an
        // `include.path` in them pulls in), while the check's own `-c` pairs carry no `filter.*` key — so this
        // fires on every repo/worktree filter key and never on a user-level one (a key that comes from system
        // or global scope is simply not seen).
        let key = g.bytes.split(|&b| b == b'\n').next().unwrap_or(b"");
        return Err(stop3(&format!(
            "a filter or attributes config key is set ({}) in {}; cannot check (a filter would run a command)",
            String::from_utf8_lossy(key),
            label
        )));
    }
    // `core.attributesFile` is read scope-limited (`--local`, then `--worktree`): an unscoped read would see
    // the check's own forced `-c core.attributesFile=/dev/null` key and refuse on itself. Together the two
    // scopes cover the repo config file and the `config.worktree` extension file and nothing else.
    for scope in ["--local", "--worktree"] {
        let g = git_bytes(
            tree,
            &[
                "config",
                "-z",
                scope,
                "--get-regexp",
                "^core\\.attributesFile$",
            ],
        );
        if g.overflow {
            return Err(stop3(
                "too many paths: a config answer went over the read cap",
            ));
        }
        if g.ok && !g.bytes.is_empty() {
            let key = g.bytes.split(|&b| b == b'\n').next().unwrap_or(b"");
            return Err(stop3(&format!(
                "a filter or attributes config key is set ({}) in {}; cannot check (a filter would run a command)",
                String::from_utf8_lossy(key),
                label
            )));
        }
    }
    let g = git_bytes(tree, &["config", "--get", "core.worktree"]);
    if g.overflow {
        return Err(stop3(
            "too many paths: a config answer went over the read cap",
        ));
    }
    if g.ok && !g.bytes.is_empty() {
        // The value may be relative (git's absorbed-submodule plumbing writes `../../sub`); it resolves
        // against the git dir. The VALUE is a path and is never printed.
        let v = String::from_utf8_lossy(&g.bytes).trim().to_string();
        let p = PathBuf::from(&v);
        let resolved = if p.is_absolute() { p } else { gitdir.join(p) };
        match resolved.canonicalize() {
            Ok(c) if c == tree => {}
            _ => {
                return Err(stop3(&format!(
                    "core.worktree redirects the work tree of {label}; cannot check"
                )))
            }
        }
    }
    Ok(())
}

/// Every gitlink (mode 160000 index entry) of a tree, raw relative path bytes. `ls-files -s -z` records are
/// `<mode> <sha> <stage>\t<path>`; only gitlinks can hold checked-out sub-repositories.
fn gitlinks(tree: &Path, label: &str) -> Result<Vec<Vec<u8>>, i32> {
    let g = git_bytes(tree, &["ls-files", "-s", "-z"]);
    if g.overflow {
        return Err(stop3(
            "too many paths: the tracked-file answer went over the read cap",
        ));
    }
    if !g.ok {
        return Err(stop3(&format!(
            "{label} cannot be inspected: a git call inside it failed"
        )));
    }
    let mut v = Vec::new();
    for t in g.bytes.split(|&b| b == 0) {
        let Some(tab) = t.iter().position(|&b| b == b'\t') else {
            continue;
        };
        if t.starts_with(b"160000 ") {
            v.push(t[tab + 1..].to_vec());
        }
    }
    Ok(v)
}

/// How a gitlink path sits on disk. `Absent` (nothing checked out — git created an empty directory at
/// checkout, or it was deinitialised), `CheckedOut` (a directory with a `.git` of its own), or `Odd` (a
/// non-empty directory with no `.git`: contents git cannot see — refuse rather than guess).
enum GitlinkState {
    Absent,
    CheckedOut,
    Odd,
}

fn gitlink_state(p: &Path) -> GitlinkState {
    let Ok(m) = std::fs::symlink_metadata(p) else {
        return GitlinkState::Absent;
    };
    if !m.file_type().is_dir() {
        return GitlinkState::Absent; // a file or link where a gitlink sits: status reports it as a change
    }
    match std::fs::read_dir(p) {
        Ok(mut it) => {
            if it.next().is_none() {
                return GitlinkState::Absent; // the empty directory git leaves for an un-updated submodule
            }
        }
        Err(_) => return GitlinkState::Odd,
    }
    match std::fs::symlink_metadata(p.join(".git")) {
        // A directory (a plain embedded repo) or a regular file (git's `gitdir:` pointer) is a git dir.
        // A SYMLINK is not: it leads wherever it pleases (outside the checked tree), so it is Odd —
        // exit 3 naming the submodule, never a pass. No-follow means a dangling `.git` link is Odd
        // too (an `.exists()` follow would read a dangling link as absent, and a valid one as a repo).
        Ok(m) if !m.file_type().is_symlink() => GitlinkState::CheckedOut,
        _ => GitlinkState::Odd,
    }
}

/// The forged-clock guard (ticket #184, item A): "modified" is decided by CONTENT, never by the stat
/// cache. Every repo checked (the worktree and each checked-out submodule) gets a second tracked-file
/// read against a throwaway index rebuilt from HEAD that carries no stat information at all, so git
/// must hash every tracked file and compare bytes — a same-size edit whose mtime and ctime were forged
/// back into the index-write second cannot read clean. The throwaway index is ONE private file in the
/// system temp folder (never inside any repo, holding nothing but paths git itself derived from HEAD);
/// git reaches it through `GIT_INDEX_FILE`, so the only lock ever taken is `<temp file>.lock` beside it
/// (never the repo's real index lock, and the lock-to-index rename never crosses a filesystem). It
/// is built and read with the same forced flags and neutral config (`core.splitIndex=false` too, so no
/// shared-index file is looked for next to it), is removed on every exit path, and a failure to build
/// or read it is exit 3 — never a pass. The paths it names are stray `modified` (strongest kind wins,
/// exactly as for the status read; inside a submodule pass they make the submodule itself stray).
fn content_read(tree: &Path, label: &str, map: &mut std::collections::BTreeMap<Vec<u8>, Kind>) -> Result<(), i32> {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let tmp = std::env::temp_dir().join(format!(
        "harness-wscheck-index-{}-{}",
        std::process::id(),
        N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    ));
    // The Drop removes the temp file on EVERY exit path below, including a panic.
    struct TmpIndex(PathBuf);
    impl Drop for TmpIndex {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _t = TmpIndex(tmp.clone());
    // F2/F3: the throwaway index is built through `GIT_INDEX_FILE` pointing straight at the temp
    // file, so git takes ONLY `<tmp>.lock` — in the temp folder, next to the index it is renamed
    // onto, so that rename can never cross a filesystem either. The r1 shape
    // (`read-tree --index-output=<tmp>`) held the REPO's real `<gitdir>/index.lock` and renamed it
    // to the temp path: a parallel `git add` in the repo could fail, a check killed mid-run left a
    // stale lock behind that blocked every later write, and a temp folder on another filesystem
    // made even a clean tree exit 3 (the rename failed EXDEV). The build call and the status read
    // below carry the same env pair, set AFTER the inherited `GIT_*` strip.
    let build = git_bytes_env(
        tree,
        &["-c", "core.splitIndex=false", "read-tree", "HEAD"],
        Some(("GIT_INDEX_FILE", tmp.as_os_str())),
    );
    if !build.ok {
        return Err(stop3(&format!("{label} cannot be inspected: a git call inside it failed")));
    }
    let read = git_bytes_env(
        tree,
        &[
            "-c",
            "core.splitIndex=false",
            "status",
            "--porcelain=v2",
            "-z",
            "--untracked-files=no",
            "--ignore-submodules=dirty",
        ],
        Some(("GIT_INDEX_FILE", tmp.as_os_str())),
    );
    if read.overflow {
        return Err(stop3(&format!("too many paths: the status answer of {label} went over the read cap")));
    }
    if !read.ok {
        return Err(stop3(&format!("{label} cannot be inspected: a git call inside it failed")));
    }
    parse_status(&read.bytes, map);
    Ok(())
}

/// The forced reads INSIDE one checked-out submodule at `tree` (worktree-relative `rel`), recursing into
/// nested submodules. Any hit — a status entry, an untracked or ignored path, a non-`H` ls-files tag, all
/// read with the same forced flags and neutral config as the superproject, plus the same content read
/// against a throwaway stat-free index — makes the SUBMODULE stray (`stray submodule <rel>`, the
/// submodule's own path, never a path inside it). The outermost level that
/// can see the change names it, so once a level is dirty its own nested submodules need no separate
/// verdict; a level that looks clean is exactly where git's own probe may have been blinded by the inner
/// repo's settings, so the descent continues there. Depth, total count and every git output are capped
/// (exit 3 past any); a submodule that cannot be inspected is exit 3 naming it. Every checked-out level
/// records its own path in `gitlinks_live`, so the tree walk (which runs after this pass) can skip the
/// `.git` of each checked-out submodule and only those.
fn check_submodule(
    tree: &Path,
    rel: &[u8],
    depth: usize,
    visited: &mut usize,
    map: &mut std::collections::BTreeMap<Vec<u8>, Kind>,
    gitlinks_live: &mut Vec<Vec<u8>>,
) -> Result<(), i32> {
    if depth >= SUBMODULE_DEPTH_CAP {
        return Err(stop3(
            "too many paths: submodule nesting is over the depth cap",
        ));
    }
    *visited += 1;
    if *visited > SUBMODULE_CAP {
        return Err(stop3("too many paths: over the checked-submodule cap"));
    }
    gitlinks_live.push(rel.to_vec());
    let label = format!("submodule {}", escape(rel));
    refuse_scan(tree, &label)?;
    let status = git_bytes(tree, &STATUS_ARGS);
    if status.overflow {
        return Err(stop3(&format!(
            "too many paths: the status answer of {label} went over the read cap"
        )));
    }
    if !status.ok {
        return Err(stop3(&format!(
            "{label} cannot be inspected: a git call inside it failed"
        )));
    }
    let dirty = status
        .bytes
        .split(|&b| b == 0)
        .any(|t| !t.is_empty() && t[0] != b'#');
    let hidden = if dirty {
        None
    } else {
        let h = git_bytes(tree, &["ls-files", "-v", "-z"]);
        if h.overflow {
            return Err(stop3(&format!(
                "too many paths: the tracked-file answer of {label} went over the read cap"
            )));
        }
        if !h.ok {
            return Err(stop3(&format!(
                "{label} cannot be inspected: a git call inside it failed"
            )));
        }
        Some(h)
    };
    let dirty = dirty
        || hidden
            .map(|h| {
                h.bytes
                    .split(|&b| b == 0)
                    .any(|t| t.len() >= 3 && t[1] == b' ' && t[0] != b'H')
            })
            .unwrap_or(false);
    if dirty {
        insert(map, rel.to_vec(), Kind::Submodule);
        record_nested_gitlinks(tree, rel, depth, visited, gitlinks_live)?;
        return Ok(());
    }
    // The same content read as the worktree's (item A): a forged-clock edit inside the submodule is
    // invisible to the status and hidden reads above, so nothing but a content compare can name it.
    let mut content = std::collections::BTreeMap::new();
    content_read(tree, &label, &mut content)?;
    if !content.is_empty() {
        insert(map, rel.to_vec(), Kind::Submodule);
        record_nested_gitlinks(tree, rel, depth, visited, gitlinks_live)?;
        return Ok(());
    }
    for sub_rel in gitlinks(tree, &label)? {
        let child = tree.join(std::ffi::OsStr::from_bytes(&sub_rel));
        match gitlink_state(&child) {
            GitlinkState::Absent => continue,
            GitlinkState::Odd => {
                let mut child_rel = rel.to_vec();
                child_rel.push(b'/');
                child_rel.extend_from_slice(&sub_rel);
                return Err(stop3(&format!(
                    "submodule {} cannot be inspected: its folder holds no git dir",
                    escape(&child_rel)
                )));
            }
            GitlinkState::CheckedOut => {
                let mut child_rel = rel.to_vec();
                child_rel.push(b'/');
                child_rel.extend_from_slice(&sub_rel);
                check_submodule(&child, &child_rel, depth + 1, visited, map, gitlinks_live)?;
            }
        }
    }
    Ok(())
}

/// Record, WITHOUT inspecting, the checked-out gitlinks nested inside an already-stray submodule
/// (r1 N1): the walk skips the `.git` of every recorded path, so a checked-out repo nested inside a
/// DIRTY submodule is covered by the submodule's own `stray` line instead of being double-named
/// `stray untracked <sub>/<inner>/.git` next to it. Depth and total count are capped exactly as the
/// inspecting pass (exit 3 past either); an `Odd` or absent nested folder is left to the walk, which
/// sees no `.git` in it either way.
fn record_nested_gitlinks(
    tree: &Path,
    rel: &[u8],
    depth: usize,
    visited: &mut usize,
    gitlinks_live: &mut Vec<Vec<u8>>,
) -> Result<(), i32> {
    if depth >= SUBMODULE_DEPTH_CAP {
        return Err(stop3(
            "too many paths: submodule nesting is over the depth cap",
        ));
    }
    let label = format!("submodule {}", escape(rel));
    for sub_rel in gitlinks(tree, &label)? {
        let child = tree.join(std::ffi::OsStr::from_bytes(&sub_rel));
        if !matches!(gitlink_state(&child), GitlinkState::CheckedOut) {
            continue;
        }
        *visited += 1;
        if *visited > SUBMODULE_CAP {
            return Err(stop3("too many paths: over the checked-submodule cap"));
        }
        let mut child_rel = rel.to_vec();
        child_rel.push(b'/');
        child_rel.extend_from_slice(&sub_rel);
        gitlinks_live.push(child_rel.clone());
        record_nested_gitlinks(&child, &child_rel, depth + 1, visited, gitlinks_live)?;
    }
    Ok(())
}

/// A no-follow walk of the tree catching what git never lists: an entry that is not a regular file, a
/// directory or a symlink (a FIFO, a socket, a device) is `stray untracked <relpath>`. An entry named
/// `.git` is skipped ONLY for the worktree itself (rel is empty) and for each checked-out gitlink
/// recorded in `gitlinks_live` (the paths `check_submodule` inspected); any OTHER entry named `.git` —
/// a stray file, a stray directory, a planted repo — is `stray untracked <path>` in its own right and
/// is never descended into. Symlinks are never followed (their targets are outside this walk's
/// ground), and the visit count is capped (exit 3 past it or on any read error — a tree that cannot be
/// walked cannot be proven clean).
fn walk_special(
    dir: &Path,
    rel: &[u8],
    visits: &mut usize,
    map: &mut std::collections::BTreeMap<Vec<u8>, Kind>,
    gitlinks_live: &[Vec<u8>],
) -> Result<(), i32> {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(_) => {
            return Err(stop3(
                "the worktree could not be walked: a folder could not be read",
            ))
        }
    };
    for entry in rd {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                return Err(stop3(
                    "the worktree could not be walked: a folder could not be read",
                ))
            }
        };
        let name = entry.file_name();
        let name_bytes = name.as_bytes();
        if name_bytes == b".git" {
            if rel.is_empty() || gitlinks_live.iter().any(|g| g == rel) {
                continue; // the worktree's own git dir, or a checked-out submodule's
            }
            *visits += 1;
            if *visits > WALK_CAP {
                return Err(stop3("too many paths: over the tree-walk visit cap"));
            }
            let mut stray_rel = rel.to_vec();
            if !stray_rel.is_empty() {
                stray_rel.push(b'/');
            }
            stray_rel.extend_from_slice(name_bytes);
            insert(map, stray_rel, Kind::Untracked);
            continue; // a stray `.git` is named once and never descended into
        }
        *visits += 1;
        if *visits > WALK_CAP {
            return Err(stop3("too many paths: over the tree-walk visit cap"));
        }
        let m = match entry.metadata() {
            Ok(m) => m,
            Err(_) => {
                return Err(stop3(
                    "the worktree could not be walked: a path could not be read",
                ))
            }
        };
        let ft = m.file_type();
        let mut child_rel = rel.to_vec();
        if !child_rel.is_empty() {
            child_rel.push(b'/');
        }
        child_rel.extend_from_slice(name_bytes);
        if ft.is_symlink() {
            continue; // judged as a path of its own by git and the no-follow stat; never followed
        } else if ft.is_dir() {
            walk_special(&entry.path(), &child_rel, visits, map, gitlinks_live)?;
        } else if !ft.is_file() {
            insert(map, child_rel, Kind::Untracked);
        }
    }
    Ok(())
}

/// Insert a stray with the strongest-kind-wins rule.
fn insert(map: &mut std::collections::BTreeMap<Vec<u8>, Kind>, path: Vec<u8>, kind: Kind) {
    let e = map.entry(path).or_insert(kind);
    if *e < kind {
        *e = kind;
    }
}

/// Parse `git status --porcelain=v2 -z` output into the stray map. Header lines (leading `#`) are skipped;
/// every entry type maps to a kind: `1`/`2`/`u` are tracked changes (`modified`, or `submodule` when the sub
/// field starts `S`), `?` is `untracked`, `!` is `ignored`. Paths are raw bytes (with `-z` git does not quote
/// them); a rename entry (`2`) carries its origPath in the NEXT NUL-separated token, which is recorded too
/// (the removed original is a stray path of its own). A
/// trailing `/` on an ignored directory entry is dropped so a directory and its spelling as a path compare equal.
fn parse_status(bytes: &[u8], map: &mut std::collections::BTreeMap<Vec<u8>, Kind>) {
    let tokens: Vec<&[u8]> = bytes.split(|&b| b == 0).collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        i += 1;
        if t.is_empty() || t.first() == Some(&b'#') {
            continue;
        }
        if t.starts_with(b"? ") || t.starts_with(b"! ") {
            let mut p = t[2..].to_vec();
            if p.last() == Some(&b'/') {
                p.pop();
            }
            if !p.is_empty() {
                insert(
                    map,
                    p,
                    if t[0] == b'?' {
                        Kind::Untracked
                    } else {
                        Kind::Ignored
                    },
                );
            }
            continue;
        }
        // `1 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <path>` — the path is the 9th field, spaces included.
        if t.starts_with(b"1 ") {
            if let Some((sub, path)) = field(t, 8, 2) {
                insert(map, path.to_vec(), sub_kind(sub));
            }
            continue;
        }
        // `2 <XY> <sub> <mH> <mI> <mW> <hH> <hI> <X><score> <path>` then, in -z, the origPath as its own token.
        // A rename carries TWO paths — the new one and the removed original — and both are recorded, or a
        // FAIL would name only half of what moved. The origPath token is still consumed either way, so
        // parsing stays aligned.
        if t.starts_with(b"2 ") {
            if let Some((sub, path)) = field(t, 9, 2) {
                let kind = sub_kind(sub);
                insert(map, path.to_vec(), kind);
                if i < tokens.len() && !tokens[i].is_empty() {
                    insert(map, tokens[i].to_vec(), kind);
                }
            }
            i += 1; // consume the origPath token
            continue;
        }
        // `u <XY> <sub> <m1> <m2> <m3> <mW> <h1> <h2> <h3> <path>` — the 11th field.
        if t.starts_with(b"u ") {
            if let Some((_, path)) = field(t, 10, 2) {
                insert(map, path.to_vec(), Kind::Modified);
            }
            continue;
        }
    }
}

/// The `idx`-th space-separated field of a record, returning it together with the field at `sub_idx` (the
/// submodule field). The path field is taken with a split limit of `idx + 1`, so it is the LAST field and a
/// path containing spaces survives intact.
fn field(t: &[u8], idx: usize, sub_idx: usize) -> Option<(&[u8], &[u8])> {
    let path = t.splitn(idx + 1, |&b| b == b' ').nth(idx)?;
    let sub = t.splitn(sub_idx + 1, |&b| b == b' ').nth(sub_idx)?;
    if path.is_empty() {
        return None;
    }
    Some((sub, path))
}

/// The porcelain v2 `<sub>` field: `S...` means the path is a submodule.
fn sub_kind(sub: &[u8]) -> Kind {
    if sub.first() == Some(&b'S') {
        Kind::Submodule
    } else {
        Kind::Modified
    }
}

/// Parse `git ls-files -v -z` output: each entry is `<tag> <path>`. The tag is a case-folded set of bits, not
/// an enumerated kind: `S` is skip-worktree, a LOWERCASE tag is assume-unchanged, and one path carrying BOTH
/// bits prints `s` — so matching `h` and `S` alone would hide exactly the doubly-hidden file. Anything that is
/// not a plain `H` (lowercase tags, both-bits tags, unknown and future tags) marks a tracked path hidden from
/// status: stray on its own, fail closed, whether or not the path was also edited.
fn parse_hidden(bytes: &[u8], map: &mut std::collections::BTreeMap<Vec<u8>, Kind>) {
    for t in bytes.split(|&b| b == 0) {
        if t.len() < 3 || t[1] != b' ' {
            continue;
        }
        if t[0] != b'H' {
            insert(map, t[2..].to_vec(), Kind::Hidden);
        }
    }
}

/// A path rendered for one stdout line: printable ASCII passes through (a backslash doubles), and every other
/// byte — control characters, space, DEL, and any non-ASCII/non-UTF-8 byte — becomes `\xNN`. One path is always
/// one line, whatever bytes it holds.
fn escape(p: &[u8]) -> String {
    let mut s = String::with_capacity(p.len());
    for &b in p {
        match b {
            b'\\' => s.push_str("\\\\"),
            0x21..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\x{:02x}", b)),
        }
    }
    s
}

/// A git call of the unpushed mode: the same forced, neutralised, capped read as every other call, plus the
/// commit graph forced off (a planted commit-graph file cannot lie about parentage on top of the already
/// forced `--no-replace-objects`). The `args` are inserted after the forced pairs, so they outrank repo,
/// worktree and (neutralised) user config exactly like the check's own settings.
fn pushed_git(tree: &Path, args: &[&str]) -> GitOut {
    let mut a: Vec<&str> = vec!["-c", "core.commitGraph=false"];
    a.extend_from_slice(args);
    git_bytes(tree, &a)
}

/// The same call that must succeed as capped UTF-8 text; `None` is a failed git call.
fn pushed_git_text(tree: &Path, args: &[&str]) -> Option<Result<String, i32>> {
    let g = pushed_git(tree, args);
    if g.overflow {
        return Some(Err(stop3(
            "too many paths: a git answer went over the read cap",
        )));
    }
    if !g.ok {
        return None;
    }
    Some(Ok(String::from_utf8_lossy(&g.bytes).trim().to_string()))
}

/// A full 40-hex commit sha.
fn sha40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The unpushed check (P1B-12), entered only with `--pushed --repo <absolute-dir>` once steps 1-6 of the
/// clean-state check have passed unchanged. One question: is every commit on `ws/<ticket>` also on the
/// branch's upstream remote-tracking ref? Everything is read locally (the tracking ref git already has);
/// there is no fetch, no push, no ref or config write, and no index refresh. Every shape the answer depends
/// on is verified before it is used, and anything that cannot be PROVEN makes the whole check exit 3 —
/// "cannot check" — never a pass: a missing upstream, a symbolic tracking ref, a grafts/shallow file, a
/// worktree of another repo, or odd `.git` shapes.
fn pushed_check(ticket: &str, tree: &Path, repo: &Path) -> i32 {
    // (a) The `.git` shapes, by no-follow stat BEFORE any git call of this mode reads them: the worktree must
    //     be a linked worktree (its `.git` a regular pointer FILE — a symlink, directory or anything else is
    //     exit 3) and `--repo` a main work tree (a real directory whose `.git` is a real directory).
    match std::fs::symlink_metadata(tree.join(".git")) {
        Ok(m) if m.file_type().is_file() => {}
        _ => {
            return stop3("the worktree is not a linked worktree (its .git is not a pointer file)")
        }
    }
    match std::fs::symlink_metadata(repo) {
        Ok(m) if m.file_type().is_dir() => {}
        _ => return stop3("--repo is not a directory"),
    }
    match std::fs::symlink_metadata(repo.join(".git")) {
        Ok(m) if m.file_type().is_dir() => {}
        _ => return stop3("--repo is not a main work tree (its .git is not a directory)"),
    }
    let Ok(repo_canon) = repo.canonicalize() else {
        return stop3("--repo cannot be resolved");
    };
    let Ok(repo_git) = repo_canon.join(".git").canonicalize() else {
        return stop3("--repo cannot be resolved");
    };

    // (b) Same repository: the worktree's canonical git COMMON dir must be exactly canonical `<repo>/.git`
    //     (a worktree of another repo, or a standalone clone planted as the tree, is exit 3), and `<repo>`
    //     must be its own top level.
    let common = match pushed_git_text(
        tree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ) {
        Some(Ok(c)) => PathBuf::from(c),
        Some(Err(c)) => return c,
        None => return stop3("a git call in the worktree failed"),
    };
    let Ok(common) = common.canonicalize() else {
        return stop3("the worktree git common dir cannot be resolved");
    };
    if common != repo_git {
        return stop3("the worktree does not belong to --repo");
    }
    match pushed_git_text(
        repo,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    ) {
        Some(Ok(top)) => {
            let Ok(top) = Path::new(&top).canonicalize() else {
                return stop3("the --repo top level cannot be resolved");
            };
            if top != repo_canon {
                return stop3("the worktree does not belong to --repo");
            }
        }
        Some(Err(c)) => return c,
        None => return stop3("a git call in --repo failed"),
    }

    // (c) History-rewriting files are refused on PRESENCE alone (any type, no-follow): an `info/grafts` file
    //     can make the tracking commit claim any ancestry, and a `shallow` file cuts the walk short.
    for name in ["info/grafts", "shallow"] {
        if common.join(name).symlink_metadata().is_ok() {
            return stop3(&format!(
                "a history-rewriting file is present in the repo ({}); cannot check",
                name
            ));
        }
    }

    // (d) The upstream, from the repo's own config: a failed lookup (no `branch.<b>.remote`/`.merge`, the
    //     remote gone, the tracking ref never fetched or pushed) fails CLOSED. The answer must name a
    //     remote-tracking ref; anything else — for example `branch.<b>.remote=.` resolving the "upstream" to
    //     a LOCAL branch — is exit 3.
    let branch = format!("refs/heads/ws/{}", ticket);
    // The `@{upstream}` suffix resolves against a branch NAME (`refs/heads/<b>@{upstream}` is not a form git
    // accepts); the ticket token is already validated (no `/`, no `@`, no `{`), so the name is inert.
    let upstream = format!("ws/{}@{{upstream}}", ticket);
    let upstream = match pushed_git_text(
        tree,
        &["rev-parse", "--verify", "--symbolic-full-name", &upstream],
    ) {
        Some(Ok(u)) if !u.is_empty() => u,
        Some(Err(c)) => return c,
        _ => return stop3("the ticket branch has no upstream"),
    };
    if !upstream.starts_with("refs/remotes/") {
        return stop3("the ticket branch's upstream is not a remote-tracking ref");
    }
    // The NON-dereferenced upstream name, as git itself configured it: `rev-parse --symbolic-full-name`
    // FOLLOWS a symbolic ref to its target, so when the refspec maps the branch somewhere other than the
    // default name and that ref is a symref, the answer above is the TARGET (a plain ref) and the symref
    // would never be seen by any later rule. `%(upstream)` applies the same fetch-refspec mapping as
    // `@{upstream}` WITHOUT following the ref it names; it must agree with git's dereferenced answer and
    // (below) with the config rebuild — any difference means a symref or a mapping disagreement: exit 3.
    let g = pushed_git(
        tree,
        &["for-each-ref", "--format=%(upstream)", "--", &branch],
    );
    if g.overflow {
        return stop3("too many paths: a git answer went over the read cap");
    }
    if !g.ok {
        return stop3("a git call in the worktree failed");
    }
    let nonderef_text = String::from_utf8_lossy(&g.bytes).into_owned();
    let mut nonderef_lines = nonderef_text.lines().filter(|l| !l.is_empty());
    let nonderef = match (nonderef_lines.next(), nonderef_lines.next()) {
        (Some(l), None) => l.to_string(),
        _ => return stop3("the ticket branch has no upstream"),
    };

    // (e) The non-dereferenced name must be a PLAIN ref: exactly one answer of `<sha> <symref>` with an
    //     empty symref field. A symbolic ref at the ref git itself configured as the upstream (textual or
    //     a symlink git reads as one, for example pointing back at `refs/heads/ws/<ticket>` so the
    //     upstream sha equals the branch tip and every unpushed commit would pass, or — after a refspec
    //     remap — at the stale default-name leftover) is exit 3. Its object must peel to a commit.
    let g = pushed_git(
        tree,
        &[
            "for-each-ref",
            "--format=%(objectname) %(symref)",
            "--",
            &nonderef,
        ],
    );
    if g.overflow {
        return stop3("too many paths: a git answer went over the read cap");
    }
    if !g.ok {
        return stop3("a git call in the worktree failed");
    }
    let line = g
        .bytes
        .split(|&b| b == b'\n')
        .find(|t| !t.is_empty())
        .map(|t| t.to_vec());
    let Some(line) = line else {
        return stop3("the upstream tracking ref cannot be read");
    };
    let line = String::from_utf8_lossy(&line).into_owned();
    let (obj, symref) = match line.split_once(' ') {
        Some(v) => v,
        None => return stop3("the upstream tracking ref cannot be read"),
    };
    if !symref.is_empty() {
        return stop3("the upstream tracking ref is a symbolic ref; cannot check");
    }
    if !sha40(obj) {
        return stop3("the upstream tracking ref cannot be read");
    }
    let peel = format!("{}^{{commit}}", obj);
    let upstream_sha =
        match pushed_git_text(tree, &["rev-parse", "--verify", "--end-of-options", &peel]) {
            Some(Ok(s)) if sha40(&s) => s,
            Some(Err(c)) => return c,
            _ => return stop3("the upstream ref does not point at a commit"),
        };

    // (f) The exact tracking ref is REBUILT from the branch's own config — `branch.ws/<ticket>.remote` +
    //     `branch.ws/<ticket>.merge`, the same pair `@{upstream}` is built from — because rev-parse follows
    //     a symbolic ref to its TARGET: a symref planted at the tracking ref would otherwise hand the checks
    //     below the target (a plain ref) and pass. `-z --get-regexp` records are `key\nvalue`; the config
    //     subsection is everything between `branch.` and the LAST dot (a subsection may itself hold dots,
    //     the key may not). A remote of `.` (a local branch as the "upstream") or a merge that is not a
    //     branch ref can never name a remote-tracking ref: exit 3.
    let g = pushed_git(tree, &["config", "-z", "--get-regexp", "^branch\\."]);
    if g.overflow {
        return stop3("too many paths: a config answer went over the read cap");
    }
    if !g.ok {
        return stop3("a git call in the worktree failed");
    }
    let want_sub = format!("ws/{}", ticket);
    let (mut remote, mut merge) = (None, None);
    for rec in g.bytes.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let Some(nl) = rec.iter().position(|&b| b == b'\n') else {
            continue;
        };
        let key = String::from_utf8_lossy(&rec[..nl]).into_owned();
        let value = String::from_utf8_lossy(&rec[nl + 1..]).trim().to_string();
        let Some(rest) = key.strip_prefix("branch.") else {
            continue;
        };
        let Some((sub, name)) = rest.rsplit_once('.') else {
            continue;
        };
        if sub != want_sub {
            continue;
        }
        match name {
            "remote" => remote = Some(value),
            "merge" => merge = Some(value),
            _ => {}
        }
    }
    let (Some(remote), Some(merge)) = (remote, merge) else {
        return stop3("the ticket branch has no upstream");
    };
    if remote == "." {
        return stop3("the ticket branch's upstream is not a remote-tracking ref");
    }
    let Some(short) = merge.strip_prefix("refs/heads/") else {
        return stop3("the ticket branch's upstream is not a remote-tracking ref");
    };
    let tracking = format!("refs/remotes/{}/{}", remote, short);
    // The rebuilt name is NEVER trusted over git's answer: `@{upstream}` applies the remote's fetch
    // refspec (which may map `refs/heads/ws/<t>` somewhere other than `refs/remotes/<remote>/ws/<t>`)
    // and uses the FIRST `branch.<b>.merge` value, while the rebuild above is last-wins and refspec-blind
    // — the two can disagree, and a rebuild that lands on a stale or wrong ref can pass commits git says
    // are unpushed. The NON-dereferenced `%(upstream)` name must agree too: it is what the plain-ref
    // rule above was asked of, and it is the only one of the three answers that exposes a mapping the
    // rebuild happens to reproduce while git resolves through something else. Any disagreement between
    // the three is exit 3, never a pass.
    if tracking != upstream || nonderef != upstream {
        return stop3("the upstream tracking ref cannot be verified");
    }

    // (g) The list: commits reachable from the branch and not from the upstream commit — reachability, not
    //     recency, so a force-moved upstream lists every ticket commit it no longer contains and an upstream
    //     AHEAD of the branch that contains it passes. Full shas only, after `--end-of-options`, capped:
    //     more than `PUSHED_CAP` commits is exit 3, never a truncated answer. Empty means pushed.
    let branch_sha = match pushed_git_text(
        tree,
        &["rev-parse", "--verify", "--end-of-options", &branch],
    ) {
        Some(Ok(s)) if sha40(&s) => s,
        Some(Err(c)) => return c,
        _ => return stop3("the ticket branch cannot be resolved"),
    };
    let neg = format!("^{}", upstream_sha);
    // Joined at run time so no `=value` literal pools in the binary's read-only data (see gitio::quotepath_off).
    let max_count = ["--max-count", "1001"].join("=");
    let g = pushed_git(
        tree,
        &[
            "rev-list",
            "--topo-order",
            &max_count,
            "--end-of-options",
            &branch_sha,
            &neg,
        ],
    );
    if g.overflow {
        return stop3("too many paths: a git answer went over the read cap");
    }
    if !g.ok {
        return stop3("a git call in the worktree failed");
    }
    let Some(text) = std::str::from_utf8(&g.bytes).ok() else {
        return stop3("a git answer in the worktree was not valid UTF-8");
    };
    let commits: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    if commits.len() > PUSHED_CAP {
        return stop3("too many commits to check (over the 1000-commit cap)");
    }
    if commits.iter().any(|c| !sha40(c)) {
        return stop3("a git answer in the worktree was not a commit list");
    }
    let upstream_esc = escape(tracking.as_bytes());
    if commits.is_empty() {
        println!(
            "workspace=pushed ticket={} branch=ws/{} upstream={}",
            ticket, ticket, upstream_esc
        );
        return 0;
    }
    for c in &commits {
        println!("unpushed {} ws/{}", c, ticket);
    }
    println!(
        "workspace=unpushed ticket={} branch=ws/{} upstream={} count={}",
        ticket,
        ticket,
        upstream_esc,
        commits.len()
    );
    4
}

/// `harness workspace check ...` (the arguments after `check`). Exit 0 clean (or, with `--pushed --repo`,
/// every commit on the branch also on its upstream); 2 at least one stray path; 3 cannot check; 4 (with
/// `--pushed`) at least one unpushed commit. Any non-zero exit means "not proven clean"/"not proven pushed".
pub fn check_cmd(a: &[String]) -> i32 {
    let (ticket, md, pushed, repo) = match parse_args(a) {
        Ok(v) => v,
        Err(code) => return code,
    };
    let md = PathBuf::from(&md);

    // 1. The manifest, read through the existing reader only, never written. A missing file, a torn tail or a
    //    corrupt line is exit 3; the reader's own message may repeat a path, so the terminal line is fixed.
    let read = match manifest::read_manifest(&md, &ticket) {
        Ok(r) if r.is_clean() => r,
        _ => return stop3("the manifest could not be read"),
    };

    // 2. The worktree is the live entry id `worktree` (class `worktree`, a path target) — never a name search,
    //    never the current folder.
    let tree_text = read
        .live
        .iter()
        .find(|e| e.id == "worktree" && e.class == manifest::Class::Worktree)
        .and_then(|e| match &e.target {
            Target::Path(p) => Some(p.clone()),
            _ => None,
        });
    let Some(tree_text) = tree_text else {
        return stop3("the manifest lists no live worktree entry");
    };
    let stored = PathBuf::from(&tree_text);

    // 3. The worktree path itself: no-follow stat — missing, a symlink (wherever it points) or not a directory
    //    is exit 3. Only then is it canonicalized once, and every later comparison uses that one spelling.
    let meta = match std::fs::symlink_metadata(&stored) {
        Ok(m) => m,
        Err(_) => return stop3("the listed worktree path is missing"),
    };
    if meta.file_type().is_symlink() {
        return stop3("the listed worktree path is a symlink");
    }
    if !meta.file_type().is_dir() {
        return stop3("the listed worktree path is not a directory");
    }
    let Ok(tree) = stored.canonicalize() else {
        return stop3("the listed worktree path cannot be resolved");
    };

    // 4. It must be its own git work tree's top level (a folder inside the tree is not the worktree).
    let Some(top) = git_text(
        &tree,
        &["rev-parse", "--path-format=absolute", "--show-toplevel"],
    ) else {
        return stop3("a git call in the worktree failed");
    };
    match top {
        Ok(top) => {
            let Ok(top) = Path::new(&top).canonicalize() else {
                return stop3("the worktree top level cannot be resolved");
            };
            if top != tree {
                return stop3(
                    "the listed worktree folder is not the top level of its git work tree",
                );
            }
        }
        Err(code) => return code,
    }

    // 5. HEAD must be exactly the ticket's own branch, `refs/heads/ws/<ticket>` — detached or any other branch
    //    is exit 3 (`symbolic-ref -q` fails on a detached HEAD).
    let expected_head = format!("refs/heads/ws/{}", ticket);
    let Some(head) = git_text(&tree, &["symbolic-ref", "--quiet", "HEAD"]) else {
        return stop3("a git call in the worktree failed");
    };
    match head {
        Ok(h) if h == expected_head => {}
        Ok(_) => return stop3("the worktree HEAD is not on the ticket's own branch ws/<ticket>"),
        Err(code) => return code,
    }

    // 6. No operation may be in progress in the worktree: a merge, rebase or cherry-pick in flight (its state
    //    files sit in the worktree's own git dir) means the tree cannot be judged at all.
    let Some(gitdir) = git_text(&tree, &["rev-parse", "--absolute-git-dir"]) else {
        return stop3("a git call in the worktree failed");
    };
    let gitdir = match gitdir {
        Ok(g) => PathBuf::from(g),
        Err(code) => return code,
    };
    let in_progress = ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"]
        .iter()
        .any(|f| gitdir.join(f).exists())
        || ["rebase-merge", "rebase-apply", "sequencer"]
            .iter()
            .any(|d| gitdir.join(d).is_dir());
    if in_progress {
        return stop3("a merge, rebase or cherry-pick is in progress in the worktree");
    }

    // 6b. The unpushed mode (P1B-12): `--pushed` switches the command to the unpushed check ONLY (the stray
    //     scan below never runs in this mode) after the same steps 1-6, and never reads a ticket file or a
    //     repo map: `--repo` is the ticket repo's top folder, taken by the caller from the repo map.
    if pushed {
        return pushed_check(&ticket, &tree, Path::new(repo.as_deref().unwrap_or("")));
    }

    // 7. The repo-settings refusal scan: attributes, filters and a redirecting core.worktree are exit 3
    //    before any content is compared (a filter must never run).
    if let Err(code) = refuse_scan(&tree, "the worktree") {
        return code;
    }

    // 8. The status read. Repo configuration cannot hide anything: untracked-files mode, submodule ignoring
    //    and the stat levers are forced on the command line (a config `status.showUntrackedFiles=no` or
    //    `core.trustctime=false` loses to the flags), ignored paths are listed (`--ignored=matching`),
    //    paths are raw bytes (`-z`, no quoting) and every file is named individually
    //    (`untracked-files=all`), never just its folder.
    let mut map = std::collections::BTreeMap::new();
    let status = git_bytes(&tree, &STATUS_ARGS);
    if status.overflow {
        return stop3("too many paths: the status answer went over the read cap");
    }
    if !status.ok {
        return stop3("a git call in the worktree failed");
    }
    parse_status(&status.bytes, &mut map);

    // 9. The hidden read: tracked paths carrying the assume-unchanged or skip-worktree bit are invisible to
    //    status, so they get their own query.
    let hidden = git_bytes(&tree, &["ls-files", "-v", "-z"]);
    if hidden.overflow {
        return stop3("too many paths: the tracked-file answer went over the read cap");
    }
    if !hidden.ok {
        return stop3("a git call in the worktree failed");
    }
    parse_hidden(&hidden.bytes, &mut map);

    // 9b. The content read (ticket #184, item A): a second tracked-file read against a throwaway index
    //     rebuilt from HEAD with no stat information at all, so git must hash every tracked file and
    //     compare bytes — the stat cache (size, mtime, ctime) can never decide "clean" here, and a
    //     same-size edit with the clock forged back into the index-write second is still named. The
    //     throwaway index is one private temp file, removed before the check returns.
    if let Err(code) = content_read(&tree, "the worktree", &mut map) {
        return code;
    }

    // 10. The submodule pass: git's own dirty-content probe would run the submodule's own status, which
    //     honours the submodule's own config and index bits — so the status read above compares submodule
    //     commit pointers only, and every CHECKED-OUT gitlink gets the same forced reads inside it
    //     (recursively), and a hit names the submodule itself. Each level records its path so the walk
    //     below skips exactly those `.git` entries.
    let mut gitlinks_live: Vec<Vec<u8>> = Vec::new();
    let mut submodules_visited = 0usize;
    let top_gitlinks = match gitlinks(&tree, "the worktree") {
        Ok(v) => v,
        Err(code) => return code,
    };
    for rel in top_gitlinks {
        let p = tree.join(std::ffi::OsStr::from_bytes(&rel));
        match gitlink_state(&p) {
            GitlinkState::Absent => continue,
            GitlinkState::Odd => {
                return stop3(&format!(
                    "submodule {} cannot be inspected: its folder holds no git dir",
                    escape(&rel)
                ));
            }
            GitlinkState::CheckedOut => {
                if let Err(code) = check_submodule(&p, &rel, 0, &mut submodules_visited, &mut map, &mut gitlinks_live) {
                    return code;
                }
            }
        }
    }

    // 11. The walk: entries git never lists (a FIFO, a socket, a device) are stray untracked. No-follow,
    //     `.git` skipped only for the worktree and the checked-out gitlinks, symlinks never followed,
    //     visits capped.
    let mut visits = 0usize;
    if let Err(code) = walk_special(&tree, b"", &mut visits, &mut map, &gitlinks_live) {
        return code;
    }

    // 12. Live manifest path entries INSIDE the worktree (strictly inside; the `worktree` entry is the tree
    //    itself). One that is still what the manifest says it is — today: a no-follow stat finds a SYMLINK at
    //    that exact stored path, the P1B-04 node_modules link — is EXPECTED: that one path (never anything
    //    under it, never anything matched through the link) is removed from the strays. One that is gone or no
    //    longer that kind is `missing`. The stored text is compared to the worktree path directly, never
    //    canonicalized first (canonicalizing the node_modules path would follow the link out of the worktree).
    for e in &read.live {
        let Target::Path(p) = &e.target else { continue };
        let Ok(rel) = Path::new(p.as_str()).strip_prefix(&tree) else {
            continue;
        };
        if rel == Path::new("") {
            continue; // the worktree entry itself
        }
        let rel = rel.as_os_str().as_bytes().to_vec();
        match std::fs::symlink_metadata(p.as_str()) {
            Ok(m) if m.file_type().is_symlink() => {
                // N1: this erases EVERY kind at this one path; safe today because the link path is never
                // tracked (nothing stronger than `untracked`/`ignored` can sit here) — a future listed
                // TRACKED path must instead keep the stronger kind.
                map.remove(&rel);
            }
            _ => insert(&mut map, rel, Kind::Missing),
        }
    }

    // 13. The entry cap: more strays than the bound means "cannot check", never a truncated PASS.
    if map.len() > STRAY_CAP {
        return stop3("too many paths: over the stray-entry cap");
    }

    if map.is_empty() {
        println!("workspace=clean ticket={}", ticket);
        return 0;
    }
    for (p, k) in &map {
        println!("stray {} {}", k.as_str(), escape(p));
    }
    println!("workspace=stray ticket={} count={}", ticket, map.len());
    2
}
