//! Process-level tests for `harness session watch` (A29). The real binary runs as a child. The host is a stub script that prints a listing
//! file the test wrote (or exits 1, or prints text that is not JSON); any other host verb is logged to a calls file, which no test expects
//! to exist. The tree is a git repo the test makes, the launch records and the ticket are files the test writes, and every transcript is a
//! file whose mtime the test sets and whose mode is 000 (the verb reads metadata only). This file reads no environment variable.
#![cfg(unix)]
use harness::gate_result::{GateResult, Status};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const SHORT: &str = "ab12cd34";
const SID: &str = "11111111-2222-4333-8444-555555555555";
const NAME: &str = "seat-a";
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct R {
    code: i32,
    out: String,
    err: String,
}

/// Runs the verb child once. `Fx::stub` warms each script it writes (one untimed run), so the run under the verb pays no macOS
/// first-exec wait of a freshly written script.
fn go_run(c: &mut Command) -> R {
    let o = c.output().unwrap();
    R { code: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into(), err: String::from_utf8_lossy(&o.stderr).into() }
}

fn now_s() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn at(age: i64) -> SystemTime {
    let n = SystemTime::now();
    if age >= 0 {
        n - Duration::from_secs(age as u64)
    } else {
        n + Duration::from_secs((-age) as u64)
    }
}

fn git(dir: &Path, args: &[&str], date: Option<u64>) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    if let Some(d) = date {
        c.env("GIT_COMMITTER_DATE", format!("{d} +0000")).env("GIT_AUTHOR_DATE", format!("{d} +0000"));
    }
    let o = c.stdin(Stdio::null()).output().unwrap();
    assert!(o.status.success(), "scratch git call failed: {args:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn touch(p: &Path, t: SystemTime) {
    fs::File::open(p).unwrap().set_modified(t).unwrap();
}

fn age_dir(d: &Path, t: SystemTime) {
    for e in fs::read_dir(d).unwrap() {
        let e = e.unwrap();
        if e.file_name() == ".git" {
            continue;
        }
        let p = e.path();
        if e.file_type().unwrap().is_dir() {
            age_dir(&p, t);
        }
        touch(&p, t);
    }
    touch(d, t);
}

/// Writes `body` after a warm line (`warm` as the first argument exits 0) to `<dir>/<name>.src` and copies it onto `<dir>/<name>` with a
/// cp child: while a direct write runs, this process holds the script's write handle, and a Linux sibling thread that forks in that
/// window inherits it, so the exec fails with ETXTBSY until that child execs. The warm exec stays for the macOS slow first exec of a
/// fresh script (f-20261122-10).
fn script(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    let src = dir.join(format!("{name}.src"));
    let text = format!("#!/bin/sh\nif [ \"$1\" = warm ]; then exit 0; fi\n{body}\n");
    fs::write(&src, text).unwrap();
    let o = Command::new("cp").arg(&src).arg(&p).stdin(Stdio::null()).status().unwrap();
    assert!(o.success(), "the cp of the stub onto {} failed: {o}", p.display());
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    let o = Command::new(&p).arg("warm").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status()
        .unwrap_or_else(|e| panic!("the warm exec of {} failed: {e}", p.display()));
    assert!(o.success(), "the warm start of the stub failed: {o}");
}

/// A process the test started; stopped at the end of the test.
struct Live(Child);

impl Drop for Live {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn live() -> Live {
    Live(Command::new("sleep").arg("120").stdin(Stdio::null()).spawn().unwrap())
}

/// The process list as the test itself reads it: pid, parent pid and state of every process.
fn ps_rows() -> Vec<(u32, u32, String)> {
    let o = Command::new("ps").args(["-A", "-o", "pid=,ppid=,state="]).stdin(Stdio::null()).output().unwrap();
    assert!(o.status.success(), "the ps of the test failed");
    String::from_utf8_lossy(&o.stdout)
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            Some((f.first()?.parse().ok()?, f.get(1)?.parse().ok()?, f.get(2)?.to_string()))
        })
        .collect()
}

/// The live (not zombie) children of `pid`.
fn children(pid: u32) -> Vec<u32> {
    ps_rows().iter().filter(|r| r.1 == pid && !r.2.starts_with('Z')).map(|r| r.0).collect()
}

/// Each live process under `root` as (pid, depth), lowest depth first and each pid once; a child of `root` is depth 1.
fn under(root: u32) -> Vec<(u32, u32)> {
    let rows = ps_rows();
    let (mut level, mut seen, mut out): (Vec<u32>, Vec<u32>, Vec<(u32, u32)>) = (vec![root], vec![root], Vec::new());
    let mut depth = 0;
    loop {
        let found: Vec<u32> = rows.iter().filter(|r| level.contains(&r.1) && !r.2.starts_with('Z') && !seen.contains(&r.0)).map(|r| r.0).collect();
        if found.is_empty() {
            return out;
        }
        depth += 1;
        for pid in &found {
            seen.push(*pid);
            out.push((*pid, depth));
        }
        level = found;
    }
}

/// The depths of the live processes under `root`, lowest first: the shape of a fixture as the verb sees it (it counts a live process
/// at depth 2 or more under the seat).
fn depths(root: u32) -> Vec<u32> {
    under(root).iter().map(|x| x.1).collect()
}

/// Every process under `root` on the process list, deepest first, each pid once.
fn family(root: u32) -> Vec<u32> {
    let rows = ps_rows();
    let (mut level, mut seen, mut out): (Vec<u32>, Vec<u32>, Vec<u32>) = (vec![root], vec![root], Vec::new());
    while !level.is_empty() {
        let found: Vec<u32> = rows.iter().filter(|r| level.contains(&r.1) && !seen.contains(&r.0)).map(|r| r.0).collect();
        for pid in &found {
            seen.push(*pid);
            out.push(*pid);
        }
        level = found;
    }
    out.reverse();
    out
}

/// Up to 5 s, in steps of 50 ms, until `done` holds; the test fails when it never does.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(std::time::Instant::now() < end, "not seen in 5 s: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Fails the test unless the live processes under the seat sit at exactly these depths (lowest first) within 5 s, and shows the
/// depths it saw otherwise. A test checks the shape its fixture describes here, before its verdict, on every OS: a shell that kept
/// a wrapper under the seat pid shows as depths one too deep.
fn shaped(seat: u32, want: &[u32]) {
    let end = std::time::Instant::now() + Duration::from_secs(5);
    let mut got = depths(seat);
    while got != want && std::time::Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
        got = depths(seat);
    }
    assert_eq!(got, want, "the live processes under the seat, by depth (wanted {want:?})");
}

