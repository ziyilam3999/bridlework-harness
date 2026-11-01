//! Process-level tests for `harness workspace create` (P1B-03). Every check runs the real binary as a child process
//! against scratch repos under the system temp folder, so the exit-code contract (0 created, 1 a ticket refusal at
//! intake, 2 every other refusal), the write-ahead manifest, the git worktree registration and the no-folder-on-the-
//! terminal rule are all exercised end to end. One test per acceptance criterion (AC 2-8); each refusal test also
//! pins that NOTHING was written (the folders stay empty, the repo's worktree list and branches are unchanged).
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// One scratch layout: the RAW temp path (what a caller would type) and its canonical spelling are both kept,
/// because on this platform they differ and neither may ever appear on stdout or stderr.
struct Tmp {
    raw: PathBuf,
    canon: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("wcli-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&raw);
        fs::create_dir_all(&raw).unwrap();
        let canon = raw.canonicalize().unwrap();
        Tmp { raw, canon }
    }

    /// Both spellings of a child path (the raw one is passed on the command line; the canonical one is what git prints).
    fn both(&self, rel: &str) -> (PathBuf, PathBuf) {
        (self.raw.join(rel), self.canon.join(rel))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.raw);
        let _ = fs::remove_dir_all(&self.canon);
    }
}

/// One git call in a scratch repo: no hooks, no inherited repo-redirecting names, no stdin, fixed identity.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A scratch repo with one tracked file and one commit, upkeep off (a background gc would flake before/after snapshots).
fn repo(t: &Tmp, name: &str, msg: &str) -> PathBuf {
    let (d, _) = t.both(name);
    fs::create_dir_all(&d).unwrap();
    git(&d, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        git(&d, &["config", k, v]);
    }
    fs::write(d.join("f.txt"), msg).unwrap();
    git(&d, &["add", "f.txt"]);
    git(&d, &["commit", "-q", "-m", msg]);
    d.canonicalize().unwrap()
}

fn head(d: &Path) -> String {
    git(d, &["rev-parse", "HEAD"])
}

fn worktree_porcelain(r: &Path) -> String {
    git(r, &["worktree", "list", "--porcelain"])
}

fn worktree_count(r: &Path) -> usize {
    worktree_porcelain(r).lines().filter(|l| l.starts_with("worktree ")).count()
}

/// A valid ticket file for `id` naming repo `repo_id` (the strict reader's eleven fields, all legal values).
fn ticket(t: &Tmp, id: &str, repo_id: &str) -> PathBuf {
    let (f, _) = t.both(&format!("{id}.json"));
    fs::write(&f, format!("{{\"id\":\"{id}\",\"repo_id\":\"{repo_id}\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}")).unwrap();
    f
}

fn run(args: &[String], cwd: Option<&Path>) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// The full `workspace create` line for one ticket. `r` and the folders are the RAW spellings.
fn create_args(t: &Tmp, ticket_file: &str, r: &Path, base: &str, ws: &str, md: &str) -> Vec<String> {
    let (f, _) = t.both(ticket_file);
    vec![
        "workspace".to_string(),
        "create".to_string(),
        f.to_str().unwrap().to_string(),
        "--repo-map".to_string(),
        format!("r={}", r.display()),
        "--base".to_string(),
        base.to_string(),
        "--workspace-root".to_string(),
        t.raw.join(ws).to_str().unwrap().to_string(),
        "--manifest-dir".to_string(),
        t.raw.join(md).to_str().unwrap().to_string(),
    ]
}

/// AC 8: neither stdout nor stderr may carry the text of `R`, `W`, `M` (given or canonicalized) or `W/t1`.
fn assert_no_folder(label: &str, o: &Output, t: &Tmp, r: &Path) {
    let text = format!("{}{}", out(o), err(o));
    let (w_raw, w_canon) = t.both("W");
    let (m_raw, m_canon) = t.both("M");
    let (_, c) = t.both("W/t1");
    let probes = [
        r.to_string_lossy().into_owned(),
        r.canonicalize().unwrap().to_string_lossy().into_owned(),
        w_raw.to_string_lossy().into_owned(),
        w_canon.to_string_lossy().into_owned(),
        m_raw.to_string_lossy().into_owned(),
        m_canon.to_string_lossy().into_owned(),
        w_raw.join("t1").to_string_lossy().into_owned(),
        c.to_string_lossy().into_owned(),
    ];
    for probe in probes {
        assert!(!text.contains(&probe), "{label}: output carries a folder text");
    }
}

