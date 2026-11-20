//! Process-level tests for `harness ticket merge-text`. The real binary runs as a child; inputs are
//! synthetic and live under the temp folder. The deny word is injected on the child only; home path,
//! email and session marker shapes are assembled at run time. No test reads a deny-list variable or path.
#![cfg(unix)]
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const DENY: &str = "zzdenyzz";
const PREFIX: &str = "harness: merge-text refused: ";
const DIR_LINE: &str = "harness: error: the output folder already holds a merge text file";
const USAGE: &str = "usage: harness ticket merge-text <ticket-file> --pr <n> --body-file <absolute-file> --out-dir <absolute-dir>";
static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("mtext-{}-{}", std::process::id(), SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        // A test may leave a folder read-only.
        let _ = fs::set_permissions(self.0.join("out"), fs::Permissions::from_mode(0o755));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn base() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_CONFIG").env_remove("HARNESS_PRIVACY_LIST").env_remove("HARNESS_PRIVACY_OLD_LIST").env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

fn run_in(args: &[String], cwd: Option<&Path>) -> Output {
    let mut c = base();
    c.arg("ticket").arg("merge-text").args(args).env("HARNESS_PRIVACY_LIST_SECRET", DENY).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap()
}

fn run(args: &[String]) -> Output {
    run_in(args, None)
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

fn is_root() -> bool {
    Command::new("id").arg("-u").output().map(|o| String::from_utf8_lossy(&o.stdout).trim() == "0").unwrap_or(false)
}

fn j(parts: &[&str]) -> String {
    parts.concat()
}

fn home_path() -> String {
    j(&["/Us", "ers/", "zztester", "/proj"])
}

fn email_shape() -> String {
    j(&["zz", "@", "example", ".", "invalid"])
}

fn session_url() -> String {
    j(&["https://clau", "de.ai/code/", "session_01Abc"])
}

fn session_trailer() -> String {
    j(&["Claude-", "Session: something"])
}

fn ticket_text(id: &str, title_json: &str, objective: &str, criterion: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"repo_id\":\"scratch-1\",\"title\":{title_json},\"group\":\"grp\",\"size\":\"M\",\"objective\":\"{objective}\",\"acceptance_criteria\":[\"{criterion}\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}"
    )
}

/// A ticket file, a body file and an empty out folder, all under one temp folder.
struct Case {
    t: Tmp,
    ticket: PathBuf,
    body: PathBuf,
    out: PathBuf,
}

fn case_with(ticket: &str, body: &[u8]) -> Case {
    let t = Tmp::new();
    let ticket_p = t.0.join("ticket.json");
    let body_p = t.0.join("body.txt");
    let out_p = t.0.join("out");
    fs::write(&ticket_p, ticket).unwrap();
    fs::write(&body_p, body).unwrap();
    fs::create_dir(&out_p).unwrap();
    Case { t, ticket: ticket_p, body: body_p, out: out_p }
}

fn case(title_json: &str, body: &[u8]) -> Case {
    case_with(&ticket_text("mt-1", title_json, "An objective", "one"), body)
}

fn good() -> Case {
    case("\"Add the thing\"", b"First line\nSecond line\n")
}

impl Case {
    fn args(&self, pr: &str) -> Vec<String> {
        vec![ss(&self.ticket), s("--pr"), s(pr), s("--body-file"), ss(&self.body), s("--out-dir"), ss(&self.out)]
    }

    fn go(&self) -> Output {
        run(&self.args("7"))
    }

    fn out_count(&self) -> usize {
        fs::read_dir(&self.out).unwrap().count()
    }

    /// Exit `want`, stdout empty, one exact stderr line, no file in the out folder, no temp path printed.
    fn expect(&self, o: &Output, want: i32, line: &str) {
        assert_eq!(code(o), want, "stderr: {}", err(o));
        assert!(out(o).is_empty(), "stdout must be empty: {}", out(o));
        assert_eq!(err(o), format!("{line}\n"));
        assert_eq!(self.out_count(), 0, "a refused run left a file");
        assert!(!err(o).contains(self.t.0.to_str().unwrap()), "stderr holds the temp path");
    }

    fn refused1(&self, o: &Output, line: &str) {
        self.expect(o, 1, &format!("{PREFIX}{line}"));
    }
}

fn body_refused(body: &[u8], line: &str) {
    let c = case("\"A title\"", body);
    let o = c.go();
    c.refused1(&o, line);
}

fn body_ok(body: &[u8]) {
    let c = case("\"A title\"", body);
    let o = c.go();
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(fs::read(c.out.join("mt-1.body")).unwrap(), body);
}

// ---------------------------------------------------------------- the worked case

#[test]
fn the_worked_case() {
    let c = good();
    let o = c.go();
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "merge-text ok ticket=mt-1 pr=7 body_lines=2\n");
    assert!(err(&o).is_empty());
    assert_eq!(fs::read(c.out.join("mt-1.subject")).unwrap(), b"Add the thing (#7)\n");
    assert_eq!(fs::read(c.out.join("mt-1.body")).unwrap(), b"First line\nSecond line\n");
    assert_eq!(c.out_count(), 2);
}

