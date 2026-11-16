//! Process-level tests for `harness owed add | list | close` (OW-1, ticket owed-store). Every check runs
//! the real binary as a child process and reads the real store file, so the exit-code contract (0 ok,
//! 1 a refused input, 2 usage, setup or a damaged store) is exercised end to end. Every row, marker and
//! deny word is synthetic, built at run time; the privacy list is injected on the child only, and no test
//! reads an environment variable.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
/// A synthetic deny word: the ONLY entry of the injected list. No clean fixture may contain it.
const DENY: &str = "zzdenyzz";
static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The head of the top-level usage line, pinned before this ticket's verbs were registered: the text of
/// cli.rs from `usage: harness --version` up to and including `lane claim|release|list|admission-check ...`.
const USAGE_HEAD: &str = "usage: harness --version | privacy classes | privacy scan|explain <mode> | friction add|lint|patterns|close-check | status <ticket-file> ... | process start|stop|check|step <ticket> ... | evidence show|check|quote <ticket> ... | workspace create <ticket-file> ... | workspace check <ticket> ... | controls approval-message|write ... | permission approval-message|write ... | session check-name|record-start ... | lesson lint <path> ... | lesson lookup <lessons-dir> --ticket <ticket-file> --scope-file <file> | lane claim|release|list|admission-check ...";

