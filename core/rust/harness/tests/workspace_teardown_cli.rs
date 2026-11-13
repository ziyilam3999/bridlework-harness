//! `harness workspace teardown` (P1B-14): one test per acceptance criterion. Scratch repos live under a temp dir;
//! helpers are local because each integration test file is its own crate (the `workspace_cli.rs` fixtures, reduced).
//! Every spawned sleeper is killed by a guard on drop, so a failed run leaves nothing behind.

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp {
    p: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("wtd-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&raw);
        fs::create_dir_all(&raw).unwrap();
        Tmp { p: raw.canonicalize().unwrap() }
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.p);
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let o = c.stdin(Stdio::null()).output().unwrap();
    assert!(o.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
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

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn free_port() -> u64 {
    TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port() as u64
}

/// Kills a process group on drop.
struct Reap(u32);
impl Drop for Reap {
    fn drop(&mut self) {
        if self.0 != 0 {
            let _ = Command::new("kill").args(["-KILL", "--", &format!("-{}", self.0)]).stderr(Stdio::null()).status();
        }
    }
}

/// One ticket's whole layout: a primary repo (with a `node_modules`), the workspace root, the manifest dir, a log.
struct Fx {
    t: Tmp,
    repo: PathBuf,
    ws: PathBuf,
    md: PathBuf,
    log: PathBuf,
    ticket: &'static str,
}

impl Fx {
    fn new(with_nm: bool) -> Fx {
        let t = Tmp::new();
        let repo = t.p.join("R");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&repo, &["config", k, v]);
        }
        fs::write(repo.join("f.txt"), "one").unwrap();
        git(&repo, &["add", "f.txt"]);
        git(&repo, &["commit", "-q", "-m", "one"]);
        if with_nm {
            fs::create_dir_all(repo.join("node_modules").join("pkg")).unwrap();
            fs::write(repo.join("node_modules").join("pkg").join("f.txt"), "pkg").unwrap();
        }
        let ws = t.p.join("W");
        let md = t.p.join("M");
        let logd = t.p.join("L");
        for d in [&ws, &md, &logd] {
            fs::create_dir_all(d).unwrap();
        }
        let repo = repo.canonicalize().unwrap();
        let fx = Fx { log: logd.join("events.jsonl"), t, repo, ws, md, ticket: "t1" };
        let base = git(&fx.repo, &["rev-parse", "HEAD"]);
        let tf = fx.t.p.join("t1.json");
        fs::write(&tf, "{\"id\":\"t1\",\"repo_id\":\"r\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}").unwrap();
        let o = run(&[
            "workspace", "create", tf.to_str().unwrap(), "--repo-map", &format!("r={}", fx.repo.display()), "--base", &base,
            "--workspace-root", fx.ws.to_str().unwrap(), "--manifest-dir", fx.md.to_str().unwrap(),
        ]);
        assert_eq!(code(&o), 0, "create: {}", err(&o));
        fx
    }

    fn root(&self) -> PathBuf {
        self.ws.join(self.ticket).canonicalize().unwrap()
    }
    fn tree(&self) -> PathBuf {
        self.root().join("tree")
    }
    fn tmp(&self) -> PathBuf {
        self.root().join("tmp")
    }

    fn teardown_with(&self, log: &Path) -> Output {
        run(&[
            "workspace", "teardown", self.ticket, "--manifest-dir", self.md.to_str().unwrap(), "--repo-dir", self.repo.to_str().unwrap(),
            "--log", log.to_str().unwrap(), "--repo", "r", "--grace-ms", "3000",
        ])
    }
    fn teardown(&self) -> Output {
        self.teardown_with(&self.log)
    }

    fn list(&self) -> Vec<String> {
        let o = run(&["manifest", "list", self.ticket, "--dir", self.md.to_str().unwrap()]);
        assert_eq!(code(&o), 0, "list: {}", err(&o));
        out(&o).lines().map(|s| s.to_string()).collect()
    }

    fn start(&self, id: &str, port: Option<u64>) -> (u32, Reap) {
        let mut v: Vec<String> = ["process", "start", self.ticket, "--id", id, "--cwd"].iter().map(|s| s.to_string()).collect();
        v.push(self.tmp().to_str().unwrap().to_string());
        if let Some(p) = port {
            v.push("--port".into());
            v.push(p.to_string());
        }
        v.extend(["--dir".to_string(), self.md.to_str().unwrap().to_string(), "--".into(), "/bin/sleep".into(), "300".into()]);
        let a: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
        let o = run(&a);
        assert_eq!(code(&o), 0, "start: {}", err(&o));
        let pid = out(&o).split('\t').nth(1).and_then(|s| s.parse::<u32>().ok()).expect("pid");
        (pid, Reap(pid))
    }

    fn add(&self, args: &[&str]) {
        let mut v = vec!["manifest", "add", self.ticket, "--dir", self.md.to_str().unwrap()];
        v.extend_from_slice(args);
        let o = run(&v);
        assert_eq!(code(&o), 0, "add {:?}: {}", args, err(&o));
    }

    fn log_lines(&self) -> Vec<String> {
        fs::read_to_string(&self.log).map(|s| s.lines().map(|l| l.to_string()).collect()).unwrap_or_default()
    }
    fn cleanup_lines(&self) -> Vec<String> {
        self.log_lines().into_iter().filter(|l| l.contains("cleanup_result")).collect()
    }
    fn worktrees(&self) -> String {
        git(&self.repo, &["worktree", "list", "--porcelain"])
    }
}

