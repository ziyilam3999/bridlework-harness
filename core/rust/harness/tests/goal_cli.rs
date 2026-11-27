//! Process-level tests for `harness goal add | list | show | close | check` (ticket goal-store, WAITING-228).
//! The checks run the real binary as a child process and read the real store file, so the exit-code contract
//! (0 ok, 1 a refused input, 2 usage, setup or a damaged store) is tested end to end. Rows, markers and the
//! deny words are synthetic; the privacy list is injected on the child only, and no test reads a deny-list
//! variable or path.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
/// A synthetic deny word: the ONLY entry of the injected list. No clean fixture may contain it.
const DENY: &str = "zzdenyzz";
/// A synthetic word that is clean under `DENY`, so a row may hold it, and is a deny word under its own list.
const LATE_DENY: &str = "zqlatezq";
static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

const T0: &str = "2026-11-26T23:49:00Z";
const T1: &str = "2026-11-27T01:16:00Z";
const T2: &str = "2026-11-27T02:00:00Z";
const PTR: &str = "pr:owner/repo#7";
const PTR2: &str = "pr:owner/repo#9";
const TOKEN_LINE: &str = "harness: goal refused: id must be a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not . or ..";
const KEY_SET_LINE: &str = "harness: error: the goal store has a bad row at line 1: the keys are not the key set of its op";
const NOT_REGULAR: &str = "harness: error: the goal store is not a regular file";

/// The head of the top-level usage line, pinned before this ticket's verb was registered: the text of
/// cli.rs from `usage: harness --version` up to and including `owed add|list|close ...`.
const USAGE_HEAD: &str = "usage: harness --version | privacy classes | privacy scan|explain <mode> | friction add|lint|patterns|close-check | status <ticket-file> ... | process start|stop|check|step <ticket> ... | evidence show|check|quote <ticket> ... | workspace create <ticket-file> ... | workspace check <ticket> ... | controls approval-message|write ... | permission approval-message|write ... | session check-name|record-start ... | lesson lint <path> ... | lesson lookup <lessons-dir> --ticket <ticket-file> --scope-file <file> | lane claim|release|list|admission-check ... | friction open|sweep|rank|suggest|triage|bind|page|controls-check|set-status ... | owed add|list|close ...";