// ---------------------------------------------------------------- usage

#[test]
fn usage_errors_exit_2_with_the_merge_text_usage() {
    let c = good();
    let (t, b, o) = (ss(&c.ticket), ss(&c.body), ss(&c.out));
    let cases: Vec<Vec<String>> = vec![
        vec![],
        vec![s("--pr"), s("7"), s("--body-file"), b.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("second.json"), s("--pr"), s("7"), s("--body-file"), b.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--nope"), s("x"), s("--pr"), s("7"), s("--body-file"), b.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr=7"), s("--body-file"), b.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), b.clone(), s("--out-dir")],
        vec![t.clone(), s("--pr"), s("7"), s("--pr"), s("8"), s("--body-file"), b.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), b.clone(), s("--body-file"), b.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), b.clone(), s("--out-dir"), o.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--body-file"), b.clone(), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), b.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), s("rel/body.txt"), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), b.clone(), s("--out-dir"), s("rel/out")],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), format!("{}/../body.txt", c.t.0.display()), s("--out-dir"), o.clone()],
        vec![t.clone(), s("--pr"), s("7"), s("--body-file"), b.clone(), s("--out-dir"), format!("{}/../out", c.t.0.display())],
    ];
    for (i, args) in cases.iter().enumerate() {
        let r = run(args);
        assert_eq!(code(&r), 2, "case {i}: {}", err(&r));
        assert!(out(&r).is_empty(), "case {i}");
        assert!(err(&r).contains(USAGE), "case {i}: {}", err(&r));
    }
    assert_eq!(c.out_count(), 0);
}