fn kill(pid: u32) {
    let _ = Command::new("kill").args(["-9", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
}

/// A shell the test started with `-c script` and `args` after its name. The spawned pid is the shell of `script` itself on every
/// OS: the spawn goes through `exec "$0" "$@"` (`tree_n`). At the end of the test every process under it on the process list is
/// killed (deepest first), then it is: a tool call outlives the shell that started it otherwise.
struct Tree(Child);

impl Drop for Tree {
    fn drop(&mut self) {
        for pid in family(self.0.id()) {
            kill(pid);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `sh -c 'exec "$0" "$@"' sh -c script args...`, so the spawned pid is the shell of `script` on every OS. A bare `sh -c script`
/// is not: macOS `sh` (bash) replaces itself with a `-c` string that is one command, while Ubuntu `sh` (dash 0.5.12, CI
/// ubuntu-latest) keeps a wrapper shell, so every process of the script sat one level deeper than its test assumed and the seat
/// pid was not the shell the script describes. `exec` means the same on every POSIX shell: the wrapper becomes the shell of
/// `script`, with `$0` `sh` and `$1..` `args` as before. Each fixture script also forces every fork it makes (a `&`, or a command
/// followed by another one), so the depths under the seat do not depend on the shell either: the tests check them (`shaped`)
/// before the verdict.
fn tree_n(script: &str, args: &[&str]) -> Tree {
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg("exec \"$0\" \"$@\"")
        .arg("sh")
        .arg("-c")
        .arg(script)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Tree(c.spawn().unwrap())
}

fn tree(script: &str) -> Tree {
    tree_n(script, &[])
}

/// A pid the test kills at the end of the test: a process that left the tree it was started in (reparented), so no `Tree` reaches it.
struct Kill(u32);

impl Drop for Kill {
    fn drop(&mut self) {
        kill(self.0);
    }
}

fn ended_pid() -> u32 {
    let mut c = Command::new("true").spawn().unwrap();
    let id = c.id();
    c.wait().unwrap();
    id
}

struct Fx {
    root: PathBuf,
    tree: PathBuf,
    base: String,
    launch_ms: u64,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn ticket_json() -> String {
    let pairs = [
        ("id", "\"T-1\""),
        ("repo_id", "\"r\""),
        ("title", "\"A title\""),
        ("group", "\"grp\""),
        ("size", "\"M\""),
        ("objective", "\"An objective\""),
        ("acceptance_criteria", "[\"one\"]"),
        ("dependencies", "[]"),
        ("risk", "\"normal\""),
        ("owner_role", "\"execution_verification\""),
        ("evidence_links", "[]"),
    ];
    let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

fn launch_rows(name: &str, run: &str, short: &str, sid: &str, ts_ms: u64) -> String {
    format!(
        "{{\"v\":1,\"kind\":\"launch\",\"ts_ms\":{ts_ms},\"run_id\":\"{run}\",\"name\":\"{name}\",\"role\":\"exec\",\"brief_sha256\":\"{}\",\"owner\":\"orchestration\"}}\n\
         {{\"v\":1,\"kind\":\"launch_ids\",\"ts_ms\":{ts_ms},\"run_id\":\"{run}\",\"session_id\":\"{sid}\",\"short_id\":\"{short}\"}}\n",
        "0".repeat(64)
    )
}

impl Fx {
    /// Tree with one base commit (5000 s old), a launch 1000 s ago, an empty listing and no transcript.
    fn new() -> Fx {
        let root = std::env::temp_dir().join(format!("swatch-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&root);
        for d in ["tree", "res", "caps", "tr", "tickets"] {
            fs::create_dir_all(root.join(d)).unwrap();
        }
        let root = root.canonicalize().unwrap();
        let tree = root.join("tree");
        git(&tree, &["init", "-q"], None);
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&tree, &["config", k, v], None);
        }
        fs::write(tree.join("f.txt"), "base").unwrap();
        git(&tree, &["add", "-A"], None);
        git(&tree, &["commit", "-q", "-m", "base"], Some(now_s() - 5000));
        let base = git(&tree, &["rev-parse", "HEAD"], None);
        let launch_ms = (now_s() - 1000) * 1000;
        let fx = Fx { root, tree, base, launch_ms };
        fs::write(fx.root.join("tickets/t.json"), ticket_json()).unwrap();
        fx.records(&launch_rows(NAME, "r1", SHORT, SID, launch_ms));
        fx.list(&[]);
        fx
    }

    fn records(&self, body: &str) {
        fs::write(self.root.join("records.jsonl"), body).unwrap();
    }

    /// Writes the stub text to host.sh.src and copies it onto host.sh with a cp child: while a direct write runs, this process
    /// holds the script's write handle, and a Linux sibling thread that forks in that window inherits it, so the exec fails with
    /// ETXTBSY until that child execs. The warm exec stays for the macOS slow first exec of a fresh script (f-20261122-10).
    fn stub(&self, body: &str) {
        let calls = self.root.join("calls");
        let body = format!("if [ \"$1\" != agents ]; then echo \"$1\" >> '{}'; exit 1; fi\n{body}\n", calls.display());
        script(&self.root, "host.sh", &body);
    }

    /// A `ps` first on the PATH of the verb (the path returned, for `go_path`): `-p <pid> -o pid=` answers the pid, so the entry pid
    /// is live; `-A` runs `on_a` and exits 0.
    fn ps_stub(&self, on_a: &str) -> String {
        let bin = self.root.join("bin");
        fs::create_dir_all(&bin).unwrap();
        script(&bin, "ps", &format!("if [ \"$1\" = -A ]; then\n{on_a}\nexit 0\nfi\necho \"$2\"\n"));
        format!("{}:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin", bin.display())
    }

    /// Runs the verb once with `path` as the PATH of the child (the rest of its environment is kept).
    fn go_path(&self, a: &[String], path: &str) -> R {
        let mut c = Command::new(BIN);
        c.args(a).env("PATH", path).stdin(Stdio::null());
        go_run(&mut c)
    }

    fn list(&self, entries: &[String]) {
        fs::write(self.root.join("list.json"), format!("[{}]", entries.join(","))).unwrap();
        self.stub(&format!("cat '{}'", self.root.join("list.json").display()));
    }

    fn cwd(&self) -> String {
        self.tree.display().to_string()
    }

    fn ent(&self, state: &str, status: Option<&str>) -> String {
        self.entry(SHORT, SID, state, status, &self.cwd(), None)
    }

    fn entry(&self, id: &str, sid: &str, state: &str, status: Option<&str>, cwd: &str, pid: Option<String>) -> String {
        let st = status.map(|s| format!(",\"status\":\"{s}\"")).unwrap_or_default();
        let pd = pid.map(|p| format!(",\"pid\":{p}")).unwrap_or_default();
        format!("{{\"id\":\"{id}\",\"cwd\":\"{cwd}\",\"kind\":\"bg\",\"startedAt\":1,\"sessionId\":\"{sid}\",\"name\":\"{NAME}\",\"state\":\"{state}\"{st}{pd}}}")
    }

    fn tpath(&self, cwd: &str, sid: &str) -> PathBuf {
        let enc: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
        self.root.join("tr").join(enc).join(format!("{sid}.jsonl"))
    }

    /// A transcript file of the given age (negative: ahead of the clock), mode 000; the test fails if it can still open it.
    fn mk_file(&self, p: &Path, age: i64) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, "{\"never\":\"read\"}\n").unwrap();
        fs::OpenOptions::new().write(true).open(p).unwrap().set_modified(at(age)).unwrap();
        fs::set_permissions(p, fs::Permissions::from_mode(0o000)).unwrap();
        assert!(fs::File::open(p).is_err(), "FAIL: the mode-000 fixture can be opened by this user, so the no-read check proves nothing");
    }

    fn transcript(&self, age: i64) {
        self.mk_file(&self.tpath(&self.cwd(), SID), age);
    }

    fn commit_at(&self, age: u64) {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        fs::write(self.tree.join(format!("w{n}.txt")), "w").unwrap();
        git(&self.tree, &["add", "-A"], None);
        git(&self.tree, &["commit", "-q", "-m", "w"], Some(now_s() - age));
    }

    fn note(&self, pass: bool) {
        let head = git(&self.tree, &["rev-parse", "HEAD"], None);
        let r = GateResult {
            status: if pass { Status::Pass } else { Status::Fail },
            gate_id: "ticket-gate".into(),
            head_sha: head,
            failure_class: if pass { None } else { Some("check-failed".into()) },
            message: "m".into(),
            file: None,
            line: None,
        };
        fs::write(self.root.join("res/T-1.json"), r.to_json().unwrap()).unwrap();
        fs::write(self.root.join("caps/T-1.rounds"), "{\"v\":1,\"ticket\":\"T-1\",\"repo_id\":\"r\",\"fix_rounds\":0,\"in_flight\":false}\n").unwrap();
    }

    /// Every file and folder of the tree outside .git, and the git index, get this age.
    fn age_tree(&self, secs: u64) {
        let t = at(secs as i64);
        age_dir(&self.tree, t);
        touch(&self.tree.join(".git/index"), t);
    }

    fn args(&self, idle: u64, silence: u64) -> Vec<String> {
        let p = |n: &str| self.root.join(n).display().to_string();
        let v = [
            "session".to_string(),
            "watch".into(),
            "--host".into(),
            p("host.sh"),
            "--records".into(),
            p("records.jsonl"),
            "--name".into(),
            NAME.into(),
            "--ticket".into(),
            p("tickets/t.json"),
            "--repo-map".into(),
            format!("r={}", self.cwd()),
            "--base".into(),
            self.base.clone(),
            "--result-dir".into(),
            p("res"),
            "--caps-dir".into(),
            p("caps"),
            "--transcript-root".into(),
            p("tr"),
            "--idle-s".into(),
            idle.to_string(),
            "--silence-s".into(),
            silence.to_string(),
        ];
        v.to_vec()
    }

    /// Runs the verb once.
    fn go(&self, a: &[String]) -> R {
        let mut c = Command::new(BIN);
        c.args(a).stdin(Stdio::null());
        go_run(&mut c)
    }

    fn run(&self) -> R {
        self.go(&self.args(300, 600))
    }

    fn run_extra(&self, extra: &[&str]) -> R {
        let mut a = self.args(300, 600);
        a.extend(extra.iter().map(|s| s.to_string()));
        self.go(&a)
    }

    /// Runs the verb once with `--child-slack-s`.
    fn run_extra_slack(&self, idle: u64, silence: u64, slack: u64) -> R {
        let mut a = self.args(idle, silence);
        a.extend(["--child-slack-s".to_string(), slack.to_string()]);
        self.go(&a)
    }

    fn no_host_writes(&self) {
        assert!(!self.root.join("calls").exists(), "the verb called a host verb other than the listing");
    }
}

fn is(r: &R, code: i32, want: &[&str]) {
    assert_eq!(r.code, code, "stdout: {} stderr: {}", r.out, r.err);
    for w in want {
        assert!(r.out.contains(w), "{w:?} missing in {:?}", r.out);
    }
    assert_eq!(r.out.lines().count(), 1, "one line expected: {:?}", r.out);
}

/// A numeric field of the line must lie in `lo..=hi` (the fixtures age a little while the machine is busy).
fn num(r: &R, key: &str, lo: u64, hi: u64) {
    let v: u64 = r.out.split(' ').find_map(|t| t.strip_prefix(key)).and_then(|s| s.trim().parse().ok()).unwrap_or(u64::MAX);
    assert!((lo..=hi).contains(&v), "{key} {v} not in {lo}..={hi}: {}", r.out);
}

fn refused(r: &R, what: &str) {
    assert_eq!(r.code, 3, "{what}: stdout {:?} stderr {:?}", r.out, r.err);
    assert!(r.out.is_empty(), "{what}: stdout must be empty: {:?}", r.out);
    assert!(!r.err.trim().is_empty(), "{what}: one stderr line expected");
}

/// A refusal whose stderr line holds the given message.
fn refused_msg(r: &R, what: &str, msg: &str) {
    refused(r, what);
    assert!(r.err.contains(msg), "{what}: {msg:?} missing in {:?}", r.err);
}

/// A working seat with a fresh tree and a transcript 30 s old.
fn working() -> Fx {
    let fx = Fx::new();
    fx.list(&[fx.ent("working", Some("busy"))]);
    fx.transcript(30);
    fx.age_tree(5);
    fx
}

// ------------------------------------------------------------------------------------------------ verdicts

#[test]
fn t1_working() {
    let fx = working();
    let r = fx.run();
    is(&r, 0, &["verdict=working", "state=working/busy", "session=ab12cd34"]);
    num(&r, "transcript_age=", 30, 120);
    fx.no_host_writes();
}

#[test]
fn t2_slow_alive_when_the_tree_is_old_and_the_transcript_is_fresh() {
    let fx = Fx::new();
    fx.list(&[fx.ent("working", Some("busy"))]);
    fx.transcript(30);
    fx.age_tree(800);
    let r = fx.run();
    is(&r, 10, &["verdict=slow-alive"]);
    num(&r, "tree_age=", 800, 900);
    num(&r, "transcript_age=", 30, 120);
}

#[test]
fn t4_stalled_when_both_are_old_and_the_pid_is_live() {
    let fx = Fx::new();
    let p = live();
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.transcript(700);
    fx.age_tree(800);
    is(&fx.run(), 11, &["verdict=stalled"]);
}

// ------------------------------------------------------------------------------------------------ a quiet working seat

/// A quiet working seat: a live pid, a commit 900 s ago, a tree 800 s old and a transcript 700 s old.
fn quiet(p: &Live, note: bool) -> Fx {
    let fx = Fx::new();
    fx.commit_at(900);
    if note {
        fx.note(true);
    }
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.transcript(700);
    fx.age_tree(800);
    fx
}

#[test]
fn t4b_a_quiet_working_seat_with_a_passed_gate_on_the_head_is_finished() {
    let p = live();
    let fx = quiet(&p, true);
    let r = fx.run();
    is(&r, 13, &["verdict=finished", "state=working/busy", "gate=pass/yes", "session=ab12cd34", "skipped_entries=0"]);
    num(&r, "tree_age=", 800, 900);
    num(&r, "transcript_age=", 700, 800);
    // the whole line is compared: the two ages come from the line itself (checked above), the rest is exact, field order included
    let t: Vec<&str> = r.out.trim_end().split(' ').collect();
    assert!(t.len() > 3, "too few fields: {:?}", r.out);
    let want = format!(
        "verdict=finished state=working/busy {} {} commits_since_launch=1 gate=pass/yes session=ab12cd34 skipped_entries=0",
        t[2], t[3]
    );
    assert!(t[2].starts_with("tree_age=") && t[3].starts_with("transcript_age="), "{:?}", r.out);
    assert_eq!(r.out, format!("{want}\n"), "the whole stdout line");
}

#[test]
fn t4c_a_quiet_working_seat_with_the_gate_off_the_head_stays_stalled() {
    let p = live();
    let fx = Fx::new();
    fx.commit_at(900);
    fx.note(true);
    fx.commit_at(850);
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.transcript(700);
    fx.age_tree(800);
    is(&fx.run(), 11, &["verdict=stalled", "gate=pass/no", "commits_since_launch=2"]);
}

#[test]
fn t4d_a_quiet_working_seat_with_a_failed_gate_stays_stalled() {
    let p = live();
    let fx = Fx::new();
    fx.commit_at(900);
    fx.note(false);
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.transcript(700);
    fx.age_tree(800);
    is(&fx.run(), 11, &["verdict=stalled", "gate=fail/yes", "commits_since_launch=1"]);
}

#[test]
fn t4e_a_quiet_working_seat_with_no_commit_since_the_launch_stays_stalled() {
    let p = live();
    let fx = Fx::new();
    fx.commit_at(2000);
    fx.note(true);
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.transcript(700);
    fx.age_tree(800);
    is(&fx.run(), 11, &["verdict=stalled", "commits_since_launch=0", "gate=pass/yes"]);
}

#[test]
fn t4f_a_quiet_working_seat_with_no_gate_note_stays_stalled() {
    let p = live();
    let fx = quiet(&p, false);
    is(&fx.run(), 11, &["verdict=stalled", "commits_since_launch=1", "gate=none/no"]);
}

#[test]
fn t4g_a_quiet_passed_seat_with_an_ended_pid_is_a_refusal() {
    let fx = Fx::new();
    fx.commit_at(900);
    fx.note(true);
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(ended_pid().to_string()))]);
    fx.transcript(700);
    fx.age_tree(800);
    refused(&fx.run(), "quiet passed seat, pid ended");
    // with the ticket unreadable too, the pid test must come first: the refusal is the pid message, not the status one
    let fx = Fx::new();
    fx.commit_at(900);
    fx.note(true);
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(ended_pid().to_string()))]);
    fs::write(fx.root.join("tickets/t.json"), "not a ticket").unwrap();
    fx.transcript(700);
    fx.age_tree(800);
    let r = fx.run();
    refused_msg(&r, "quiet seat, pid ended, ticket unreadable", "the entry pid is not a live process");
    assert!(!r.err.contains("status refused"), "the pid test comes before the facts read: {:?}", r.err);
}