const OWED_USAGE: &str = "usage: harness owed add --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --what <text> [--home <home>] | owed list --store <absolute-file> [--open] | owed close --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --status <done|dropped> (--ref <pointers> | --reason <text>)";

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("owed-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// The store, created once by hand as an empty file (no verb may create it).
    fn store(&self) -> PathBuf {
        let p = self.0.join("owed.jsonl");
        fs::write(&p, "").unwrap();
        p
    }

    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.0.join(name);
        fs::write(&f, body).unwrap();
        f
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The child with no config or list variable inherited: every input arrives as a flag or an explicit `.env`.
fn base() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

/// The usual child: the synthetic deny list injected as the secret.
fn run(args: &[String]) -> Output {
    let mut c = base();
    c.args(args).env("HARNESS_PRIVACY_LIST_SECRET", DENY).stdin(Stdio::null());
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

/// A refusal leaves the store byte for byte unchanged (or still absent).
fn untouched(store: &Path, o: &Output, want: i32, line: &str, before: Option<Vec<u8>>) {
    if want == 1 {
        refused1(o, line);
    } else {
        refused2(o, line);
    }
    assert_eq!(fs::read(store).ok(), before, "a refused call changed the store");
}

fn add_args(store: &Path, id: &str, at: &str, by: &str, what: &str) -> Vec<String> {
    vec![
        s("owed"), s("add"), s("--store"), ss(store), s("--id"), s(id), s("--at"), s(at), s("--by"), s(by), s("--what"), s(what),
    ]
}

fn close_args(store: &Path, id: &str, at: &str, by: &str, status: &str, r: &str, reason: &str) -> Vec<String> {
    let mut a = vec![s("owed"), s("close"), s("--store"), ss(store), s("--id"), s(id), s("--at"), s(at), s("--by"), s(by), s("--status"), s(status)];
    if !r.is_empty() {
        a.push(s("--ref"));
        a.push(s(r));
    }
    if !reason.is_empty() {
        a.push(s("--reason"));
        a.push(s(reason));
    }
    a
}

fn list_args(store: &Path) -> Vec<String> {
    vec![s("owed"), s("list"), s("--store"), ss(store)]
}

/// One planted add row (with its newline) for a hand-written store.
fn planted_add(id: &str, at: &str, what: &str) -> String {
    format!("{{\"op\":\"add\",\"id\":\"{id}\",\"at\":\"{at}\",\"by\":\"orch-a\",\"what\":\"{what}\",\"home\":\"\"}}\n")
}

/// One planted close row (with its newline) for a hand-written store.
fn planted_close(id: &str, at: &str, status: &str, r: &str, reason: &str) -> String {
    format!("{{\"op\":\"close\",\"id\":\"{id}\",\"at\":\"{at}\",\"by\":\"orch-a\",\"status\":\"{status}\",\"ref\":\"{r}\",\"reason\":\"{reason}\"}}\n")
}

/// A home-path shape, assembled at run time so this file never spells it out.
fn home_path() -> String {
    ["/Us", "ers/", "zztester", "/proj"].concat()
}

/// An email shape, assembled at run time so this file never spells it out.
fn email_shape() -> String {
    ["zz", "@", "example", ".", "invalid"].concat()
}

// ---------------------------------------------------------------- the pinned usage head

/// The bare binary prints the top-level usage line and exits 2; this ticket registers new verbs behind
/// that line, and the pinned head must survive them byte for byte.
#[test]
fn usage_line_head_is_unchanged() {
    let o = Command::new(BIN).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(out(&o).is_empty());
    assert!(err(&o).starts_with(USAGE_HEAD), "stderr: {}", err(&o));
}

// ---------------------------------------------------------------- the worked case (exact bytes)

#[test]
fn worked_case_bytes() {
    let t = Tmp::new();
    let store = t.store();
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:48:00Z", "orch-a", "write the lesson page for the menu stall"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "stored id=ow-1\n");
    let l1 = r#"{"op":"add","id":"ow-1","at":"2026-11-16T00:48:00Z","by":"orch-a","what":"write the lesson page for the menu stall","home":""}"#;
    assert_eq!(fs::read_to_string(&store).unwrap(), format!("{l1}\n"));
    let mut a = add_args(&store, "ow-2", "2026-11-16T01:00:00Z", "orch-a", "re-run the gate on the new head");
    a.push(s("--home"));
    a.push(s("ticket:lane-release-record"));
    let o = run(&a);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "stored id=ow-2\n");
    let mut open_args = list_args(&store);
    open_args.push(s("--open"));
    let o = run(&open_args);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "owed id=ow-1 status=open\nowed id=ow-2 status=open\nowed open=2\n");
    let close = close_args(&store, "ow-2", "2026-11-16T09:00:00Z", "orch-a", "done", "pr:owner/repo#7", "");
    let o = run(&close);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "closed id=ow-2 status=done\n");
    let body = fs::read_to_string(&store).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], l1);
    assert_eq!(lines[2], r#"{"op":"close","id":"ow-2","at":"2026-11-16T09:00:00Z","by":"orch-a","status":"done","ref":"pr:owner/repo#7","reason":""}"#);
    let o = run(&list_args(&store));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "owed id=ow-1 status=open\nowed id=ow-2 status=done\nowed open=1 done=1 dropped=0\n");
    let before = fs::read(&store).unwrap();
    let o = run(&close);
    refused1(&o, "harness: owed refused: id is already closed");
    assert_eq!(fs::read(&store).unwrap(), before);
}

// ---------------------------------------------------------------- hostile stores