const GOAL_USAGE: &str = "usage: harness goal add --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --title <text> --done-when <line> --source <pointers> | goal list --store <absolute-file> [--open] | goal show --store <absolute-file> --id <id> | goal close --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --status <met|dropped> (--ref <pointers> | --reason <text>) | goal check --store <absolute-file> --id <id>";

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("goal-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// The store, created once by hand as an empty file (no verb may create it).
    fn store(&self) -> PathBuf {
        self.named("goal.jsonl", "")
    }

    fn named(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.0.join(name);
        fs::write(&f, body).unwrap();
        f
    }

    /// A byte copy of the store made before a call, to compare the store against afterwards.
    fn copy_of(&self, store: &Path) -> Vec<u8> {
        let c = self.0.join("copy.jsonl");
        fs::copy(store, &c).unwrap();
        fs::read(&c).unwrap()
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The child with no config or list variable inherited: inputs arrive as flags or an explicit `.env`.
fn bare(program: &str) -> Command {
    let mut c = Command::new(program);
    c.env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

/// The usual child program: the harness binary.
fn base() -> Command {
    bare(BIN)
}

/// The usual child: the synthetic deny list injected as the secret.
fn run(args: &[String]) -> Output {
    run_with(args, DENY)
}

fn run_with(args: &[String], list: &str) -> Output {
    let mut c = base();
    c.args(args).env("HARNESS_PRIVACY_LIST_SECRET", list).stdin(Stdio::null());
    c.output().unwrap()
}

/// The child with no privacy list at all.
fn run_no_list(args: &[String]) -> Output {
    let mut c = base();
    c.args(args).stdin(Stdio::null());
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

fn s(v: &str) -> String {
    v.to_string()
}

fn ss(p: &Path) -> String {
    p.to_str().unwrap().to_string()
}

/// Exit 2, one exact stderr line, nothing on stdout.
fn refused2(o: &Output, line: &str) {
    assert_eq!(code(o), 2, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty: {}", out(o));
    assert_eq!(err(o), format!("{line}\n"), "stderr must be one exact line");
}

/// Exit 1, one exact stderr line, nothing on stdout.
fn refused1(o: &Output, line: &str) {
    assert_eq!(code(o), 1, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty: {}", out(o));
    assert_eq!(err(o), format!("{line}\n"), "stderr must be one exact line");
}

/// The store is byte-identical to the copy made before the call.
fn cmp(store: &Path, copy: &[u8]) {
    assert_eq!(fs::read(store).ok(), Some(copy.to_vec()), "a refused call changed the store");
}

/// A refused call: the exit code and one exact stderr line, and the store byte-identical to its copy.
fn refused_unchanged(store: &Path, o: &Output, want: i32, line: &str, copy: &[u8]) {
    if want == 1 {
        refused1(o, line);
    } else {
        refused2(o, line);
    }
    cmp(store, copy);
}

fn add_args(store: &Path, id: &str, at: &str, by: &str, title: &str, done: &str, source: &str) -> Vec<String> {
    vec![
        s("goal"), s("add"), s("--store"), ss(store), s("--id"), s(id), s("--at"), s(at), s("--by"), s(by), s("--title"), s(title), s("--done-when"), s(done), s("--source"), s(source),
    ]
}

fn close_args(store: &Path, id: &str, at: &str, by: &str, status: &str, r: &str, reason: &str) -> Vec<String> {
    let mut a = vec![s("goal"), s("close"), s("--store"), ss(store), s("--id"), s(id), s("--at"), s(at), s("--by"), s(by), s("--status"), s(status)];
    if !r.is_empty() || !reason.is_empty() {
        // an empty value is still a given flag (the pairing is judged on values)
        a.push(s(if reason.is_empty() { "--ref" } else { "--reason" }));
        a.push(s(if reason.is_empty() { r } else { reason }));
    }
    a
}

fn list_args(store: &Path) -> Vec<String> {
    vec![s("goal"), s("list"), s("--store"), ss(store)]
}

fn show_args(store: &Path, id: &str) -> Vec<String> {
    vec![s("goal"), s("show"), s("--store"), ss(store), s("--id"), s(id)]
}

fn check_args(store: &Path, id: &str) -> Vec<String> {
    vec![s("goal"), s("check"), s("--store"), ss(store), s("--id"), s(id)]
}

/// A valid add of `gs-video-1` (the id every planted store holds).
fn seed(store: &Path) -> Output {
    run(&add_args(store, "gs-video-1", T0, "orch-a", "video 1 ships", "the first video is published", PTR))
}

/// The five verbs, each called with valid flags, on one store path, under one injected list.
fn every_verb(store: &Path, list: &str) -> Vec<Output> {
    let calls = [
        add_args(store, "gs-new", T2, "orch-a", "a second goal", "the line is done", PTR),
        list_args(store),
        show_args(store, "gs-video-1"),
        close_args(store, "gs-video-1", T2, "orch-a", "met", PTR2, ""),
        check_args(store, "gs-video-1"),
    ];
    calls.iter().map(|a| run_with(a, list)).collect()
}

/// Every verb on a damaged store: exit 2, the one exact stderr line, nothing on stdout (so `check` never
/// prints `goal ok`), and the store byte-identical to what it held before the calls. The bytes are compared
/// only for a regular file: reading a fifo or a folder would block or fail for its own reason.
fn all_verbs_refused(store: &Path, line: &str) {
    let regular = fs::symlink_metadata(store).is_ok_and(|m| m.is_file());
    let before = if regular { Some(fs::read(store).unwrap()) } else { None };
    for (i, o) in every_verb(store, DENY).iter().enumerate() {
        assert_eq!(code(o), 2, "verb {i}: {}", err(o));
        assert_eq!(err(o), format!("{line}\n"), "verb {i}");
        assert!(out(o).is_empty(), "verb {i} printed on a damaged store: {}", out(o));
    }
    if regular {
        assert_eq!(Some(fs::read(store).unwrap()), before, "a refused call changed the store");
    }
}

/// One planted add row (with its newline) for a hand-written store.
fn planted_add(id: &str, at: &str, title: &str, done: &str, source: &str) -> String {
    format!("{{\"op\":\"add\",\"id\":\"{id}\",\"at\":\"{at}\",\"by\":\"orch-a\",\"title\":\"{title}\",\"done_when\":\"{done}\",\"source\":\"{source}\"}}\n")
}

/// One planted close row (with its newline) for a hand-written store.
fn planted_close(id: &str, at: &str, status: &str, r: &str, reason: &str) -> String {
    format!("{{\"op\":\"close\",\"id\":\"{id}\",\"at\":\"{at}\",\"by\":\"orch-a\",\"status\":\"{status}\",\"ref\":\"{r}\",\"reason\":\"{reason}\"}}\n")
}

const ADD1: &str = r#"{"op":"add","id":"gs-video-1","at":"2026-11-26T23:49:00Z","by":"orch-a","title":"video 1 ships","done_when":"the first video is published","source":"pr:owner/repo#7"}"#;

/// One good planted add row.
fn add1() -> String {
    format!("{ADD1}\n")
}

/// A home-path shape, assembled at run time so the file holds no literal one.
fn home_path() -> String {
    ["/Us", "ers/", "zztester", "/proj"].concat()
}

/// A store of three goals (open, met, dropped), built through the binary.
fn three_goals(store: &Path) {
    assert_eq!(code(&seed(store)), 0);
    let o = run(&add_args(store, "gs-harness", T0, "orch-a", "harness runs clean", "the gate passes on the new head", "~/notes/ask-228"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = run(&add_args(store, "bw-harness-running", T0, "orch-a", "bw keeps a seat running", "a seat ends by itself", "cmd:harness goal list"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = run(&close_args(store, "gs-harness", T1, "orch-a", "met", PTR2, ""));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = run(&close_args(store, "bw-harness-running", T1, "orch-a", "dropped", "", "folded into the harness goal"));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

// ---------------------------------------------------------------- the pinned usage line

/// The bare binary prints the top-level usage line and exits 2; the pinned head must survive this ticket's
/// verb byte for byte, and the line names the goal verbs.
#[test]
fn usage_line_names_the_goal_verbs() {
    let o = Command::new(BIN).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(out(&o).is_empty());
    let e = err(&o);
    assert!(e.starts_with(USAGE_HEAD), "stderr: {e}");
    assert!(e.contains("| goal add|list|show|close|check ..."), "stderr: {e}");
}

// ---------------------------------------------------------------- the worked case (exact bytes)

#[test]
fn worked_case_bytes() {
    let t = Tmp::new();
    let store = t.store();
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "video 1 ships", "the first video is published", PTR));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal added id=gs-video-1 state=open\n");
    assert!(err(&o).is_empty());
    assert_eq!(fs::read_to_string(&store).unwrap(), add1());
    let o = run(&check_args(&store, "gs-video-1"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal ok id=gs-video-1 state=open\n");
    assert!(err(&o).is_empty());
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "met", PTR2, ""));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal closed id=gs-video-1 state=met\n");
    assert!(err(&o).is_empty());
    let body = fs::read_to_string(&store).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0], ADD1);
    assert_eq!(lines[1], r#"{"op":"close","id":"gs-video-1","at":"2026-11-27T01:16:00Z","by":"orch-a","status":"met","ref":"pr:owner/repo#9","reason":""}"#);
    let o = run(&show_args(&store, "gs-video-1"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal id=gs-video-1 state=met\ntitle: video 1 ships\ndone_when: the first video is published\nadded 2026-11-26T23:49:00Z by orch-a source: pr:owner/repo#7\nclosed 2026-11-27T01:16:00Z by orch-a ref: pr:owner/repo#9\n");
    assert!(err(&o).is_empty());
}

// ---------------------------------------------------------------- list and show (exact stdout)

#[test]
fn list_and_show_of_three_goals() {
    let t = Tmp::new();
    let store = t.store();
    three_goals(&store);
    let o = run(&list_args(&store));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal id=gs-video-1 state=open\ngoal id=gs-harness state=met\ngoal id=bw-harness-running state=dropped\ngoal open=1 met=1 dropped=1\n");
    assert!(err(&o).is_empty());
    let mut a = list_args(&store);
    a.push(s("--open"));
    let o = run(&a);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal id=gs-video-1 state=open\ngoal open=1\n");
    assert!(err(&o).is_empty());
    let o = run(&show_args(&store, "gs-video-1"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal id=gs-video-1 state=open\ntitle: video 1 ships\ndone_when: the first video is published\nadded 2026-11-26T23:49:00Z by orch-a source: pr:owner/repo#7\n");
    assert!(err(&o).is_empty());
    let o = run(&show_args(&store, "gs-harness"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "goal id=gs-harness state=met\ntitle: harness runs clean\ndone_when: the gate passes on the new head\nadded 2026-11-26T23:49:00Z by orch-a source: ~/notes/ask-228\nclosed 2026-11-27T01:16:00Z by orch-a ref: pr:owner/repo#9\n"
    );
    assert!(err(&o).is_empty());
    let o = run(&show_args(&store, "bw-harness-running"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(
        out(&o),
        "goal id=bw-harness-running state=dropped\ntitle: bw keeps a seat running\ndone_when: a seat ends by itself\nadded 2026-11-26T23:49:00Z by orch-a source: cmd:harness goal list\nclosed 2026-11-27T01:16:00Z by orch-a reason: folded into the harness goal\n"
    );
    assert!(err(&o).is_empty());
    // an unknown id: the refusal line is on stdout, stderr is empty
    let o = run(&show_args(&store, "gs-nope"));
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "goal refused id=gs-nope reason=unknown\n");
    assert!(err(&o).is_empty());
}

#[test]
fn list_of_an_empty_store() {
    let t = Tmp::new();
    let store = t.store();
    let o = run(&list_args(&store));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal open=0 met=0 dropped=0\n");
    assert!(err(&o).is_empty());
    let mut a = list_args(&store);
    a.push(s("--open"));
    let o = run(&a);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal open=0\n");
    assert!(err(&o).is_empty());
}

// ---------------------------------------------------------------- check

#[test]
fn check_of_an_open_goal() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let o = run(&check_args(&store, "gs-video-1"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal ok id=gs-video-1 state=open\n");
    assert!(err(&o).is_empty());
    cmp(&store, &copy);
}

#[test]
fn check_refuses_a_closed_goal() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "dropped", "", "set aside"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let copy = t.copy_of(&store);
    let o = run(&check_args(&store, "gs-video-1"));
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "goal refused id=gs-video-1 reason=closed\n");
    assert!(err(&o).is_empty());
    cmp(&store, &copy);
}

#[test]
fn check_refuses_an_unknown_goal() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let o = run(&check_args(&store, "gs-nope"));
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "goal refused id=gs-nope reason=unknown\n");
    assert!(err(&o).is_empty());
}

/// A malformed id is refused on stderr, the value not echoed, before the store is read: the store path names
/// no file, and the id refusal comes first.
#[test]
fn a_malformed_id_is_refused_before_the_store_is_read() {
    let t = Tmp::new();
    let missing = t.0.join("nope.jsonl");
    let o = run(&check_args(&missing, "A63"));
    refused1(&o, TOKEN_LINE);
    assert!(!err(&o).contains("A63"), "the value is echoed: {}", err(&o));
    let o = run(&show_args(&missing, "A63"));
    refused1(&o, TOKEN_LINE);
    assert!(!missing.exists(), "no file is created");
}

/// An id that holds a deny token is never printed.
#[test]
fn a_deny_token_in_the_id_is_never_printed() {
    let t = Tmp::new();
    let store = t.store();
    let o = run(&check_args(&store, DENY));
    refused1(&o, "harness: goal refused: field id has a privacy hit (deny-token)");
    let o = run(&show_args(&store, DENY));
    refused1(&o, "harness: goal refused: field id has a privacy hit (deny-token)");
}

// ---------------------------------------------------------------- add: the field rules

#[test]
fn add_bad_id() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    for id in ["GS-Video-1", "A63", "..", ""] {
        let o = run(&add_args(&store, id, T0, "orch-a", "t", "a line", PTR));
        refused_unchanged(&store, &o, 1, TOKEN_LINE, &copy);
    }
    let long = "a".repeat(65);
    let o = run(&add_args(&store, &long, T0, "orch-a", "t", "a line", PTR));
    refused_unchanged(&store, &o, 1, TOKEN_LINE, &copy);
    // 64 bytes and the id convention (one id per focus group) are accepted
    let ok = "a".repeat(64);
    for id in [ok.as_str(), "gs-video-1", "bw-harness-running"] {
        let o = run(&add_args(&store, id, T0, "orch-a", "t", "a line", PTR));
        assert_eq!(code(&o), 0, "{}", err(&o));
    }
}

#[test]
fn add_bad_by() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: by must be a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not . or ..";
    let long_by = "a".repeat(65);
    for by in ["Orch-A", "..", "", long_by.as_str()] {
        let o = run(&add_args(&store, "gs-video-1", T0, by, "t", "a line", PTR));
        refused_unchanged(&store, &o, 1, line, &copy);
    }
}

#[test]
fn add_bad_at() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: at must be a UTC time written YYYY-MM-DDTHH:MM:SSZ";
    for at in ["2026-02-30T00:00:00Z", "2026-11-26 23:49:00", "2026-11-26T24:00:00Z", ""] {
        let o = run(&add_args(&store, "gs-video-1", at, "orch-a", "t", "a line", PTR));
        refused_unchanged(&store, &o, 1, line, &copy);
    }
}

#[test]
fn add_bad_title() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: title must be one non-blank line of at most 500 bytes";
    let long = "a".repeat(501);
    for title in ["   ", "a\nb", "a\u{2028}b", "a\rb", long.as_str()] {
        let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", title, "a line", PTR));
        refused_unchanged(&store, &o, 1, line, &copy);
    }
    // 500 bytes is accepted, in title and in done_when
    let (five, five2) = ("a".repeat(500), "b".repeat(500));
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", &five, &five2, PTR));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

/// A goal needs a finish line (WAITING-229: a line-less goal is refused).
#[test]
fn add_refuses_a_lineless_done_when() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    let blank = "harness: goal refused: done_when must be one non-blank line of at most 500 bytes";
    let long = "a".repeat(501);
    for done in ["", "   ", "a\nb", long.as_str()] {
        let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "t", done, PTR));
        refused_unchanged(&store, &o, 1, blank, &copy);
    }
    let placeholder = "harness: goal refused: done_when must be one non-blank line of at most 500 bytes, not only -, tbd or ?";
    for done in ["-", "tbd", "?", " TBD ", "Tbd", "? "] {
        let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "t", done, PTR));
        refused_unchanged(&store, &o, 1, placeholder, &copy);
    }
    // a line that holds one of those words is a line
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "t", "the tbd list is empty", PTR));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn add_bad_source() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: source is not a valid pointer list";
    for source in ["not-a-pointer", "", "ask A63", "pr:owner/repo"] {
        let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "t", "a line", source));
        refused_unchanged(&store, &o, 1, line, &copy);
    }
    // a pointer list is accepted
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "t", "a line", "pr:owner/repo#7; session:abc123"));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn add_repeated_id() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: id is already in the store";
    let o = run(&add_args(&store, "gs-video-1", T1, "orch-a", "t", "a line", PTR));
    refused_unchanged(&store, &o, 1, line, &copy);
    // a closed id is still in the store
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "met", PTR, ""));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let copy = t.copy_of(&store);
    let o = run(&add_args(&store, "gs-video-1", T2, "orch-a", "t", "a line", PTR));
    refused_unchanged(&store, &o, 1, line, &copy);
}

