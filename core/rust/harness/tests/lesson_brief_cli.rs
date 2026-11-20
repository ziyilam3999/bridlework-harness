//! `harness lesson brief`: real binary, synthetic fixtures only (no real ids, names, paths or text). Each
//! check runs the real binary as a child process. The privacy list is injected on the child only; the deny
//! word and the test-mode marker are built at run time so this file does not spell them out. A test that
//! ends well removes its scratch folders with `done()`; a failed test leaves them for a look.
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    /// The scratch paths this test made (a path may be absent when the test ends).
    static SCRATCH: RefCell<Vec<PathBuf>> = const { RefCell::new(Vec::new()) };
}

/// The usage text of `harness lesson`: the four lines of today plus the new brief line.
const USAGE: &str = concat!(
    "usage: harness lesson lint <path> [<path> ...]\n",
    "       harness lesson lookup <lessons-dir> --ticket <ticket-file> --scope-file <file>\n",
    "       harness lesson lookup <lessons-dir> --repo-dir <absolute-dir> --from <full-sha> --to <full-sha>\n",
    "       harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir> [--config <absolute-file> --controls <absolute-file> [--rows <absolute-file>]]\n",
    "       harness lesson brief <lessons-dir> --ticket <ticket-file> --scope-file <file> --out <absolute-dir>\n",
);