#[test]
fn store_is_symlink_to_file() {
    let t = Tmp::new();
    let target = t.0.join("real.jsonl");
    fs::write(&target, planted_add("ow-1", "2026-11-16T00:00:00Z", "a row behind a link")).unwrap();
    let link = t.0.join("owed.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let before = fs::read(&target).unwrap();
    let line = "harness: error: the owed store is not a regular file";
    untouched(&link, &run(&add_args(&link, "ow-2", "2026-11-16T01:00:00Z", "orch-a", "must not follow the link")), 2, line, Some(before.clone()));
    let o = run(&list_args(&link));
    refused2(&o, line);
    assert_eq!(fs::read(&target).unwrap(), before, "the link target keeps its bytes");
}

#[test]
fn store_is_dangling_symlink() {
    let t = Tmp::new();
    let target = t.0.join("nothing.jsonl");
    let link = t.0.join("owed.jsonl");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    let o = run(&add_args(&link, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "must not create the target"));
    refused2(&o, "harness: error: the owed store is not a regular file");
    assert!(!target.exists(), "no file is created at the link target");
}

#[test]
fn store_is_folder() {
    let t = Tmp::new();
    let dir = t.0.join("owed.jsonl");
    fs::create_dir_all(&dir).unwrap();
    let line = "harness: error: the owed store is not a regular file";
    let o = run(&add_args(&dir, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = run(&list_args(&dir));
    refused2(&o, line);
}

#[test]
fn store_is_root() {
    let root = Path::new("/");
    let line = "harness: error: the owed store is not a regular file";
    let o = run(&add_args(root, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = run(&list_args(root));
    refused2(&o, line);
}

#[test]
fn store_is_fifo() {
    let t = Tmp::new();
    let fifo = t.0.join("owed.jsonl");
    let mk = Command::new("mkfifo").arg(&fifo).output().unwrap();
    assert!(mk.status.success(), "mkfifo must exist on this host");
    let line = "harness: error: the owed store is not a regular file";
    // Both verbs return at once: the stat refuses the fifo before any open.
    let o = run(&add_args(&fifo, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = run(&list_args(&fifo));
    refused2(&o, line);
}

#[test]
fn store_missing() {
    let t = Tmp::new();
    let missing = t.0.join("nope.jsonl");
    let line = "harness: error: the owed store does not exist";
    let o = run(&add_args(&missing, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    assert!(!missing.exists(), "add creates no file");
    let o = run(&list_args(&missing));
    refused2(&o, line);
    let o = run(&close_args(&missing, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "done", "pr:owner/repo#7", ""));
    refused2(&o, line);
}

#[test]
fn store_parent_is_a_file() {
    let t = Tmp::new();
    let parent = t.file("afile", "x");
    let under = parent.join("owed.jsonl");
    let line = "harness: error: the owed store does not exist";
    let o = run(&add_args(&under, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = run(&list_args(&under));
    refused2(&o, line);
    let o = run(&close_args(&under, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "done", "pr:owner/repo#7", ""));
    refused2(&o, line);
}

#[test]
fn store_linked_parent_is_accepted() {
    let t = Tmp::new();
    let real_dir = t.0.join("real-dir");
    fs::create_dir_all(&real_dir).unwrap();
    let store = real_dir.join("owed.jsonl");
    fs::write(&store, "").unwrap();
    let link_dir = t.0.join("link-dir");
    std::os::unix::fs::symlink(&real_dir, &link_dir).unwrap();
    let via = link_dir.join("owed.jsonl");
    let o = run(&add_args(&via, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "a linked parent folder is fine"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(fs::read_to_string(&store).unwrap().lines().count(), 1);
}

#[test]
fn store_unreadable() {
    if String::from_utf8_lossy(&Command::new("id").arg("-u").output().unwrap().stdout).trim() == "0" {
        return; // root reads a mode-000 file; the check cannot bite
    }
    let t = Tmp::new();
    let store = t.store();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&store, fs::Permissions::from_mode(0o000)).unwrap();
    let line = "harness: error: the owed store cannot be opened";
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = run(&list_args(&store));
    refused2(&o, line);
}

#[test]
fn store_over_4_mib() {
    let t = Tmp::new();
    let store = t.store();
    fs::write(&store, [b"a".repeat(4 * 1024 * 1024), b"\n".to_vec()].concat()).unwrap();
    let before = fs::read(&store).unwrap().len();
    let line = "harness: error: the owed store is over 4194304 bytes";
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = run(&list_args(&store));
    refused2(&o, line);
    let o = run(&close_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "done", "pr:owner/repo#7", ""));
    refused2(&o, line);
    assert_eq!(fs::read(&store).unwrap().len(), before);
}

#[test]
fn store_not_utf8() {
    let t = Tmp::new();
    let store = t.store();
    let mut row = planted_add("ow-1", "2026-11-16T00:00:00Z", "a row").into_bytes();
    let cut = row.len() - 3;
    row.splice(cut..cut, [0xFF]);
    fs::write(&store, &row).unwrap();
    let line = "harness: error: the owed store is not utf-8";
    let o = run(&add_args(&store, "ow-2", "2026-11-16T01:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = run(&list_args(&store));
    refused2(&o, line);
}

#[test]
fn store_torn_last_line() {
    let t = Tmp::new();
    let store = t.store();
    let torn = format!("{}{{\"op\":\"add\",\"id\":\"ow-2\"", planted_add("ow-1", "2026-11-16T00:00:00Z", "a whole row"));
    fs::write(&store, &torn).unwrap();
    let before = fs::read(&store).unwrap();
    let line = "harness: error: the owed store has a torn last line at line 2";
    let o = run(&add_args(&store, "ow-3", "2026-11-16T02:00:00Z", "orch-a", "appends nothing onto a torn tail"));
    refused2(&o, line);
    assert_eq!(fs::read(&store).unwrap(), before, "nothing was appended");
    let o = run(&list_args(&store));
    refused2(&o, line);
}

#[test]
fn store_duplicate_added_id() {
    let t = Tmp::new();
    let store = t.store();
    fs::write(&store, planted_add("ow-1", "2026-11-16T00:00:00Z", "one") + &planted_add("ow-1", "2026-11-16T01:00:00Z", "two")).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a bad row at line 2: id is added twice");
}

#[test]
fn store_close_without_add() {
    let t = Tmp::new();
    let store = t.store();
    fs::write(&store, planted_close("ow-404", "2026-11-16T01:00:00Z", "done", "pr:owner/repo#7", "")).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a bad row at line 1: close names no earlier added id");
}

#[test]
fn store_closed_twice() {
    let t = Tmp::new();
    let store = t.store();
    let body = planted_add("ow-1", "2026-11-16T00:00:00Z", "a row")
        + &planted_close("ow-1", "2026-11-16T01:00:00Z", "done", "pr:owner/repo#7", "")
        + &planted_close("ow-1", "2026-11-16T02:00:00Z", "done", "pr:owner/repo#8", "");
    fs::write(&store, body).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a bad row at line 3: id is closed twice");
}

#[test]
fn store_close_before_add() {
    let t = Tmp::new();
    let store = t.store();
    let body = planted_add("ow-1", "2026-11-16T09:00:00Z", "a row") + &planted_close("ow-1", "2026-11-16T08:00:00Z", "done", "pr:owner/repo#7", "");
    fs::write(&store, body).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a bad row at line 2: close is before the time the id was added");
}

#[test]
fn store_crlf_row() {
    let t = Tmp::new();
    let store = t.store();
    let row = planted_add("ow-1", "2026-11-16T00:00:00Z", "a row");
    let body = format!("{}\r\n", row.trim_end_matches('\n'));
    fs::write(&store, body).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a bad row at line 1: not one JSON object of strings");
}

#[test]
fn store_empty_object_and_no_op() {
    let t = Tmp::new();
    let line = "harness: error: the owed store has a bad row at line 1: op must be add or close";
    let store = t.store();
    fs::write(&store, "{}\n").unwrap();
    let o = run(&list_args(&store));
    refused2(&o, line);
    let store = t.store();
    let no_op = r#"{"id":"ow-1","at":"2026-11-16T00:00:00Z","by":"orch-a","what":"a row with no op","home":""}"#;
    fs::write(&store, format!("{no_op}\n")).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, line);
}

#[test]
fn store_close_row_with_ref_and_reason() {
    let t = Tmp::new();
    let store = t.store();
    let body = planted_add("ow-1", "2026-11-16T00:00:00Z", "a row")
        + &planted_close("ow-1", "2026-11-16T01:00:00Z", "done", "pr:owner/repo#7", "and a reason too");
    fs::write(&store, body).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a bad row at line 2: status done needs --ref and takes no --reason");
}

#[test]
fn store_status_open() {
    let t = Tmp::new();
    let store = t.store();
    fs::write(&store, planted_close("ow-1", "2026-11-16T01:00:00Z", "open", "", "")).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a bad row at line 1: status must be done or dropped");
}

#[test]
fn store_privacy_hit_in_stored_row() {
    let t = Tmp::new();
    let store = t.store();
    fs::write(&store, planted_add("ow-1", "2026-11-16T00:00:00Z", &format!("see {}", home_path()))).unwrap();
    let o = run(&list_args(&store));
    refused2(&o, "harness: error: the owed store has a privacy hit at line 1 in field what (home-path)");
    assert!(!err(&o).contains("zztester"), "no planted user segment on stderr: {}", err(&o));
}

#[test]
fn store_empty_file_is_accepted() {
    let t = Tmp::new();
    let store = t.store();
    let o = run(&list_args(&store));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "owed open=0 done=0 dropped=0\n");
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "the first row of a hand-made store"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(fs::read_to_string(&store).unwrap().lines().count(), 1);
}

// ---------------------------------------------------------------- input rules

#[test]
fn add_duplicate_id_refused() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "one"))), 0);
    let before = fs::read(&store).unwrap();
    let o = run(&add_args(&store, "ow-1", "2026-11-16T01:00:00Z", "orch-a", "two"));
    untouched(&store, &o, 1, "harness: owed refused: id is already in the store", Some(before));
}

#[test]
fn readd_of_closed_id_refused() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "one"))), 0);
    assert_eq!(code(&run(&close_args(&store, "ow-1", "2026-11-16T01:00:00Z", "orch-a", "done", "pr:owner/repo#7", ""))), 0);
    let before = fs::read(&store).unwrap();
    let o = run(&add_args(&store, "ow-1", "2026-11-16T02:00:00Z", "orch-a", "a closed id is still in the store"));
    untouched(&store, &o, 1, "harness: owed refused: id is already in the store", Some(before));
}

/// Start N child adds of one id at once, then wait for all: (exit code, stderr) per child.
fn race_adds(store: &Path, id: &str, n: usize) -> Vec<(Option<i32>, String)> {
    let kids: Vec<_> = (0..n)
        .map(|_| {
            let mut c = base();
            c.args(["owed", "add", "--store", store.to_str().unwrap(), "--id", id, "--at", "2026-11-16T00:00:00Z", "--by", "orch-a", "--what", "one row per id"])
                .env("HARNESS_PRIVACY_LIST_SECRET", DENY)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped());
            c.spawn().unwrap()
        })
        .collect();
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
        let store = t.0.join(format!("owed-{round}.jsonl"));
        fs::write(&store, "").unwrap();
        let res = race_adds(&store, "ow-same", 48);
        assert_eq!(res.iter().filter(|r| r.0 == Some(0)).count(), 1, "round {round}: exactly one add may store: {res:?}");
        for r in res.iter().filter(|r| r.0 != Some(0)) {
            assert_eq!(r.0, Some(1), "round {round}: a loser is a refusal: {r:?}");
            assert_eq!(r.1, "harness: owed refused: id is already in the store\n", "round {round}: {r:?}");
        }
        assert_eq!(fs::read_to_string(&store).unwrap().lines().count(), 1, "round {round}: one row stored");
    }
}

