//! Process-level tests for `harness plan stale-notes`. Made-up ids and text only; the real binary runs on scratch folders.
//! No test calls `std::env::var`.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const USAGE: &str = "usage: harness plan stale-notes --index <absolute-file> --tickets <absolute-dir> --done <absolute-dir> <absolute-file> [<absolute-file> ...]";
const DEV_OK: &str = r#"{"reason":"r","decided_by":"operator","date":"2026-01-01"}"#;
static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Out {
    code: i32,
    out: String,
    err: String,
}

struct Env {
    root: PathBuf,
}

impl Env {
    fn new() -> Env {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!("stale-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("tickets")).unwrap();
        fs::create_dir_all(p.join("done")).unwrap();
        let e = Env { root: p.canonicalize().unwrap() };
        e.index(&[row("R-9", "later", &[], None)]);
        e
    }

    fn index(&self, rows: &[String]) {
        fs::write(self.root.join("index.jsonl"), rows.join("\n") + "\n").unwrap();
    }

    fn done(&self, id: &str) {
        fs::write(self.root.join("done").join(format!("{id}.done")), format!("ticket={id}\nat=1\n")).unwrap();
    }

    fn note(&self, name: &str, text: &str) -> PathBuf {
        let p = self.root.join(name);
        fs::write(&p, text).unwrap();
        p
    }

    fn base(&self) -> Vec<String> {
        let s = |n: &str| self.root.join(n).to_str().unwrap().to_string();
        vec!["--index".into(), s("index.jsonl"), "--tickets".into(), s("tickets"), "--done".into(), s("done")]
    }

    fn run(&self, files: &[&Path]) -> Out {
        let mut a = self.base();
        a.extend(files.iter().map(|f| f.to_str().unwrap().to_string()));
        exec(&a)
    }

    /// One file with this text.
    fn one(&self, text: &str) -> Out {
        let f = self.note("n.txt", text);
        self.run(&[&f])
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).to_str().unwrap().to_string()
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn exec(args: &[String]) -> Out {
    let o = Command::new(BIN).arg("plan").arg("stale-notes").args(args).output().unwrap();
    Out { code: o.status.code().unwrap(), out: String::from_utf8(o.stdout).unwrap(), err: String::from_utf8(o.stderr).unwrap() }
}

fn row(id: &str, state: &str, tickets: &[&str], est: Option<u64>) -> String {
    let t: Vec<String> = tickets.iter().map(|t| format!("\"{t}\"")).collect();
    let e = est.map_or(String::new(), |v| format!(r#","left_estimate":{v}"#));
    let dev = if state == "deviation" { DEV_OK } else { "null" };
    format!(r#"{{"id":"{id}","title":"t","plan_line":3,"state":"{state}","tickets":[{}],"deviation":{dev},"trigger":null,"code":[]{e}}}"#, t.join(","))
}

fn is(o: &Out, code: i32, out: &str) {
    assert_eq!(o.err, "", "stderr");
    assert_eq!(o.out, out, "stdout");
    assert_eq!(o.code, code, "exit code");
}

fn refused(o: &Out, err: &str) {
    assert_eq!(o.out, "", "stdout");
    assert_eq!(o.err, format!("{err}\n"), "stderr");
    assert_eq!(o.code, 2, "exit code");
}

fn hit(id: &str, by: &str, line: usize, file: &str) -> String {
    format!("stale-note id={id} by={by} line={line} file={file}\n")
}

fn foot(notes: usize, rows: usize, files: usize) -> String {
    format!("stale-notes notes={notes} rows={rows} files={files}\n")
}

/// A file whose lines list nothing: exit 0 and the empty footer.
fn nothing(e: &Env, text: &str) {
    is(&e.one(text), 0, &foot(0, 0, 1));
}

#[test]
fn a_pending_line_naming_a_done_ticket_is_listed() {
    let e = Env::new();
    e.done("T-1");
    let o = e.one("until ticket T-1 is built\n");
    is(&o, 1, &(hit("T-1", "done", 1, &e.path("n.txt")) + &foot(1, 0, 1)));
}

#[test]
fn the_same_line_with_no_done_record_lists_nothing() {
    let e = Env::new();
    nothing(&e, "until ticket T-1 is built\n");
}

#[test]
fn a_pending_line_naming_a_built_row_is_listed() {
    let e = Env::new();
    e.index(&[row("T-1", "built", &[], None)]);
    let o = e.one("until ticket T-1 is built\n");
    is(&o, 1, &(hit("T-1", "built", 1, &e.path("n.txt")) + &foot(1, 0, 1)));
}

#[test]
fn an_id_that_is_done_and_built_says_both() {
    let e = Env::new();
    e.done("T-1");
    e.index(&[row("T-1", "built", &[], None)]);
    let o = e.one("until ticket T-1 is built\n");
    is(&o, 1, &(hit("T-1", "done,built", 1, &e.path("n.txt")) + &foot(1, 0, 1)));
}

#[test]
fn a_line_with_an_id_and_no_phrase_is_not_listed() {
    let e = Env::new();
    e.done("T-1");
    nothing(&e, "ticket T-1 is built\n");
}

#[test]
fn a_phrase_and_an_id_on_two_lines_are_not_listed() {
    let e = Env::new();
    e.done("T-1");
    nothing(&e, "until the day\nT-1 is here\n");
}

#[test]
fn an_id_that_is_a_prefix_of_a_longer_id_is_not_listed() {
    let e = Env::new();
    e.done("T-1");
    nothing(&e, "until T-10\nuntil T-1.5\nuntil T-1.done\nuntil xT-1\nuntil T-1-b\n");
}

#[test]
fn an_id_at_the_end_of_a_sentence_is_listed() {
    let e = Env::new();
    e.done("T-1");
    let f = e.path("n.txt");
    let o = e.one("until T-1.\nuntil (T-1)\nuntil `T-1`\n");
    is(&o, 1, &(hit("T-1", "done", 1, &f) + &hit("T-1", "done", 2, &f) + &hit("T-1", "done", 3, &f) + &foot(3, 0, 1)));
}

#[test]
fn a_phrase_inside_a_longer_word_is_not_listed() {
    let e = Env::new();
    e.done("T-1");
    nothing(&e, "untill T-1\ntodos T-1\nxuntil T-1\nnotbuilt T-1\n");
}

#[test]
fn phrase_letter_case_is_ignored() {
    let e = Env::new();
    e.done("T-1");
    let f = e.path("n.txt");
    let o = e.one("Until T-1\nTODO T-1\nWaits For T-1\n");
    is(&o, 1, &(hit("T-1", "done", 1, &f) + &hit("T-1", "done", 2, &f) + &hit("T-1", "done", 3, &f) + &foot(3, 0, 1)));
}

#[test]
fn a_phrase_with_two_blanks_or_a_tab_between_its_words_is_listed() {
    let e = Env::new();
    e.done("T-1");
    let f = e.path("n.txt");
    let o = e.one("not  yet built T-1\nwaits\tfor T-1\n");
    is(&o, 1, &(hit("T-1", "done", 1, &f) + &hit("T-1", "done", 2, &f) + &foot(2, 0, 1)));
}

#[test]
fn a_hyphenated_state_name_is_not_a_phrase() {
    let e = Env::new();
    e.done("T-1");
    nothing(&e, "T-1 not-built\n");
}

#[test]
fn a_line_with_two_ids_gives_two_lines_in_id_order() {
    let e = Env::new();
    e.done("T-1");
    e.done("T-2");
    let f = e.path("n.txt");
    let o = e.one("until T-2 and T-1\n");
    is(&o, 1, &(hit("T-1", "done", 1, &f) + &hit("T-2", "done", 1, &f) + &foot(2, 0, 1)));
}

#[test]
fn an_id_twice_on_a_line_gives_one_line() {
    let e = Env::new();
    e.done("T-1");
    let o = e.one("until T-1 and T-1\n");
    is(&o, 1, &(hit("T-1", "done", 1, &e.path("n.txt")) + &foot(1, 0, 1)));
}

#[test]
fn a_crlf_file_gives_the_same_lines() {
    let e = Env::new();
    e.done("T-1");
    let f = e.path("n.txt");
    let o = e.one("until T-1\r\nT-1 until\r\n");
    is(&o, 1, &(hit("T-1", "done", 1, &f) + &hit("T-1", "done", 2, &f) + &foot(2, 0, 1)));
}

#[test]
fn rows_in_other_states_give_no_id() {
    let e = Env::new();
    e.index(&[
        row("R-nb", "not-built", &[], Some(1)),
        row("R-ib", "in-build", &[], Some(1)),
        row("R-dev", "deviation", &[], None),
        row("R-lat", "later", &[], None),
        row("R-unk", "unknown", &[], None),
    ]);
    nothing(&e, "until R-nb R-ib R-dev R-lat R-unk\n");
}

#[test]
fn an_in_build_row_with_its_tickets_done_and_no_work_left_is_listed() {
    let e = Env::new();
    e.done("T-1");
    e.index(&[row("R-1", "in-build", &["T-1"], Some(0)), row("R-2", "in-build", &["T-1"], None)]);
    let o = e.one("nothing here\n");
    is(&o, 1, &format!("stale-row id=R-1 tickets=1 left_estimate=0\nstale-row id=R-2 tickets=1 left_estimate=none\n{}", foot(0, 2, 1)));
}

#[test]
fn an_in_build_row_with_work_left_is_not_listed() {
    let e = Env::new();
    e.done("T-1");
    e.index(&[row("R-1", "in-build", &["T-1"], Some(3))]);
    nothing(&e, "nothing here\n");
}

#[test]
fn an_in_build_row_with_a_ticket_not_done_is_not_listed() {
    let e = Env::new();
    e.done("T-1");
    e.index(&[row("R-1", "in-build", &["T-1", "T-2"], Some(0))]);
    nothing(&e, "nothing here\n");
}

#[test]
fn an_in_build_row_with_no_ticket_is_not_listed() {
    let e = Env::new();
    e.index(&[row("R-1", "in-build", &[], Some(0))]);
    nothing(&e, "nothing here\n");
}

#[test]
fn files_come_in_argument_order() {
    let e = Env::new();
    e.done("T-1");
    let a = e.note("a.txt", "until T-1\n");
    let b = e.note("b.txt", "until T-1\n");
    let o = e.run(&[&b, &a]);
    is(&o, 1, &(hit("T-1", "done", 1, &e.path("b.txt")) + &hit("T-1", "done", 1, &e.path("a.txt")) + &foot(2, 0, 2)));
}

#[test]
fn the_day_card_shape_is_listed() {
    let e = Env::new();
    e.index(&[row("A", "built", &[], None), row("B", "built", &[], None), row("C", "built", &[], None)]);
    let f = e.path("n.txt");
    let o = e.one("ticket A). Until ticket B (...) is built, the row text\nfrom x; until ticket C\n");
    is(&o, 1, &(hit("A", "built", 1, &f) + &hit("B", "built", 1, &f) + &hit("C", "built", 2, &f) + &foot(3, 0, 1)));
}

#[test]
fn usage_errors_exit_2() {
    let e = Env::new();
    let f = e.note("n.txt", "x\n");
    let g = e.note("g.txt", "x\n");
    let (i, t, d) = (e.path("index.jsonl"), e.path("tickets"), e.path("done"));
    let (f, g) = (f.to_str().unwrap().to_string(), g.to_str().unwrap().to_string());
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let cases: Vec<Vec<String>> = vec![
        s(&["--index", &i, "--tickets", &t, &f]),
        s(&["--index", &i, "--index", &i, "--tickets", &t, "--done", &d, &f]),
        s(&["--index", &i, "--tickets", &t, &f, "--done"]),
        s(&["--index", &i, "--tickets", &t, "--done", &d, "--x", "v", &f]),
        s(&["--index", &i, "--tickets", &t, "--done", &d, "--ticket", "T-1", &f]),
        s(&["--index", "index.jsonl", "--tickets", &t, "--done", &d, &f]),
        s(&["--index", &format!("{t}/../index.jsonl"), "--tickets", &t, "--done", &d, &f]),
        s(&["--index", &i, "--tickets", &t, "--done", &d, "n.txt"]),
        s(&["--index", &i, "--tickets", &t, "--done", &d, &format!("{t}/../n.txt")]),
        s(&["--index", &i, "--tickets", &t, "--done", &d]),
        s(&["--index", &i, "--tickets", &t, "--done", &d, &f, &f]),
        s(&["--index", &i, "--tickets", &t, "--done", &d, &f, &g, &f]),
        vec![],
    ];
    for c in &cases {
        refused(&exec(c), USAGE);
    }
    // flags and files in any order
    is(&exec(&s(&[&f, "--done", &d, &g, "--tickets", &t, "--index", &i])), 0, &foot(0, 0, 2));
}

#[test]
fn a_bad_file_exits_2_with_nothing_on_stdout() {
    let e = Env::new();
    e.done("T-1");
    let good = e.note("good.txt", "until T-1\n");
    let dir = e.root.join("adir");
    fs::create_dir(&dir).unwrap();
    let big = e.note("big.txt", &"a".repeat(4194305));
    let bad = e.root.join("bad.txt");
    fs::write(&bad, [0xffu8, 0xfe, b'\n']).unwrap();
    let cases: [(&Path, &str); 4] = [
        (&e.root.join("missing.txt"), "is missing or cannot be read"),
        (&dir, "is not a regular file"),
        (&big, "is over 4194304 bytes"),
        (&bad, "is not valid UTF-8"),
    ];
    for (p, what) in cases {
        refused(&e.run(&[p, &good]), &format!("harness: error: file 1: {what}"));
        refused(&e.run(&[&good, p]), &format!("harness: error: file 2: {what}"));
    }
    // exactly 4194304 bytes is read
    let text = format!("{}\nuntil T-1", "a".repeat(4194294));
    assert_eq!(text.len(), 4194304);
    let ok = e.note("edge.txt", &text);
    is(&e.run(&[&ok]), 1, &(hit("T-1", "done", 2, &e.path("edge.txt")) + &foot(1, 0, 1)));
}

#[test]
fn an_input_refusal_exits_2() {
    let e = Env::new();
    let f = e.note("n.txt", "until T-1\n");
    fs::write(e.root.join("index.jsonl"), "not json\n").unwrap();
    let o = e.run(&[&f]);
    assert_eq!(o.out, "");
    assert!(o.err.starts_with("harness: error: plan index: "), "{}", o.err);
    assert_eq!(o.code, 2);
    let e = Env::new();
    let f = e.note("n.txt", "until T-1\n");
    fs::write(e.root.join("done").join("T-1.done"), "ticket=T-2\n").unwrap();
    refused(&e.run(&[&f]), "harness: error: done record T-1: its first line is not ticket=<id>");
}

fn listing(p: &Path, out: &mut Vec<(PathBuf, u64)>) {
    for e in fs::read_dir(p).unwrap() {
        let e = e.unwrap();
        let m = e.metadata().unwrap();
        out.push((e.path(), m.len()));
        if m.is_dir() {
            listing(&e.path(), out);
        }
    }
    out.sort();
}

#[test]
fn stale_notes_writes_nothing() {
    let e = Env::new();
    e.done("T-1");
    let f = e.note("n.txt", "until T-1\n");
    let (mut before, mut after) = (Vec::new(), Vec::new());
    listing(&e.root, &mut before);
    let o = e.run(&[&f]);
    assert_eq!(o.code, 1);
    listing(&e.root, &mut after);
    assert_eq!(before, after);
}
