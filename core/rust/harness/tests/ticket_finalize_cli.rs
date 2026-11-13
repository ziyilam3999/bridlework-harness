//! `harness ticket finalize` (P1B-15): one test per acceptance criterion. Scratch repos and a scratch bare remote live under a temp dir;
//! helpers are local because each integration test file is its own crate (the `ticket_gate.rs` and `workspace_teardown_cli.rs` fixtures, reduced).
//! Every spawned process is killed by a guard on drop, so a failed run leaves nothing behind.

use std::fs;
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp {
    p: PathBuf,
}

impl Tmp {
    fn new() -> Tmp {
        let raw = std::env::temp_dir().join(format!("fin-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    // `evidence check` scans what it prints and refuses without a privacy list: a one-entry list that never matches.
    c.env("HARNESS_PRIVACY_LIST_SECRET", "zz-never-in-output-zz");
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
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

fn alive(pid: u32) -> bool {
    Command::new("kill").arg("-0").arg(pid.to_string()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
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

fn put(path: &Path, body: &str, exec: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
}

fn gate_json(id: &str, program: &str) -> String {
    format!("{{\"id\":\"{id}\",\"program\":\"{program}\",\"args\":[],\"timeout_ms\":20000,\"max_output_bytes\":4096,\"both_ends\":false}}")
}

fn ticket_json() -> String {
    "{\"id\":\"t1\",\"repo_id\":\"r\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}".to_string()
}

/// One ticket's whole layout: a primary repo with a gate, a bare remote, workspace root, manifest dir, log dir, result and caps dirs, a shelf.
struct Fx {
    t: Tmp,
    repo: PathBuf,
    ws: PathBuf,
    md: PathBuf,
    log: PathBuf,
    res: PathBuf,
    caps: PathBuf,
    shelf: PathBuf,
    tf: PathBuf,
    base: String,
}

const T: &str = "t1";

impl Fx {
    /// create → commit a change on `ws/t1` in the tree → push to the bare remote. The gate has not run yet.
    fn new() -> Fx {
        let t = Tmp::new();
        let repo = t.p.join("R");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&repo, &["config", k, v]);
        }
        put(&repo.join("gates/g.json"), &gate_json("g", "ci/g.sh"), false);
        put(&repo.join("gates/path-map.json"), r#"{"src":["g"]}"#, false);
        put(&repo.join("ci/g.sh"), "#!/bin/sh\n[ \"$(cat src/value.txt)\" != bad ]\n", true);
        put(&repo.join("src/value.txt"), "ok0", false);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c0"]);
        let remote = t.p.join("B");
        fs::create_dir_all(&remote).unwrap();
        git(&remote, &["init", "-q", "--bare"]);
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&remote, &["config", k, v]);
        }
        git(&repo, &["remote", "add", "origin", remote.to_str().unwrap()]);
        let (ws, md, logd, res, caps, shelf) = (t.p.join("W"), t.p.join("M"), t.p.join("L"), t.p.join("res"), t.p.join("caps"), t.p.join("Q"));
        for d in [&ws, &md, &logd, &res, &caps, &shelf] {
            fs::create_dir_all(d).unwrap();
        }
        let tf = t.p.join("t1.json");
        fs::write(&tf, ticket_json()).unwrap();
        let repo = repo.canonicalize().unwrap();
        let base = git(&repo, &["rev-parse", "HEAD"]);
        let fx = Fx {
            log: logd.join("events.jsonl"),
            ws: ws.canonicalize().unwrap(),
            md: md.canonicalize().unwrap(),
            res: res.canonicalize().unwrap(),
            caps: caps.canonicalize().unwrap(),
            shelf: shelf.canonicalize().unwrap(),
            t,
            repo,
            tf,
            base,
        };
        let o = run(&[
            "workspace", "create", fx.tf.to_str().unwrap(), "--repo-map", &format!("r={}", fx.repo.display()), "--base", &fx.base,
            "--workspace-root", fx.ws.to_str().unwrap(), "--manifest-dir", fx.md.to_str().unwrap(),
        ]);
        assert_eq!(code(&o), 0, "create: {}", err(&o));
        fx.commit("ok1");
        git(&fx.tree(), &["push", "-q", "-u", "origin", "ws/t1"]);
        fx
    }

    fn root(&self) -> PathBuf {
        self.ws.join(T).canonicalize().unwrap()
    }
    fn tree(&self) -> PathBuf {
        self.root().join("tree")
    }
    fn tmp(&self) -> PathBuf {
        self.root().join("tmp")
    }
    fn head(&self) -> String {
        git(&self.tree(), &["rev-parse", "HEAD"])
    }

    /// A commit on `ws/t1` in the tree (not pushed).
    fn commit(&self, value: &str) {
        put(&self.tree().join("src/value.txt"), value, false);
        git(&self.tree(), &["add", "-A"]);
        git(&self.tree(), &["commit", "-q", "-m", value]);
    }

    fn push(&self) {
        git(&self.tree(), &["push", "-q", "origin", "ws/t1"]);
    }

    fn gate(&self) -> Output {
        run(&[
            "ticket", "gate", self.tf.to_str().unwrap(), "--repo-map", &format!("r={}", self.tree().display()), "--base", &self.base,
            "--result-dir", self.res.to_str().unwrap(), "--caps-dir", self.caps.to_str().unwrap(),
        ])
    }

    fn gate_pass(&self) {
        let o = self.gate();
        assert_eq!(code(&o), 0, "gate: {} {}", out(&o), err(&o));
    }

    fn finalize_with(&self, drop_flag: Option<&str>, log: &Path, md: &str) -> Output {
        let (ws, shelf, res, caps, repo) = (self.ws.to_str().unwrap(), self.shelf.to_str().unwrap(), self.res.to_str().unwrap(), self.caps.to_str().unwrap(), self.repo.to_str().unwrap());
        let mut v: Vec<(&str, &str)> = vec![
            ("--manifest-dir", md), ("--repo-dir", repo), ("--workspace-root", ws), ("--quarantine-dir", shelf), ("--result-dir", res),
            ("--caps-dir", caps), ("--log", log.to_str().unwrap()), ("--role", "executor"), ("--model", "m1"), ("--grace-ms", "3000"),
        ];
        v.retain(|(k, _)| Some(*k) != drop_flag);
        let mut a: Vec<&str> = vec!["ticket", "finalize", self.tf.to_str().unwrap()];
        for (k, x) in &v {
            a.push(k);
            a.push(x);
        }
        run(&a)
    }

    fn finalize(&self) -> Output {
        self.finalize_with(None, &self.log, self.md.to_str().unwrap())
    }

    fn list(&self) -> Vec<String> {
        let o = run(&["manifest", "list", T, "--dir", self.md.to_str().unwrap()]);
        assert_eq!(code(&o), 0, "list: {}", err(&o));
        out(&o).lines().map(|s| s.to_string()).collect()
    }

    fn start(&self, port: Option<u64>, prog: &[&str]) -> u32 {
        let mut v: Vec<String> = ["process", "start", T, "--id", "srv", "--cwd"].iter().map(|s| s.to_string()).collect();
        v.push(self.tree().to_str().unwrap().to_string());
        if let Some(p) = port {
            v.push("--port".into());
            v.push(p.to_string());
        }
        v.extend(["--dir".to_string(), self.md.to_str().unwrap().to_string(), "--".into()]);
        v.extend(prog.iter().map(|s| s.to_string()));
        let a: Vec<&str> = v.iter().map(|s| s.as_str()).collect();
        let o = run(&a);
        assert_eq!(code(&o), 0, "start: {}", err(&o));
        out(&o).split('\t').nth(1).and_then(|s| s.parse::<u32>().ok()).expect("pid")
    }

    fn evidence_check(&self) -> Output {
        run(&["evidence", "check", T, "--manifest-dir", self.md.to_str().unwrap()])
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

    fn listed(&self) -> bool {
        self.worktrees().contains("/tree")
    }

    fn leak_free(&self, o: &Output) -> bool {
        let p = self.t.p.to_str().unwrap();
        !out(o).contains(p) && !err(o).contains(p)
    }
}

fn has_cleanup(l: &str, result: &str, left: u64) -> bool {
    l.contains(&format!("\"result\":\"{}\"", result)) && l.contains(&format!("\"manifest_left\":{}", left))
}

fn refused(check: &str, reason: &str) -> String {
    format!("finalize=refused ticket=t1 check={check} reason={reason}")
}

/// Nothing destructive ran: tree listed and on disk, worktree entry live, no cleanup result.
fn untouched(fx: &Fx) {
    assert!(fx.listed(), "tree no longer listed:\n{}", fx.worktrees());
    assert!(fx.tree().is_dir());
    assert!(fx.list().iter().any(|l| l.starts_with("worktree\tworktree\t")), "{:?}", fx.list());
    assert!(fx.cleanup_lines().is_empty(), "{:?}", fx.cleanup_lines());
}

/// AC1: the happy path with a LIVE process; test (d) holds.
#[test]
fn finalize_happy_path_test_d() {
    let fx = Fx::new();
    fx.gate_pass();
    let port = free_port();
    let pid = fx.start(Some(port), &["/bin/sleep", "30"]);
    let _g = Reap(pid);
    assert!(alive(pid), "the process must be running when finalize starts");
    let head = fx.head();
    let o = fx.finalize();
    assert_eq!(code(&o), 0, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), format!("finalize=done ticket=t1 repo_id=r head={head}"));
    assert!(fx.list().is_empty(), "manifest not empty: {:?}", fx.list());
    assert!(!fx.listed(), "tree still listed:\n{}", fx.worktrees());
    assert!(!fx.root().join("tree").exists());
    assert!(TcpListener::bind(("127.0.0.1", port as u16)).is_ok(), "port still bound");
    assert!(!alive(pid), "process still alive");
    let cl = fx.cleanup_lines();
    assert_eq!(cl.len(), 1, "{:?}", cl);
    assert!(has_cleanup(&cl[0], "pass", 0), "{}", cl[0]);
    let ev = fx.evidence_check();
    assert_eq!(code(&ev), 0);
    assert_eq!(out(&ev).trim_end(), "evidence=allowed ticket=t1 runs=1");
}

/// AC2: an unmanifested leftover is quarantined and Done is refused; a second run is Done.
#[test]
fn finalize_unmanifested_leftover_quarantined_not_done() {
    let fx = Fx::new();
    fx.gate_pass();
    let stray = fx.root().join("stray-dir");
    fs::create_dir_all(&stray).unwrap();
    fs::write(stray.join("leftover.txt"), "mine").unwrap();
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("sweep", "fail"));
    assert!(!stray.join("leftover.txt").exists(), "the leftover is still at its old path");
    let shelved = fs::read_dir(fx.shelf.join(T)).map(|d| d.count()).unwrap_or(0);
    assert!(shelved > 0, "nothing under the shelf");
    assert!(fx.log_lines().iter().any(|l| l.contains("quarantined")), "{:?}", fx.log_lines());
    untouched(&fx);
    let o2 = fx.finalize();
    assert_eq!(code(&o2), 0, "second run: {} {}", out(&o2), err(&o2));
    assert!(out(&o2).starts_with("finalize=done ticket=t1 "), "{}", out(&o2));
}

/// AC3: an unpushed commit is refused; after the push it is Done.
#[test]
fn finalize_unpushed_commit_not_done() {
    let fx = Fx::new();
    fx.commit("ok2");
    fx.gate_pass();
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("pushed", "fail"));
    untouched(&fx);
    fx.push();
    let o2 = fx.finalize();
    assert_eq!(code(&o2), 0, "after push: {} {}", out(&o2), err(&o2));
}

/// AC4: a failed cleanup is refused.
#[test]
fn finalize_failed_cleanup_not_done() {
    let fx = Fx::new();
    fx.gate_pass();
    let keep = fx.tmp().join("keep.log");
    fs::write(&keep, "x").unwrap();
    let o = run(&["manifest", "add", T, "--dir", fx.md.to_str().unwrap(), "--class", "log", "--id", "hand", "--path", keep.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "add: {}", err(&o));
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("teardown", "fail"));
    assert!(!out(&o).contains("finalize=done"));
    let cl = fx.cleanup_lines();
    assert_eq!(cl.len(), 1, "{:?}", cl);
    assert!(cl[0].contains("\"result\":\"fail\""), "{}", cl[0]);
}

/// AC5: a FAIL gate and a stale gate are both refused with nothing destructive run.
#[test]
fn finalize_fail_or_stale_gate_not_done() {
    let fx = Fx::new();
    fx.commit("bad");
    fx.push();
    let g = fx.gate();
    assert_eq!(code(&g), 1, "the gate must fail: {} {}", out(&g), err(&g));
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("gate", "fail"));
    untouched(&fx);
    assert!(!fx.shelf.join(T).exists() || fs::read_dir(fx.shelf.join(T)).unwrap().count() == 0);