#[test]
fn add_bad_id() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: owed refused: id must be a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not . or ..";
    let before = fs::read(&store).unwrap();
    for id in ["OW-1", "..", ""] {
        let o = run(&add_args(&store, id, "2026-11-16T00:00:00Z", "orch-a", "t"));
        untouched(&store, &o, 1, line, Some(before.clone()));
    }
    let long = "a".repeat(65);
    let o = run(&add_args(&store, &long, "2026-11-16T00:00:00Z", "orch-a", "t"));
    untouched(&store, &o, 1, line, Some(before));
    // 64 bytes is accepted.
    let ok = "a".repeat(64);
    let o = run(&add_args(&store, &ok, "2026-11-16T00:00:00Z", "orch-a", "t"));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn add_bad_by() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: owed refused: by must be a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not . or ..";
    let before = fs::read(&store).unwrap();
    let long_by = "a".repeat(65);
    for by in ["Orch-A", "..", "", long_by.as_str()] {
        let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", by, "t"));
        untouched(&store, &o, 1, line, Some(before.clone()));
    }
}

#[test]
fn add_bad_at() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: owed refused: at must be a UTC time written YYYY-MM-DDTHH:MM:SSZ";
    let before = fs::read(&store).unwrap();
    for at in ["2026-02-30T10:00:00Z", "2026-11-16 10:00:00", ""] {
        let o = run(&add_args(&store, "ow-1", at, "orch-a", "t"));
        untouched(&store, &o, 1, line, Some(before.clone()));
    }
}