#[test]
fn t4h_a_quiet_working_seat_with_an_unreadable_ticket_is_a_refusal() {
    let p = live();
    let fx = Fx::new();
    fx.commit_at(900);
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fs::write(fx.root.join("tickets/t.json"), "not a ticket").unwrap();
    fx.transcript(700);
    fx.age_tree(800);
    refused_msg(&fx.run(), "quiet working seat, status refuses", "status refused (exit 1)");
}

#[test]
fn t4i_the_commits_and_gate_are_not_read_before_the_idle_or_the_silence_test() {
    // the ticket is unreadable, so a seat that reaches the facts read refuses; a seat inside a window does not reach it
    let fx = Fx::new();
    fs::write(fx.root.join("tickets/t.json"), "not a ticket").unwrap();
    fx.list(&[fx.ent("working", Some("busy"))]);
    fx.transcript(30);
    fx.age_tree(5);
    is(&fx.run(), 0, &["verdict=working"]);
    let fx = Fx::new();
    fs::write(fx.root.join("tickets/t.json"), "not a ticket").unwrap();
    fx.list(&[fx.ent("working", Some("busy"))]);
    fx.transcript(30);
    fx.age_tree(800);
    is(&fx.run(), 10, &["verdict=slow-alive"]);
}

#[test]
fn t4j_follow_ends_at_once_on_a_quiet_finished_seat() {
    let p = live();
    let fx = quiet(&p, true);
    let r = fx.run_extra(&["--follow", "--poll-s", "1", "--max-s", "3"]);
    assert_eq!(r.code, 13, "{} {}", r.out, r.err);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 1, "{:?}", r.out);
    assert!(lines[0].contains("verdict=finished"), "{}", lines[0]);
    num(&r, "max_transcript_age=", 700, 800);
}

#[test]
fn t5_stopped_early_no_commit_for_done_and_idle() {
    for state in ["done", "idle"] {
        let fx = Fx::new();
        fx.list(&[fx.ent(state, Some("idle"))]);
        fx.transcript(30);
        is(&fx.run(), 12, &["verdict=stopped-early", "why=no-commit", "commits_since_launch=0"]);
    }
}

#[test]
fn t5b_blocked_is_stopped_early_even_with_a_commit_and_a_gate_pass() {
    let fx = Fx::new();
    fx.commit_at(900);
    fx.note(true);
    fx.list(&[fx.ent("blocked", None)]);
    fx.transcript(30);
    is(&fx.run(), 12, &["verdict=stopped-early", "why=blocked"]);
}

#[test]
fn t6_stopped_early_no_gate() {
    let fx = Fx::new();
    fx.commit_at(900);
    fx.list(&[fx.ent("done", Some("idle"))]);
    fx.transcript(30);
    is(&fx.run(), 12, &["why=no-gate", "commits_since_launch=1", "gate=none/no"]);
}

#[test]
fn t7_finished_with_a_gate_pass_and_with_a_gate_fail() {
    for (pass, word) in [(true, "gate=pass"), (false, "gate=fail")] {
        let fx = Fx::new();
        fx.commit_at(900);
        fx.note(pass);
        fx.list(&[fx.ent("done", Some("idle"))]);
        fx.transcript(30);
        is(&fx.run(), 13, &["verdict=finished", word, "commits_since_launch=1"]);
    }
}

#[test]
fn t8_fix_round_commits_before_the_launch_do_not_count() {
    let fx = Fx::new();
    fx.commit_at(2000);
    fx.note(true);
    fx.list(&[fx.ent("done", Some("idle"))]);
    fx.transcript(30);
    is(&fx.run(), 12, &["why=no-commit", "commits_since_launch=0", "gate=pass/yes"]);
}

#[test]
fn t9_stopped_by_bw_and_by_host_is_never_stalled() {
    let fx = working();
    for kind in ["stopped", "absent"] {
        let r = fx.run_extra(&["--stopped-at", "1700000000", "--stop", kind]);
        is(&r, 14, &["verdict=stopped", "by=bw", &format!("stop={kind}")]);
    }
    let fx = Fx::new();
    fx.list(&[fx.ent("stopped", None)]);
    fx.transcript(900);
    fx.age_tree(800);
    let r = fx.run();
    is(&r, 14, &["verdict=stopped", "by=host"]);
    assert!(!r.out.contains("stalled"));
    fx.no_host_writes();
}

#[test]
fn t10_gone_without_an_entry_with_both_ids() {
    let fx = Fx::new();
    is(&fx.run(), 15, &["verdict=gone", "transcript_age=-"]);
    fx.list(&[fx.entry(SHORT, "99999999-2222-4333-8444-555555555555", "working", Some("busy"), &fx.cwd(), None)]);
    is(&fx.run(), 15, &["verdict=gone"]);
    fx.list(&[fx.entry("deadbeef", SID, "working", Some("busy"), &fx.cwd(), None)]);
    is(&fx.run(), 15, &["verdict=gone"]);
}

// ------------------------------------------------------------------------------------------------ entries left out of the join

const OTHER_SID: &str = "99999999-2222-4333-8444-555555555555";

/// A second folder that exists (its path canonicalizes), so a probe entry cwd is real but is not the tree.
fn other_dir(fx: &Fx) -> String {
    let d = fx.root.join("probe-cwd");
    fs::create_dir_all(&d).unwrap();
    d.display().to_string()
}