/// There is no `--ask` and no `--group` flag (WAITING-232 A, WAITING-233 A): each is an unknown option.
#[test]
fn add_has_no_ask_and_no_group_flag() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    for flag in ["--ask", "--group"] {
        let mut a = add_args(&store, "gs-video-1", T0, "orch-a", "t", "a line", PTR);
        a.push(s(flag));
        a.push(s("g"));
        let o = run(&a);
        refused_unchanged(&store, &o, 2, "harness: error: unknown option", &copy);
    }
}

/// One word for `sh -c`, single-quoted.
fn q(p: &str) -> String {
    format!("'{}'", p.replace('\'', "'\\''"))
}

/// Start N child adds of one id at once, then wait for all: (exit code, stderr) per child. Each child
/// waits on a start flag before it runs the verb, so the N adds are truly in flight together: spawned one
/// after another, a child is done before the next one reaches its read, and no race is ever made.
fn race_adds(flag: &Path, store: &Path, id: &str, n: usize) -> Vec<(Option<i32>, String)> {
    let script = format!(
        "while [ ! -e {} ]; do :; done; exec {} goal add --store {} --id {} --at {} --by orch-a --title 'one goal' --done-when 'the line is done' --source {}",
        q(&ss(flag)),
        q(BIN),
        q(&ss(store)),
        q(id),
        T0,
        q(PTR)
    );
    let kids: Vec<_> = (0..n)
        .map(|_| {
            let mut c = bare("sh");
            c.args(["-c", &script])
                .env("HARNESS_PRIVACY_LIST_SECRET", DENY)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            c.spawn().unwrap()
        })
        .collect();
    // every child is started at once
    fs::write(flag, "").unwrap();
    kids.into_iter()
        .map(|k| {
            let o = k.wait_with_output().unwrap();
            (o.status.code(), String::from_utf8_lossy(&o.stderr).to_string())
        })
        .collect()
}