#[test]
fn add_bad_what() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: owed refused: what must be one non-blank line of at most 500 bytes";
    let before = fs::read(&store).unwrap();
    let long = "a".repeat(501);
    for what in ["   ", "a\nb", "a\u{2028}b", "a\rb", long.as_str()] {
        let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", what));
        untouched(&store, &o, 1, line, Some(before.clone()));
    }
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", &"a".repeat(500)));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

#[test]
fn add_bad_home() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: owed refused: home must be empty, ticket:<id>, ledger:<row> or pr:<owner>/<repo>#<n>, at most 200 bytes";
    let before = fs::read(&store).unwrap();
    for home in ["ticket:Bad", "ledger:", "pr:owner/repo", "mail:x"] {
        let mut a = add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t");
        a.push(s("--home"));
        a.push(s(home));
        let o = run(&a);
        untouched(&store, &o, 1, line, Some(before.clone()));
    }
}

#[test]
fn add_refuses_home_path_in_what() {
    let t = Tmp::new();
    let store = t.store();
    let before = fs::read(&store).unwrap();
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", &format!("see {}", home_path())));
    untouched(&store, &o, 1, "harness: owed refused: field what has a privacy hit (home-path)", Some(before));
}

#[test]
fn add_refuses_email_in_what() {
    let t = Tmp::new();
    let store = t.store();
    let before = fs::read(&store).unwrap();
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", &format!("write to {}", email_shape())));
    untouched(&store, &o, 1, "harness: owed refused: field what has a privacy hit (email)", Some(before));
}