/// The verdict line with the two age fields taken out: they tick between two runs of one fixture.
fn norm(l: &str) -> String {
    l.split(' ')
        .filter(|t| !t.starts_with("tree_age=") && !t.starts_with("transcript_age="))
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn t10b_an_entry_with_no_id_that_cannot_be_the_watched_one_is_left_out() {
    let fx = working();
    let base = fx.run();
    is(&base, 0, &["verdict=working", "skipped_entries=0"]);
    fx.list(&[
        fx.ent("working", Some("busy")),
        format!("{{\"name\":\"probe\",\"cwd\":\"{}\",\"sessionId\":\"{OTHER_SID}\",\"state\":\"working\"}}", other_dir(&fx)),
    ]);
    let with = fx.run();
    assert_eq!(with.code, 0, "{} {}", with.out, with.err);
    num(&with, "transcript_age=", 30, 120);
    assert_eq!(
        norm(&with.out),
        norm(&base.out).replace("skipped_entries=0", "skipped_entries=1"),
        "the run with the probe entry must give the same line with skipped_entries=1"
    );
    fx.no_host_writes();
}

#[test]
fn t10b_an_entry_with_no_name_or_a_bad_id_is_left_out_too() {
    let fx = working();
    let base = fx.run();
    // an id other than the short id, and no name
    fx.list(&[
        fx.ent("working", Some("busy")),
        format!("{{\"id\":\"deadbeef\",\"cwd\":\"{}\",\"sessionId\":\"{OTHER_SID}\",\"state\":\"working\"}}", other_dir(&fx)),
    ]);
    let r = fx.run();
    assert_eq!(r.code, 0, "{} {}", r.out, r.err);
    assert_eq!(norm(&r.out), norm(&base.out).replace("skipped_entries=0", "skipped_entries=1"), "an id other than the short id, no name");
    // an id that is not a plain token
    fx.list(&[
        fx.ent("working", Some("busy")),
        format!("{{\"id\":\"a b\",\"name\":\"probe\",\"cwd\":\"{}\",\"sessionId\":\"{OTHER_SID}\",\"state\":\"working\"}}", other_dir(&fx)),
    ]);
    let r = fx.run();
    assert_eq!(r.code, 0, "{} {}", r.out, r.err);
    assert_eq!(norm(&r.out), norm(&base.out).replace("skipped_entries=0", "skipped_entries=1"), "an id that is not a plain token");
}

#[test]
fn t11_an_entry_that_could_be_the_watched_one_still_refuses_without_an_id_or_name() {
    // no id, but the sessionId equals the record session id
    let fx = working();
    fx.list(&[
        fx.ent("working", Some("busy")),
        format!("{{\"name\":\"probe\",\"cwd\":\"{}\",\"sessionId\":\"{SID}\",\"state\":\"working\"}}", other_dir(&fx)),
    ]);
    refused_msg(&fx.run(), "no id, the record session id", "a listing entry has no string id and name");
    // no id, another session id, but the cwd is the tree
    fx.list(&[
        fx.ent("working", Some("busy")),
        format!("{{\"name\":\"probe\",\"cwd\":\"{}\",\"sessionId\":\"{OTHER_SID}\",\"state\":\"working\"}}", fx.cwd()),
    ]);
    refused_msg(&fx.run(), "no id, the tree as cwd", "a listing entry has no string id and name");
    // the id equals the short id, but no name
    fx.list(&[
        fx.ent("working", Some("busy")),
        format!("{{\"id\":\"{SHORT}\",\"cwd\":\"{}\",\"sessionId\":\"{OTHER_SID}\",\"state\":\"working\"}}", other_dir(&fx)),
    ]);
    refused_msg(&fx.run(), "the short id, no name", "a listing entry has no string id and name");
    // no id, another session id, another cwd, but the name is the watched seat's name
    fx.list(&[
        fx.ent("working", Some("busy")),
        format!("{{\"name\":\"{NAME}\",\"cwd\":\"{}\",\"sessionId\":\"{OTHER_SID}\",\"state\":\"working\"}}", other_dir(&fx)),
    ]);
    refused_msg(&fx.run(), "no id, the seat name", "a listing entry has no string id and name");
}

// ------------------------------------------------------------------------------------------------ refusals

#[test]
fn t11_listing_failures_are_refusals() {
    let fx = working();
    fx.stub("exit 1");
    refused(&fx.run(), "listing exit 1");
    fx.stub("echo not json");
    refused(&fx.run(), "listing not JSON");
    fx.stub("echo '{\"a\":1}'");
    refused(&fx.run(), "listing not an array");
}

#[test]
fn t11_launch_record_problems_are_refusals() {
    let fx = working();
    let mut a = fx.args(300, 600);
    let i = a.iter().position(|s| s == NAME).unwrap();
    a[i] = "other-seat".into();
    refused(&fx.go(&a), "no record for the name");
    let pending = launch_rows(NAME, "r1", SHORT, SID, fx.launch_ms).lines().next().unwrap().to_string() + "\n";
    fx.records(&pending);
    refused(&fx.run(), "newest record pending");
    fx.records(&format!("{}{}", launch_rows(NAME, "r1", SHORT, SID, fx.launch_ms), pending.replace("r1", "r2").replace(&fx.launch_ms.to_string(), &(fx.launch_ms + 1000).to_string())));
    refused(&fx.run(), "newer record pending");
}

#[test]
fn t11_two_joined_entries_refuse() {
    let fx = working();
    fx.list(&[fx.ent("working", Some("busy")), fx.ent("working", Some("busy"))]);
    refused(&fx.run(), "two entries");
}

#[test]
fn t11_the_entry_cwd_must_be_the_tree() {
    let fx = Fx::new();
    let other = fx.root.join("other");
    fs::create_dir_all(&other).unwrap();
    let cwd = other.display().to_string();
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &cwd, None)]);
    fx.mk_file(&fx.tpath(&cwd, SID), 30);
    refused(&fx.run(), "cwd is a second folder");
    // twin: a link that canonicalizes to the tree is accepted
    let link = fx.root.join("lnk");
    symlink(&fx.tree, &link).unwrap();
    let lc = link.display().to_string();
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &lc, None)]);
    fx.mk_file(&fx.tpath(&lc, SID), 30);
    fx.age_tree(5);
    is(&fx.run(), 0, &["verdict=working"]);
}

#[test]
fn t11_a_missing_transcript_is_a_refusal_with_an_old_and_with_a_fresh_tree() {
    for tree_age in [800, 5] {
        let fx = Fx::new();
        let p = live();
        fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
        fx.age_tree(tree_age);
        refused(&fx.run(), "transcript missing");
    }
}

#[test]
fn t11_a_transcript_link_is_a_refusal() {
    let fx = Fx::new();
    fx.list(&[fx.ent("working", Some("busy"))]);
    let real = fx.root.join("real.jsonl");
    fx.mk_file(&real, 30);
    let p = fx.tpath(&fx.cwd(), SID);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    symlink(&real, &p).unwrap();
    fx.age_tree(5);
    refused(&fx.run(), "transcript symlink");
}

#[test]
fn t11_a_transcript_ahead_of_the_clock_is_a_refusal() {
    let fx = Fx::new();
    fx.list(&[fx.ent("working", Some("busy"))]);
    fx.transcript(-60);
    fx.age_tree(5);
    refused(&fx.run(), "transcript 60 s ahead");
}

#[test]
fn t11_a_stalled_looking_seat_needs_a_live_pid() {
    for (pid, what) in [(None, "pid absent"), (Some("\"12x\"".to_string()), "pid not a number"), (Some(ended_pid().to_string()), "pid ended")] {
        let fx = Fx::new();
        fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), pid)]);
        fx.transcript(700);
        fx.age_tree(800);
        refused(&fx.run(), what);
    }
}

#[test]
fn t11_an_unknown_state_is_a_refusal() {
    let fx = working();
    fx.list(&[fx.ent("paused", Some("busy"))]);
    refused(&fx.run(), "state paused");
}

#[test]
fn t11_read_launches_refuses_a_session_id_that_is_not_a_uuid() {
    let fx = Fx::new();
    let bad = "../evil";
    fx.records(&launch_rows(NAME, "r1", SHORT, bad, fx.launch_ms));
    fx.list(&[fx.entry(SHORT, bad, "working", Some("busy"), &fx.cwd(), None)]);
    // the file the path would reach outside the transcript folder of the cwd, so a missing refusal would judge it working
    fs::create_dir_all(fx.tpath(&fx.cwd(), "x").parent().unwrap()).unwrap();
    fx.mk_file(&fx.root.join("tr/evil.jsonl"), 30);
    fx.age_tree(5);
    let r = fx.run();
    refused(&r, "session id with ../");
    // the refusal comes from read_launches (uuid rule); the token check of session_watch.rs has its own unit test
    assert!(r.err.contains("is not a uuid"), "unexpected refusal text: {}", r.err);
}

#[test]
fn t11_a_status_refusal_is_a_refusal() {
    let fx = Fx::new();
    fx.commit_at(900);
    fs::write(fx.root.join("tickets/t.json"), "not a ticket").unwrap();
    fx.list(&[fx.ent("done", Some("idle"))]);
    fx.transcript(30);
    refused(&fx.run(), "status refuses");
}

#[test]
fn t11_the_records_file_must_be_a_regular_file_not_a_link() {
    let fx = working();
    let real = fx.root.join("real-records.jsonl");
    fs::write(&real, launch_rows(NAME, "r1", SHORT, SID, fx.launch_ms)).unwrap();
    let link = fx.root.join("link-records.jsonl");
    symlink(&real, &link).unwrap();
    let swap = |v: &Path| {
        let mut a = fx.args(300, 600);
        let i = a.iter().position(|s| s.ends_with("records.jsonl")).unwrap();
        a[i] = v.display().to_string();
        fx.go(&a)
    };
    refused(&swap(&link), "records symlink");
    let dir = fx.root.join("recdir");
    fs::create_dir_all(&dir).unwrap();
    refused(&swap(&dir), "records folder");
    is(&swap(&real), 0, &["verdict=working"]);
}

// ------------------------------------------------------------------------------------------------ the rest

#[test]
fn t12_no_byte_of_the_transcript_is_read() {
    // every fixture here has a mode-000 transcript (mk_file fails the test when the user can still open it)
    let fx = working();
    is(&fx.run(), 0, &["verdict=working"]);
    let fx = Fx::new();
    fx.list(&[fx.ent("working", Some("busy"))]);
    fx.transcript(30);
    fx.age_tree(800);
    is(&fx.run(), 10, &["verdict=slow-alive"]);
    let fx = Fx::new();
    fx.commit_at(900);
    fx.note(true);
    fx.list(&[fx.ent("done", Some("idle"))]);
    fx.transcript(30);
    is(&fx.run(), 13, &["verdict=finished"]);
}

#[test]
fn t13_follow_ends_at_the_time_limit_on_a_slow_alive_seat() {
    let fx = Fx::new();
    fx.list(&[fx.ent("working", Some("busy"))]);
    fx.transcript(30);
    fx.age_tree(800);
    let r = fx.run_extra(&["--follow", "--poll-s", "1", "--max-s", "3"]);
    assert_eq!(r.code, 16, "{} {}", r.out, r.err);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 2, "{:?}", r.out);
    assert!(lines[0].contains("verdict=slow-alive"));
    assert!(lines[1].starts_with("verdict=watch-limit last=slow-alive max_transcript_age="), "{}", lines[1]);
    let n: u64 = lines[1].rsplit('=').next().unwrap().parse().unwrap();
    assert!((30..=90).contains(&n), "max_transcript_age {n}");
}

#[test]
fn t13_follow_ends_at_once_on_a_stopped_early_seat() {
    let fx = Fx::new();
    fx.list(&[fx.ent("done", Some("idle"))]);
    fx.transcript(30);
    let r = fx.run_extra(&["--follow", "--poll-s", "1", "--max-s", "3"]);
    is(&r, 12, &["verdict=stopped-early", "max_transcript_age=-"]);
}

#[test]
fn t14_no_output_line_holds_a_path() {
    let mut runs: Vec<(Fx, R)> = Vec::new();
    let f = working();
    let r = f.run();
    runs.push((f, r));
    let f = Fx::new();
    let r = f.run();
    runs.push((f, r));
    let f = Fx::new();
    f.list(&[f.ent("stopped", None)]);
    f.transcript(30);
    let r = f.run();
    runs.push((f, r));
    let f = Fx::new();
    f.list(&[f.ent("done", Some("idle"))]);
    f.transcript(30);
    let r = f.run();
    runs.push((f, r));
    for (f, r) in &runs {
        assert!(!r.out.is_empty());
        for p in [f.root.display().to_string(), f.cwd(), f.root.join("tr").display().to_string()] {
            assert!(!r.out.contains(&p), "{} leaks {p}", r.out);
        }
    }
}

#[test]
fn t15_the_source_opens_and_reads_no_file() {
    let src = include_str!("../src/session_watch.rs");
    for w in ["File::open", "OpenOptions", "fs::read", "read_to_string", "read_to_end"] {
        assert!(!src.contains(w), "session_watch.rs holds {w}");
    }
}