/// The scratch name: the tag, the process id and a sequence number; it is noted for `done()`.
fn scratch_name(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("lbrief-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    SCRATCH.with(|s| s.borrow_mut().push(p.clone()));
    p
}

/// Remove what is at `d` (a link is removed, not followed); nothing happens when nothing is there.
fn remove_any(d: &Path) {
    if let Ok(md) = fs::symlink_metadata(d) {
        if md.is_dir() {
            fs::remove_dir_all(d).unwrap_or_else(|e| panic!("scratch folder {} was not removed: {}", d.display(), e));
        } else {
            fs::remove_file(d).unwrap_or_else(|e| panic!("scratch entry {} was not removed: {}", d.display(), e));
        }
    }
}

/// Remove the scratch paths of this test (called as the last step of a test that ended well).
fn done() {
    let all: Vec<PathBuf> = SCRATCH.with(|s| s.borrow_mut().drain(..).collect());
    for p in all {
        remove_any(&p);
    }
}

/// Remove what is at `d` and make an empty folder there.
fn make_scratch(d: &Path) {
    remove_any(d);
    fs::create_dir(d).unwrap_or_else(|e| panic!("scratch folder {} was not made: {}", d.display(), e));
}

fn tmp(tag: &str) -> PathBuf {
    let d = scratch_name(tag);
    make_scratch(&d);
    d
}

/// A path that is absent now (the out folder of a run is made by the run, not by the test).
fn fresh(tag: &str) -> PathBuf {
    let d = scratch_name(tag);
    remove_any(&d);
    d
}

fn link(target: &Path, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// The synthetic deny word of the injected list, built at run time.
fn deny() -> String {
    ["zz", "deny", "word", "zz"].concat()
}

/// The one entry of the clean list: a word no fixture holds.
fn clean() -> String {
    ["zz", "clean", "word", "zz"].concat()
}

/// The synthetic test-mode marker, assembled at run time.
fn marker() -> String {
    ["Synthetic", "-Test", "-Marker"].concat()
}

/// The child with no config or list variable inherited: each input arrives as a flag or an explicit `.env`.
fn base() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

fn run_with(args: &[String], list: &str) -> Output {
    let mut c = base();
    c.args(args).env("HARNESS_PRIVACY_LIST_SECRET", list).stdin(Stdio::null());
    c.output().unwrap()
}

/// A child with no list at all (the three HARNESS_PRIVACY names removed, nothing set).
fn run_no_list(args: &[String]) -> Output {
    let mut c = base();
    c.args(args).stdin(Stdio::null());
    c.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn s(v: &str) -> String {
    v.to_string()
}

// ---------------------------------------------------------------- fixtures

fn ticket_json(repo: &str, title: &str, criteria: &[&str]) -> String {
    let crit: Vec<String> = criteria.iter().map(|c| format!("\"{}\"", c)).collect();
    format!(
        "{{\"id\":\"T-1\",\"repo_id\":\"{}\",\"title\":\"{}\",\"group\":\"grp\",\"size\":\"S\",\"objective\":\"obj\",\"acceptance_criteria\":[{}],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}",
        repo,
        title,
        crit.join(",")
    )
}

fn default_ticket(title: &str) -> Vec<u8> {
    ticket_json("example", title, &["checks/hello.sh passes"]).into_bytes()
}

/// A scratch folder with `lessons/`, `ticket.json`, `scope.txt` and the given pages.
fn setup(tag: &str, ticket: &[u8], scope: &[u8], pages: &[(&str, String)]) -> PathBuf {
    let d = tmp(tag);
    fill(&d, ticket, scope, pages);
    d
}

fn fill(d: &Path, ticket: &[u8], scope: &[u8], pages: &[(&str, String)]) {
    fs::create_dir_all(d.join("lessons")).unwrap();
    fs::write(d.join("ticket.json"), ticket).unwrap();
    fs::write(d.join("scope.txt"), scope).unwrap();
    for (n, t) in pages {
        fs::write(d.join("lessons").join(n), t).unwrap();
    }
}

/// A matched page: token `example` (the repo id of the default ticket), heading `Widget lesson`.
fn good_match() -> String {
    "---\nmatch: [example]\n---\n# Widget lesson\n".to_string()
}

/// An always page with the given rule sentence.
fn always_page(rule: &str) -> String {
    format!("---\nalways: {}\n---\n# Widget rule\n", rule)
}

fn brief_args(d: &Path, out: &Path) -> Vec<String> {
    vec![
        s("lesson"),
        s("brief"),
        d.join("lessons").to_str().unwrap().to_string(),
        s("--ticket"),
        d.join("ticket.json").to_str().unwrap().to_string(),
        s("--scope-file"),
        d.join("scope.txt").to_str().unwrap().to_string(),
        s("--out"),
        out.to_str().unwrap().to_string(),
    ]
}

/// The usual run: the clean list injected, the out folder absent.
fn brief(d: &Path, out: &Path) -> Output {
    run_with(&brief_args(d, out), &clean())
}

/// The ticket-mode lookup over the same inputs.
fn lookup(d: &Path) -> Output {
    let mut c = base();
    c.args([
        "lesson",
        "lookup",
        d.join("lessons").to_str().unwrap(),
        "--ticket",
        d.join("ticket.json").to_str().unwrap(),
        "--scope-file",
        d.join("scope.txt").to_str().unwrap(),
    ])
    .env("HARNESS_PRIVACY_LIST_SECRET", clean())
    .stdin(Stdio::null());
    c.output().unwrap()
}

fn absent(p: &Path) -> bool {
    fs::symlink_metadata(p).is_err()
}

/// The names of the entries directly inside `d`, sorted.
fn listing(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
    v.sort();
    v
}

// ---------------------------------------------------------------- argument stage

#[test]
fn an_unknown_option_exits_2() {
    let d = setup("arg-unknown", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-unknown");
    for flag in ["--repo-dir", "--from", "--to", "--bogus"] {
        let mut a = brief_args(&d, &out);
        a.push(s(flag));
        a.push(s("x"));
        let o = run_with(&a, &clean());
        assert_eq!(code(&o), 2, "{}: {}", flag, stderr(&o));
        assert!(stdout(&o).is_empty(), "{}", flag);
        assert_eq!(stderr(&o), format!("harness: error: unknown option {}\n", flag), "{}", flag);
    }
    done();
}

#[test]
fn a_flag_with_no_value_exits_2() {
    let d = setup("arg-noval", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-noval");
    let mut a = brief_args(&d, &out);
    a.pop();
    a.pop(); // drop --out and its value
    a.push(s("--out"));
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: --out needs a value\n");
    done();
}

#[test]
fn a_flag_given_twice_exits_2() {
    let d = setup("arg-twice", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-twice");
    let mut a = brief_args(&d, &out);
    a.push(s("--ticket"));
    a.push(d.join("ticket.json").to_str().unwrap().to_string());
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: --ticket given twice\n");
    done();
}

#[test]
fn two_lessons_folders_exit_2() {
    let d = setup("arg-twofold", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-twofold");
    let mut a = brief_args(&d, &out);
    a.insert(2, d.join("lessons").to_str().unwrap().to_string());
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: more than one lessons folder given\n");
    done();
}

#[test]
fn no_lessons_folder_prints_the_usage() {
    let d = setup("arg-nodir", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-nodir");
    let mut a = brief_args(&d, &out);
    a.remove(2);
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    done();
}

#[test]
fn the_usage_text_keeps_its_four_lines_and_gains_the_brief_line() {
    let o = run_with(&[s("lesson"), s("brief")], &clean());
    assert_eq!(stderr(&o), USAGE);
    assert_eq!(code(&o), 2);
    done();
}

#[test]
fn no_ticket_flag_exits_2() {
    let d = setup("arg-noticket", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-noticket");
    let mut a = brief_args(&d, &out);
    a.remove(3);
    a.remove(3); // drop --ticket and its value
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: --ticket is required\n");
    done();
}

#[test]
fn no_scope_file_flag_exits_2() {
    let d = setup("arg-noscope", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-noscope");
    let mut a = brief_args(&d, &out);
    a.remove(5);
    a.remove(5); // drop --scope-file and its value
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: --scope-file is required\n");
    done();
}

#[test]
fn no_out_flag_exits_2() {
    let d = setup("arg-noout", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-noout");
    let mut a = brief_args(&d, &out);
    a.pop();
    a.pop(); // drop --out and its value
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: --out is required\n");
    done();
}

#[test]
fn no_ticket_and_no_out_prints_the_ticket_line() {
    let d = setup("arg-none", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("arg-none");
    let a = vec![
        s("lesson"),
        s("brief"),
        d.join("lessons").to_str().unwrap().to_string(),
        s("--scope-file"),
        d.join("scope.txt").to_str().unwrap().to_string(),
    ];
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: --ticket is required\n");
    assert!(absent(&out));
    done();
}

#[test]
fn a_relative_out_value_prints_the_usage() {
    let d = setup("arg-rel", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let a = brief_args(&d, Path::new("out-here"));
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    done();
}

#[test]
fn an_out_value_with_a_dot_dot_part_prints_the_usage() {
    let d = setup("arg-dotdot", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let base = tmp("arg-dotdot-base");
    let out = base.join("..").join("gone-out");
    let a = brief_args(&d, &out);
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    done();
}

#[test]
fn an_out_value_with_a_control_character_prints_the_usage() {
    let d = setup("arg-ctrl", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let base = tmp("arg-ctrl-base");
    let out = base.join("out\u{1b}x");
    let a = brief_args(&d, &out);
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    done();
}

#[test]
fn an_out_value_with_a_blank_other_than_a_space_prints_the_usage() {
    let d = setup("arg-nbsp", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let base = tmp("arg-nbsp-base");
    for name in ["out\u{a0}x", "out\tx"] {
        let out = base.join(name);
        let a = brief_args(&d, &out);
        let o = run_with(&a, &clean());
        assert_eq!(code(&o), 2, "{}", name);
        assert!(stdout(&o).is_empty(), "{}", name);
        assert_eq!(stderr(&o), USAGE, "{}", name);
    }
    done();
}

/// The out value with a slash added at its end.
fn with_slash(p: &Path) -> PathBuf {
    PathBuf::from(format!("{}/", p.to_str().unwrap()))
}

#[test]
fn an_out_value_ending_in_a_slash_over_a_dangling_link_prints_the_usage_and_makes_nothing() {
    let d = setup("slash-dangling", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let gone = fresh("slash-dangling-gone");
    let out = fresh("slash-dangling-path");
    link(&gone, &out);
    let o = brief(&d, &with_slash(&out));
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    assert!(absent(&gone), "the link target was made");
    assert!(fs::symlink_metadata(&out).unwrap().file_type().is_symlink());
    done();
}

#[test]
fn an_out_value_ending_in_a_slash_over_a_free_path_prints_the_usage_and_makes_nothing() {
    let d = setup("slash-free", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("slash-free-path");
    let o = brief(&d, &with_slash(&out));
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    assert!(absent(&out));
    done();
}

#[test]
fn an_out_value_ending_in_a_slash_over_a_link_to_a_folder_prints_the_usage_and_leaves_the_target() {
    let d = setup("slash-link", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let target = tmp("slash-link-target");
    let out = fresh("slash-link-path");
    link(&target, &out);
    let o = brief(&d, &with_slash(&out));
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    assert_eq!(listing(&target), Vec::<String>::new());
    assert!(fs::symlink_metadata(&out).unwrap().file_type().is_symlink());
    done();
}

#[test]
fn a_root_only_out_value_prints_the_usage() {
    let d = setup("slash-root", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let o = brief(&d, Path::new("/"));
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), USAGE);
    done();
}

// ---------------------------------------------------------------- the block

#[test]
fn the_block_prints_the_always_lines_before_the_lesson_lines() {
    let d = setup(
        "block-order",
        &default_ticket("t"),
        b"src/\n",
        &[
            ("ISSUE-101-a.md", good_match()),
            ("ISSUE-102-b.md", always_page("Restart the counter after a bump.")),
            ("ISSUE-103-c.md", good_match()),
            ("ISSUE-104-d.md", always_page("Check the queue depth before a restart.")),
        ],
    );
    let out = fresh("block-order");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).is_empty());
    let want = format!(
        "Lessons for this ticket (harness lesson brief): always=2 matched=2\n\
         Rules from earlier mistakes, kept for this change:\n\
         always: ISSUE-102 Restart the counter after a bump.\n\
         always: ISSUE-104 Check the queue depth before a restart.\n\
         Pages on earlier mistakes that match this ticket. Read each file before you write code.\n\
         folder: {}\n\
         lesson: ISSUE-101 Widget lesson ISSUE-101-a.md\n\
         lesson: ISSUE-103 Widget lesson ISSUE-103-c.md\n",
        out.to_str().unwrap()
    );
    assert_eq!(stdout(&o), want);
    done();
}

#[test]
fn two_always_pages_and_no_matched_page() {
    let d = setup(
        "block-always",
        &default_ticket("t"),
        b"src/\n",
        &[
            ("ISSUE-101-a.md", always_page("Restart the counter after a bump.")),
            ("ISSUE-102-b.md", always_page("Check the queue depth before a restart.")),
        ],
    );
    let out = fresh("block-always");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).is_empty());
    assert_eq!(
        stdout(&o),
        "Lessons for this ticket (harness lesson brief): always=2 matched=0\n\
         Rules from earlier mistakes, kept for this change:\n\
         always: ISSUE-101 Restart the counter after a bump.\n\
         always: ISSUE-102 Check the queue depth before a restart.\n"
    );
    assert!(out.is_dir());
    assert_eq!(listing(&out), Vec::<String>::new());
    done();
}

#[test]
fn two_matched_pages_and_no_always_page() {
    let d = setup(
        "block-matched",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", good_match()), ("ISSUE-102-b.md", good_match())],
    );
    let out = fresh("block-matched");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).is_empty());
    let want = format!(
        "Lessons for this ticket (harness lesson brief): always=0 matched=2\n\
         Pages on earlier mistakes that match this ticket. Read each file before you write code.\n\
         folder: {}\n\
         lesson: ISSUE-101 Widget lesson ISSUE-101-a.md\n\
         lesson: ISSUE-102 Widget lesson ISSUE-102-b.md\n",
        out.to_str().unwrap()
    );
    assert_eq!(stdout(&o), want);
    done();
}

#[test]
fn pages_but_none_offered_prints_one_line() {
    let d = setup(
        "block-none",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", "---\nmatch: [nomatchtoken]\n---\n# Widget lesson\n".to_string())],
    );
    let out = fresh("block-none");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stderr(&o).is_empty());
    assert_eq!(stdout(&o), "Lessons for this ticket (harness lesson brief): always=0 matched=0\n");
    assert!(out.is_dir());
    assert_eq!(listing(&out), Vec::<String>::new());
    done();
}

#[test]
fn the_always_and_lesson_lines_match_the_lookup_stdout() {
    let d = setup(
        "block-agree",
        &default_ticket("t"),
        b"src/\n",
        &[
            ("ISSUE-101-a.md", good_match()),
            ("ISSUE-102-b.md", always_page("Restart the counter after a bump.")),
            ("ISSUE-103-c.md", good_match()),
        ],
    );
    let out = fresh("block-agree");
    let b = brief(&d, &out);
    assert_eq!(code(&b), 0, "{}", stderr(&b));
    let l = lookup(&d);
    assert_eq!(code(&l), 0, "{}", stderr(&l));
    let pick = |t: String| -> Vec<String> {
        t.lines().filter(|l| l.starts_with("always: ") || l.starts_with("lesson: ")).map(s).collect()
    };
    assert_eq!(pick(stdout(&b)), pick(stdout(&l)));
    done();
}

// ---------------------------------------------------------------- copies

#[test]
fn the_out_folder_holds_exactly_the_matched_page_copies() {
    let d = setup(
        "copies-list",
        &default_ticket("t"),
        b"src/\n",
        &[
            ("ISSUE-101-a.md", good_match()),
            ("ISSUE-102-b.md", always_page("Restart the counter after a bump.")),
            ("ISSUE-103-c.md", good_match()),
            ("ISSUE-104-d.md", "---\nstatus: retired\nmatch: [example]\n---\n# Widget lesson\n".to_string()),
            ("ISSUE-105-e.md", "---\nmatch: [nomatchtoken]\n---\n# Widget lesson\n".to_string()),
        ],
    );
    let out = fresh("copies-list");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(listing(&out), vec![s("ISSUE-101-a.md"), s("ISSUE-103-c.md")]);
    done();
}

#[test]
fn each_copy_is_byte_equal_to_its_page() {
    // CRLF line ends and no line feed at the end of the file: the copy keeps the bytes as they were read.
    let p1 = "---\r\nmatch: [example]\r\n---\r\n# Widget lesson\r\nbody one";
    let p2 = "---\nmatch: [example]\n---\n# Widget lesson\nbody two\n";
    let d = setup(
        "copies-bytes",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", p1.to_string()), ("ISSUE-102-b.md", p2.to_string())],
    );
    let out = fresh("copies-bytes");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    for n in ["ISSUE-101-a.md", "ISSUE-102-b.md"] {
        assert_eq!(fs::read(out.join(n)).unwrap(), fs::read(d.join("lessons").join(n)).unwrap(), "{}", n);
    }
    done();
}

#[test]
fn two_matched_pages_with_one_id_give_two_lines_and_two_copies() {
    let d = setup(
        "copies-oneid",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", good_match()), ("ISSUE-101-b.md", good_match())],
    );
    let out = fresh("copies-oneid");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(listing(&out), vec![s("ISSUE-101-a.md"), s("ISSUE-101-b.md")]);
    let so = stdout(&o);
    let lines: Vec<&str> = so.lines().collect();
    assert_eq!(lines[0], "Lessons for this ticket (harness lesson brief): always=0 matched=2");
    assert_eq!(lines[3], "lesson: ISSUE-101 Widget lesson ISSUE-101-a.md");
    assert_eq!(lines[4], "lesson: ISSUE-101 Widget lesson ISSUE-101-b.md");
    done();
}

#[test]
fn a_page_whose_file_name_the_escape_rewrites_refuses() {
    let bad = format!("ISSUE-102-a{}b.md", "\u{a0}");
    let d = setup(
        "copies-name",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", good_match()), (&bad, good_match())],
    );
    let out = fresh("copies-name");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson brief refused: ISSUE-102: NAME the file name cannot be printed as it is\n");
    assert!(absent(&out));
    done();
}

// ---------------------------------------------------------------- the out folder

#[test]
fn an_empty_folder_at_the_out_path_refuses() {
    let d = setup("out-empty", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = tmp("out-empty-dir");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson brief refused: the out folder exists already\n");
    assert!(out.is_dir());
    assert_eq!(listing(&out), Vec::<String>::new());
    done();
}

#[test]
fn a_folder_with_files_at_the_out_path_refuses() {
    let d = setup("out-files", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = tmp("out-files-dir");
    fs::write(out.join("keep.txt"), "kept").unwrap();
    let o = brief(&d, &out);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson brief refused: the out folder exists already\n");
    assert_eq!(listing(&out), vec![s("keep.txt")]);
    assert_eq!(fs::read(out.join("keep.txt")).unwrap(), b"kept");
    done();
}

#[test]
fn a_file_at_the_out_path_refuses() {
    let d = setup("out-file", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("out-file-path");
    fs::write(&out, "a file").unwrap();
    let o = brief(&d, &out);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson brief refused: the out folder exists already\n");
    assert!(fs::symlink_metadata(&out).unwrap().is_file());
    assert_eq!(fs::read(&out).unwrap(), b"a file");
    done();
}

#[test]
fn a_link_to_a_folder_at_the_out_path_refuses_and_is_not_followed() {
    let d = setup("out-link", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let target = tmp("out-link-target");
    fs::write(target.join("keep.txt"), "kept").unwrap();
    let out = fresh("out-link-path");
    link(&target, &out);
    let o = brief(&d, &out);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson brief refused: the out folder exists already\n");
    assert!(fs::symlink_metadata(&out).unwrap().file_type().is_symlink());
    assert_eq!(listing(&target), vec![s("keep.txt")]);
    done();
}

#[test]
fn a_dangling_link_at_the_out_path_refuses() {
    let d = setup("out-dangling", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let gone = fresh("out-dangling-gone");
    let out = fresh("out-dangling-path");
    link(&gone, &out);
    let o = brief(&d, &out);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson brief refused: the out folder exists already\n");
    assert!(fs::symlink_metadata(&out).unwrap().file_type().is_symlink());
    assert!(absent(&gone), "the link target was made");
    done();
}

#[test]
fn a_second_run_with_the_same_arguments_is_refused() {
    let d = setup("out-second", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("out-second");
    let first = brief(&d, &out);
    assert_eq!(code(&first), 0, "{}", stderr(&first));
    let snap = listing(&out).into_iter().map(|n| (n.clone(), fs::read(out.join(&n)).unwrap())).collect::<Vec<_>>();
    let second = brief(&d, &out);
    assert_eq!(code(&second), 1);
    assert!(stdout(&second).is_empty());
    assert_eq!(stderr(&second), "harness: lesson brief refused: the out folder exists already\n");
    let again = listing(&out).into_iter().map(|n| (n.clone(), fs::read(out.join(&n)).unwrap())).collect::<Vec<_>>();
    assert_eq!(snap, again);
    done();
}

#[test]
fn a_missing_parent_of_the_out_path_exits_2() {
    let d = setup("out-parent", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let base = tmp("out-parent-base");
    let out = base.join("nosuch").join("out");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: the out folder cannot be made\n");
    assert!(absent(&base.join("nosuch")));
    done();
}

#[test]
fn a_parent_that_is_a_file_exits_2() {
    let d = setup("out-fileparent", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let base = tmp("out-fileparent-base");
    let file_p = base.join("afile");
    fs::write(&file_p, "x").unwrap();
    let out = file_p.join("out");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: the out folder cannot be made\n");
    assert!(fs::symlink_metadata(&file_p).unwrap().is_file());
    done();
}

// ---------------------------------------------------------------- lookup reuse

/// The reuse check: a good matched page whose file name sorts first, then the bad input; the brief must print
/// the lookup's own stderr and exit code, with stdout empty and no out folder made.
fn reuses_lookup(tag: &str, pages: &[(&str, String)], ticket: &[u8], scope_path: Option<PathBuf>) -> (Output, Output) {
    let d = setup(tag, ticket, b"src/\n", pages);
    let out = fresh(tag);
    let mut a = brief_args(&d, &out);
    if let Some(p) = &scope_path {
        let i = a.iter().position(|x| x == &d.join("scope.txt").to_str().unwrap().to_string()).unwrap();
        a[i] = p.to_str().unwrap().to_string();
    }
    let b = run_with(&a, &clean());
    let l = lookup(&d);
    assert!(stdout(&b).is_empty(), "{}", stdout(&b));
    assert!(absent(&out), "{}", out.display());
    assert_eq!(stderr(&b), stderr(&l), "{}", tag);
    assert_eq!(code(&b), code(&l), "{}", tag);
    (b, l)
}

#[test]
fn a_page_with_no_closing_front_matter_refuses_as_the_lookup() {
    let bad = "---\nmatch: [example]\n".to_string();
    let (b, _) = reuses_lookup("reuse-front", &[("ISSUE-101-a.md", good_match()), ("ISSUE-102-b.md", bad)], &default_ticket("t"), None);
    assert_eq!(code(&b), 1);
    assert!(stderr(&b).contains("FRONT-MATTER"), "{}", stderr(&b));
    done();
}

#[test]
fn an_unknown_status_refuses_as_the_lookup() {
    let bad = "---\nstatus: draft\nmatch: [example]\n---\n# Widget lesson\n".to_string();
    let (b, _) = reuses_lookup("reuse-status", &[("ISSUE-101-a.md", good_match()), ("ISSUE-102-b.md", bad)], &default_ticket("t"), None);
    assert_eq!(code(&b), 1);
    assert!(stderr(&b).contains("STATUS"), "{}", stderr(&b));
    done();
}

#[test]
fn a_page_with_always_and_match_refuses_as_the_lookup() {
    let bad = "---\nmatch: [example]\nalways: Restart the counter.\n---\n# Widget lesson\n".to_string();
    let (b, _) = reuses_lookup("reuse-always", &[("ISSUE-101-a.md", good_match()), ("ISSUE-102-b.md", bad)], &default_ticket("t"), None);
    assert_eq!(code(&b), 1);
    assert!(stderr(&b).contains("ALWAYS always and match"), "{}", stderr(&b));
    done();
}

#[test]
fn six_always_pages_refuse_as_the_lookup() {
    let mut pages: Vec<(String, String)> = vec![("ISSUE-101-a.md".to_string(), good_match())];
    for i in 2..=7 {
        pages.push((format!("ISSUE-10{}-b.md", i), always_page(&format!("Rule number {} of the run.", i))));
    }
    let refs: Vec<(&str, String)> = pages.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
    let (b, _) = reuses_lookup("reuse-cap", &refs, &default_ticket("t"), None);
    assert_eq!(code(&b), 1);
    assert!(
        stderr(&b).contains("harness: lesson lookup refused: ALWAYS-CAP 6 pages carry always, at most 5"),
        "{}",
        stderr(&b)
    );
    done();
}

#[test]
fn a_refused_ticket_file_refuses_as_the_lookup() {
    let (b, _) = reuses_lookup("reuse-ticket", &[("ISSUE-101-a.md", good_match())], b"\xff\xfe{}", None);
    assert_eq!(code(&b), 1);
    assert!(stderr(&b).contains("harness: ticket refused:"), "{}", stderr(&b));
    done();
}

#[test]
fn a_page_shaped_symbolic_link_refuses_as_the_lookup() {
    let d = setup("reuse-link", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let target = d.join("lessons").join("ISSUE-101-a.md");
    link(&target, &d.join("lessons").join("ISSUE-102-b.md"));
    let out = fresh("reuse-link");
    let b = brief(&d, &out);
    let l = lookup(&d);
    assert_eq!(code(&b), 2);
    assert!(stdout(&b).is_empty());
    assert!(absent(&out));
    assert_eq!(stderr(&b), stderr(&l));
    assert!(stderr(&b).ends_with("is a symbolic link\n"), "{}", stderr(&b));
    done();
}

#[test]
fn a_missing_scope_file_refuses_as_the_lookup() {
    let d = setup("reuse-scope", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let gone = fresh("reuse-scope-file");
    let out = fresh("reuse-scope-out");
    let mut a = brief_args(&d, &out);
    let i = a.iter().position(|x| x == &d.join("scope.txt").to_str().unwrap().to_string()).unwrap();
    a[i] = gone.to_str().unwrap().to_string();
    let b = run_with(&a, &clean());
    let mut l = base();
    l.args([
        "lesson",
        "lookup",
        d.join("lessons").to_str().unwrap(),
        "--ticket",
        d.join("ticket.json").to_str().unwrap(),
        "--scope-file",
        gone.to_str().unwrap(),
    ])
    .stdin(Stdio::null());
    let l = l.output().unwrap();
    assert_eq!(code(&b), 2);
    assert!(stdout(&b).is_empty());
    assert!(absent(&out));
    assert_eq!(stderr(&b), stderr(&l));
    assert!(stderr(&b).contains("the scope file cannot be read"), "{}", stderr(&b));
    done();
}

// ---------------------------------------------------------------- retired pages

#[test]
fn a_retired_page_that_would_match_gives_no_line_and_no_copy() {
    let d = setup(
        "retired-match",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", "---\nstatus: retired\nmatch: [example]\n---\n# Widget lesson\n".to_string())],
    );
    let out = fresh("retired-match");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(stdout(&o), "Lessons for this ticket (harness lesson brief): always=0 matched=0\n");
    assert!(out.is_dir());
    assert_eq!(listing(&out), Vec::<String>::new());
    done();
}

#[test]
fn a_retired_always_page_gives_no_line() {
    let d = setup(
        "retired-always",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", "---\nstatus: retired\nalways: Restart the counter after a bump.\n---\n# Widget rule\n".to_string())],
    );
    let out = fresh("retired-always");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(stdout(&o), "Lessons for this ticket (harness lesson brief): always=0 matched=0\n");
    done();
}

#[test]
fn five_always_pages_and_one_retired_always_page_pass() {
    let mut pages: Vec<(String, String)> = Vec::new();
    for i in 1..=5 {
        pages.push((format!("ISSUE-10{}-a.md", i), always_page(&format!("Rule number {} of the run.", i))));
    }
    pages.push((
        "ISSUE-106-f.md".to_string(),
        "---\nstatus: retired\nalways: A retired rule.\n---\n# Widget rule\n".to_string(),
    ));
    let refs: Vec<(&str, String)> = pages.iter().map(|(n, t)| (n.as_str(), t.clone())).collect();
    let d = setup("retired-cap", &default_ticket("t"), b"src/\n", &refs);
    let out = fresh("retired-cap");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).starts_with("Lessons for this ticket (harness lesson brief): always=5 matched=0\n"), "{}", stdout(&o));
    assert_eq!(listing(&out), Vec::<String>::new());
    done();
}

// ---------------------------------------------------------------- the privacy list

#[test]
fn no_list_configured_exits_2() {
    let d = setup("list-none", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("list-none");
    let o = run_no_list(&brief_args(&d, &out));
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(
        stderr(&o),
        "harness: error: no privacy list configured: a lesson brief is not written unscanned\n"
    );
    done();
}

#[test]
fn a_list_file_that_cannot_be_read_exits_2() {
    let d = setup("list-missing", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = fresh("list-missing");
    let mut c = base();
    c.args(brief_args(&d, &out))
        .env("HARNESS_PRIVACY_LIST", fresh("list-missing-file"))
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(
        stderr(&o),
        "harness: error: no privacy list configured: a lesson brief is not written unscanned\n"
    );
    done();
}

#[test]
fn no_list_with_a_broken_ticket_and_a_missing_lessons_folder_prints_the_list_line_only() {
    let d = tmp("list-only");
    fs::write(d.join("ticket.json"), b"\xff\xfe{}").unwrap();
    fs::write(d.join("scope.txt"), b"src/\n").unwrap();
    let out = fresh("list-only");
    let a = vec![
        s("lesson"),
        s("brief"),
        d.join("no-such-lessons").to_str().unwrap().to_string(),
        s("--ticket"),
        d.join("ticket.json").to_str().unwrap().to_string(),
        s("--scope-file"),
        d.join("scope.txt").to_str().unwrap().to_string(),
        s("--out"),
        out.to_str().unwrap().to_string(),
    ];
    let o = run_no_list(&a);
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(
        stderr(&o),
        "harness: error: no privacy list configured: a lesson brief is not written unscanned\n"
    );
    done();
}

// ---------------------------------------------------------------- the privacy scan

#[test]
fn privacy_hits_are_printed_in_the_named_order() {
    // ISSUE-101: a clean matched page. ISSUE-103: the deny word on body lines 9 and 12 (a sort by text
    // fails). ISSUE-104: an always page with the deny word in its rule.
    let w = deny();
    let body_page = [
        "---",
        "match: [example]",
        "---",
        "# Widget lesson",
        "one",
        "two",
        "three",
        "four",
        &format!("five {}", w),
        "six",
        "seven",
        &format!("eight {}", w),
    ]
    .join("\n")
        + "\n";
    let d = setup(
        "scan-order",
        &default_ticket("t"),
        b"src/\n",
        &[
            ("ISSUE-101-a.md", good_match()),
            ("ISSUE-103-c.md", body_page),
            ("ISSUE-104-d.md", always_page(&format!("Check the {} before a restart.", w))),
        ],
    );
    let out = fresh("scan-order");
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(
        stderr(&o),
        "harness: lesson brief refused: ISSUE-104: PRIVACY brief line deny-token\n\
         harness: lesson brief refused: ISSUE-103: PRIVACY page line 9 deny-token\n\
         harness: lesson brief refused: ISSUE-103: PRIVACY page line 12 deny-token\n"
    );
    done();
}

#[test]
fn the_deny_word_in_a_file_name_gives_one_brief_line() {
    let w = deny();
    let d = setup(
        "scan-name",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", good_match()), (&format!("ISSUE-102-{}.md", w), good_match())],
    );
    let out = fresh("scan-name");
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(stderr(&o), "harness: lesson brief refused: ISSUE-102: PRIVACY brief line deny-token\n");
    assert!(!stdout(&o).contains(&w) && !stderr(&o).contains(&w));
    done();
}

#[test]
fn the_deny_word_in_pages_that_are_not_scanned_passes() {
    let w = deny();
    let d = setup(
        "scan-skip",
        &default_ticket("t"),
        b"src/\n",
        &[
            // does not match
            ("ISSUE-101-a.md", format!("---\nmatch: [nomatchtoken]\n---\n# Widget {}\n", w)),
            // retired
            ("ISSUE-102-b.md", format!("---\nstatus: retired\nmatch: [example]\n---\n# Widget {}\n", w)),
            // the body of an always page with a clean rule
            ("ISSUE-103-c.md", format!("---\nalways: Check the depth.\n---\n# Widget rule {}\n", w)),
        ],
    );
    let out = fresh("scan-skip");
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "Lessons for this ticket (harness lesson brief): always=1 matched=0\n\
         Rules from earlier mistakes, kept for this change:\n\
         always: ISSUE-103 Check the depth.\n"
    );
    done();
}

#[test]
fn the_deny_word_in_the_out_folder_name_passes() {
    let w = deny();
    let d = setup("scan-out", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let base = tmp("scan-out-base");
    let out = base.join(format!("out-{}", w));
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    let want = format!(
        "Lessons for this ticket (harness lesson brief): always=0 matched=1\n\
         Pages on earlier mistakes that match this ticket. Read each file before you write code.\n\
         folder: {}\n\
         lesson: ISSUE-101 Widget lesson ISSUE-101-a.md\n",
        out.to_str().unwrap()
    );
    assert_eq!(stdout(&o), want);
    done();
}

#[test]
fn the_same_pages_pass_with_one_list_and_refuse_with_the_other() {
    let w = deny();
    let d = setup(
        "scan-list",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", format!("---\nmatch: [example]\n---\n# Widget lesson\nbody {}\n", w))],
    );
    let out_ok = fresh("scan-list-ok");
    let o = run_with(&brief_args(&d, &out_ok), &clean());
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(out_ok.is_dir());
    let out_refused = fresh("scan-list-no");
    let o = run_with(&brief_args(&d, &out_refused), &w);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out_refused));
    assert_eq!(stderr(&o), "harness: lesson brief refused: ISSUE-101: PRIVACY page line 5 deny-token\n");
    done();
}

#[test]
fn the_test_mode_marker_is_refused_also_with_the_flag() {
    let w = clean();
    let m = marker();
    let d = setup(
        "scan-marker",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", format!("---\nmatch: [example]\n---\n# Widget lesson\nbody {}\n", m))],
    );
    for with_flag in [false, true] {
        let out = fresh("scan-marker");
        let mut a = brief_args(&d, &out);
        if with_flag {
            a.insert(0, s("--testmode"));
            a.insert(1, s("synthetic-test"));
        }
        let o = run_with(&a, &w);
        assert_eq!(code(&o), 1, "flag={}", with_flag);
        assert!(stdout(&o).is_empty(), "flag={}", with_flag);
        assert!(absent(&out), "flag={}", with_flag);
        assert_eq!(
            stderr(&o),
            "harness: lesson brief refused: ISSUE-101: PRIVACY page line 5 synthetic-test\n",
            "flag={}",
            with_flag
        );
    }
    done();
}

#[test]
fn two_classes_on_one_page_line_print_in_the_scanner_order() {
    let w = deny();
    let m = marker();
    let d = setup(
        "scan-two",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", format!("---\nmatch: [example]\n---\n# Widget lesson\nbody {} and {}\n", w, m))],
    );
    let out = fresh("scan-two");
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(
        stderr(&o),
        "harness: lesson brief refused: ISSUE-101: PRIVACY page line 5 deny-token\n\
         harness: lesson brief refused: ISSUE-101: PRIVACY page line 5 synthetic-test\n"
    );
    done();
}

#[test]
fn the_deny_word_in_a_heading_gives_two_lines() {
    let w = deny();
    let d = setup(
        "scan-heading",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", format!("---\nmatch: [example]\n---\n# Widget {}\n", w))],
    );
    let out = fresh("scan-heading");
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(
        stderr(&o),
        "harness: lesson brief refused: ISSUE-101: PRIVACY brief line deny-token\n\
         harness: lesson brief refused: ISSUE-101: PRIVACY page line 4 deny-token\n"
    );
    done();
}

// ---------------------------------------------------------------- order of the stages

#[test]
fn no_list_and_no_out_prints_the_out_line() {
    let d = setup("stage-out", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let mut a = brief_args(&d, &fresh("stage-out"));
    a.pop();
    a.pop();
    let o = run_no_list(&a);
    assert_eq!(code(&o), 2);
    assert_eq!(stderr(&o), "harness: error: --out is required\n");
    done();
}

#[test]
fn no_list_and_a_thing_at_the_out_path_prints_the_list_line() {
    let d = setup("stage-list", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let out = tmp("stage-list-dir");
    let o = run_no_list(&brief_args(&d, &out));
    assert_eq!(code(&o), 2);
    assert_eq!(
        stderr(&o),
        "harness: error: no privacy list configured: a lesson brief is not written unscanned\n"
    );
    done();
}

#[test]
fn a_thing_at_the_out_path_and_a_refused_page_print_the_out_folder_line_only() {
    let d = setup(
        "stage-outfirst",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-102-b.md", "---\nmatch: [example]\n".to_string())],
    );
    let out = tmp("stage-outfirst-dir");
    let o = brief(&d, &out);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson brief refused: the out folder exists already\n");
    done();
}

#[test]
fn a_missing_parent_and_a_refused_page_print_the_lookup_lines_only() {
    let d = setup(
        "stage-parent",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-102-b.md", "---\nmatch: [example]\n".to_string())],
    );
    let base = tmp("stage-parent-base");
    let out = base.join("nosuch").join("out");
    let o = brief(&d, &out);
    let l = lookup(&d);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), stderr(&l));
    assert!(absent(&base.join("nosuch")));
    done();
}

#[test]
fn a_refused_page_and_a_privacy_hit_in_another_page_print_the_lookup_lines_only() {
    let w = deny();
    let d = setup(
        "stage-lookup",
        &default_ticket("t"),
        b"src/\n",
        &[
            ("ISSUE-101-a.md", "---\nmatch: [example]\n".to_string()),
            ("ISSUE-102-b.md", format!("---\nmatch: [example]\n---\n# Widget lesson\nbody {}\n", w)),
        ],
    );
    let out = fresh("stage-lookup");
    let o = run_with(&brief_args(&d, &out), &w);
    let l = lookup(&d);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(stderr(&o), stderr(&l));
    done();
}

#[test]
fn a_name_page_and_a_privacy_hit_in_another_page_print_the_name_line_first() {
    let w = deny();
    let bad_name = format!("ISSUE-101-a{}b.md", "\u{a0}");
    let d = setup(
        "stage-name",
        &default_ticket("t"),
        b"src/\n",
        &[
            (&bad_name, good_match()),
            ("ISSUE-102-b.md", format!("---\nmatch: [example]\n---\n# Widget lesson\nbody {}\n", w)),
        ],
    );
    let out = fresh("stage-name");
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(
        stderr(&o),
        "harness: lesson brief refused: ISSUE-101: NAME the file name cannot be printed as it is\n\
         harness: lesson brief refused: ISSUE-102: PRIVACY page line 5 deny-token\n"
    );
    done();
}

#[test]
fn a_privacy_refusal_after_one_clean_matched_page_leaves_the_out_path_absent() {
    let w = deny();
    let d = setup(
        "stage-clean",
        &default_ticket("t"),
        b"src/\n",
        &[
            ("ISSUE-101-a.md", good_match()),
            ("ISSUE-102-b.md", format!("---\nmatch: [example]\n---\n# Widget lesson\nbody {}\n", w)),
        ],
    );
    let out = fresh("stage-clean");
    let o = run_with(&brief_args(&d, &out), &w);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    done();
}

// ---------------------------------------------------------------- reads and repeat runs

type Snap = (Vec<(String, Vec<u8>)>, Vec<String>, Vec<String>);

/// The bytes of each input file and the entry names of the root, the lessons folder and the current folder.
fn snapshot(root: &Path) -> Snap {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    for n in ["ticket.json", "scope.txt"] {
        files.push((n.to_string(), fs::read(root.join(n)).unwrap()));
    }
    let mut lesson: Vec<(String, Vec<u8>)> = listing(&root.join("lessons"))
        .into_iter()
        .map(|n| (n.clone(), fs::read(root.join("lessons").join(&n)).unwrap()))
        .collect();
    lesson.sort();
    files.extend(lesson);
    let cwd = listing(&std::env::current_dir().unwrap());
    (files, listing(root), cwd)
}

#[test]
fn the_inputs_stay_byte_identical_and_nothing_is_written_beside_them() {
    let w = deny();
    let bad_name = format!("ISSUE-102-a{}b.md", "\u{a0}");
    // a passing run, then a second run with the same arguments (the out folder exists)
    let d = setup("reads", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let before = snapshot(&d);
    let out_ok = fresh("reads-ok");
    let o = brief(&d, &out_ok);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(snapshot(&d), before);
    let o = brief(&d, &out_ok);
    assert_eq!(code(&o), 1);
    assert_eq!(snapshot(&d), before);
    // a privacy refusal
    let d_no = setup(
        "reads-no",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", format!("---\nmatch: [example]\n---\n# Widget lesson\nbody {}\n", w))],
    );
    let before_no = snapshot(&d_no);
    let out_no = fresh("reads-no");
    let o = run_with(&brief_args(&d_no, &out_no), &w);
    assert_eq!(code(&o), 1);
    assert!(absent(&out_no));
    assert_eq!(snapshot(&d_no), before_no);
    // a NAME refusal
    let d2 = setup("reads-name", &default_ticket("t"), b"src/\n", &[(&bad_name, good_match())]);
    let before2 = snapshot(&d2);
    let out_name = fresh("reads-name");
    let o = brief(&d2, &out_name);
    assert_eq!(code(&o), 1);
    assert!(absent(&out_name));
    assert_eq!(snapshot(&d2), before2);
    done();
}

#[test]
fn the_inputs_stay_byte_identical_after_the_other_kinds_of_refusal() {
    // an argument refusal (no --out), then one with an out value that ends in a slash
    let d = setup("reads-arg", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let before = snapshot(&d);
    let out = fresh("reads-arg");
    let mut a = brief_args(&d, &out);
    a.pop();
    a.pop();
    let o = run_with(&a, &clean());
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert_eq!(snapshot(&d), before);
    let o = brief(&d, &with_slash(&out));
    assert_eq!(code(&o), 2);
    assert_eq!(stderr(&o), USAGE);
    assert!(absent(&out));
    assert_eq!(snapshot(&d), before);
    // a privacy list refusal
    let o = run_no_list(&brief_args(&d, &out));
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out));
    assert_eq!(snapshot(&d), before);
    // a lookup refusal (a page with no closing front matter line)
    let d_front = setup(
        "reads-front",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", good_match()), ("ISSUE-102-b.md", "---\nmatch: [example]\n".to_string())],
    );
    let before_front = snapshot(&d_front);
    let out_front = fresh("reads-front");
    let o = brief(&d_front, &out_front);
    assert_eq!(code(&o), 1);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out_front));
    assert_eq!(snapshot(&d_front), before_front);
    // a lookup read error (a page-shaped symbolic link)
    let d_link = setup("reads-link", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    link(&d_link.join("lessons").join("ISSUE-101-a.md"), &d_link.join("lessons").join("ISSUE-102-b.md"));
    let before_link = snapshot(&d_link);
    let out_link = fresh("reads-link");
    let o = brief(&d_link, &out_link);
    assert_eq!(code(&o), 2);
    assert!(stdout(&o).is_empty());
    assert!(absent(&out_link));
    assert_eq!(snapshot(&d_link), before_link);
    // the out folder cannot be made (a missing parent)
    let d_par = setup("reads-parent", &default_ticket("t"), b"src/\n", &[("ISSUE-101-a.md", good_match())]);
    let before_par = snapshot(&d_par);
    let base = tmp("reads-parent-base");
    let o = brief(&d_par, &base.join("nosuch").join("out"));
    assert_eq!(code(&o), 2);
    assert_eq!(stderr(&o), "harness: error: the out folder cannot be made\n");
    assert!(absent(&base.join("nosuch")));
    assert_eq!(snapshot(&d_par), before_par);
    done();
}

#[test]
fn two_runs_with_fresh_out_folders_differ_in_the_folder_line_only() {
    let d = setup(
        "repeat",
        &default_ticket("t"),
        b"src/\n",
        &[("ISSUE-101-a.md", good_match()), ("ISSUE-102-b.md", always_page("Restart the counter after a bump."))],
    );
    let out1 = fresh("repeat-1");
    let out2 = fresh("repeat-2");
    let o1 = brief(&d, &out1);
    let o2 = brief(&d, &out2);
    assert_eq!(code(&o1), 0, "{}", stderr(&o1));
    assert_eq!(code(&o2), 0, "{}", stderr(&o2));
    let strip = |t: String, o: &Path| -> String {
        t.lines().filter(|l| !l.starts_with("folder: ")).map(s).collect::<Vec<_>>().join("\n")
            + &format!("\n{}", o.to_str().unwrap().is_empty())
    };
    assert_eq!(strip(stdout(&o1), &out1), strip(stdout(&o2), &out2));
    assert!(stdout(&o1).contains(&format!("folder: {}\n", out1.to_str().unwrap())));
    assert!(stdout(&o2).contains(&format!("folder: {}\n", out2.to_str().unwrap())));
    assert_eq!(fs::read(out1.join("ISSUE-101-a.md")).unwrap(), fs::read(out2.join("ISSUE-101-a.md")).unwrap());
    done();
}

#[test]
fn a_page_matched_only_through_the_scope_file_changes_the_answer() {
    let page = "---\nmatch: [src/svc-a]\n---\n# Widget lesson\n".to_string();
    let d1 = setup("scope-hit", &default_ticket("t"), b"src/svc-a/x.rs\n", &[("ISSUE-101-a.md", page.clone())]);
    let out1 = fresh("scope-hit");
    let o = brief(&d1, &out1);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("lesson: ISSUE-101 Widget lesson ISSUE-101-a.md\n"), "{}", stdout(&o));
    assert!(out1.join("ISSUE-101-a.md").is_file());
    let d2 = setup("scope-miss", &default_ticket("t"), b"docs/x.rs\n", &[("ISSUE-101-a.md", page)]);
    let out2 = fresh("scope-miss");
    let o = brief(&d2, &out2);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(stdout(&o), "Lessons for this ticket (harness lesson brief): always=0 matched=0\n");
    assert_eq!(listing(&out2), Vec::<String>::new());
    done();
}

#[test]
fn a_page_matched_only_through_the_ticket_title_changes_the_answer() {
    let page = "---\nmatch: [gadget]\n---\n# Widget lesson\n".to_string();
    let d1 = setup("title-hit", &default_ticket("Fix the gadget queue"), b"src/\n", &[("ISSUE-101-a.md", page.clone())]);
    let out1 = fresh("title-hit");
    let o = brief(&d1, &out1);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert!(stdout(&o).contains("lesson: ISSUE-101 Widget lesson ISSUE-101-a.md\n"), "{}", stdout(&o));
    let d2 = setup("title-miss", &default_ticket("Fix the widget queue"), b"src/\n", &[("ISSUE-101-a.md", page)]);
    let out2 = fresh("title-miss");
    let o = brief(&d2, &out2);
    assert_eq!(code(&o), 0, "{}", stderr(&o));
    assert_eq!(stdout(&o), "Lessons for this ticket (harness lesson brief): always=0 matched=0\n");
    done();
}