#[test]
fn add_refuses_deny_token_in_by() {
    let t = Tmp::new();
    let store = t.store();
    let before = fs::read(&store).unwrap();
    let o = run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", DENY, "clean text, poisoned by"));
    untouched(&store, &o, 1, "harness: owed refused: field by has a privacy hit (deny-token)", Some(before));
}

#[test]
fn close_refuses_home_path_in_reason() {
    let t = Tmp::new();
    let store = t.store();
    let before = fs::read(&store).unwrap();
    let o = run(&close_args(&store, "ow-1", "2026-11-16T01:00:00Z", "orch-a", "dropped", "", &format!("see {}", home_path())));
    untouched(&store, &o, 1, "harness: owed refused: field reason has a privacy hit (home-path)", Some(before));
}

#[test]
fn no_privacy_list_fails_closed() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: error: no privacy list configured: an unscanned row is never stored";
    let bare = |a: &[String]| {
        let mut c = base();
        c.args(a).stdin(Stdio::null());
        c.output().unwrap()
    };
    let o = bare(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "t"));
    refused2(&o, line);
    let o = bare(&list_args(&store));
    refused2(&o, line);
    let o = bare(&close_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "done", "pr:owner/repo#7", ""));
    refused2(&o, line);
    assert_eq!(fs::read(&store).unwrap(), b"", "nothing is stored unscanned");
}

#[test]
fn close_unknown_id_refused() {
    let t = Tmp::new();
    let store = t.store();
    let before = fs::read(&store).unwrap();
    let o = run(&close_args(&store, "ow-404", "2026-11-16T01:00:00Z", "orch-a", "done", "pr:owner/repo#7", ""));
    untouched(&store, &o, 1, "harness: owed refused: id is not in the store", Some(before));
}