#[test]
fn a_bad_pr_number_exits_2() {
    let c = good();
    for pr in ["0", "012", "12a", "-1", "+1", "1234567890", ""] {
        let o = run(&c.args(pr));
        assert_eq!(code(&o), 2, "pr {pr:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).contains(USAGE));
    }
    let o = run(&c.args("123456789"));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

// ---------------------------------------------------------------- order and exit 2 inputs

const NO_LIST: &str = "harness: error: no privacy list configured: merge text is not written unscanned";

fn run_no_list(args: &[String]) -> Output {
    let mut c = base();
    c.arg("ticket").arg("merge-text").args(args).stdin(Stdio::null());
    c.output().unwrap()
}

#[test]
fn no_privacy_list_exits_2_before_the_body_is_read() {
    let c = case("\"A title\"", &[0xff, 0xfe, b'\n']);
    let o = run_no_list(&c.args("7"));
    c.expect(&o, 2, NO_LIST);
}

#[test]
fn no_privacy_list_exits_2_before_the_ticket_is_read() {
    let mut c = good();
    c.ticket = c.t.0.join("missing.json");
    let o = run_no_list(&c.args("7"));
    c.expect(&o, 2, NO_LIST);
}

#[test]
fn a_missing_ticket_file_exits_2() {
    let mut c = good();
    c.ticket = c.t.0.join("missing.json");
    let o = c.go();
    c.expect(&o, 2, "harness: error: the ticket file is missing or cannot be read");
}

#[test]
fn a_ticket_link_exits_2() {
    let mut c = good();
    let link = c.t.0.join("ticket-link.json");
    std::os::unix::fs::symlink(&c.ticket, &link).unwrap();
    c.ticket = link;
    let o = c.go();
    c.expect(&o, 2, "harness: error: the ticket file is not a regular file");
}

fn mkfifo(p: &Path) {
    let st = Command::new("mkfifo").arg(p).status().unwrap();
    assert!(st.success());
}

/// Run with a 10 second limit; a hang kills the child and fails the test.
fn run_limited(args: &[String]) -> Output {
    let mut c = base();
    let mut child = c
        .arg("ticket")
        .arg("merge-text")
        .args(args)
        .env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the child hung on a fifo");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn a_ticket_fifo_exits_2_without_a_hang() {
    let mut c = good();
    let f = c.t.0.join("ticket.fifo");
    mkfifo(&f);
    c.ticket = f;
    let o = run_limited(&c.args("7"));
    c.expect(&o, 2, "harness: error: the ticket file is not a regular file");
}

#[test]
fn a_ticket_over_1_mib_is_refused() {
    let c = case_with(&" ".repeat(1024 * 1024 + 10), b"one\n");
    let o = c.go();
    c.expect(&o, 1, "harness: ticket refused: ticket is over 1 MiB");
}

#[test]
fn a_ticket_that_is_not_utf8_is_refused() {
    let t = Tmp::new();
    let c = Case { ticket: t.0.join("ticket.json"), body: t.0.join("body.txt"), out: t.0.join("out"), t };
    fs::write(&c.ticket, [0xffu8, 0xfe]).unwrap();
    fs::write(&c.body, b"one\n").unwrap();
    fs::create_dir(&c.out).unwrap();
    let o = c.go();
    c.expect(&o, 1, "harness: ticket refused: ticket is not utf-8");
}

#[test]
fn an_empty_title_is_refused_by_the_reader() {
    let c = case("\"\"", b"one\n");
    let o = c.go();
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert!(err(&o).starts_with("harness: ticket refused: "));
    assert!(err(&o).contains("title"));
    assert_eq!(c.out_count(), 0);
}

#[test]
fn a_ticket_id_with_a_slash_exits_2() {
    let c = case_with(&ticket_text("a/b", "\"A title\"", "An objective", "one"), b"one\n");
    let o = c.go();
    c.expect(&o, 2, "harness: error: the ticket id is not a plain file name");
}

#[test]
fn a_missing_out_dir_exits_2() {
    let c = good();
    fs::remove_dir(&c.out).unwrap();
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), "harness: error: the output folder is missing or unreadable\n");
    assert!(!c.out.exists(), "the verb created a folder");
}

#[test]
fn an_out_dir_that_is_a_file_exits_2() {
    let mut c = good();
    let f = c.t.0.join("out-file");
    fs::write(&f, b"x").unwrap();
    c.out = f;
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), "harness: error: the output folder must be a real folder, not a link or a file\n");
}

#[test]
fn an_out_dir_link_exits_2() {
    let mut c = good();
    let link = c.t.0.join("out-link");
    std::os::unix::fs::symlink(&c.out, &link).unwrap();
    let real = c.out.clone();
    c.out = link;
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), "harness: error: the output folder must be a real folder, not a link or a file\n");
    assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
}

/// The out-dir path is the link plus `suffix`; the run is refused like a plain linked out-dir.
fn out_link_with_suffix(suffix: &str) {
    let mut c = good();
    let link = c.t.0.join("out-link");
    std::os::unix::fs::symlink(&c.out, &link).unwrap();
    let real = c.out.clone();
    c.out = PathBuf::from(format!("{}{suffix}", link.display()));
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), "harness: error: the output folder must be a real folder, not a link or a file\n");
    assert_eq!(fs::read_dir(&real).unwrap().count(), 0);
}

#[test]
fn an_out_dir_link_with_a_trailing_slash_exits_2() {
    out_link_with_suffix("/");
}

#[test]
fn an_out_dir_link_with_a_trailing_dot_exits_2() {
    out_link_with_suffix("/.");
}

const WORK_TREE_LINE: &str = "harness: error: the output folder lies inside a work tree";