#[test]
fn racing_adds_of_one_id_store_one_row() {
    let t = Tmp::new();
    for round in 0..4 {
        let store = t.named(&format!("goal-{round}.jsonl"), "");
        let flag = t.0.join(format!("start-{round}"));
        let res = race_adds(&flag, &store, "gs-same", 48);
        assert_eq!(res.iter().filter(|r| r.0 == Some(0)).count(), 1, "round {round}: exactly one add may store: {res:?}");
        for r in res.iter().filter(|r| r.0 != Some(0)) {
            assert_eq!(r.0, Some(1), "round {round}: a loser is a refusal: {r:?}");
            assert_eq!(r.1, "harness: goal refused: id is already in the store\n", "round {round}: {r:?}");
        }
        assert_eq!(fs::read_to_string(&store).unwrap().lines().count(), 1, "round {round}: one row stored");
    }
}

// ---------------------------------------------------------------- close

#[test]
fn close_met_needs_ref() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: status met needs --ref and takes no --reason";
    let head = vec![
        s("goal"), s("close"), s("--store"), ss(&store), s("--id"), s("gs-video-1"),
        s("--at"), s(T1), s("--by"), s("orch-a"), s("--status"), s("met"),
    ];
    // met with no --ref: WAITING-229, the goal closes only with a proof pointer
    let o = run(&head);
    refused_unchanged(&store, &o, 1, line, &copy);
    // met with --reason
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "met", "", "a reason"));
    refused_unchanged(&store, &o, 1, line, &copy);
    // met with an empty --ref
    let mut a = head.clone();
    a.push(s("--ref"));
    a.push(s(""));
    let o = run(&a);
    refused_unchanged(&store, &o, 1, line, &copy);
}