fn empty(p: &Path) -> bool {
    fs::read_dir(p).map(|d| d.count() == 0).unwrap_or(true)
}

// ---------------------------------------------------------------- AC 2: create makes one listed worktree

#[test]
fn ac2_create_makes_one_listed_worktree() {
    let t = Tmp::new();
    let (r_raw, _) = t.both("R");
    let r = repo(&t, "R", "one");
    let s = head(&r);
    ticket(&t, "t1", "r");
    for d in ["W", "M"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    let args = create_args(&t, "t1.json", &r_raw, &s, "W", "M");
    let o = run(&args, None);
    assert_eq!(code(&o), 0, "create: {}", err(&o));
    // One stdout line of fixed fields, starting with the required prefix.
    let stdout_text = out(&o);
    let lines: Vec<&str> = stdout_text.lines().collect();
    assert_eq!(lines.len(), 1, "create printed more than one line");
    assert!(lines[0].starts_with("workspace=created ticket=t1"), "line: {}", lines[0]);
    assert_eq!(lines[0], format!("workspace=created ticket=t1 repo_id=r base={s} branch=ws/t1"));
    // The worktree is registered in R's own list, on branch ws/t1, at the base sha.
    let (_, tree) = t.both("W/t1/tree");
    let por = worktree_porcelain(&r);
    assert!(por.contains(&format!("worktree {}\nHEAD {}\nbranch refs/heads/ws/t1", tree.display(), s)), "porcelain:\n{}", por);
    // The manifest lists exactly two live entries with the fixed names, and both paths exist.
    let (m, _) = t.both("M");
    let (_, tmp) = t.both("W/t1/tmp");
    let l = run(&["manifest".into(), "list".into(), "t1".into(), "--dir".into(), m.to_str().unwrap().into()], None);
    assert_eq!(code(&l), 0, "manifest list: {}", err(&l));
    let binding = out(&l);
    let listed: Vec<&str> = binding.lines().collect();
    assert_eq!(listed.len(), 2, "manifest list:\n{}", out(&l));
    assert_eq!(listed[0], format!("worktree\tworktree\t{}", tree.display()));
    assert_eq!(listed[1], format!("tmp\ttemp_dir\t{}", tmp.display()));
    assert!(tree.is_dir() && tmp.is_dir(), "a listed path is missing");
    // The ticket root and the scratch folder are owner-only.
    let (root, _) = t.both("W/t1");
    for p in [&root, &tmp] {
        let mode = fs::metadata(p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "folder not 0700");
    }
    assert_no_folder("ac2", &o, &t, &r);
}

// ---------------------------------------------------------------- AC 3: a second workspace is refused

#[test]
fn ac3_second_and_racing_creates_give_exactly_one() {
    let t = Tmp::new();
    let (r_raw, _) = t.both("R");
    let r = repo(&t, "R", "one");
    let s = head(&r);
    for d in ["W", "M"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    ticket(&t, "t1", "r");
    ticket(&t, "t2", "r");
    let first = run(&create_args(&t, "t1.json", &r_raw, &s, "W", "M"), None);
    assert_eq!(code(&first), 0, "first create: {}", err(&first));
    let (m, _) = t.both("M");
    let before_list = out(&run(&["manifest".into(), "list".into(), "t1".into(), "--dir".into(), m.to_str().unwrap().into()], None));
    let before_por = worktree_porcelain(&r);
    let before_file = fs::read(m.join("t1.jsonl")).unwrap();
    // A second workspace for one ticket is refused, changing nothing.
    let second = run(&create_args(&t, "t1.json", &r_raw, &s, "W", "M"), None);
    assert_eq!(code(&second), 2, "second create: {} {}", out(&second), err(&second));
    let after_list = out(&run(&["manifest".into(), "list".into(), "t1".into(), "--dir".into(), m.to_str().unwrap().into()], None));
    assert_eq!(before_list, after_list, "manifest list changed");
    assert_eq!(before_por, worktree_porcelain(&r), "worktree list changed");
    assert_eq!(before_file, fs::read(m.join("t1.jsonl")).unwrap(), "manifest file changed");
    assert_no_folder("ac3-second", &second, &t, &r);
    // Two creates for a fresh ticket started at the same time: exactly one exit 0.
    let args = create_args(&t, "t2.json", &r_raw, &s, "W", "M");
    let a = Command::new(BIN).args(&args).stdin(Stdio::null()).spawn().unwrap();
    let b = Command::new(BIN).args(&args).stdin(Stdio::null()).spawn().unwrap();
    let (ca, cb) = (a.wait_with_output().unwrap(), b.wait_with_output().unwrap());
    let zeros = [code(&ca), code(&cb)].iter().filter(|c| **c == 0).count();
    assert_eq!(zeros, 1, "racing creates gave {} exits 0 ({} / {})", zeros, code(&ca), code(&cb));
    assert_eq!(worktree_count(&r), 3, "exactly one new worktree");
    let l2 = run(&["manifest".into(), "list".into(), "t2".into(), "--dir".into(), m.to_str().unwrap().into()], None);
    assert_eq!(code(&l2), 0, "t2 manifest list: {}", err(&l2));
    assert_eq!(out(&l2).lines().count(), 2, "t2 manifest has more than the two entries");
}

// ---------------------------------------------------------------- AC 4: unknown repo, nothing written

#[test]
fn ac4_unknown_repo_refused_writing_nothing() {
    let t = Tmp::new();
    let (r_raw, _) = t.both("R");
    let r = repo(&t, "R", "one");
    let s = head(&r);
    let (nr, _) = t.both("notrepo");
    fs::create_dir_all(&nr).unwrap();
    for d in ["W", "M"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    // A ticket whose repo id is not in the map.
    let nope = ticket(&t, "tnope", "nope");
    let o = run(
        &["workspace".into(), "create".into(), nope.to_str().unwrap().into(), "--repo-map".into(), format!("r={}", r_raw.display()), "--base".into(), s.clone(), "--workspace-root".into(), t.raw.join("W").to_str().unwrap().into(), "--manifest-dir".into(), t.raw.join("M").to_str().unwrap().into()],
        None,
    );
    assert_eq!(code(&o), 1, "unknown repo id: {} {}", out(&o), err(&o));
    // A map entry pointing at a folder that is not a git repo.
    let nq = ticket(&t, "tnq", "q");
    let o2 = run(
        &["workspace".into(), "create".into(), nq.to_str().unwrap().into(), "--repo-map".into(), format!("q={}", nr.display()), "--base".into(), s, "--workspace-root".into(), t.raw.join("W").to_str().unwrap().into(), "--manifest-dir".into(), t.raw.join("M").to_str().unwrap().into()],
        None,
    );
    assert_eq!(code(&o2), 1, "non-repo map entry: {} {}", out(&o2), err(&o2));
    let (w, _) = t.both("W");
    let (m, _) = t.both("M");
    assert!(empty(&w) && empty(&m), "a refusal wrote something");
    assert_eq!(worktree_count(&r), 1);
    assert_no_folder("ac4", &o, &t, &r);
    assert_no_folder("ac4b", &o2, &t, &r);
}

// ---------------------------------------------------------------- AC 5: outside the fence is refused

#[test]
fn ac5_paths_outside_the_fence_refused() {
    let t = Tmp::new();
    let (r_raw, _) = t.both("R");
    let r = repo(&t, "R", "one");
    let s = head(&r);
    for d in ["W", "M", "W/md", "elsewhere"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    fs::create_dir_all(r_raw.join("sub")).unwrap();
    ticket(&t, "t3", "r");
    let before_por = worktree_porcelain(&r);
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("ws-root inside the repo", vec!["--ws".to_string(), r_raw.join("sub").to_str().unwrap().to_string()]),
        ("ws-root is /", vec!["--ws".to_string(), "/".to_string()]),
        ("relative ws-root", vec!["--rel-ws".to_string()]),
        ("manifest-dir inside W", vec!["--md".to_string(), t.raw.join("W/md").to_str().unwrap().to_string()]),
    ];
    for (label, tweak) in cases {
        let mut args = create_args(&t, "t3.json", &r_raw, &s, "W", "M");
        match tweak[0].as_str() {
            "--ws" => {
                let i = args.iter().position(|a| a == "--workspace-root").unwrap();
                args[i + 1] = tweak[1].clone();
            }
            "--rel-ws" => {
                let i = args.iter().position(|a| a == "--workspace-root").unwrap();
                args[i + 1] = "relws".to_string();
            }
            "--md" => {
                let i = args.iter().position(|a| a == "--manifest-dir").unwrap();
                args[i + 1] = tweak[1].clone();
            }
            _ => unreachable!(),
        }
        let o = run(&args, None);
        assert_eq!(code(&o), 2, "{label}: exit {} stdout {} stderr {}", code(&o), out(&o), err(&o));
        assert_eq!(worktree_porcelain(&r), before_por, "{label}: worktree list changed");
        assert_no_folder(label, &o, &t, &r);
    }
    let (w, _) = t.both("W");
    let (m, _) = t.both("M");
    // Only the probe's own `md` child sits in W; M is untouched; nothing new in the repo.
    let names: Vec<String> = fs::read_dir(&w).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["md".to_string()], "a refusal wrote into W");
    assert!(empty(&m), "a refusal wrote into M");
    // A planted symlink at the ticket's name: the exclusive create fails, nothing is written through the link.
    let (elsewhere, _) = t.both("elsewhere");
    let (link, _) = t.both("W/t3");
    std::os::unix::fs::symlink(&elsewhere, &link).unwrap();
    let o = run(&create_args(&t, "t3.json", &r_raw, &s, "W", "M"), None);
    assert_eq!(code(&o), 2, "symlinked ticket root: exit {}", code(&o));
    assert!(empty(&elsewhere), "something was written through the link");
    assert_eq!(worktree_porcelain(&r), before_por, "symlink case: worktree list changed");
    assert!(fs::read_dir(&m).unwrap().count() == 0, "symlink case: manifest dir gained a file");
    assert_no_folder("ac5-symlink", &o, &t, &r);
}

// ---------------------------------------------------------------- AC 6: other refusals

#[test]
fn ac6_other_refusals_write_nothing() {
    let t = Tmp::new();
    let (r_raw, _) = t.both("R");
    let r = repo(&t, "R", "one");
    let s = head(&r);
    for d in ["W", "M"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    let (w, _) = t.both("W");
    let (m, _) = t.both("M");
    let before_por = worktree_porcelain(&r);
    // A ticket id that is not a plain token is refused, never rewritten.
    let upper = ticket(&t, "T-1", "r");
    let o = run(
        &["workspace".into(), "create".into(), upper.to_str().unwrap().into(), "--repo-map".into(), format!("r={}", r_raw.display()), "--base".into(), s.clone(), "--workspace-root".into(), t.raw.join("W").to_str().unwrap().into(), "--manifest-dir".into(), t.raw.join("M").to_str().unwrap().into()],
        None,
    );
    assert_eq!(code(&o), 2, "upper-case id: {} {}", out(&o), err(&o));
    assert!(empty(&w) && empty(&m), "upper-case id wrote something");
    assert_no_folder("ac6-id", &o, &t, &r);
    ticket(&t, "t3", "r");
    // A --base that is not a full sha; and a full sha that is not in R.
    for bad in ["HEAD".to_string(), format!("{:040x}", 0xdeadbeefu64)] {
        let o = run(&create_args(&t, "t3.json", &r_raw, &bad, "W", "M"), None);
        assert_eq!(code(&o), 2, "base {bad}: {} {}", out(&o), err(&o));
        assert!(empty(&w) && empty(&m), "a bad base wrote something");
        assert_eq!(worktree_porcelain(&r), before_por);
        assert_no_folder("ac6-base", &o, &t, &r);
    }
    // A linked worktree of another repo passed as the mapped repo.
    let p = repo(&t, "P", "parent");
    let (lw, _) = t.both("linked");
    git(&p, &["worktree", "add", "--detach", lw.to_str().unwrap(), &head(&p)]);
    let o = run(
        &["workspace".into(), "create".into(), t.raw.join("t3.json").to_str().unwrap().into(), "--repo-map".into(), format!("r={}", lw.display()), "--base".into(), s, "--workspace-root".into(), t.raw.join("W").to_str().unwrap().into(), "--manifest-dir".into(), t.raw.join("M").to_str().unwrap().into()],
        None,
    );
    assert_eq!(code(&o), 2, "linked worktree: {} {}", out(&o), err(&o));
    assert!(empty(&w) && empty(&m), "linked worktree case wrote something");
    assert_no_folder("ac6-linked", &o, &t, &r);
    // A branch that already exists: refused, left at its old commit, nothing written.
    ticket(&t, "t4", "r");
    git(&r, &["branch", "ws/t4", &head(&r)]);
    let o = run(&create_args(&t, "t4.json", &r_raw, &head(&r), "W", "M"), None);
    assert_eq!(code(&o), 2, "existing branch: {} {}", out(&o), err(&o));
    assert_eq!(git(&r, &["rev-parse", "ws/t4"]), head(&r), "the branch moved");
    assert!(empty(&w) && empty(&m), "existing-branch case wrote something");
    assert_eq!(worktree_porcelain(&r).lines().filter(|l| l.starts_with("worktree ")).count(), 1);
    assert_no_folder("ac6-branch", &o, &t, &r);
}

// ---------------------------------------------------------------- AC 7: the repo comes from the ticket

#[test]
fn ac7_repo_comes_from_the_ticket_not_the_working_folder() {
    let t = Tmp::new();
    let (r_raw, _) = t.both("R");
    let r = repo(&t, "R", "one");
    let r2 = repo(&t, "R2", "two");
    let s = head(&r);
    for d in ["W", "M"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    ticket(&t, "t1", "r");
    // Run from INSIDE R2 with the map naming R: the worktree lands in R's list, never R2's.
    let o = run(&create_args(&t, "t1.json", &r_raw, &s, "W", "M"), Some(&r2));
    assert_eq!(code(&o), 0, "create from another repo's folder: {}", err(&o));
    let (_, tree) = t.both("W/t1/tree");
    assert!(worktree_porcelain(&r).contains(&format!("worktree {}", tree.display())), "R's list lacks the worktree");
    assert_eq!(worktree_count(&r2), 1, "R2 gained a worktree");
    assert_no_folder("ac7", &o, &t, &r);
}

// ---------------------------------------------------------------- AC 8: no folder is printed

#[test]
fn ac8_no_folder_on_stdout_or_stderr() {
    let t = Tmp::new();
    let (r_raw, _) = t.both("R");
    let r = repo(&t, "R", "one");
    let s = head(&r);
    for d in ["W", "M"] {
        let (p, _) = t.both(d);
        fs::create_dir_all(&p).unwrap();
    }
    ticket(&t, "t1", "r");
    ticket(&t, "tnope", "nope");
    ticket(&t, "T-1x", "r");
    let (w_raw, _) = t.both("W");
    fs::create_dir_all(r_raw.join("inside")).unwrap();
    // A successful create; a second one; an unknown repo; a non-token id; a bad base; a mis-placed root.
    let flows: Vec<(&str, Vec<String>)> = vec![
        ("ok", create_args(&t, "t1.json", &r_raw, &s, "W", "M")),
        ("again", create_args(&t, "t1.json", &r_raw, &s, "W", "M")),
        ("unknown-repo", vec!["workspace".into(), "create".into(), t.raw.join("tnope.json").to_str().unwrap().into(), "--repo-map".into(), format!("r={}", r_raw.display()), "--base".into(), s.clone(), "--workspace-root".into(), w_raw.to_str().unwrap().into(), "--manifest-dir".into(), t.raw.join("M").to_str().unwrap().into()]),
        ("non-token-id", create_args(&t, "T-1x.json", &r_raw, &s, "W", "M")),
        ("bad-base", create_args(&t, "t1.json", &r_raw, "HEAD", "W", "M")),
        ("bad-root", {
            let mut a = create_args(&t, "t1.json", &r_raw, &s, "W", "M");
            let i = a.iter().position(|x| x == "--workspace-root").unwrap();
            a[i + 1] = r_raw.join("inside").to_str().unwrap().to_string();
            a
        }),
        ("no-args", vec!["workspace".into(), "create".into()]),
    ];
    for (label, args) in flows {
        let o = run(&args, None);
        assert_no_folder(label, &o, &t, &r);
    }
}
