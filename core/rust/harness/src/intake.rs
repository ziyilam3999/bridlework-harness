//! Ticket intake logic (std only, read-only). A ticket names its repo by id; this turns the id into a verified repo and its head commit.
//! The repo comes from the ticket and the repo map only, never from the folder the process runs in. Nothing here writes anything.
//! No error string carries a folder path: a refusal names the repo id at most, so a later log cannot leak a private path.

use crate::repo_id::RepoId;
use crate::ticket::Ticket;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The stand-in repo map: repo id to an absolute folder. Built entry by entry from `id=dir` text.
#[derive(Debug, Default, Clone)]
pub struct RepoMap {
    entries: Vec<(RepoId, PathBuf)>,
}

impl RepoMap {
    pub fn new() -> RepoMap {
        RepoMap::default()
    }

    /// Add one `id=dir` entry. A usage error if there is no `=`, the id is not a valid repo id, the dir is empty or relative, or the id is already present.
    /// The message never repeats the entry, so a bad dir cannot leak through it.
    pub fn add(&mut self, spec: &str) -> Result<(), String> {
        let (id_text, dir_text) = spec.split_once('=').ok_or_else(|| "repo map entry must look like id=dir".to_string())?;
        let id = RepoId::parse(id_text).map_err(|_| "repo map entry has a malformed repo id".to_string())?;
        if dir_text.is_empty() {
            return Err("repo map entry has an empty folder".to_string());
        }
        let dir = PathBuf::from(dir_text);
        if !dir.is_absolute() {
            return Err("repo map entry folder must be an absolute path".to_string());
        }
        if self.lookup(&id).is_some() {
            return Err(format!("repo map lists repo id `{}` more than once", id.as_str()));
        }
        self.entries.push((id, dir));
        Ok(())
    }

    /// The one lookup: the folder mapped to this id, if any. The current folder is never consulted.
    pub fn lookup(&self, id: &RepoId) -> Option<&Path> {
        self.entries.iter().find(|(k, _)| k == id).map(|(_, d)| d.as_path())
    }
}

/// An accepted ticket: its id, the repo id and the repo's head commit sha.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accepted {
    pub ticket_id: String,
    pub repo_id: RepoId,
    pub head_sha: String,
}

/// Why intake refused a ticket. Each variant names the repo id only, never a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    UnknownRepo(String),
    RepoNotFound(String),
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refusal::UnknownRepo(id) => write!(f, "repo id `{id}` is not in the repo map"),
            Refusal::RepoNotFound(id) => write!(f, "repo not found for repo id `{id}`"),
        }
    }
}

/// One git call: no hooks, no inherited `GIT_*` name (so an inherited `GIT_DIR` cannot redirect the check), no stdin.
fn git_out(dir: &Path, args: &[&str]) -> Option<String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null"]).args(args);
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

/// The head commit sha of the repo whose top-level folder is exactly `dir`. None if `dir` is missing, not a folder, not a git work tree,
/// only a subfolder inside one, or has no commit yet.
fn head_sha(dir: &Path) -> Option<String> {
    let canon = dir.canonicalize().ok()?;
    if !canon.is_dir() {
        return None;
    }
    let top = git_out(&canon, &["rev-parse", "--show-toplevel"])?;
    if Path::new(&top).canonicalize().ok()? != canon {
        return None;
    }
    let sha = git_out(&canon, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])?;
    let hex = sha.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if (sha.len() == 40 || sha.len() == 64) && hex {
        Some(sha)
    } else {
        None
    }
}