#[test]
fn t16_a_restart_judges_the_newer_record_and_entry() {
    let fx = Fx::new();
    let old_sid = "99999999-2222-4333-8444-555555555555";
    fx.records(&format!("{}{}", launch_rows(NAME, "r0", "0ld00000", old_sid, fx.launch_ms - 500_000), launch_rows(NAME, "r1", SHORT, SID, fx.launch_ms)));
    fx.list(&[fx.entry("0ld00000", old_sid, "stopped", None, &fx.cwd(), None), fx.ent("working", Some("busy"))]);
    fx.transcript(30);
    fx.age_tree(5);
    is(&fx.run(), 0, &["verdict=working", "session=ab12cd34"]);
}

#[test]
fn usage_errors_are_exit_2() {
    let fx = working();
    let base = fx.args(300, 600);
    let cut = |flag: &str| -> Vec<String> {
        let i = base.iter().position(|s| s == flag).unwrap();
        base.iter().enumerate().filter(|(j, _)| *j != i && *j != i + 1).map(|(_, s)| s.clone()).collect()
    };
    for flag in ["--host", "--records", "--name", "--ticket", "--repo-map", "--base", "--result-dir", "--caps-dir", "--transcript-root", "--idle-s", "--silence-s"] {
        assert_eq!(fx.go(&cut(flag)).code, 2, "missing {flag}");
    }
    let with = |extra: &[&str]| {
        let mut a = base.clone();
        a.extend(extra.iter().map(|s| s.to_string()));
        fx.go(&a).code
    };
    assert_eq!(with(&["--stopped-at", "1700000000"]), 2);
    assert_eq!(with(&["--stop", "stopped"]), 2);
    assert_eq!(with(&["--stopped-at", "1700000000", "--stop", "dead"]), 2);
    assert_eq!(with(&["--poll-s", "1", "--max-s", "3"]), 2);
    assert_eq!(with(&["--follow"]), 2);
    assert_eq!(with(&["--follow", "--poll-s", "0", "--max-s", "3"]), 2);
    assert_eq!(with(&["--follow", "--poll-s", "1", "--max-s", "86401"]), 2);
    assert_eq!(with(&["--name", "again"]), 2);
    assert_eq!(fx.go(&fx.args(0, 600)).code, 2);
    let mut bad = base.clone();
    let i = bad.iter().position(|s| s == &fx.base).unwrap();
    bad[i] = "abc".into();
    assert_eq!(fx.go(&bad).code, 2);
}

// ------------------------------------------------------------------------------------------------ blocked kind and the seat log

const PRIV_VARS: [&str; 3] = ["HARNESS_PRIVACY_LIST_SECRET", "HARNESS_PRIVACY_LIST", "HARNESS_PRIVACY_OLD_LIST"];

/// Runs the verb with the three privacy variables removed from the child, then the given secret list set (when there is one).
fn go_priv(fx: &Fx, a: &[String], secret: Option<&str>) -> R {
    let mut c = Command::new(BIN);
    c.args(a).stdin(Stdio::null());
    for k in PRIV_VARS {
        c.env_remove(k);
    }
    if let Some(s) = secret {
        c.env("HARNESS_PRIVACY_LIST_SECRET", s);
    }
    let _ = fx;
    go_run(&mut c)
}

/// `go_priv` of the fifo test only: the child is spawned and polled, never waited on without a bound, so a run that has not exited within
/// 30 s is killed and fails the test (an open that blocked on the fifo, the O_NONBLOCK switch-off, ends red instead of hanging the run).
fn go_priv_bounded(fx: &Fx, a: &[String], secret: Option<&str>) -> R {
    use std::io::Read as _;
    let _ = fx;
    let mut c = Command::new(BIN);
    c.args(a).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    for k in PRIV_VARS {
        c.env_remove(k);
    }
    if let Some(s) = secret {
        c.env("HARNESS_PRIVACY_LIST_SECRET", s);
    }
    let mut ch = c.spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(s) = ch.try_wait().unwrap() {
            break s;
        }
        if std::time::Instant::now() >= deadline {
            let _ = ch.kill();
            let _ = ch.wait();
            panic!("the run did not return");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let mut out = String::new();
    let mut err = String::new();
    if let Some(mut o) = ch.stdout.take() {
        o.read_to_string(&mut out).unwrap();
    }
    if let Some(mut e) = ch.stderr.take() {
        e.read_to_string(&mut err).unwrap();
    }
    R { code: status.code().unwrap_or(-1), out, err }
}

const LIST: &str = "zzdenytoken\n";

fn blocked_ent(fx: &Fx, status: Option<&str>, wf: Option<&str>) -> String {
    let mut e = fx.ent("blocked", status);
    if let Some(w) = wf {
        e.pop();
        e.push_str(&format!(",\"waitingFor\":\"{w}\"}}"));
    }
    e
}

/// A blocked seat (status and waitingFor as given) with a fresh transcript.
fn blocked(status: Option<&str>, wf: Option<&str>) -> Fx {
    let fx = Fx::new();
    fx.list(&[blocked_ent(&fx, status, wf)]);
    fx.transcript(30);
    fx
}

fn dialog() -> Fx {
    blocked(Some("waiting"), Some("permission prompt"))
}

fn asked() -> Fx {
    blocked(Some("idle"), None)
}

fn log_path(fx: &Fx) -> PathBuf {
    fx.root.join("seat.log")
}

fn perm(ts: u64, tool: &str, command: &str) -> String {
    format!("{{\"ts_ms\":{ts},\"event\":\"PermissionRequest\",\"tool\":\"{tool}\",\"command\":\"{command}\"}}\n")
}

fn stop(ts: u64, message: &str) -> String {
    format!("{{\"ts_ms\":{ts},\"event\":\"Stop\",\"message\":\"{message}\"}}\n")
}

fn with_log(fx: &Fx, body: &str, secret: Option<&str>) -> R {
    fs::write(log_path(fx), body).unwrap();
    let mut a = fx.args(300, 600);
    a.extend(["--seat-log".to_string(), log_path(fx).display().to_string()]);
    go_priv(fx, &a, secret)
}

fn lines(r: &R) -> Vec<&str> {
    r.out.lines().collect()
}

#[test]
fn t20a_a_dialog_block_shows_the_tool_and_the_command() {
    let fx = dialog();
    let r = with_log(&fx, &perm(fx.launch_ms + 1000, "Bash", "echo hi"), Some(LIST));
    assert_eq!(r.code, 12, "{} {}", r.out, r.err);
    let l = lines(&r);
    assert_eq!(l.len(), 2, "{:?}", r.out);
    for w in ["verdict=stopped-early", "why=blocked", "block=dialog", "waiting_for=permission-prompt"] {
        assert!(l[0].contains(w), "{w} missing in {}", l[0]);
    }
    assert_eq!(l[1], "detail tool=Bash command=echo hi");
}

#[test]
fn t20b_an_asked_block_shows_the_question() {
    let fx = asked();
    let r = with_log(&fx, &stop(fx.launch_ms + 1000, "Should I drop the table?"), Some(LIST));
    assert_eq!(r.code, 12, "{} {}", r.out, r.err);
    let l = lines(&r);
    assert_eq!(l.len(), 2, "{:?}", r.out);
    assert!(l[0].contains("block=asked") && l[0].contains("waiting_for=-"), "{}", l[0]);
    assert_eq!(l[1], "detail message=Should I drop the table?");
}

#[test]
fn t20c_a_record_older_than_the_launch_does_not_count() {
    let fx = dialog();
    let r = with_log(&fx, &perm(fx.launch_ms - 1, "Bash", "echo hi"), Some(LIST));
    assert_eq!(r.code, 12);
    assert_eq!(lines(&r)[1], "detail none");
    // twin: a record at the launch time counts
    let r = with_log(&fx, &perm(fx.launch_ms, "Bash", "echo hi"), Some(LIST));
    assert_eq!(lines(&r)[1], "detail tool=Bash command=echo hi");
}

#[test]
fn t20d_a_message_with_a_deny_list_token_is_withheld() {
    let fx = asked();
    let r = with_log(&fx, &stop(fx.launch_ms + 1000, "please use zzdenytoken here"), Some(LIST));
    assert_eq!(r.code, 12);
    let l = lines(&r);
    assert_eq!(l[1], "detail message=withheld");
    assert!(!r.out.contains("zzdenytoken"));
}

#[test]
fn t20e_no_privacy_list_withholds_the_text() {
    let fx = dialog();
    let r = with_log(&fx, &perm(fx.launch_ms + 1000, "Bash", "echo hi"), None);
    assert_eq!(r.code, 12);
    assert_eq!(lines(&r)[1], "detail tool=Bash command=withheld");
}

#[test]
fn t20f_a_long_two_line_message_is_one_line_of_at_most_200_bytes() {
    let fx = asked();
    let msg = format!("{}\\n{}", "x".repeat(150), "y".repeat(150));
    let r = with_log(&fx, &stop(fx.launch_ms + 1000, &msg), Some(LIST));
    assert_eq!(r.code, 12);
    let l = lines(&r);
    assert_eq!(l.len(), 2, "{:?}", r.out);
    let text = l[1].strip_prefix("detail message=").expect("a message detail");
    assert!(text.len() <= 200, "{} bytes", text.len());
    assert_eq!(text, format!("{} {}", "x".repeat(150), "y".repeat(49)));
}

#[test]
fn t20g_a_link_as_the_seat_log_is_unreadable_not_a_refusal() {
    let fx = dialog();
    let real = fx.root.join("real.log");
    fs::write(&real, perm(fx.launch_ms + 1000, "Bash", "echo hi")).unwrap();
    symlink(&real, log_path(&fx)).unwrap();
    let mut a = fx.args(300, 600);
    a.extend(["--seat-log".to_string(), log_path(&fx).display().to_string()]);
    let r = go_priv(&fx, &a, Some(LIST));
    assert_eq!(r.code, 12, "{} {}", r.out, r.err);
    assert_eq!(lines(&r)[1], "detail unreadable");
}

#[test]
fn t20h_a_working_seat_with_a_seat_log_prints_the_same_line() {
    let fx = working();
    let plain = go_priv(&fx, &fx.args(300, 600), Some(LIST));
    let r = with_log(&fx, &perm(fx.launch_ms + 1000, "Bash", "echo hi"), Some(LIST));
    assert_eq!(r.code, 0, "{} {}", r.out, r.err);
    assert_eq!(r.out.lines().count(), 1, "{:?}", r.out);
    assert_eq!(norm(&r.out), norm(&plain.out));
}

#[test]
fn t20i_a_blocked_seat_with_no_seat_log_has_no_detail_line() {
    let fx = dialog();
    let r = fx.run();
    is(&r, 12, &["block=dialog"]);
    assert!(!r.out.contains("detail"), "{:?}", r.out);
}

#[test]
fn t20j_a_fifo_as_the_seat_log_is_unreadable_and_does_not_block() {
    let fx = dialog();
    let fifo = log_path(&fx);
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    let mut a = fx.args(300, 600);
    a.extend(["--seat-log".to_string(), fifo.display().to_string()]);
    // the run is bounded, not timed: a run that has not exited within 30 s is killed and fails the test, so an open that blocked on the
    // fifo (the O_NONBLOCK switch-off) ends red in 30 s instead of hanging the run
    let r = go_priv_bounded(&fx, &a, Some(LIST));
    assert_eq!(r.code, 12, "{} {}", r.out, r.err);
    assert_eq!(lines(&r)[1], "detail unreadable");
}

#[test]
fn t20k_a_bad_seat_log_flag_is_a_usage_error() {
    let fx = dialog();
    for extra in [vec!["--seat-log", "rel/seat.log"], vec!["--seat-log", ""], vec!["--seat-log", "/tmp/a", "--seat-log", "/tmp/b"]] {
        let mut a = fx.args(300, 600);
        a.extend(extra.iter().map(|s| s.to_string()));
        let r = fx.go(&a);
        assert_eq!(r.code, 2, "{extra:?}: {} {}", r.out, r.err);
        assert!(r.err.contains("usage: harness session watch"), "{}", r.err);
        assert!(r.err.contains("--seat-log"), "{}", r.err);
    }
}

#[test]
fn t20l_a_waiting_seat_with_no_waiting_for_is_block_other() {
    let fx = blocked(Some("waiting"), None);
    let r = fx.run();
    is(&r, 12, &["why=blocked", "block=other", "waiting_for=-"]);
    let fx = blocked(Some("waiting"), Some(""));
    is(&fx.run(), 12, &["block=other", "waiting_for=-"]);
}

#[test]
fn t20l_waiting_for_is_lowercased_made_plain_and_cut_to_40() {
    let fx = blocked(Some("waiting"), Some("Permission Prompt: ABC/def 0123456789 0123456789 0123456789 0123456789"));
    let r = fx.run();
    assert_eq!(r.code, 12);
    let v = r.out.split(' ').find_map(|t| t.strip_prefix("waiting_for=")).unwrap().trim();
    assert_eq!(v, "permission-prompt--abc-def-0123456789-01");
    assert_eq!(v.len(), 40);
}

#[test]
fn t20m_follow_prints_the_detail_line_after_the_blocked_line() {
    let fx = dialog();
    fs::write(log_path(&fx), perm(fx.launch_ms + 1000, "Bash", "echo hi")).unwrap();
    let mut a = fx.args(300, 600);
    a.extend(["--follow", "--poll-s", "1", "--max-s", "3", "--seat-log"].map(String::from));
    a.push(log_path(&fx).display().to_string());
    let r = go_priv(&fx, &a, Some(LIST));
    assert_eq!(r.code, 12, "{} {}", r.out, r.err);
    let l = lines(&r);
    assert_eq!(l.len(), 2, "{:?}", r.out);
    assert!(l[0].contains("verdict=stopped-early") && l[0].contains("max_transcript_age="), "{}", l[0]);
    assert_eq!(l[1], "detail tool=Bash command=echo hi");
}

fn filler(n: usize) -> String {
    let pad = "p".repeat(900);
    (0..n).map(|_| format!("{{\"ts_ms\":1,\"event\":\"Other\",\"note\":\"{pad}\"}}\n")).collect()
}

#[test]
fn t20n_only_the_last_65536_bytes_of_the_log_are_read() {
    // a log over the window whose newest record is last
    let fx = dialog();
    let body = format!("{}{}", filler(75), perm(fx.launch_ms + 1000, "Bash", "echo last"));
    assert!(body.len() > 65536);
    let r = with_log(&fx, &body, Some(LIST));
    assert_eq!(lines(&r)[1], "detail tool=Bash command=echo last");
    // a record outside the window with a larger ts_ms is not seen; the one inside is
    let outside = perm(fx.launch_ms + 5000, "Bash", "echo outside");
    let inside = perm(fx.launch_ms + 1000, "Bash", "echo inside");
    let mut body = outside.clone();
    let room = 70000 - outside.len() - inside.len();
    body.push_str(&"q".repeat(room - 1));
    body.push('\n');
    body.push_str(&inside);
    assert_eq!(body.len(), 70000);
    let r = with_log(&fx, &body, Some(LIST));
    assert_eq!(lines(&r)[1], "detail tool=Bash command=echo inside");
}

#[test]
fn t20o_a_folder_as_the_seat_log_is_unreadable() {
    let fx = dialog();
    let d = fx.root.join("logdir");
    fs::create_dir_all(&d).unwrap();
    let mut a = fx.args(300, 600);
    a.extend(["--seat-log".to_string(), d.display().to_string()]);
    let r = go_priv(&fx, &a, Some(LIST));
    assert_eq!(r.code, 12, "{} {}", r.out, r.err);
    assert_eq!(lines(&r)[1], "detail unreadable");
}

#[test]
fn t20s_a_device_as_the_seat_log_is_unreadable() {
    // /dev/null is a character device: the open works, the regular-file check of the fstat on the opened handle does not pass
    let fx = dialog();
    let mut a = fx.args(300, 600);
    a.extend(["--seat-log".to_string(), "/dev/null".to_string()]);
    let r = go_priv(&fx, &a, Some(LIST));
    assert_eq!(r.code, 12, "{} {}", r.out, r.err);
    assert_eq!(lines(&r)[1], "detail unreadable");
}

#[test]
fn t20p_a_missing_seat_log_is_detail_none() {
    let fx = dialog();
    let mut a = fx.args(300, 600);
    a.extend(["--seat-log".to_string(), fx.root.join("nope.log").display().to_string()]);
    let r = go_priv(&fx, &a, Some(LIST));
    assert_eq!(r.code, 12);
    assert_eq!(lines(&r)[1], "detail none");
}

#[test]
fn t20q_the_newest_record_wins_and_a_block_kind_picks_the_event() {
    let body_of = |fx: &Fx| {
        format!(
            "{}{}{}garbage\n{{\"ts_ms\":1.5,\"event\":\"PermissionRequest\",\"tool\":\"X\"}}\n",
            perm(fx.launch_ms + 100, "Bash", "old"),
            perm(fx.launch_ms + 300, "Edit", "new"),
            stop(fx.launch_ms + 900, "not a dialog")
        )
    };
    let fx = dialog();
    let r = with_log(&fx, &body_of(&fx), Some(LIST));
    assert_eq!(lines(&r)[1], "detail tool=Edit command=new");
    // block=other takes the newest of either kind
    let fx = blocked(Some("waiting"), None);
    let r = with_log(&fx, &body_of(&fx), Some(LIST));
    assert_eq!(lines(&r)[1], "detail message=not a dialog");
}

#[test]
fn t20r_the_seat_log_module_builds_no_transcript_path() {
    let src = include_str!("../src/session_watch_log.rs");
    for w in ["transcript", "join(", "jsonl", "read_dir"] {
        assert!(!src.contains(w), "session_watch_log.rs holds {w}");
    }
}

// ------------------------------------------------------------------------------------------------ faulted checks in follow mode

const UNLISTED: &str = "the tree cannot be listed";
const HOST_FAULT: &str = "the host's listing could not be run or timed out";
const NOTE: &str = "harness: session watch note: ";

/// A mode-000 folder inside the tree; its mode is restored on drop. Declare it after the `Fx`, so it drops first.
struct Locked(PathBuf);

impl Drop for Locked {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o755));
    }
}