#[test]
fn an_out_dir_inside_a_work_tree_exits_2() {
    let mut c = good();
    let wt = c.t.0.join("wt");
    fs::create_dir_all(wt.join(".git")).unwrap();
    let deep = wt.join("a").join("b");
    fs::create_dir_all(&deep).unwrap();
    c.out = deep.clone();
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), format!("{WORK_TREE_LINE}\n"));
    assert_eq!(fs::read_dir(&deep).unwrap().count(), 0);
}

#[test]
fn an_out_dir_holding_a_git_file_exits_2() {
    let c = good();
    fs::write(c.out.join(".git"), b"gitdir: elsewhere\n").unwrap();
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), format!("{WORK_TREE_LINE}\n"));
    assert_eq!(c.out_count(), 1);
}

#[test]
fn an_out_dir_under_a_linked_parent_inside_a_work_tree_exits_2() {
    let mut c = good();
    let wt = c.t.0.join("wt");
    let inner = wt.join("inner");
    fs::create_dir_all(wt.join(".git")).unwrap();
    fs::create_dir_all(inner.join("real-out")).unwrap();
    let link = c.t.0.join("link");
    std::os::unix::fs::symlink(&inner, &link).unwrap();
    c.out = link.join("real-out");
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), format!("{WORK_TREE_LINE}\n"));
    assert_eq!(fs::read_dir(inner.join("real-out")).unwrap().count(), 0);
}

#[test]
fn a_not_writable_out_dir_exits_2() {
    if is_root() {
        eprintln!("skipped: running as root, mode 555 does not stop a write");
        return;
    }
    let c = good();
    fs::set_permissions(&c.out, fs::Permissions::from_mode(0o555)).unwrap();
    let o = c.go();
    c.expect(&o, 2, "harness: error: the output folder cannot be written");
}

#[test]
fn a_missing_body_file_exits_2() {
    let mut c = good();
    c.body = c.t.0.join("missing.txt");
    let o = c.go();
    c.expect(&o, 2, "harness: error: the body file is missing or cannot be read");
}

#[test]
fn an_unreadable_body_file_exits_2() {
    if is_root() {
        eprintln!("skipped: running as root, mode 000 does not stop a read");
        return;
    }
    let c = good();
    fs::set_permissions(&c.body, fs::Permissions::from_mode(0o000)).unwrap();
    let o = c.go();
    c.expect(&o, 2, "harness: error: the body file is missing or cannot be read");
}

#[test]
fn a_body_link_exits_2() {
    let mut c = good();
    let link = c.t.0.join("body-link.txt");
    std::os::unix::fs::symlink(&c.body, &link).unwrap();
    c.body = link;
    let o = c.go();
    c.expect(&o, 2, "harness: error: the body file is not a regular file");
}

#[test]
fn a_body_folder_exits_2() {
    let mut c = good();
    let d = c.t.0.join("body-dir");
    fs::create_dir(&d).unwrap();
    c.body = d;
    let o = c.go();
    c.expect(&o, 2, "harness: error: the body file is not a regular file");
}

#[test]
fn a_body_fifo_exits_2_without_a_hang() {
    let mut c = good();
    let f = c.t.0.join("body.fifo");
    mkfifo(&f);
    c.body = f;
    let o = run_limited(&c.args("7"));
    c.expect(&o, 2, "harness: error: the body file is not a regular file");
}

// ---------------------------------------------------------------- subject checks

#[test]
fn a_blank_title_is_refused() {
    let c = case("\"   \"", b"one\n");
    let o = c.go();
    c.refused1(&o, "title is blank");
}

#[test]
fn a_title_with_a_newline_is_refused() {
    let c = case("\"a\\nb\"", b"one\n");
    let o = c.go();
    c.refused1(&o, "title has a control character");
}

#[test]
fn a_title_with_a_tab_is_refused() {
    let c = case("\"a\\tb\"", b"one\n");
    let o = c.go();
    c.refused1(&o, "title has a control character");
}

#[test]
fn a_title_with_a_line_separator_is_refused() {
    let c = case("\"a\\u2028b\"", b"one\n");
    let o = c.go();
    c.refused1(&o, "title has a control character");
}

