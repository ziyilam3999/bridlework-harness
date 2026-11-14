//! `harness workspace teardown` (P1B-14, finalize steps 4, 5, 7, 8; std only, Unix). Walks the ticket's manifest and
//! clears each thing with the tools that already exist — never a delete path of its own:
//! step 4 stops every live process through the CHILD verb `process stop` (which also removes its port entry) and
//! releases any remaining listed port after the same 127.0.0.1 snapshot bind `process start` uses;
//! step 5 deletes every `temp_dir` entry through the CHILD verb `quarantine delete-listed` (one id per call, the
//! #185 single-thread rule) and releases `evidence` entries from the manifest, keeping the files;
//! step 7 removes the worktree with `git worktree remove` and NEVER `--force`;
//! step 8 re-reads the manifest and appends ONE `cleanup_result` event.
//! A failed step 4 skips 5 and 7 (nothing is deleted under a process that may be alive); a failed step 5 skips 7.
//! No folder is ever printed on stdout or stderr. See docs/workspace.md, "Teardown (P1B-14)".

use crate::events::{self, token_ok, Field, Kind, Record};
use crate::manifest::{self, Class, Op, Target};
use crate::supervisor::port_bind_check;
use crate::workspace::{git, resolve_main_work_tree};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

pub const USAGE: &str = "usage: harness workspace teardown <ticket> --manifest-dir <absolute-dir> --repo-dir <absolute-dir> --log <absolute-path> --repo <token> [--grace-ms <n>] [--ts-ms <n>]";

struct Args {
    ticket: String,
    manifest_dir: String,
    repo_dir: String,
    log: String,
    repo: String,
    grace_ms: Option<String>,
    ts_ms: Option<u64>,
}

fn refuse(m: &str) -> i32 {
    eprintln!("harness: error: {}", m);
    2
}

/// Digits only, at most 20 of them: the cheap up-front bound (anything longer fails closed before a parse).
fn num_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 20 && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse(a: &[String]) -> Result<Args, String> {
    let ticket = match a.first() {
        Some(t) if !t.starts_with("--") => t.clone(),
        _ => return Err(format!("missing ticket\n{}", USAGE)),
    };
    let (mut md, mut rd, mut log, mut repo, mut grace, mut ts) = (None, None, None, None, None, None);
    let mut i = 1;
    while i < a.len() {
        let name = a[i].strip_prefix("--").ok_or_else(|| format!("unexpected argument\n{}", USAGE))?;
        let slot = match name {
            "manifest-dir" => &mut md,
            "repo-dir" => &mut rd,
            "log" => &mut log,
            "repo" => &mut repo,
            "grace-ms" => &mut grace,
            "ts-ms" => &mut ts,
            _ => return Err(format!("unknown option --{}\n{}", name, USAGE)),
        };
        let v = a.get(i + 1).ok_or_else(|| format!("--{} needs a value\n{}", name, USAGE))?;
        if slot.replace(v.clone()).is_some() {
            return Err(format!("--{} given twice\n{}", name, USAGE));
        }
        i += 2;
    }
    let need = |v: Option<String>, n: &str| v.ok_or_else(|| format!("--{} is required\n{}", n, USAGE));
    if let Some(g) = &grace {
        if !num_ok(g) || g.parse::<u64>().is_err() {
            return Err("--grace-ms must be a non-negative integer".to_string());
        }
    }
    let ts_ms = match ts {
        Some(t) => {
            if !num_ok(&t) {
                return Err("--ts-ms must be a non-negative integer".to_string());
            }
            Some(t.parse::<u64>().map_err(|_| "--ts-ms out of range".to_string())?)
        }
        None => None,
    };
    Ok(Args {
        ticket,
        manifest_dir: need(md, "manifest-dir")?,
        repo_dir: need(rd, "repo-dir")?,
        log: need(log, "log")?,
        repo: need(repo, "repo")?,
        grace_ms: grace,
        ts_ms,
    })
}

/// Canonicalize the longest existing prefix and keep the missing tail as plain names (a `..` in the tail is refused).
fn canon_lenient(p: &Path) -> Option<PathBuf> {
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = p.to_path_buf();
    loop {
        match cur.canonicalize() {
            Ok(c) => {
                let mut out = c;
                for t in tail.iter().rev() {
                    out.push(t);
                }
                return Some(out);
            }
            Err(_) => {
                let name = cur.file_name()?.to_os_string();
                if name == ".." || name == "." {
                    return None;
                }
                tail.push(name);
                cur = cur.parent()?.to_path_buf();
            }
        }
    }
}

