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
//! The check never writes, deletes, moves or locks anything: it never touches the manifest (read through the
//! existing reader only), and every git call runs under `--no-optional-locks` with fsmonitor off, so not even the
//! index's stat cache is refreshed. Repo configuration cannot hide anything: untracked-files mode, submodule
//! ignoring and fsmonitor are forced on the command line, and ignored files are listed, not skipped. Git output
//! is read through a hard 8 MiB cap and stray entries through a 10,000 cap; hitting either is exit 3
//! ("too many paths"), never a truncated PASS and never an unbounded read (fail closed).
//!
//! Exit codes: 0 clean (one line `workspace=clean ticket=<t>`); 2 at least one stray path (one `stray <kind>
//! <path>` line per path, sorted by raw path bytes, then `workspace=stray ticket=<t> count=<n>`); 3 cannot check
//! — usage errors, a missing/torn/corrupt manifest, no live `worktree` entry, a worktree path that is missing, a
//! symlink or not a directory, a folder that is not that worktree's own top level, a HEAD that is detached or not
//! `refs/heads/ws/<ticket>`, an in-progress merge/rebase/cherry-pick, a failed git call, or a size cap. Paths are
//! worktree-relative only and escaped (`\\` for a backslash, `\xNN` for any control, space, or non-ASCII byte),
//! so one path is always one line and no folder is ever printed.

use crate::events::token_ok;
use crate::manifest::{self, Target};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

pub const USAGE: &str = "usage: harness workspace check <ticket> --manifest-dir <absolute-dir>";

/// The hard cap on any single git output read: an "unbounded read" is impossible past this, and hitting it is
/// exit 3, never a silently truncated answer.
const GIT_CAP: usize = 8 * 1024 * 1024;
/// The hard cap on stray entries: a folder with more strays than this cannot be proven clean, so it is exit 3.
const STRAY_CAP: usize = 10_000;

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