    let fx = Fx::new();
    fx.gate_pass();
    fx.commit("ok2");
    fx.push();
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("gate", "fail"));
    untouched(&fx);
    assert!(!fx.shelf.join(T).exists() || fs::read_dir(fx.shelf.join(T)).unwrap().count() == 0);
}

/// AC6: an uncommitted edit to a tracked file is refused and left on disk.
#[test]
fn finalize_stray_edit_not_done() {
    let fx = Fx::new();
    fx.gate_pass();
    fs::write(fx.tree().join("src/value.txt"), "edited").unwrap();
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("clean", "fail"));
    assert_eq!(fs::read_to_string(fx.tree().join("src/value.txt")).unwrap(), "edited");
    untouched(&fx);
}

/// AC7: a refusal before any child runs: exit 2, empty stdout, nothing written.
#[test]
fn finalize_usage_refused_before_any_check() {
    let fx = Fx::new();
    fx.gate_pass();
    let md = fx.md.to_str().unwrap().to_string();
    let cases: Vec<Output> = vec![
        fx.finalize_with(Some("--log"), &fx.log, &md),
        fx.finalize_with(None, &fx.log, "relative/md"),
        fx.finalize_with(None, &fx.log, &format!("{}/../M", md)),
        {
            let empty = fx.t.p.join("emptymd");
            fs::create_dir_all(&empty).unwrap();
            fx.finalize_with(None, &fx.log, empty.to_str().unwrap())
        },
    ];
    for (i, o) in cases.iter().enumerate() {
        assert_eq!(code(o), 2, "case {i}: {} {}", out(o), err(o));
        assert!(out(o).is_empty(), "case {i} stdout: {}", out(o));
    }
    assert!(!fx.log.exists(), "the log was created");
    untouched(&fx);
}