/// Made after `age_tree`; the test fails if the folder can still be read.
fn lock_folder(fx: &Fx) -> Locked {
    let l = Locked(fx.tree.join("locked"));
    fs::create_dir(&l.0).unwrap();
    fs::set_permissions(&l.0, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(fs::read_dir(&l.0).is_err(), "FAIL: the mode-000 folder can be read by this user, so no tree fault is made");
    l
}

/// The host stub counts its `agents` runs, runs `before` (the shell variable `n` is the run number), then prints the listing.
fn counted(fx: &Fx, before: &str) {
    let count = fx.root.join("agents-runs").display().to_string();
    let list = fx.root.join("list.json").display().to_string();
    fx.stub(&format!("echo x >> '{count}'\nn=$(($(wc -l < '{count}')))\n{before}\ncat '{list}'"));
}

fn runs(fx: &Fx) -> usize {
    fs::read_to_string(fx.root.join("agents-runs")).map_or(0, |s| s.lines().count())
}

fn follow_run(fx: &Fx, max: &str) -> R {
    fx.run_extra(&["--follow", "--poll-s", "1", "--max-s", max])
}

/// A note line holds the fault and the count, and no path.
fn note_line(fx: &Fx, l: &str, fault: &str, count: usize) {
    assert!(l.starts_with(NOTE), "{l}");
    assert!(l.contains(fault) && l.contains(&format!("fault {count} of 3")), "{l}");
    assert!(!l.contains(&fx.root.display().to_string()) && !l.contains('/'), "the note line holds a path: {l}");
}

/// Exit 3, empty stdout, and stderr is `notes` note lines that hold the fault, then the one refusal line that holds `msg`.
fn notes_then_refusal(fx: &Fx, r: &R, notes: usize, fault: &str, msg: &str) {
    refused(r, msg);
    let l: Vec<&str> = r.err.lines().collect();
    assert_eq!(l.len(), notes + 1, "{:?}", r.err);
    for (i, n) in l[..notes].iter().enumerate() {
        note_line(fx, n, fault, i + 1);
    }
    assert!(l[notes].starts_with("harness: session watch refused: ") && l[notes].contains(msg), "{}", l[notes]);
}

/// A follow run that kept watching to the time limit: one working line, then the watch-limit line.
fn watched_to_the_limit(r: &R) {
    assert_eq!(r.code, 16, "{} {}", r.out, r.err);
    let l = lines(r);
    assert_eq!(l.len(), 2, "{:?}", r.out);
    assert!(l[0].starts_with("verdict=working state=working/busy "), "{}", l[0]);
    assert!(l[1].starts_with("verdict=watch-limit last=working max_transcript_age="), "{}", l[1]);
}

#[test]
fn t21a_a_tree_fault_on_each_try_ends_after_the_bound() {
    let fx = working();
    counted(&fx, "");
    let _l = lock_folder(&fx);
    let r = follow_run(&fx, "30");
    notes_then_refusal(&fx, &r, 2, UNLISTED, UNLISTED);
    assert_eq!(runs(&fx), 3);
}

#[test]
fn t21b_a_host_listing_fault_on_each_try_ends_after_the_bound() {
    let fx = working();
    fs::set_permissions(fx.root.join("host.sh"), fs::Permissions::from_mode(0o644)).unwrap();
    let r = follow_run(&fx, "30");
    notes_then_refusal(&fx, &r, 2, HOST_FAULT, HOST_FAULT);
}

#[test]
fn t21c_a_tree_fault_then_success_keeps_watching() {
    let fx = working();
    let l = lock_folder(&fx);
    counted(&fx, &format!("if [ $n -ge 2 ]; then chmod 755 '{}'; fi", l.0.display()));
    let r = follow_run(&fx, "4");
    watched_to_the_limit(&r);
    let e: Vec<&str> = r.err.lines().collect();
    assert_eq!(e.len(), 1, "{:?}", r.err);
    note_line(&fx, e[0], UNLISTED, 1);
}

#[test]
fn t21d_a_host_listing_timeout_then_success_keeps_watching() {
    let fx = working();
    counted(&fx, "if [ $n -eq 1 ]; then sleep 30; fi");
    let r = follow_run(&fx, "15");
    watched_to_the_limit(&r);
    let e: Vec<&str> = r.err.lines().collect();
    assert_eq!(e.len(), 1, "{:?}", r.err);
    note_line(&fx, e[0], HOST_FAULT, 1);
}

#[test]
fn t21e_the_host_state_failed_ends_at_once() {
    let fx = working();
    fx.list(&[fx.ent("failed", Some("busy"))]);
    counted(&fx, "");
    let r = follow_run(&fx, "30");
    notes_then_refusal(&fx, &r, 0, "", "the host state failed is not one this verb judges");
    assert_eq!(runs(&fx), 1);
}

#[test]
fn t21f_other_real_refusals_are_final_on_first_sight() {
    let fx = working();
    let cwd = other_dir(&fx);
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &cwd, None)]);
    counted(&fx, "");
    let r = follow_run(&fx, "30");
    notes_then_refusal(&fx, &r, 0, "", "the entry cwd is not the tree");
    assert_eq!(runs(&fx), 1);
    let fx = working();
    counted(&fx, "exit 1");
    let r = follow_run(&fx, "30");
    notes_then_refusal(&fx, &r, 0, "", "the host's listing exited non-zero");
    assert_eq!(runs(&fx), 1);
}