fn has_cleanup(l: &str, result: &str, left: u64) -> bool {
    l.contains(&format!("\"result\":\"{}\"", result)) && l.contains(&format!("\"manifest_left\":{}", left))
}

/// AC1: all four steps.
#[test]
fn teardown_happy_path() {
    let fx = Fx::new(true);
    let port = free_port();
    let (pid, _g) = fx.start("p1", Some(port));
    assert!(alive(pid));
    let o = fx.teardown();
    assert_eq!(code(&o), 0, "stderr {}", err(&o));
    assert_eq!(out(&o).trim(), "teardown=pass ticket=t1 manifest_left=0");
    assert!(fx.list().is_empty(), "manifest not empty: {:?}", fx.list());
    assert!(!fx.worktrees().contains("/tree"), "tree still listed:\n{}", fx.worktrees());
    assert!(!fx.root().join("tree").exists());
    assert!(!alive(pid), "process still alive");
    assert!(TcpListener::bind(("127.0.0.1", port as u16)).is_ok(), "port still bound");
    let cl = fx.cleanup_lines();
    assert_eq!(cl.len(), 1, "{:?}", cl);
    assert!(has_cleanup(&cl[0], "pass", 0), "{}", cl[0]);
    assert!(fx.repo.join("node_modules").join("pkg").join("f.txt").is_file(), "primary node_modules damaged");
    assert!(git(&fx.repo, &["branch", "--list", "ws/t1"]).contains("ws/t1"), "branch gone");
    let ev = run(&["evidence", "show", "t1", "--manifest-dir", fx.md.to_str().unwrap()]);
    assert!(out(&ev).contains("p1"), "evidence lost: {} {}", out(&ev), err(&ev));
    assert!(!out(&o).contains(fx.t.p.to_str().unwrap()) && !err(&o).contains(fx.t.p.to_str().unwrap()), "a folder was printed");
}

/// AC2: untracked work is never forced.
#[test]
fn teardown_dirty_tree_refused() {
    let fx = Fx::new(true);
    let (pid, _g) = fx.start("p1", None);
    fs::write(fx.tree().join("work.txt"), "mine").unwrap();
    let o = fx.teardown();
    assert_eq!(code(&o), 1, "stderr {}", err(&o));
    assert_eq!(out(&o).trim(), "teardown=fail ticket=t1 manifest_left=1");
    assert!(fx.tree().join("work.txt").is_file());
    assert!(fx.list().iter().any(|l| l.starts_with("worktree\tworktree\t")));
    assert_eq!(fx.list().len(), 1, "{:?}", fx.list());
    assert!(!alive(pid));
    assert!(!fx.tmp().exists(), "tmp not deleted");
    let cl = fx.cleanup_lines();
    assert!(cl.len() == 1 && has_cleanup(&cl[0], "fail", 1), "{:?}", cl);
}

/// AC3: a stop failure skips steps 5 and 7.
#[test]
fn teardown_stop_failure_gates_deletes() {
    let fx = Fx::new(true);
    // This test's own pid: live, but not a group leader of its own group in the stop sense (stop refuses, exit 2).
    let mut sleeper = Command::new("/bin/sleep").arg("300").stdin(Stdio::null()).spawn().unwrap();
    let pid = sleeper.id().to_string();
    fx.add(&["--class", "process", "--id", "hand", "--pid", &pid]);
    let o = fx.teardown();
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    assert_eq!(code(&o), 1, "stderr {}", err(&o));
    assert!(fx.tmp().is_dir(), "tmp deleted despite a failed stop");
    assert!(fx.tree().is_dir(), "tree removed despite a failed stop");
    let live = fx.list().len() as u64;
    assert_eq!(live, 4, "{:?}", fx.list());
    let cl = fx.cleanup_lines();
    assert!(cl.len() == 1 && has_cleanup(&cl[0], "fail", live), "{:?}", cl);
}