#[test]
fn close_before_add_time_refused() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&run(&add_args(&store, "ow-1", "2026-11-16T09:00:00Z", "orch-a", "one"))), 0);
    let before = fs::read(&store).unwrap();
    let o = run(&close_args(&store, "ow-1", "2026-11-16T08:00:00Z", "orch-a", "done", "pr:owner/repo#7", ""));
    untouched(&store, &o, 1, "harness: owed refused: at is before the time the id was added", Some(before));
}

#[test]
fn close_done_needs_ref() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: owed refused: status done needs --ref and takes no --reason";
    let before = fs::read(&store).unwrap();
    let head = vec![
        s("owed"), s("close"), s("--store"), ss(&store), s("--id"), s("ow-1"),
        s("--at"), s("2026-11-16T01:00:00Z"), s("--by"), s("orch-a"), s("--status"), s("done"),
    ];
    // no --ref at all
    let o = run(&head);
    untouched(&store, &o, 1, line, Some(before.clone()));
    // done with --reason
    let o = run(&close_args(&store, "ow-1", "2026-11-16T01:00:00Z", "orch-a", "done", "", "a reason"));
    untouched(&store, &o, 1, line, Some(before.clone()));
    // done with an empty --ref
    let mut a = head.clone();
    a.push(s("--ref"));
    a.push(s(""));
    let o = run(&a);
    untouched(&store, &o, 1, line, Some(before));
}

#[test]
fn close_dropped_needs_reason() {
    let t = Tmp::new();
    let store = t.store();
    let line = "harness: owed refused: status dropped needs --reason and takes no --ref";
    let before = fs::read(&store).unwrap();
    let o = run(&[
        s("owed"), s("close"), s("--store"), ss(&store), s("--id"), s("ow-1"),
        s("--at"), s("2026-11-16T01:00:00Z"), s("--by"), s("orch-a"), s("--status"), s("dropped"),
    ]);
    untouched(&store, &o, 1, line, Some(before.clone()));
    let o = run(&close_args(&store, "ow-1", "2026-11-16T01:00:00Z", "orch-a", "dropped", "pr:owner/repo#7", ""));
    untouched(&store, &o, 1, line, Some(before.clone()));
    let o = run(&vec![
        s("owed"), s("close"), s("--store"), ss(&store), s("--id"), s("ow-1"),
        s("--at"), s("2026-11-16T01:00:00Z"), s("--by"), s("orch-a"), s("--status"), s("dropped"), s("--reason"), s(""),
    ]);
    untouched(&store, &o, 1, line, Some(before));
}

#[test]
fn close_bad_ref() {
    let t = Tmp::new();
    let store = t.store();
    let before = fs::read(&store).unwrap();
    let o = run(&close_args(&store, "ow-1", "2026-11-16T01:00:00Z", "orch-a", "done", "not-a-pointer", ""));
    untouched(&store, &o, 1, "harness: owed refused: ref is not a valid pointer list", Some(before));
}

#[test]
fn close_dropped_then_list() {
    let t = Tmp::new();
    let store = t.store();
    assert_eq!(code(&run(&add_args(&store, "ow-1", "2026-11-16T00:00:00Z", "orch-a", "one"))), 0);
    assert_eq!(code(&run(&close_args(&store, "ow-1", "2026-11-16T01:00:00Z", "orch-a", "dropped", "", "no longer worth doing"))), 0);
    let o = run(&list_args(&store));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "owed id=ow-1 status=dropped\nowed open=0 done=0 dropped=1\n");
}

// ---------------------------------------------------------------- usage and flags

#[test]
fn owed_no_verb_prints_usage() {
    let o = run(&[s("owed")]);
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), format!("{OWED_USAGE}\n"));
}