fn title_of(len: usize) -> String {
    let mut t = "ab ".repeat(len / 3 + 1);
    t.truncate(len - 1);
    t.push('x');
    assert_eq!(t.len(), len);
    t
}

#[test]
fn a_subject_of_256_bytes_is_written() {
    let c = case(&format!("\"{}\"", title_of(251)), b"one\n");
    let o = c.go_with_pr("1");
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(fs::read(c.out.join("mt-1.subject")).unwrap().len(), 257);
}

#[test]
fn a_subject_of_257_bytes_is_refused() {
    let c = case(&format!("\"{}\"", title_of(252)), b"one\n");
    let o = c.go_with_pr("1");
    c.refused1(&o, "subject is over 256 bytes");
}

impl Case {
    fn go_with_pr(&self, pr: &str) -> Output {
        run(&self.args(pr))
    }
}

// ---------------------------------------------------------------- body checks

#[test]
fn a_body_of_4096_bytes_is_written() {
    let mut b = "word ".repeat(819).into_bytes();
    b.push(b'\n');
    assert_eq!(b.len(), 4096);
    body_ok(&b);
}

#[test]
fn a_body_of_4097_bytes_is_refused() {
    let mut b = "word ".repeat(819).into_bytes();
    b.extend_from_slice(b"x\n");
    assert_eq!(b.len(), 4097);
    body_refused(&b, "body is over 4096 bytes");
}

#[test]
fn an_empty_body_is_refused() {
    body_refused(b"", "body is empty (0 bytes)");
}

#[test]
fn a_body_that_is_not_utf8_is_refused() {
    body_refused(&[b'a', 0xff, b'\n'], "body is not UTF-8");
}

#[test]
fn a_body_with_no_final_newline_is_refused() {
    body_refused(b"one\ntwo", "body does not end with a newline");
}

fn numbered(n: usize) -> Vec<u8> {
    (1..=n).map(|i| format!("line {i}\n")).collect::<String>().into_bytes()
}

#[test]
fn a_body_of_20_lines_is_written() {
    body_ok(&numbered(20));
}

#[test]
fn a_body_of_21_lines_is_refused() {
    body_refused(&numbered(21), "body has more than 20 lines");
}

#[test]
fn a_crlf_body_is_refused() {
    body_refused(b"one\r\ntwo\r\n", "body line 1 has a control character");
}

#[test]
fn a_body_with_a_tab_is_refused() {
    body_refused(b"one\ntw\to\n", "body line 2 has a control character");
}

#[test]
fn a_body_with_a_nul_byte_is_refused() {
    body_refused(b"ab\0cd\n", "body line 1 has a control character");
}

#[test]
fn a_body_line_with_u2029_is_refused() {
    body_refused("one\ntwo\u{2029}x\n".as_bytes(), "body line 2 has a control character");
}

#[test]
fn a_body_of_one_blank_line_is_refused() {
    body_refused(b"\n", "body line 1 is blank at the start or the end");
}

#[test]
fn a_blank_last_line_is_refused() {
    body_refused(b"one\n\n", "body line 2 is blank at the start or the end");
}

#[test]
fn a_blank_line_in_the_middle_is_kept() {
    body_ok(b"one\n\ntwo\n");
}

#[test]
fn a_trailer_line_is_refused() {
    let session = session_trailer();
    for t in ["Co-authored-by: x", "Co-authored-by : x", "Signed-off-by:y", "Follow-up: x", session.as_str()] {
        body_refused(format!("one\ntwo\n{t}\nfour\n").as_bytes(), "body line 3 looks like a trailer");
    }
}

#[test]
fn a_trailer_line_after_leading_blanks_is_refused() {
    body_refused(b"one\n  Co-authored-by: x\nthree\n", "body line 2 looks like a trailer");
}

#[test]
fn lines_that_only_look_close_to_a_trailer_pass() {
    body_ok(b"Note: x\na - b: c\nsee core/rust: x\n");
}

#[test]
fn a_plain_url_passes() {
    body_ok(b"see https://example.invalid/path for more\n");
}

// ---------------------------------------------------------------- the subject-line rule