#[test]
fn t21g_once_does_not_retry() {
    let fx = working();
    counted(&fx, "");
    let _l = lock_folder(&fx);
    let r = fx.run();
    notes_then_refusal(&fx, &r, 0, "", UNLISTED);
    assert_eq!(runs(&fx), 1);
}

#[test]
fn t21h_faults_that_alternate_with_good_checks_do_not_end_the_run() {
    let fx = working();
    let l = lock_folder(&fx);
    let d = l.0.display();
    counted(&fx, &format!("if [ $n -le 5 ] && [ $((n % 2)) -eq 1 ]; then chmod 000 '{d}'; else chmod 755 '{d}'; fi"));
    let r = follow_run(&fx, "10");
    watched_to_the_limit(&r);
    let e: Vec<&str> = r.err.lines().collect();
    assert_eq!(e.len(), 3, "{:?}", r.err);
    for n in e {
        note_line(&fx, n, UNLISTED, 1);
    }
}

#[test]
fn t21i_the_time_limit_during_faults_is_the_refusal_of_the_fault() {
    let fx = working();
    let _l = lock_folder(&fx);
    let r = follow_run(&fx, "1");
    refused(&r, "time limit during faults");
    let last = r.err.lines().last().unwrap();
    assert!(last.starts_with("harness: session watch refused: ") && last.contains(UNLISTED), "{last}");
    // the one fault is followed by the time limit, so it gets no note line
    assert!(!r.err.contains(NOTE), "{}", r.err);
}

#[test]
fn t21j_a_missing_tree_root_is_final_at_once() {
    // the cwd test sees the missing tree before the walk does; the walk's own root case has a unit test in session_watch.rs
    let fx = working();
    counted(&fx, "");
    fs::rename(&fx.tree, fx.root.join("tree-moved")).unwrap();
    let r = follow_run(&fx, "30");
    notes_then_refusal(&fx, &r, 0, "", "the entry cwd is not the tree");
    assert_eq!(runs(&fx), 1);
}

#[test]
fn t21k_a_real_refusal_after_a_fault_ends_the_run() {
    let fx = working();
    let cwd = other_dir(&fx);
    let moved = fx.root.join("moved.json");
    fs::write(&moved, format!("[{}]", fx.entry(SHORT, SID, "working", Some("busy"), &cwd, None))).unwrap();
    counted(&fx, &format!("if [ $n -ge 2 ]; then cat '{}'; exit 0; fi", moved.display()));
    let _l = lock_folder(&fx);
    let r = follow_run(&fx, "30");
    notes_then_refusal(&fx, &r, 1, UNLISTED, "the entry cwd is not the tree");
    assert_eq!(runs(&fx), 2);
}

// ------------------------------------------------------------------------------------------------ the child check

/// A seat busy in a long tool call: the entry shell waits on a shell that runs a two-minute `sleep`, so the sleep is at depth 2.
const BUSY: &str = "sh -c 'sh -c \"sleep 120; :\" & wait'";
/// A seat that holds the wait in a sleep of its own while another shell runs a two-minute sleep.
const HELD: &str = "sh -c 'sh -c \"sleep 120; :\" & sleep 120; wait'";
const UNREAD: &str = "harness: session watch refused: the process list cannot be read\n";

/// The busy seat of `BUSY`, once its shape was seen: the middle shell at depth 1, its two-minute sleep at depth 2, nothing else,
/// both just started. So the fresh process the verdict counts is at depth 2, whatever shell `sh` is.
fn busy() -> Tree {
    let p = tree(BUSY);
    shaped(p.0.id(), &[1, 2]);
    p
}

/// A working seat past both limits whose entry is `pid`: transcript 700 s old, tree 800 s old.
fn past(fx: &Fx, pid: u32) {
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(pid.to_string()))]);
    fx.transcript(700);
    fx.age_tree(800);
}

/// The stdout holds no path of the fixture and no command name of a process under the seat.
fn no_names(fx: &Fx, out: &str) {
    for p in [fx.root.display().to_string(), fx.cwd(), fx.root.join("tr").display().to_string(), "sleep".to_string(), "perl".to_string(), " sh ".to_string(), "sh -c".to_string()] {
        assert!(!out.contains(&p), "the output holds {p:?}: {out}");
    }
}

#[test]
fn t22_a_busy_seat_in_a_long_tool_call_is_slow_alive_by_its_child() {
    let fx = Fx::new();
    let p = busy();
    past(&fx, p.0.id());
    let r = fx.go(&fx.args(720, 600));
    is(&r, 10, &["verdict=slow-alive", "why=child", "state=working/busy", "ps_skip=0"]);
    num(&r, "child_age=", 0, 30);
    num(&r, "transcript_age=", 700, 760);
    no_names(&fx, &r.out);
    fx.no_host_writes();
}

#[test]
fn t22b_a_child_older_than_the_transcript_time_and_the_slack_does_not_count() {
    let fx = Fx::new();
    let p = busy();
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.age_tree(800);
    // the depth-2 sleep is over 4 s old; the transcript is 1 s old
    std::thread::sleep(Duration::from_secs(5));
    fx.mk_file(&fx.tpath(&fx.cwd(), SID), 1);
    // the same two processes under the seat, the sleep still at depth 2
    shaped(p.0.id(), &[1, 2]);
    let r = fx.run_extra_slack(1, 1, 1);
    is(&r, 11, &["verdict=stalled", "ps_skip=0"]);
    assert!(!r.out.contains("why="), "{}", r.out);
    // twin: the same seat with the default slack of 60 s is busy by its child
    is(&fx.run_extra_slack(720, 1, 60), 10, &["why=child"]);
}

#[test]
fn t22c_a_fresh_helper_at_depth_1_does_not_count() {
    // the entry shell runs a 3 s helper, then a two-minute sleep: the only fresh process under it is at depth 1, with no child
    let fx = Fx::new();
    let p = tree("sh -c 'sleep 3; sleep 120 & wait'");
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.age_tree(800);
    fx.mk_file(&fx.tpath(&fx.cwd(), SID), 0);
    std::thread::sleep(Duration::from_millis(3500));
    // the two-minute sleep alone at depth 1, the 3 s helper gone: nothing at depth 2
    shaped(p.0.id(), &[1]);
    let r = fx.run_extra_slack(720, 1, 1);
    is(&r, 11, &["verdict=stalled"]);
    assert!(!r.out.contains("why="), "{}", r.out);
}

#[test]
fn t22d_the_child_check_comes_after_finished_and_the_two_windows() {
    // a quiet passed seat is finished, with a fresh child at depth 2 under it
    let p = busy();
    let fx = Fx::new();
    fx.commit_at(900);
    fx.note(true);
    past(&fx, p.0.id());
    let r = fx.run();
    is(&r, 13, &["verdict=finished", "gate=pass/yes"]);
    assert!(!r.out.contains("why=") && !r.out.contains("ps_skip="), "{}", r.out);
    // a fresh tree is working
    let p = busy();
    let fx = Fx::new();
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.transcript(700);
    fx.age_tree(5);
    let r = fx.go(&fx.args(720, 600));
    is(&r, 0, &["verdict=working"]);
    assert!(!r.out.contains("why=") && !r.out.contains("ps_skip="), "{}", r.out);
    // a fresh transcript is the plain slow-alive
    let p = busy();
    let fx = Fx::new();
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.transcript(30);
    fx.age_tree(800);
    let r = fx.go(&fx.args(720, 600));
    is(&r, 10, &["verdict=slow-alive"]);
    assert!(!r.out.contains("why=") && !r.out.contains("ps_skip="), "{}", r.out);
}

#[test]
fn t22f_a_process_list_that_cannot_be_read_is_a_refusal() {
    for on_a in ["exit 1", "printf 'x y z\\n'", ":"] {
        let fx = Fx::new();
        let path = fx.ps_stub(on_a);
        let p = live();
        past(&fx, p.0.id());
        let r = fx.go_path(&fx.args(720, 600), &path);
        assert_eq!(r.code, 3, "{on_a}: stdout {:?} stderr {:?}", r.out, r.err);
        assert!(r.out.is_empty() && !r.err.contains("slow-alive") && !r.err.contains("stalled"), "{on_a}: {:?}", r.out);
        assert_eq!(r.err, UNREAD, "{on_a}");
        // follow mode: final on the first check
        let mut a = fx.args(720, 600);
        a.extend(["--follow", "--poll-s", "1", "--max-s", "3"].map(String::from));
        let r = fx.go_path(&a, &path);
        assert_eq!(r.code, 3, "{on_a}, follow: stdout {:?} stderr {:?}", r.out, r.err);
        assert!(r.out.is_empty(), "{on_a}, follow: {:?}", r.out);
        assert_eq!(r.err, UNREAD, "{on_a}, follow");
    }
}