#[test]
fn owed_unknown_verb_prints_usage() {
    let o = run(&[s("owed"), s("nope")]);
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), format!("{OWED_USAGE}\n"));
}

#[test]
fn flag_errors_exit_2() {
    let t = Tmp::new();
    let store = t.store();
    // unknown option (also --open on add and on close)
    for args in [
        vec![s("owed"), s("add"), s("--store"), ss(&store), s("--wat")],
        vec![s("owed"), s("add"), s("--store"), ss(&store), s("--open")],
        vec![s("owed"), s("close"), s("--store"), ss(&store), s("--open")],
    ] {
        let o = run(&args);
        refused2(&o, "harness: error: unknown option");
    }
    // an unexpected argument (a word that is not a flag or a value)
    for args in [
        vec![s("owed"), s("add"), s("stray")],
        vec![s("owed"), s("list"), s("--store"), ss(&store), s("stray")],
    ] {
        let o = run(&args);
        refused2(&o, "harness: error: an unexpected argument");
    }
    // a flag is missing its value
    for args in [
        vec![s("owed"), s("add"), s("--store")],
        vec![s("owed"), s("list"), s("--store")],
    ] {
        let o = run(&args);
        refused2(&o, "harness: error: a flag is missing its value");
    }
    // a flag was given twice (also --open twice)
    for args in [
        vec![s("owed"), s("add"), s("--id"), s("a"), s("--id"), s("b")],
        vec![s("owed"), s("list"), s("--store"), ss(&store), s("--open"), s("--open")],
    ] {
        let o = run(&args);
        refused2(&o, "harness: error: a flag was given twice");
    }
    // a missing required flag, in usage order
    for (args, name) in [
        (vec![s("owed"), s("add")], "--store is required"),
        (vec![s("owed"), s("add"), s("--store"), ss(&store)], "--id is required"),
        (vec![s("owed"), s("add"), s("--store"), ss(&store), s("--id"), s("a")], "--at is required"),
        (vec![s("owed"), s("add"), s("--store"), ss(&store), s("--id"), s("a"), s("--at"), s("2026-11-16T00:00:00Z")], "--by is required"),
        (vec![s("owed"), s("add"), s("--store"), ss(&store), s("--id"), s("a"), s("--at"), s("2026-11-16T00:00:00Z"), s("--by"), s("b")], "--what is required"),
        (vec![s("owed"), s("list")], "--store is required"),
        (vec![s("owed"), s("close")], "--store is required"),
        (vec![s("owed"), s("close"), s("--store"), ss(&store), s("--id"), s("a"), s("--at"), s("2026-11-16T00:00:00Z"), s("--by"), s("b")], "--status is required"),
    ] {
        let o = run(&args);
        refused2(&o, &format!("harness: error: {name}"));
    }
}

#[test]
fn a_relative_store_is_refused() {
    let t = Tmp::new();
    let o = run(&[s("owed"), s("add"), s("--store"), s("owed.jsonl"), s("--id"), s("a"), s("--at"), s("2026-11-16T00:00:00Z"), s("--by"), s("b"), s("--what"), s("t")]);
    refused2(&o, "harness: error: --store must be an absolute path");
    let o = run(&[s("owed"), s("list"), s("--store"), s("owed.jsonl")]);
    refused2(&o, "harness: error: --store must be an absolute path");
    assert!(!t.0.join("owed.jsonl").exists());
}

/// A value that starts with `--` is taken as the value, so `--what --by` stores the text `--by`.
#[test]
fn a_value_starting_with_dashes_is_taken_as_the_value() {
    let t = Tmp::new();
    let store = t.store();
    let a = vec![
        s("owed"), s("add"), s("--store"), ss(&store), s("--id"), s("ow-1"),
        s("--at"), s("2026-11-16T00:00:00Z"), s("--by"), s("orch-a"), s("--what"), s("--by"),
    ];
    let o = run(&a);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(fs::read_to_string(&store).unwrap().contains(r#""what":"--by""#));
}