#[test]
fn close_dropped_needs_reason() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: status dropped needs --reason and takes no --ref";
    let head = vec![
        s("goal"), s("close"), s("--store"), ss(&store), s("--id"), s("gs-video-1"),
        s("--at"), s(T1), s("--by"), s("orch-a"), s("--status"), s("dropped"),
    ];
    let o = run(&head);
    refused_unchanged(&store, &o, 1, line, &copy);
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "dropped", PTR, ""));
    refused_unchanged(&store, &o, 1, line, &copy);
    let mut a = head;
    a.push(s("--reason"));
    a.push(s(""));
    let o = run(&a);
    refused_unchanged(&store, &o, 1, line, &copy);
}

#[test]
fn close_bad_ref() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: ref is not a valid pointer list";
    for r in ["not-a-pointer", "pr:owner/repo#7; zz"] {
        let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "met", r, ""));
        refused_unchanged(&store, &o, 1, line, &copy);
    }
}

#[test]
fn close_bad_status() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let line = "harness: goal refused: status must be met or dropped";
    for status in ["open", "done", "met ", ""] {
        let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", status, PTR, ""));
        refused_unchanged(&store, &o, 1, line, &copy);
    }
}

#[test]
fn close_unknown_early_and_double() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let o = run(&close_args(&store, "gs-nope", T1, "orch-a", "met", PTR, ""));
    refused_unchanged(&store, &o, 1, "harness: goal refused: id is not in the store", &copy);
    // earlier than the add row, the last row of the id
    let o = run(&close_args(&store, "gs-video-1", "2026-11-26T23:00:00Z", "orch-a", "met", PTR, ""));
    refused_unchanged(&store, &o, 1, "harness: goal refused: at is before the time the id was added", &copy);
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "met", PTR, ""));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let copy = t.copy_of(&store);
    let o = run(&close_args(&store, "gs-video-1", T2, "orch-a", "dropped", "", "again"));
    refused_unchanged(&store, &o, 1, "harness: goal refused: id is already closed", &copy);
}