/// AC4: a still-bound listed port is not released.
#[test]
fn teardown_bound_port_kept() {
    let fx = Fx::new(true);
    let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = held.local_addr().unwrap().port().to_string();
    fx.add(&["--class", "port", "--id", "hp", "--port", &port]);
    let o = fx.teardown();
    assert_eq!(code(&o), 1, "stderr {}", err(&o));
    assert!(fx.list().iter().any(|l| l.starts_with("hp\tport\t")), "{:?}", fx.list());
    assert!(fx.tmp().is_dir() && fx.tree().is_dir(), "steps 5/7 ran");
    let cl = fx.cleanup_lines();
    assert!(cl.len() == 1 && cl[0].contains("\"result\":\"fail\""), "{:?}", cl);
    drop(held);
    // Once the holder is gone the port is released and the rest follows.
    let o2 = fx.teardown();
    assert_eq!(code(&o2), 0, "stderr {}", err(&o2));
}

/// AC5: a damaged manifest is refused before anything.
#[test]
fn teardown_damaged_manifest_refused() {
    let fx = Fx::new(true);
    let mf = fs::read_dir(&fx.md).unwrap().next().unwrap().unwrap().path();
    let mut b = fs::read(&mf).unwrap();
    b.extend_from_slice(b"{\"torn");
    fs::write(&mf, &b).unwrap();
    let o = fx.teardown();
    assert_eq!(code(&o), 2, "stderr {}", err(&o));
    assert_eq!(fs::read(&mf).unwrap(), b, "manifest changed");
    assert!(fx.tree().is_dir() && fx.tmp().is_dir());
    assert!(fx.cleanup_lines().is_empty());
    assert!(!fx.log.exists(), "a log was created");
}

/// AC6: a log inside a root is refused.
#[test]
fn teardown_log_inside_root_refused() {
    let fx = Fx::new(true);
    let before = fx.list();
    let bad = fx.root().join("x.jsonl");
    let o = fx.teardown_with(&bad);
    assert_eq!(code(&o), 2, "stderr {}", err(&o));
    assert!(!bad.exists(), "log created");
    assert!(fx.tree().is_dir() && fx.tmp().is_dir());
    assert_eq!(fx.list(), before);
}

/// AC7: idempotent re-run, and an out-of-band worktree removal.
#[test]
fn teardown_rerun_and_absent_worktree() {
    let fx = Fx::new(true);
    let o = fx.teardown();
    assert_eq!(code(&o), 0, "stderr {}", err(&o));
    let o2 = fx.teardown();
    assert_eq!(code(&o2), 0, "stderr {}", err(&o2));
    assert_eq!(fx.cleanup_lines().len(), 2);
    assert!(fx.cleanup_lines().iter().all(|l| has_cleanup(l, "pass", 0)));

    let fx = Fx::new(true);
    // Unlink the node_modules link first (git refuses an untracked link), then remove the tree out-of-band.
    fs::remove_file(fx.tree().join("node_modules")).unwrap();
    git(&fx.repo, &["worktree", "remove", fx.tree().to_str().unwrap()]);
    let o3 = fx.teardown();
    assert_eq!(code(&o3), 0, "stderr {}", err(&o3));
    assert!(fx.list().is_empty(), "{:?}", fx.list());
    let cl = fx.cleanup_lines();
    assert!(cl.len() == 1 && has_cleanup(&cl[0], "pass", 0), "{:?}", cl);
}

/// AC8: a `log` entry forces fail but does not block the rest.
#[test]
fn teardown_log_entry_left() {
    let fx = Fx::new(true);
    let lp = fx.root().join("some.log");
    fs::write(&lp, "x").unwrap();
    fx.add(&["--class", "log", "--id", "lg", "--path", lp.to_str().unwrap()]);
    let o = fx.teardown();
    assert_eq!(code(&o), 1, "stderr {}", err(&o));
    assert_eq!(out(&o).trim(), "teardown=fail ticket=t1 manifest_left=1");
    assert!(err(&o).contains("1 log entries left"), "{}", err(&o));
    assert!(!fx.root().join("tree").exists() && !fx.tmp().exists());
    assert!(lp.is_file(), "a log file was deleted");
    let cl = fx.cleanup_lines();
    assert!(cl.len() == 1 && has_cleanup(&cl[0], "fail", 1), "{:?}", cl);
}