/// AC10: a failed run is still refused after teardown.
#[test]
fn finalize_failed_run_refused_after_teardown() {
    let fx = Fx::new();
    fx.gate_pass();
    fx.start(None, &["sh", "-c", "exit 3"]);
    let t0 = Instant::now();
    loop {
        let o = run(&["evidence", "show", T, "--manifest-dir", fx.md.to_str().unwrap()]);
        if out(&o).contains("failed") {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "the run never reported failed: {}", out(&o));
        std::thread::sleep(Duration::from_millis(100));
    }
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "stdout {} stderr {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("evidence", "fail"));
    assert!(!out(&o).contains("finalize=done"));
    let cl = fx.cleanup_lines();
    assert_eq!(cl.len(), 1, "{:?}", cl);
    assert!(has_cleanup(&cl[0], "pass", 0), "{}", cl[0]);
    assert!(!fx.root().join("tree").exists());
    git(&fx.repo, &["rev-parse", "--verify", "ws/t1"]);
    assert!(fx.root().join("evidence").exists(), "evidence folder gone");
}

/// AC8: no folder is printed on any path.
#[test]
fn finalize_prints_no_folder() {
    // done
    let fx = Fx::new();
    fx.gate_pass();
    let o = fx.finalize();
    assert_eq!(code(&o), 0);
    assert!(fx.leak_free(&o), "{} {}", out(&o), err(&o));
    // sweep refusal
    let fx = Fx::new();
    fx.gate_pass();
    fs::create_dir_all(fx.root().join("stray")).unwrap();
    fs::write(fx.root().join("stray/f"), "x").unwrap();
    let o = fx.finalize();
    assert_eq!(code(&o), 1);
    assert!(fx.leak_free(&o), "{} {}", out(&o), err(&o));
    // stale gate, usage refusal
    let fx = Fx::new();
    let o = fx.finalize();
    assert_eq!(code(&o), 1, "no gate run yet: {} {}", out(&o), err(&o));
    assert_eq!(out(&o).trim_end(), refused("gate", "fail"));
    assert!(fx.leak_free(&o), "{} {}", out(&o), err(&o));
    let o = fx.finalize_with(Some("--log"), &fx.log, fx.md.to_str().unwrap());
    assert_eq!(code(&o), 2);
    assert!(fx.leak_free(&o), "{} {}", out(&o), err(&o));
    // dirty tree, unpushed
    let fx = Fx::new();
    fx.commit("ok2");
    fx.gate_pass();
    let o = fx.finalize();
    assert_eq!(code(&o), 1);
    assert!(fx.leak_free(&o), "{} {}", out(&o), err(&o));
    // failed run after teardown
    let fx = Fx::new();
    fx.gate_pass();
    fx.start(None, &["sh", "-c", "exit 3"]);
    std::thread::sleep(Duration::from_millis(1500));
    let o = fx.finalize();
    assert!(fx.leak_free(&o), "{} {}", out(&o), err(&o));
}
