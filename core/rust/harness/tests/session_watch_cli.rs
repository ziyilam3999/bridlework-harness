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

    fn stub(&self, body: &str) {
        let p = self.root.join("host.sh");
        let calls = self.root.join("calls");
        let text = format!("#!/bin/sh\nif [ \"$1\" != agents ]; then echo \"$1\" >> '{}'; exit 1; fi\n{body}\n", calls.display());
        fs::write(&p, text).unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
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

    /// Runs the verb. A stub script written a moment ago can meet a busy-file error at exec when another test forks; that run is repeated.
    fn go(&self, a: &[String]) -> R {
        let mut tries = 0;
        loop {
            let o = Command::new(BIN).args(a).stdin(Stdio::null()).output().unwrap();
            let r = R { code: o.status.code().unwrap_or(-1), out: String::from_utf8_lossy(&o.stdout).into(), err: String::from_utf8_lossy(&o.stderr).into() };
            tries += 1;
            if r.code == 3 && r.err.contains("could not be run or timed out") && tries < 8 {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
            return r;
        }
    }

    fn run(&self) -> R {
        self.go(&self.args(300, 600))
    }

    fn run_extra(&self, extra: &[&str]) -> R {
        let mut a = self.args(300, 600);
        a.extend(extra.iter().map(|s| s.to_string()));
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