/// Settle which repo a parsed ticket is about. Read-only; refuses rather than guesses.
pub fn intake(ticket: &Ticket, map: &RepoMap) -> Result<Accepted, Refusal> {
    let id = ticket.repo_id.as_str().to_string();
    let dir = map.lookup(&ticket.repo_id).ok_or_else(|| Refusal::UnknownRepo(id.clone()))?;
    let head_sha = head_sha(dir).ok_or(Refusal::RepoNotFound(id))?;
    Ok(Accepted { ticket_id: ticket.id.clone(), repo_id: ticket.repo_id.clone(), head_sha })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);

    /// A fresh scratch folder under the system temp dir, named by pid and a counter.
    fn scratch(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("intake-{}-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst), tag));
        std::fs::create_dir_all(&p).unwrap();
        p.canonicalize().unwrap()
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let mut c = Command::new("git");
        let contact = format!("user.email=t{}scratch", '@');
        c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
        for (k, _) in std::env::vars_os() {
            if k.to_string_lossy().starts_with("GIT_") {
                c.env_remove(k);
            }
        }
        let out = c.stdin(Stdio::null()).output().unwrap();
        assert!(out.status.success(), "scratch git call failed");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A scratch repo with one commit whose message is `msg`, so two repos get different head shas.
    fn repo(tag: &str, msg: &str) -> PathBuf {
        let d = scratch(tag);
        git(&d, &["init", "-q"]);
        git(&d, &["commit", "-q", "--allow-empty", "-m", msg]);
        d
    }

    fn ticket(repo_id: &str) -> Ticket {
        Ticket {
            id: "T-1".to_string(),
            repo_id: RepoId::parse(repo_id).unwrap(),
            title: "t".to_string(),
            group: "g".to_string(),
            size: "S".to_string(),
            objective: "o".to_string(),
            acceptance_criteria: vec!["a".to_string()],
            dependencies: vec![],
            risk: "normal".to_string(),
            owner_role: "execution_verification".to_string(),
            evidence_links: vec![],
        }
    }

    fn map_of(entries: &[(&str, &Path)]) -> RepoMap {
        let mut m = RepoMap::new();
        for (id, d) in entries {
            m.add(&format!("{id}={}", d.display())).unwrap();
        }
        m
    }

    /// True if `msg` repeats the folder: the AC 10 assertion, planted below against a known leak.
    fn leaks(msg: &str, dir: &Path) -> bool {
        msg.contains(dir.to_string_lossy().as_ref())
    }

    // AC 10 twin: the leak check really does catch an error string that echoes the folder.
    #[test]
    fn the_leak_check_catches_a_message_that_echoes_the_folder() {
        let d = scratch("leakcheck");
        assert!(leaks(&format!("repo not found at {}", d.display()), &d), "a leaking message must be caught");
        assert!(!leaks("repo not found for repo id `b`", &d), "a clean message passes");
    }

    // AC 1 (function level): the ticket's repo is used, and the answer carries that repo's head, not another repo's.
    #[test]
    fn intake_reports_the_head_of_the_ticket_repo_not_another_repo() {
        let a = repo("a", "commit in a");
        let b = repo("b", "commit in b");
        let (sha_a, sha_b) = (git(&a, &["rev-parse", "HEAD"]), git(&b, &["rev-parse", "HEAD"]));
        assert_ne!(sha_a, sha_b, "the two scratch repos must differ");
        let got = intake(&ticket("b"), &map_of(&[("a", &a), ("b", &b)])).expect("b is mapped and real");
        assert_eq!(got.head_sha, sha_b, "the head sha is b's");
        assert_ne!(got.head_sha, sha_a, "the head sha is not a's");
        assert_eq!((got.ticket_id.as_str(), got.repo_id.as_str()), ("T-1", "b"));
    }

    // AC 3 (function level): an id missing from the map is refused by name; adding it flips the result; a same-named folder is not a map.
    #[test]
    fn an_unknown_repo_id_is_refused_and_the_same_id_mapped_is_accepted() {
        let b = repo("b", "in b");
        let c = repo("c", "in c");
        let refused = intake(&ticket("c"), &map_of(&[("b", &b)])).expect_err("c is not mapped");
        assert_eq!(refused, Refusal::UnknownRepo("c".to_string()));
        assert!(refused.to_string().contains('c'), "the refusal names the id");
        assert!(intake(&ticket("c"), &map_of(&[("b", &b), ("c", &c)])).is_ok(), "twin A: mapped, accepted");
        // Twin B: a real repo whose folder is named like the id exists, but the map still lacks it.
        let parent = scratch("named");
        let named = parent.join("c");
        std::fs::create_dir_all(&named).unwrap();
        git(&named, &["init", "-q"]);
        git(&named, &["commit", "-q", "--allow-empty", "-m", "x"]);
        assert_eq!(intake(&ticket("c"), &map_of(&[("b", &b)])), Err(Refusal::UnknownRepo("c".to_string())), "a folder name is not a map");
    }

    // AC 5 and AC 10 (function level): missing dir, plain dir, subfolder of a repo are refused without leaking the folder; the top level is accepted.
    #[test]
    fn a_folder_that_is_not_the_top_of_a_repo_is_refused_without_naming_it() {
        let b = repo("b", "in b");
        let sub = b.join("subdir");
        std::fs::create_dir_all(&sub).unwrap();
        let plain = scratch("plain");
        let missing = scratch("gone").join("nothing-here");
        let file_parent = scratch("file");
        let a_file = file_parent.join("f.txt");
        std::fs::write(&a_file, "x").unwrap();
        for (label, dir) in [("missing", &missing), ("plain", &plain), ("subfolder", &sub), ("file", &a_file)] {
            let err = intake(&ticket("b"), &map_of(&[("b", dir)])).expect_err(&format!("{label} must be refused"));
            assert_eq!(err, Refusal::RepoNotFound("b".to_string()), "{label}");
            let text = err.to_string();
            assert!(text.contains('b'), "{label}: names the id");
            assert!(!leaks(&text, dir), "{label}: the refusal must not echo the folder: {text}");
        }
        assert!(intake(&ticket("b"), &map_of(&[("b", &b)])).is_ok(), "twin: the top level of the repo is accepted");
    }

    // AC 5: a repo with no commit yet has no head to report, so it is refused rather than given an empty sha.
    #[test]
    fn a_repo_with_no_commit_is_refused() {
        let d = scratch("empty");
        git(&d, &["init", "-q"]);
        assert_eq!(intake(&ticket("b"), &map_of(&[("b", &d)])), Err(Refusal::RepoNotFound("b".to_string())));
        git(&d, &["commit", "-q", "--allow-empty", "-m", "first"]);
        assert!(intake(&ticket("b"), &map_of(&[("b", &d)])).is_ok(), "twin: after a commit it is accepted");
    }

    // AC 7 (function level) and the map rules: each bad entry is a usage error that does not echo the folder; the valid entry is accepted.
    #[test]
    fn bad_map_entries_are_usage_errors_and_never_echo_the_folder() {
        let marker = "/marker-dir-xyz";
        let long_id = format!("Bad{marker}");
        let bad: Vec<(&str, String)> = vec![
            ("no equals sign", format!("b{marker}")),
            ("relative folder", "b=relative/marker-dir-xyz".to_string()),
            ("dot-relative folder", "b=./marker-dir-xyz".to_string()),
            ("empty folder", "b=".to_string()),
            ("capital id", format!("Repo={marker}")),
            ("path as id", format!("../b={marker}")),
            ("empty id", format!("={marker}")),
            ("id with slash", long_id.clone() + "=" + marker),
        ];
        for (label, spec) in &bad {
            let err = RepoMap::new().add(spec).expect_err(&format!("{label} must be refused"));
            assert!(!err.contains("marker-dir-xyz"), "{label}: the message must not echo the folder: {err}");
        }
        assert!(RepoMap::new().add(&format!("b={marker}")).is_ok(), "twin: the valid entry is accepted");
    }

    // AC 7 (function level): the same id twice is ambiguous and refused; a different id is fine.
    #[test]
    fn the_same_repo_id_twice_is_refused() {
        let mut m = RepoMap::new();
        m.add("b=/one-dir").unwrap();
        let err = m.add("b=/two-dir").expect_err("duplicate id");
        assert!(err.contains('b') && !err.contains("two-dir") && !err.contains("one-dir"), "names the id, not a folder: {err}");
        assert!(m.add("c=/two-dir").is_ok(), "twin: a different id is accepted");
        assert_eq!(m.lookup(&RepoId::parse("b").unwrap()), Some(Path::new("/one-dir")), "the first entry is kept");
    }
}