fn is_real_dir(p: &Path) -> bool {
    std::fs::symlink_metadata(p).map(|m| m.file_type().is_dir()).unwrap_or(false)
}

/// One child run of this same binary: no stdin, stdout/stderr dropped (a child line may name a folder).
/// Returns the exit code, or None when it could not be spawned or ended by a signal.
fn child(args: &[String]) -> Option<i32> {
    let exe = std::env::current_exe().ok()?;
    Command::new(exe).args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().ok()?.code()
}

fn now_ms() -> Option<u64> {
    SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as u64)
}

/// The canonical `worktree <path>` records of the primary's porcelain list, in order (the first is the main one).
/// Both sides of every later comparison are canonicalized (`/var` vs `/private/var`).
fn listed_worktrees(repo: &Path) -> Option<Vec<PathBuf>> {
    let out = git(repo, &["worktree", "list", "--porcelain"])?;
    Some(
        out.lines()
            .filter_map(|l| l.strip_prefix("worktree "))
            .map(|p| canon_lenient(Path::new(p)).unwrap_or_else(|| PathBuf::from(p)))
            .collect(),
    )
}

/// `git worktree remove <tree>` — no hooks, no inherited `GIT_*` name, never `--force`; output dropped.
fn git_worktree_remove(repo: &Path, tree: &Path) -> bool {
    let mut c = Command::new("git");
    c.arg("-C").arg(repo).args(["-c", "core.hooksPath=/dev/null", "worktree", "remove"]).arg(tree);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    matches!(c.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status(), Ok(s) if s.success())
}

fn remove_entry(md: &Path, ticket: &str, id: &str, ts: u64) -> bool {
    manifest::apply(md, ticket, &Op::Remove { id }, ts).is_ok()
}

/// Step 7 for one worktree entry. True = settled and the manifest line written.
fn remove_worktree(md: &Path, ticket: &str, id: &str, listed: &str, repo_top: &Path, repo_dir: &Path, ts: u64) -> bool {
    let path = Path::new(listed);
    let meta = std::fs::symlink_metadata(path);
    let Some(wts) = listed_worktrees(repo_dir) else { return false };
    let Some(want) = canon_lenient(path) else { return false };
    match meta {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Already gone (crash between the git remove and the manifest line): settle only when git agrees.
            if wts.contains(&want) {
                return false;
            }
            return remove_entry(md, ticket, id, ts);
        }
        Err(_) => return false,
        Ok(m) => {
            if !m.file_type().is_dir() {
                return false; // a link or file: never followed
            }
        }
    }
    if want == repo_top || !wts.contains(&want) || wts.first().map(|w| *w == want).unwrap_or(true) {
        return false;
    }
    if !git_worktree_remove(repo_dir, &want) {
        return false;
    }
    let gone = matches!(std::fs::symlink_metadata(path), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    let unlisted = listed_worktrees(repo_dir).map(|l| !l.contains(&want)).unwrap_or(false);
    gone && unlisted && remove_entry(md, ticket, id, ts)
}