// ---------------------------------------------------------------- privacy

#[test]
fn add_and_close_refuse_a_privacy_hit() {
    let t = Tmp::new();
    let store = t.store();
    let copy = t.copy_of(&store);
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", &format!("clean text, poisoned by {DENY}"), "a line", PTR));
    refused_unchanged(&store, &o, 1, "harness: goal refused: field title has a privacy hit (deny-token)", &copy);
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "t", &format!("see {}", home_path()), PTR));
    refused_unchanged(&store, &o, 1, "harness: goal refused: field done_when has a privacy hit (home-path)", &copy);
    // a pointer that is well-formed and holds the deny word: the field rule passes, the scan refuses
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", "t", "a line", &format!("session:{DENY}")));
    refused_unchanged(&store, &o, 1, "harness: goal refused: field source has a privacy hit (deny-token)", &copy);
    let o = seed(&store);
    assert_eq!(code(&o), 0, "{}", err(&o));
    let copy = t.copy_of(&store);
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "dropped", "", &format!("see {}", home_path())));
    refused_unchanged(&store, &o, 1, "harness: goal refused: field reason has a privacy hit (home-path)", &copy);
    let o = run(&close_args(&store, "gs-video-1", T1, "orch-a", "met", &format!("session:{DENY}"), ""));
    refused_unchanged(&store, &o, 1, "harness: goal refused: field ref has a privacy hit (deny-token)", &copy);
}

#[test]
fn no_privacy_list_fails_closed() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&seed(&store)), 0);
    let copy = t.copy_of(&store);
    let stored = "harness: error: no privacy list configured: an unscanned row is never stored";
    let read = "harness: error: no privacy list configured: an unscanned store is never read";
    // the field rules come first, so these calls have valid flags and fields
    let o = run_no_list(&add_args(&store, "gs-new", T2, "orch-a", "t", "a line", PTR));
    refused_unchanged(&store, &o, 2, stored, &copy);
    let o = run_no_list(&list_args(&store));
    refused_unchanged(&store, &o, 2, read, &copy);
    let o = run_no_list(&show_args(&store, "gs-video-1"));
    refused_unchanged(&store, &o, 2, read, &copy);
    let o = run_no_list(&close_args(&store, "gs-video-1", T1, "orch-a", "met", PTR, ""));
    refused_unchanged(&store, &o, 2, stored, &copy);
    let o = run_no_list(&check_args(&store, "gs-video-1"));
    refused_unchanged(&store, &o, 2, read, &copy);
}

/// A stored row that holds a token listed after the row was written: every verb exits 2, naming the line and
/// the field, never the value, until the row is fixed by hand.
#[test]
fn a_newly_listed_token_stops_every_verb() {
    let t = Tmp::new();
    let store = t.store();
    // written under the first list, which does not hold the word
    let o = run(&add_args(&store, "gs-video-1", T0, "orch-a", &format!("ship it, says {LATE_DENY}"), "a line", PTR));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = run(&check_args(&store, "gs-video-1"));
    assert_eq!(code(&o), 0, "the row is clean under the list it was written with");
    let copy = t.copy_of(&store);
    let line = "harness: error: the goal store has a privacy hit at line 1 in field title (deny-token)";
    for (i, o) in every_verb(&store, LATE_DENY).iter().enumerate() {
        assert_eq!(code(o), 2, "verb {i}: {}", err(o));
        assert_eq!(err(o), format!("{line}\n"), "verb {i}");
        assert!(out(o).is_empty(), "verb {i} printed: {}", out(o));
        assert!(!err(o).contains(LATE_DENY), "verb {i}: the value is echoed: {}", err(o));
    }
    cmp(&store, &copy);
}

// ---------------------------------------------------------------- damaged stores

#[test]
fn store_missing() {
    let t = Tmp::new();
    let missing = t.0.join("nope.jsonl");
    all_verbs_refused(&missing, "harness: error: the goal store does not exist");
    assert!(!missing.exists(), "no verb creates the store");
}

#[test]
fn store_parent_is_a_file() {
    let t = Tmp::new();
    let parent = t.named("afile", "x");
    let under = parent.join("goal.jsonl");
    all_verbs_refused(&under, "harness: error: the goal store does not exist");
}

#[test]
fn a_relative_store_is_refused() {
    let rel = Path::new("goal.jsonl");
    let line = "harness: error: --store must be an absolute path";
    for o in every_verb(rel, DENY) {
        refused2(&o, line);
    }
    // The child runs in this folder: no store file is created there.
    assert!(!std::env::current_dir().unwrap().join("goal.jsonl").exists());
}

#[test]
fn store_is_folder() {
    let t = Tmp::new();
    let dir = t.0.join("folder.jsonl");
    fs::create_dir_all(&dir).unwrap();
    all_verbs_refused(&dir, NOT_REGULAR);
    assert!(dir.is_dir() && fs::read_dir(&dir).unwrap().count() == 0, "the folder is unchanged");
}