fn subject_rule_ok(title_json: &str, body: &[u8]) {
    let c = case(title_json, body);
    let o = c.go();
    let lines = body.iter().filter(|b| **b == b'\n').count();
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("merge-text ok ticket=mt-1 pr=7 body_lines={lines}\n"));
    assert!(err(&o).is_empty());
    assert_eq!(fs::read(c.out.join("mt-1.body")).unwrap(), body);
}

fn subject_rule_refused(title_json: &str, body: &[u8], line: &str) {
    let c = case(title_json, body);
    let o = c.go();
    c.refused1(&o, line);
}

#[test]
fn a_body_line_1_equal_to_the_title_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"Add the thing\nmore\n", "body line 1 is the ticket title");
}

#[test]
fn a_body_line_1_equal_to_the_title_with_blanks_around_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"  Add the thing  \nmore\n", "body line 1 is the ticket title");
}

#[test]
fn a_body_line_1_equal_to_the_squash_subject_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"Add the thing (#7)\nmore\n", "body line 1 is the squash subject");
}

#[test]
fn a_body_line_1_in_type_scope_form_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"feat(ticket): other words\nmore\n", "body line 1 has the form of a commit subject");
}

#[test]
fn a_body_line_1_in_type_scope_form_with_a_bang_and_a_dashed_scope_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"fix(a-b)!: x\nmore\n", "body line 1 has the form of a commit subject");
}

#[test]
fn a_body_line_1_in_type_only_form_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"fix: x\nmore\n", "body line 1 has the form of a commit subject");
}

#[test]
fn a_body_line_1_in_type_only_form_with_a_bang_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"docs!: x\nmore\n", "body line 1 has the form of a commit subject");
}

#[test]
fn a_body_line_1_in_type_scope_form_after_two_blanks_is_refused() {
    subject_rule_refused("\"Add the thing\"", b"  feat(x): y\nmore\n", "body line 1 has the form of a commit subject");
}

#[test]
fn a_type_scope_title_equal_to_body_line_1_is_the_title_refusal() {
    subject_rule_refused("\"feat(x): add the thing\"", b"feat(x): add the thing\nmore\n", "body line 1 is the ticket title");
}

#[test]
fn a_type_scope_squash_subject_as_body_line_1_is_refused() {
    subject_rule_refused("\"feat(x): add the thing\"", b"feat(x): add the thing (#7)\nmore\n", "body line 1 is the squash subject");
}

#[test]
fn the_body_checks_come_before_the_subject_line_rule() {
    subject_rule_refused("\"Add the thing\"", b"Add the thing\ntwo\nCo-authored-by: x\n", "body line 3 looks like a trailer");
}

#[test]
fn the_subject_line_rule_comes_before_the_privacy_scan() {
    subject_rule_refused("\"Add the thing\"", format!("Add the thing\n{DENY}\n").as_bytes(), "body line 1 is the ticket title");
}

#[test]
fn a_capitalized_type_only_line_1_passes() {
    subject_rule_ok("\"Add the thing\"", b"Fix: a typo\nmore\n");
}

#[test]
fn a_type_only_word_outside_the_list_as_line_1_passes() {
    subject_rule_ok("\"Add the thing\"", b"data: rows 1 to 5\nmore\n");
    subject_rule_ok("\"Add the thing\"", b"probe: x\nmore\n");
}

#[test]
fn a_line_1_with_no_space_after_the_colon_passes() {
    subject_rule_ok("\"Add the thing\"", b"feat:x\nmore\n");
}

#[test]
fn a_line_1_with_a_space_before_the_parenthesis_passes() {
    subject_rule_ok("\"Add the thing\"", b"fix (x): y\nmore\n");
}

#[test]
fn a_see_line_1_passes() {
    subject_rule_ok("\"Add the thing\"", b"see core/rust: x\n");
}

#[test]
fn the_title_as_line_2_passes() {
    subject_rule_ok("\"Add the thing\"", b"First line\nAdd the thing\n");
}

#[test]
fn a_line_1_holding_the_title_inside_a_sentence_passes() {
    subject_rule_ok("\"Add the thing\"", b"the plan adds Add the thing today\nmore\n");
}

#[test]
fn a_line_1_with_the_title_and_a_full_stop_passes() {
    subject_rule_ok("\"Add the thing\"", b"Add the thing.\nmore\n");
}