#[test]
fn t22g_a_bad_row_of_the_process_list_is_skipped_and_counted() {
    // entry 100, its helper 101, the helper's 3 s old child 102, and one row that does not parse
    let fx = Fx::new();
    let path = fx.ps_stub("printf '100 1 10:00 S\\n101 100 00:03 S\\n102 101 00:03 S\\nx y z\\n'");
    past(&fx, 100);
    let r = fx.go_path(&fx.args(720, 600), &path);
    is(&r, 10, &["verdict=slow-alive", "why=child", "ps_skip=1"]);
    num(&r, "child_age=", 3, 3);
    // a helper alone under the entry (the 3 s old child gone): stalled, no row skipped
    let path = fx.ps_stub("printf '100 1 10:00 S\\n101 100 00:03 S\\n'");
    let r = fx.go_path(&fx.args(720, 600), &path);
    is(&r, 11, &["verdict=stalled", "ps_skip=0"]);
    assert!(!r.out.contains("why="), "{}", r.out);
}

#[test]
fn t22h_child_slack_s_is_1_to_86400_and_given_once() {
    let fx = working();
    for extra in [
        vec!["--child-slack-s", "0"],
        vec!["--child-slack-s", "x"],
        vec!["--child-slack-s", "86401"],
        vec!["--child-slack-s", "1", "--child-slack-s", "2"],
        vec!["--child-slack-s"],
    ] {
        let r = fx.run_extra(&extra);
        assert_eq!(r.code, 2, "{extra:?}: stdout {:?} stderr {:?}", r.out, r.err);
        assert!(r.err.contains("usage: harness session watch") && r.err.contains("[--child-slack-s <n>]"), "{}", r.err);
    }
    is(&fx.run_extra(&["--child-slack-s", "1"]), 0, &["verdict=working"]);
    is(&fx.run_extra(&["--child-slack-s", "86400"]), 0, &["verdict=working"]);
}

#[test]
fn t22i_follow_holds_the_wait_while_the_child_is_fresh() {
    // the entry shell waits in a sleep of its own on a shell that runs a two-minute sleep: two processes at depth 1 (the shell
    // and the entry's own sleep), the two-minute sleep at depth 2
    let fx = Fx::new();
    let p = tree(HELD);
    shaped(p.0.id(), &[1, 1, 2]);
    past(&fx, p.0.id());
    let r = follow_run(&fx, "3");
    assert_eq!(r.code, 16, "{} {}", r.out, r.err);
    let l = lines(&r);
    assert_eq!(l.len(), 2, "{:?}", r.out);
    assert!(l[0].contains("verdict=slow-alive") && l[0].contains("why=child"), "{}", l[0]);
    assert!(l[1].starts_with("verdict=watch-limit last=slow-alive "), "{}", l[1]);
    no_names(&fx, &r.out);
    // the depth-2 sleep killed: the seat is alive in its own sleep with nothing at depth 2, and is stalled
    let fx = Fx::new();
    let p = tree(HELD);
    shaped(p.0.id(), &[1, 1, 2]);
    past(&fx, p.0.id());
    let prey: Vec<u32> = under(p.0.id()).into_iter().filter(|x| x.1 == 2).map(|x| x.0).collect();
    assert_eq!(prey.len(), 1, "the fixture must have one live process at depth 2 under the seat");
    for pid in prey {
        kill(pid);
    }
    // the entry's own sleep alone is left: the seat is alive, and no live process is at depth 2
    shaped(p.0.id(), &[1]);
    let r = fx.run();
    is(&r, 11, &["verdict=stalled"]);
    assert!(!r.out.contains("why="), "{}", r.out);
    no_names(&fx, &r.out);
}

#[test]
fn t22j_follow_prints_a_line_when_the_why_changes() {
    let fx = Fx::new();
    let p = busy();
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some(p.0.id().to_string()))]);
    fx.age_tree(800);
    // the transcript is as new as the child: the seat is the plain slow-alive until 2 s pass, then busy by its child
    fx.mk_file(&fx.tpath(&fx.cwd(), SID), 0);
    let mut a = fx.args(720, 2);
    a.extend(["--follow", "--poll-s", "1", "--max-s", "5"].map(String::from));
    let r = fx.go(&a);
    assert_eq!(r.code, 16, "{} {}", r.out, r.err);
    let l = lines(&r);
    assert_eq!(l.len(), 3, "{:?}", r.out);
    assert!(l[0].contains("verdict=slow-alive") && !l[0].contains("why="), "{}", l[0]);
    assert!(l[1].contains("verdict=slow-alive") && l[1].contains("why=child"), "{}", l[1]);
    assert!(l[2].starts_with("verdict=watch-limit last=slow-alive "), "{}", l[2]);
    no_names(&fx, &r.out);
}

#[test]
fn t22l_a_zombie_under_the_seat_does_not_count() {
    // perl (depth 1) forks a child that ends at once and is never reaped: a zombie at depth 2, on the list with an etime
    let fx = Fx::new();
    let p = tree("sh -c 'perl -e \"if(fork()==0){exit 0} sleep 120\"; :'");
    past(&fx, p.0.id());
    // perl is the one live process under the seat, at depth 1; the zombie is the child of a live child of the seat
    wait_until("perl alone at depth 1 and its zombie at depth 2", || {
        depths(p.0.id()) == vec![1] && ps_rows().iter().any(|r| r.2.starts_with('Z') && children(p.0.id()).contains(&r.1))
    });
    let r = fx.run();
    is(&r, 11, &["verdict=stalled"]);
    assert!(!r.out.contains("why="), "{}", r.out);
}

#[test]
fn t22m_the_entry_pid_1_is_not_walked() {
    // the seat's own tree hangs under pid 1 too, so walking from 1 would find this fixture's fresh depth-2 sleep
    let fx = Fx::new();
    let _p = busy();
    past(&fx, 1);
    let r = fx.run();
    is(&r, 11, &["verdict=stalled"]);
    assert!(!r.out.contains("why="), "{}", r.out);
}

#[test]
fn t22n_the_watcher_skips_its_own_subtree() {
    let fx = Fx::new();
    let pidfile = fx.root.join("seat.pid");
    fx.list(&[fx.entry(SHORT, SID, "working", Some("busy"), &fx.cwd(), Some("PIDPLACEHOLDER".to_string()))]);
    fx.stub(&format!("sed \"s/PIDPLACEHOLDER/$(cat '{}')/\" '{}'", pidfile.display(), fx.root.join("list.json").display()));
    fx.transcript(700);
    fx.age_tree(800);
    // the entry shell writes its pid, then its child shell execs the verb: the verb and its ps child are under the entry. The
    // verb's exit status is the shell's (`exit $?`, not a trailing `:`). The entry shell runs three commands, so no shell
    // replaces itself here, and the inner `exec` is the same on every shell: the tree is the same under bash and under dash.
    let mut c = Command::new("sh");
    c.arg("-c")
        .arg(format!("echo $$ > '{}'; sh -c 'exec \"$0\" \"$@\"' \"$@\"; exit $?", pidfile.display()))
        .arg("harness")
        .arg(BIN)
        .args(fx.args(720, 600))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut p = Tree(c.spawn().unwrap());
    let (mut out, mut err) = (String::new(), String::new());
    {
        use std::io::Read as _;
        p.0.stdout.take().unwrap().read_to_string(&mut out).unwrap();
        p.0.stderr.take().unwrap().read_to_string(&mut err).unwrap();
    }
    let code = p.0.wait().unwrap().code().unwrap_or(-1);
    let r = R { code, out, err };
    is(&r, 11, &["verdict=stalled"]);
    assert!(!r.out.contains("why="), "{}", r.out);
    no_names(&fx, &r.out);
}

#[test]
fn t22o_a_fresh_process_40_levels_down_is_a_child() {
    // 40 nested shells by one script (`$1` is the script, `$2` the levels left), the sleep at the bottom: depth 41 under the seat.
    // The arguments reach the script (`$1` and `$2`) through the exec of `tree_n`; each shell waits on the next, so every level
    // is a forced fork.
    const DEEP: &str = "n=$2; if [ \"$n\" -gt 0 ]; then sh -c \"$1\" sh \"$1\" $(($n - 1)); else sleep 120; fi; :";
    let fx = Fx::new();
    let p = tree_n(DEEP, &["sh", DEEP, "40"]);
    shaped(p.0.id(), &(1..=41).collect::<Vec<u32>>());
    past(&fx, p.0.id());
    let r = fx.run();
    is(&r, 10, &["verdict=slow-alive", "why=child", "ps_skip=0"]);
    num(&r, "child_age=", 0, 30);
}

#[test]
fn t22p_a_process_reparented_away_from_the_seat_does_not_count() {
    let fx = Fx::new();
    // the first child shell ends at once, so its sleep is adopted elsewhere: no longer under the entry. What is left under the
    // entry is its own two-minute sleep, at depth 1 (the trailing `:` keeps the entry a shell around it on every OS).
    let pidfile = fx.root.join("orphan.pid");
    let p = tree(&format!("sh -c 'sleep 120 & echo $! > \"{}\"'; sleep 120; :", pidfile.display()));
    past(&fx, p.0.id());
    let written = || fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<u32>().ok());
    wait_until("the pid of the orphan", || written().is_some());
    let _k = Kill(written().unwrap());
    wait_until("the sleep leaves the tree of the seat", || !family(p.0.id()).contains(&_k.0));
    shaped(p.0.id(), &[1]);
    let r = fx.run();
    is(&r, 11, &["verdict=stalled"]);
    assert!(!r.out.contains("why="), "{}", r.out);
}

#[test]
fn t22q_a_process_list_over_its_cap_is_a_refusal() {
    let fx = Fx::new();
    let rows = fx.root.join("rows");
    fs::write(&rows, (0..=harness::session_watch::PROC_CAP).map(|i| format!("{} 1 00:01 S\n", 10_000 + i)).collect::<String>()).unwrap();
    let path = fx.ps_stub(&format!("cat '{}'", rows.display()));
    past(&fx, 100);
    let r = fx.go_path(&fx.args(720, 600), &path);
    assert_eq!(r.code, 3, "stdout {:?} stderr {:?}", r.out, r.err);
    assert!(r.out.is_empty(), "{:?}", r.out);
    assert_eq!(r.err, "harness: session watch refused: the process list is over its cap\n");
}