#[test]
fn store_is_fifo() {
    let t = Tmp::new();
    let fifo = t.0.join("fifo.jsonl");
    let mk = Command::new("mkfifo").arg(&fifo).output().unwrap();
    assert!(mk.status.success(), "mkfifo must exist on this host");
    // Every verb returns at once: the stat refuses the fifo before any open.
    all_verbs_refused(&fifo, NOT_REGULAR);
    use std::os::unix::fs::FileTypeExt;
    assert!(fs::symlink_metadata(&fifo).unwrap().file_type().is_fifo(), "the fifo is unchanged");
}

#[test]
fn store_last_part_is_a_symlink() {
    let t = Tmp::new();
    let target = t.named("real.jsonl", add1());
    let link = t.0.join("link.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let before = fs::read(&target).unwrap();
    all_verbs_refused(&link, NOT_REGULAR);
    assert_eq!(fs::read(&target).unwrap(), before, "the link target keeps its bytes");
    assert_eq!(lines(&link), 1, "nothing was appended through the link");
}

/// The number of lines of a file (0 for a missing file).
fn lines(p: &Path) -> usize {
    fs::read_to_string(p).map(|s| s.lines().count()).unwrap_or(0)
}

#[test]
fn store_is_a_dangling_symlink() {
    let t = Tmp::new();
    let target = t.0.join("nothing.jsonl");
    let link = t.0.join("link.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    all_verbs_refused(&link, NOT_REGULAR);
    assert!(!target.exists(), "no file is created at the link target");
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link itself is unchanged");
}

#[test]
fn a_linked_parent_folder_is_accepted() {
    let t = Tmp::new();
    let real_dir = t.0.join("real-dir");
    fs::create_dir_all(&real_dir).unwrap();
    let store = real_dir.join("goal.jsonl");
    fs::write(&store, "").unwrap();
    let link_dir = t.0.join("link-dir");
    std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
    let via = link_dir.join("goal.jsonl");
    let o = run(&add_args(&via, "gs-video-1", T0, "orch-a", "a linked parent folder is fine", "a line", PTR));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let o = run(&list_args(&via));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "goal id=gs-video-1 state=open\ngoal open=1 met=0 dropped=0\n");
    assert_eq!(lines(&store), 1);
}

#[test]
fn store_over_4_mib() {
    let t = Tmp::new();
    let store = t.named("big.jsonl", [b"a".repeat(4 * 1024 * 1024), b"\n".to_vec()].concat());
    all_verbs_refused(&store, "harness: error: the goal store is over 4194304 bytes");
}

#[test]
fn store_not_utf8() {
    let t = Tmp::new();
    let mut row = add1().into_bytes();
    let cut = row.len() - 3;
    row.splice(cut..cut, [0xFF]);
    let store = t.named("goal.jsonl", row);
    all_verbs_refused(&store, "harness: error: the goal store is not utf-8");
}

#[test]
fn store_torn_last_line() {
    let t = Tmp::new();
    let torn = format!("{}{{\"op\":\"add\",\"id\":\"gs-2\"", add1());
    let store = t.named("goal.jsonl", torn);
    all_verbs_refused(&store, "harness: error: the goal store has a torn last line at line 2");
}

#[test]
fn store_empty_file_is_accepted() {
    let t = Tmp::new();
    let store = t.store();
    let o = run(&check_args(&store, "gs-video-1"));
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "goal refused id=gs-video-1 reason=unknown\n");
    assert!(err(&o).is_empty());
}

/// One damaged store: its bytes and the one lint line every verb prints.
struct Bad {
    body: String,
    line: String,
}

fn bad(line: &str, body: String) -> Bad {
    Bad { body, line: line.to_string() }
}

fn bad_row(line: usize, reason: &str) -> String {
    format!("harness: error: the goal store has a bad row at line {line}: {reason}")
}