// ---------------------------------------------------------------- privacy scan

fn hit_prefix(what: &str) -> String {
    format!("{PREFIX}{what} has a privacy hit (")
}

fn assert_hit(c: &Case, o: &Output, what: &str, class: Option<&str>) {
    assert_eq!(code(o), 1, "stderr: {}", err(o));
    assert!(out(o).is_empty());
    let e = err(o);
    assert!(e.starts_with(&hit_prefix(what)), "stderr: {e}");
    assert!(e.ends_with(")\n"), "stderr: {e}");
    assert_eq!(e.lines().count(), 1);
    if let Some(cl) = class {
        assert!(e.contains(cl), "stderr: {e}");
    }
    assert!(!e.contains(DENY) && !e.contains("zztester") && !e.contains("session_01"));
    assert_eq!(c.out_count(), 0);
}

#[test]
fn a_deny_word_in_the_title_is_refused() {
    let c = case(&format!("\"a {DENY} b\""), b"one\n");
    let o = c.go();
    assert_hit(&c, &o, "subject", Some("deny-token"));
}

#[test]
fn a_home_path_in_the_body_is_refused() {
    let c = case("\"A title\"", format!("one\nsee {} here\n", home_path()).as_bytes());
    let o = c.go();
    assert_hit(&c, &o, "body line 2", Some("home-path"));
}

#[test]
fn an_email_in_the_body_is_refused() {
    let c = case("\"A title\"", format!("one\nmail {} now\n", email_shape()).as_bytes());
    let o = c.go();
    assert_hit(&c, &o, "body line 2", Some("email"));
}

#[test]
fn a_session_link_in_the_body_is_refused() {
    let c = case("\"A title\"", format!("one\nsee {} now\n", session_url()).as_bytes());
    let o = c.go();
    assert_hit(&c, &o, "body line 2", Some("session-link"));
}

#[test]
fn a_privacy_hit_on_line_20_is_refused() {
    let mut b = numbered(19);
    b.extend_from_slice(format!("path {} end\n", home_path()).as_bytes());
    let c = case("\"A title\"", &b);
    let o = c.go();
    assert_hit(&c, &o, "body line 20", Some("home-path"));
}

// ---------------------------------------------------------------- writes

#[test]
fn a_second_run_into_the_same_folder_exits_2() {
    let c = good();
    let first = c.go();
    assert_eq!(code(&first), 0, "{}", err(&first));
    let (s1, b1) = (fs::read(c.out.join("mt-1.subject")).unwrap(), fs::read(c.out.join("mt-1.body")).unwrap());
    let second = c.go();
    assert_eq!(code(&second), 2);
    assert!(out(&second).is_empty());
    assert_eq!(err(&second), format!("{DIR_LINE}\n"));
    assert_eq!(fs::read(c.out.join("mt-1.subject")).unwrap(), s1);
    assert_eq!(fs::read(c.out.join("mt-1.body")).unwrap(), b1);
    assert_eq!(c.out_count(), 2);
}

#[test]
fn an_existing_subject_link_is_not_written_through() {
    let c = good();
    let outside = c.t.0.join("outside.txt");
    fs::write(&outside, b"keep\n").unwrap();
    let link = c.out.join("mt-1.subject");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), format!("{DIR_LINE}\n"));
    assert_eq!(fs::read(&outside).unwrap(), b"keep\n");
    assert_eq!(fs::read_link(&link).unwrap(), outside);
    assert_eq!(c.out_count(), 1);
}

#[test]
fn an_existing_body_folder_exits_2() {
    let c = good();
    let d = c.out.join("mt-1.body");
    fs::create_dir(&d).unwrap();
    let o = c.go();
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    assert_eq!(err(&o), format!("{DIR_LINE}\n"));
    assert!(d.is_dir());
    assert_eq!(c.out_count(), 1);
}

#[test]
fn a_refused_run_leaves_no_file() {
    let c = case("\"A title\"", b"one\ntwo\nCo-authored-by: x\n");
    let o = c.go();
    c.refused1(&o, "body line 3 looks like a trailer");
}