/// `<ticket> --manifest-dir <absolute-dir>`. The ticket must be a plain token exactly as the manifest defines it:
/// refused, never rewritten. No default folder, no config file, no current-folder lookup.
fn parse_args(a: &[String]) -> Result<(String, String), i32> {
    let (mut ticket, mut md) = (None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--manifest-dir" => {
                let val = it.next().ok_or_else(|| usage3("--manifest-dir is missing its value"))?;
                if md.replace(val.as_str()).is_some() {
                    return Err(usage3("--manifest-dir was given twice"));
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
        return Err(usage3("--manifest-dir must be an absolute path with no `..` component"));
    }
    Ok((ticket.to_string(), md.to_string()))
}

/// One captured git call: read-only (`--no-optional-locks`, so not even the index stat cache is refreshed),
/// no hooks, no inherited `GIT_*` name (an inherited `GIT_DIR` cannot redirect it), no stdin, stderr dropped
/// (git's own message may name a folder). The stdout read is hard-capped: past `GIT_CAP` the flag `overflow`
/// is set and the rest is drained and discarded, never buffered.
struct GitOut {
    ok: bool,
    overflow: bool,
    bytes: Vec<u8>,
}

fn git_bytes(tree: &Path, args: &[&str]) -> GitOut {
    let mut c = Command::new("git");
    c.arg("--no-optional-locks")
        .arg("-C")
        .arg(tree)
        .args(["-c", "core.hooksPath=/dev/null", "-c", "core.fsmonitor=false"])
        .args(args);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    let mut child = match c.stdin(Stdio::null()).stderr(Stdio::null()).stdout(Stdio::piped()).spawn() {
        Ok(ch) => ch,
        Err(_) => return GitOut { ok: false, overflow: false, bytes: Vec::new() },
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
    GitOut { ok, overflow, bytes }
}

/// A git call that must succeed as UTF-8 text under the cap (rev-parse-style queries). Anything else is None.
fn git_text(tree: &Path, args: &[&str]) -> Option<Result<String, i32>> {
    let g = git_bytes(tree, args);
    if g.overflow {
        return Some(Err(stop3("too many paths: a git answer went over the read cap")));
    }
    if !g.ok {
        return None;
    }
    Some(Ok(String::from_utf8_lossy(&g.bytes).trim().to_string()))
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
/// them); a rename entry (`2`) carries its origPath in the NEXT NUL-separated token, which is skipped. A
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
                insert(map, p, if t[0] == b'?' { Kind::Untracked } else { Kind::Ignored });
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
        if t.starts_with(b"2 ") {
            if let Some((sub, path)) = field(t, 9, 2) {
                insert(map, path.to_vec(), sub_kind(sub));
            }
            i += 1; // skip origPath
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
fn field<'a>(t: &'a [u8], idx: usize, sub_idx: usize) -> Option<(&'a [u8], &'a [u8])> {
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

/// Parse `git ls-files -v -z` output: each entry is `<tag> <path>`. A LOWERCASE `h` tag (assume-unchanged) or
/// an `S` tag (skip-worktree) marks a tracked path hidden from status — stray on its own, fail closed, whether
/// or not the path was also edited.
fn parse_hidden(bytes: &[u8], map: &mut std::collections::BTreeMap<Vec<u8>, Kind>) {
    for t in bytes.split(|&b| b == 0) {
        if t.len() < 3 || t[1] != b' ' {
            continue;
        }
        if matches!(t[0], b'h' | b'S') {
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

/// `harness workspace check ...` (the arguments after `check`). Exit 0 clean; 2 at least one stray path; 3
/// cannot check. Any non-zero exit means "not proven clean".
pub fn check_cmd(a: &[String]) -> i32 {
    let (ticket, md) = match parse_args(a) {
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
    let Some(top) = git_text(&tree, &["rev-parse", "--path-format=absolute", "--show-toplevel"]) else {
        return stop3("a git call in the worktree failed");
    };
    match top {
        Ok(top) => {
            let Ok(top) = Path::new(&top).canonicalize() else {
                return stop3("the worktree top level cannot be resolved");
            };
            if top != tree {
                return stop3("the listed worktree folder is not the top level of its git work tree");
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
        || ["rebase-merge", "rebase-apply", "sequencer"].iter().any(|d| gitdir.join(d).is_dir());
    if in_progress {
        return stop3("a merge, rebase or cherry-pick is in progress in the worktree");
    }

    // 7. The status read. Repo configuration cannot hide anything: untracked-files mode, submodule ignoring and
    //    fsmonitor are forced on the command line (a config `status.showUntrackedFiles=no` loses to the flag),
    //    ignored paths are listed (`--ignored=matching`), paths are raw bytes (`-z`, no quoting) and every file
    //    is named individually (`untracked-files=all`), never just its folder.
    let mut map = std::collections::BTreeMap::new();
    let status = git_bytes(
        &tree,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--ignore-submodules=none",
        ],
    );
    if status.overflow {
        return stop3("too many paths: the status answer went over the read cap");
    }
    if !status.ok {
        return stop3("a git call in the worktree failed");
    }
    parse_status(&status.bytes, &mut map);

    // 8. The hidden read: tracked paths carrying the assume-unchanged or skip-worktree bit are invisible to
    //    status, so they get their own query.
    let hidden = git_bytes(&tree, &["ls-files", "-v", "-z"]);
    if hidden.overflow {
        return stop3("too many paths: the tracked-file answer went over the read cap");
    }
    if !hidden.ok {
        return stop3("a git call in the worktree failed");
    }
    parse_hidden(&hidden.bytes, &mut map);

    // 9. Live manifest path entries INSIDE the worktree (strictly inside; the `worktree` entry is the tree
    //    itself). One that is still what the manifest says it is — today: a no-follow stat finds a SYMLINK at
    //    that exact stored path, the P1B-04 node_modules link — is EXPECTED: that one path (never anything
    //    under it, never anything matched through the link) is removed from the strays. One that is gone or no
    //    longer that kind is `missing`. The stored text is compared to the worktree path directly, never
    //    canonicalized first (canonicalizing the node_modules path would follow the link out of the worktree).
    for e in &read.live {
        let Target::Path(p) = &e.target else { continue };
        let Ok(rel) = Path::new(p.as_str()).strip_prefix(&tree) else { continue };
        if rel == Path::new("") {
            continue; // the worktree entry itself
        }
        let rel = rel.as_os_str().as_bytes().to_vec();
        match std::fs::symlink_metadata(p.as_str()) {
            Ok(m) if m.file_type().is_symlink() => {
                map.remove(&rel);
            }
            _ => insert(&mut map, rel, Kind::Missing),
        }
    }

    // 10. The entry cap: more strays than the bound means "cannot check", never a truncated PASS.
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