pub fn teardown_cmd(a: &[String]) -> i32 {
    let args = match parse(a) {
        Ok(x) => x,
        Err(m) => return refuse(&m),
    };
    if !token_ok(&args.ticket) {
        return refuse("ticket must be a plain token (lowercase letters, digits, '-', '_', '.'; not empty, not '.' or '..')");
    }
    if !token_ok(&args.repo) {
        return refuse("--repo must be a plain token");
    }
    let md = PathBuf::from(&args.manifest_dir);
    let rd = PathBuf::from(&args.repo_dir);
    let log = PathBuf::from(&args.log);
    for (flag, p) in [("--manifest-dir", &md), ("--repo-dir", &rd), ("--log", &log)] {
        if !p.is_absolute() || p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
            return refuse(&format!("{} must be an absolute path without . or .. parts", flag));
        }
    }
    if !is_real_dir(&md) {
        return refuse("--manifest-dir must be an existing real directory");
    }
    if !is_real_dir(&rd) {
        return refuse("--repo-dir must be an existing real directory");
    }
    let repo = match resolve_main_work_tree(&rd) {
        Ok(r) => r,
        Err(l) => return refuse(l.strip_prefix("harness: error: ").unwrap_or(&l)),
    };
    let read = match manifest::read_manifest(&md, &args.ticket) {
        Ok(r) => r,
        Err(_) => return refuse("the manifest is missing or unreadable"),
    };
    if !read.is_clean() {
        return refuse("the manifest is damaged; nothing was changed");
    }
    // The log must resolve outside every manifest root (a log inside a root is swept or removed with it).
    let Some(log_canon) = log.parent().and_then(|p| p.canonicalize().ok()).and_then(|p| log.file_name().map(|n| p.join(n))) else {
        return refuse("--log must name a file inside an existing directory");
    };
    if std::fs::symlink_metadata(&log_canon).map(|m| m.file_type().is_dir()).unwrap_or(false) {
        return refuse("--log must name a file");
    }
    for r in &read.roots {
        let rc = canon_lenient(Path::new(r)).unwrap_or_else(|| PathBuf::from(r));
        if log_canon.starts_with(&rc) {
            return refuse("--log lies inside a manifest root; the event log must live outside every root");
        }
    }
    let ts = match args.ts_ms.or_else(now_ms) {
        Some(t) => t,
        None => return refuse("the clock is before the epoch"),
    };
    let mut failures = 0u64;

    // ---- step 4a: processes (child verb; it removes the entry and its port entry)
    for e in read.live.iter().filter(|e| e.class == Class::Process) {
        let mut c = vec![
            "process".to_string(),
            "stop".into(),
            args.ticket.clone(),
            "--id".into(),
            e.id.clone(),
            "--dir".into(),
            args.manifest_dir.clone(),
        ];
        if let Some(g) = &args.grace_ms {
            c.push("--grace-ms".into());
            c.push(g.clone());
        }
        if !matches!(child(&c), Some(0) | Some(3)) {
            failures += 1;
        }
    }
    // ---- step 4b: ports still listed
    match manifest::read_manifest(&md, &args.ticket) {
        Ok(r) if r.is_clean() => {
            for e in r.live.iter().filter(|e| e.class == Class::Port) {
                let free = matches!(&e.target, Target::Port(n) if *n <= 65535 && port_bind_check(*n as u16).is_ok());
                if !(free && remove_entry(&md, &args.ticket, &e.id, ts)) {
                    failures += 1;
                }
            }
        }
        _ => failures += 1,
    }

    // ---- step 5
    if failures == 0 {
        match manifest::read_manifest(&md, &args.ticket) {
            Ok(r) if r.is_clean() => {
                for e in r.live.iter().rev().filter(|e| e.class == Class::TempDir) {
                    let c = vec![
                        "quarantine".to_string(),
                        "delete-listed".into(),
                        args.ticket.clone(),
                        "--id".into(),
                        e.id.clone(),
                        "--manifest-dir".into(),
                        args.manifest_dir.clone(),
                        "--ts-ms".into(),
                        ts.to_string(),
                    ];
                    if child(&c) != Some(0) {
                        failures += 1;
                    }
                }
                for e in r.live.iter().filter(|e| e.class == Class::Evidence) {
                    if !remove_entry(&md, &args.ticket, &e.id, ts) {
                        failures += 1;
                    }
                }
            }
            _ => failures += 1,
        }
    }

    // ---- step 7
    if failures == 0 {
        match manifest::read_manifest(&md, &args.ticket) {
            Ok(r) if r.is_clean() => {
                for e in r.live.iter().filter(|e| e.class == Class::Worktree) {
                    let ok = match &e.target {
                        Target::Path(p) => remove_worktree(&md, &args.ticket, &e.id, p, &repo.top, &rd, ts),
                        _ => false,
                    };
                    if !ok {
                        failures += 1;
                    }
                }
            }
            _ => failures += 1,
        }
    }

    // ---- step 8
    let fin = match manifest::read_manifest(&md, &args.ticket) {
        Ok(r) if r.is_clean() => r,
        _ => return refuse("the manifest could not be re-read; the teardown ran but is unrecorded"),
    };
    let left = fin.live.len() as u64;
    let logs = fin.live.iter().filter(|e| e.class == Class::Log).count();
    let pass = failures == 0 && left == 0;
    let result = if pass { "pass" } else { "fail" };
    let rec = match Record::build(
        Kind::CleanupResult,
        ts,
        &[
            ("ticket", Field::Str(args.ticket.clone())),
            ("repo", Field::Str(args.repo.clone())),
            ("result", Field::Str(result.to_string())),
            ("manifest_left", Field::Num(left)),
        ],
    ) {
        Ok(r) => r,
        Err(_) => return refuse("the cleanup_result event could not be built; the teardown ran but is unrecorded"),
    };
    let root = log_canon.parent().unwrap_or(Path::new("."));
    if events::append_record(root, &log_canon, &rec).is_err() {
        return refuse("the cleanup_result event could not be written; the teardown ran but is unrecorded (a re-run records)");
    }
    println!("teardown={} ticket={} manifest_left={}", result, args.ticket, left);
    if !pass {
        eprintln!("harness: teardown incomplete: {} step failure(s), {} entries left, {} log entries left", failures, left, logs);
        return 1;
    }
    0
}