fn snapshot(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, root: &Path, m: &mut BTreeMap<String, String>) {
        for e in fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            let key = p.strip_prefix(root).unwrap().to_str().unwrap().to_string();
            let md = fs::symlink_metadata(&p).unwrap();
            if md.file_type().is_symlink() {
                m.insert(key, format!("link {}", fs::read_link(&p).unwrap().display()));
            } else if md.is_dir() {
                m.insert(key, "dir".to_string());
                walk(&p, root, m);
            } else {
                m.insert(key, format!("file {} {:?}", md.len(), fs::read(&p).unwrap()));
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

#[test]
fn nothing_else_is_written() {
    let t = Tmp::new();
    let d = &t.0;
    let cwd = d.join("cwd");
    fs::create_dir(&cwd).unwrap();
    fs::write(d.join("ticket.json"), ticket_text("mt-1", "\"A title\"", "An objective", "one")).unwrap();
    fs::write(d.join("good.txt"), b"one\ntwo\n").unwrap();
    fs::write(d.join("bad.txt"), b"one\nSigned-off-by:y\n").unwrap();
    fs::create_dir(d.join("out0")).unwrap();
    fs::create_dir(d.join("out1")).unwrap();
    let args = |body: &str, out_dir: &str| vec![ss(&d.join("ticket.json")), s("--pr"), s("5"), s("--body-file"), ss(&d.join(body)), s("--out-dir"), ss(&d.join(out_dir))];
    // exit 0
    let before = snapshot(d);
    let o = run_in(&args("good.txt", "out0"), Some(&cwd));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let mut after = snapshot(d);
    assert!(after.remove("out0/mt-1.subject").is_some());
    assert!(after.remove("out0/mt-1.body").is_some());
    assert_eq!(before, after);
    // exit 1
    let before = snapshot(d);
    let o = run_in(&args("bad.txt", "out1"), Some(&cwd));
    assert_eq!(code(&o), 1);
    assert_eq!(before, snapshot(d));
    // exit 2
    let o = run_in(&args("good.txt", "no-such-out"), Some(&cwd));
    assert_eq!(code(&o), 2);
    assert_eq!(before, snapshot(d));
    assert_eq!(fs::read_dir(&cwd).unwrap().count(), 0);
}

// ---------------------------------------------------------------- no input text is printed

#[test]
fn no_input_text_is_printed() {
    const M: &str = "zzmarkerzz";
    let t = Tmp::new();
    let d = &t.0;
    let ticket = d.join("ticket.json");
    fs::write(&ticket, ticket_text("mt-1", &format!("\"{M} title\""), &format!("{M} objective"), &format!("{M} criterion"))).unwrap();
    let home_line = format!("{M} {}\n", home_path());
    let bodies: Vec<(i32, Vec<u8>)> = vec![
        (0, format!("a line with {M} in it\n").into_bytes()),
        (1, format!("one\n{M}-x: y\nend\n").into_bytes()),
        (1, format!("one\n{M}\tnext\n").into_bytes()),
        (1, home_line.into_bytes()),
    ];
    let mut runs: Vec<(i32, PathBuf)> = Vec::new();
    for (i, (want, b)) in bodies.iter().enumerate() {
        let p = d.join(format!("body{i}.txt"));
        fs::write(&p, b).unwrap();
        runs.push((*want, p));
    }
    let target = d.join(format!("{M}-target.txt"));
    fs::write(&target, b"one\n").unwrap();
    let link = d.join("body-link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    runs.push((2, link));
    // the link target name holds the marker; a path to the link is exit 2
    for (i, (want, body)) in runs.iter().enumerate() {
        let out_dir = d.join(format!("out{i}"));
        fs::create_dir(&out_dir).unwrap();
        let a = vec![ss(&ticket), s("--pr"), s("3"), s("--body-file"), ss(body), s("--out-dir"), ss(&out_dir)];
        let o = run(&a);
        assert_eq!(code(&o), *want, "run {i}: {}", err(&o));
        for text in [out(&o), err(&o)] {
            assert!(!text.contains(M), "run {i} printed the marker: {text}");
            assert!(!text.contains(&ss(body)), "run {i} printed the body path");
            assert!(!text.contains(&ss(&out_dir)), "run {i} printed the out path");
        }
    }
}