/// Every planted store of criterion 5: each one planted on its own, and every verb run on it.
#[test]
fn planted_bad_rows_refuse_every_verb() {
    const DONE: &str = "done_when must be one non-blank line of at most 500 bytes";
    const PLACE: &str = "done_when must be one non-blank line of at most 500 bytes, not only -, tbd or ?";
    let mut v: Vec<Bad> = Vec::new();
    // an unknown op, and no op
    for row in [
        r#"{"op":"reword","id":"gs-video-1","at":"2026-11-26T23:49:00Z","by":"orch-a","title":"t","done_when":"a line","source":"pr:owner/repo#7"}"#.to_string(),
        r#"{"id":"gs-video-1","at":"2026-11-26T23:49:00Z","by":"orch-a","title":"t","done_when":"a line","source":"pr:owner/repo#7"}"#.to_string(),
    ] {
        v.push(bad(&bad_row(1, "op must be add or close"), format!("{row}\n")));
    }
    // the keys are not the key set of its op: one key too many, one missing, on both ops
    v.push(bad(KEY_SET_LINE, format!("{}\n", ADD1.replace("\"source\":", "\"x\":\"\",\"source\":"))));
    v.push(bad(KEY_SET_LINE, format!("{}\n", ADD1.replace(",\"source\":\"pr:owner/repo#7\"", ""))));
    v.push(bad(&bad_row(2, "the keys are not the key set of its op"), format!("{}{}\n", add1(), r#"{"op":"close","id":"gs-video-1","at":"2026-11-27T01:16:00Z","by":"orch-a","status":"met","ref":"pr:owner/repo#7"}"#)));
    // an add row with a bad field
    v.push(bad(&bad_row(1, "title must be one non-blank line of at most 500 bytes"), format!("{}\n", ADD1.replace("video 1 ships", "   "))));
    v.push(bad(&bad_row(1, "source is not a valid pointer list"), format!("{}\n", ADD1.replace(PTR, "ask A63"))));
    v.push(bad(&bad_row(1, "at must be a UTC time written YYYY-MM-DDTHH:MM:SSZ"), format!("{}\n", ADD1.replace(T0, "2026-02-30T00:00:00Z"))));
    // a line-less add row (store damage: add never writes one)
    for done in ["", "   "] {
        v.push(bad(&bad_row(1, DONE), planted_add("gs-video-1", T0, "t", done, PTR)));
    }
    for done in ["-", "TBD", " ? "] {
        v.push(bad(&bad_row(1, PLACE), planted_add("gs-video-1", T0, "t", done, PTR)));
    }
    // the cross rules
    v.push(bad(&bad_row(2, "id is added twice"), format!("{}{}", add1(), planted_add("gs-video-1", T1, "two", "a line", PTR))));
    v.push(bad(&bad_row(1, "close names no earlier added id"), planted_close("gs-video-1", T1, "met", PTR, "")));
    v.push(bad(&bad_row(2, "close is before the time the id was added"), format!("{}{}", add1(), planted_close("gs-video-1", "2026-11-26T23:00:00Z", "met", PTR, ""))));
    v.push(bad(&bad_row(3, "id is closed twice"), format!("{}{}{}", add1(), planted_close("gs-video-1", T1, "met", PTR, ""), planted_close("gs-video-1", T2, "dropped", "", "again"))));
    // a stored close row that breaks the pairing, or carries a bad ref or status
    for (row, reason) in [
        (planted_close("gs-video-1", T1, "met", "", ""), "status met needs --ref and takes no --reason"),
        (planted_close("gs-video-1", T1, "met", PTR, "and a reason too"), "status met needs --ref and takes no --reason"),
        (planted_close("gs-video-1", T1, "dropped", PTR, "a reason"), "status dropped needs --reason and takes no --ref"),
        (planted_close("gs-video-1", T1, "met", "not-a-pointer", ""), "ref is not a valid pointer list"),
        (planted_close("gs-video-1", T1, "open", "", ""), "status must be met or dropped"),
    ] {
        v.push(bad(&bad_row(2, reason), format!("{}{row}", add1())));
    }
    for b in &v {
        let t = Tmp::new();
        let store = t.named("goal.jsonl", &b.body);
        all_verbs_refused(&store, &b.line);
    }
}

// ---------------------------------------------------------------- usage and flags

#[test]
fn goal_no_verb_and_unknown_verb_print_usage() {
    for args in [vec![s("goal")], vec![s("goal"), s("nope")]] {
        let o = run(&args);
        assert_eq!(code(&o), 2);
        assert!(out(&o).is_empty());
        assert_eq!(err(&o), format!("{GOAL_USAGE}\n"));
    }
}

#[test]
fn flag_errors_exit_2() {
    let t = Tmp::new();
    let store = t.store();
    let head = vec![s("goal"), s("add"), s("--store"), ss(&store)];
    for a in [
        vec![s("goal"), s("add"), s("--store"), ss(&store), s("--done")],
        vec![s("goal"), s("add"), s("--store"), ss(&store), s("--open")],
        vec![s("goal"), s("list"), s("--store"), ss(&store), s("--id"), s("gs-video-1")],
        vec![s("goal"), s("show"), s("--store"), ss(&store), s("--open")],
        vec![s("goal"), s("check"), s("--store"), ss(&store), s("--ask"), s("A63")],
    ] {
        refused2(&run(&a), "harness: error: unknown option");
    }
    refused2(&run(&[s("goal"), s("add"), s("stray")]), "harness: error: an unexpected argument");
    refused2(&run(&[s("goal"), s("show"), s("--store")]), "harness: error: a flag is missing its value");
    refused2(&run(&[s("goal"), s("add"), s("--id"), s("a"), s("--id"), s("b")]), "harness: error: a flag was given twice");
    refused2(&run(&[s("goal"), s("list"), s("--store"), ss(&store), s("--open"), s("--open")]), "harness: error: a flag was given twice");
    // a missing required flag, in usage order
    for (args, name) in [
        (vec![s("goal"), s("add")], "--store is required"),
        (head.clone(), "--id is required"),
        (vec![s("goal"), s("list")], "--store is required"),
        (vec![s("goal"), s("show"), s("--store"), ss(&store)], "--id is required"),
        (vec![s("goal"), s("check"), s("--store"), ss(&store)], "--id is required"),
        (vec![s("goal"), s("close"), s("--store"), ss(&store), s("--id"), s("a"), s("--at"), s(T0), s("--by"), s("b")], "--status is required"),
    ] {
        refused2(&run(&args), &format!("harness: error: {name}"));
    }
    assert_eq!(fs::read(&store).unwrap(), b"", "nothing was stored");
}
