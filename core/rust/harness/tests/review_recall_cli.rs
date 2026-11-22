//! Process-level tests for `harness review recall <cases-file> --results <dir>`, one per criterion.
//! Every case runs the real binary as a child process on a scratch fixture in a temp folder: a JSON
//! lines case file and one result file per case and stage. The base fixture is 4 FAIL cases (2 tune,
//! 2 test) and 2 controls with known marks, and each printed line of its passing run is asserted in
//! full. The fail-closed cases each assert exit 2, an empty stdout and the one stderr line; the
//! privacy cases plant a word in an answer key and use repeated-character heads, so nothing private
//! can leak into a printed line by accident. The read-only case snapshots the fixture folder and the
//! working folder before and after.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

const USAGE: &str = "usage: harness review recall <absolute-cases-file> --results <absolute-dir>\n";

/// The planted answer-key word: it must reach neither stdout nor stderr on any run.
const SECRET: &str = "WOMBATANSWER";

struct Tmp {
    raw: PathBuf,
}

impl Tmp {
    fn new(tag: &str) -> Tmp {
        let raw = std::env::temp_dir().join(format!("rrcall-{tag}-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&raw);
        fs::create_dir_all(&raw).unwrap();
        Tmp { raw }
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.raw);
    }
}

/// A head of one repeated character (40 of them): every case gets its own.
fn head(c: char) -> String {
    c.to_string().repeat(40)
}

fn case_line(id: &str, kind: &str, set: &str, head: &str, answer: Option<&str>) -> String {
    match answer {
        Some(a) => format!("{{\"id\":\"{id}\",\"kind\":\"{kind}\",\"set\":\"{set}\",\"repo\":\"r\",\"head\":\"{head}\",\"answer_key\":\"{a}\"}}"),
        None => format!("{{\"id\":\"{id}\",\"kind\":\"{kind}\",\"set\":\"{set}\",\"repo\":\"r\",\"head\":\"{head}\"}}"),
    }
}

/// The base case file: tune-a and tune-b are FAIL cases of the tune set, test-c and test-d FAIL
/// cases of the test set, test-x and test-y its controls. tune-a holds the planted word.
fn base_lines() -> Vec<String> {
    vec![
        case_line("tune-a", "fail", "tune", &head('a'), Some(&format!("The retry path takes {SECRET} the lock twice."))),
        case_line("tune-b", "fail", "tune", &head('b'), Some("The inbox writer lock is dropped on the error path.")),
        case_line("test-c", "fail", "test", &head('c'), Some("The session id is echoed into the log line.")),
        case_line("test-d", "fail", "test", &head('d'), Some("The plan row id is read past its end.")),
        case_line("test-x", "control", "test", &head('e'), None),
        case_line("test-y", "control", "test", &head('f'), None),
    ]
}

fn result(dir: &Path, id: &str, stage: &str, verdict: &str, matched: Option<&str>) {
    let body = match matched {
        Some(m) => format!("{{\"verdict\":\"{verdict}\",\"matched\":\"{m}\"}}\n"),
        None => format!("{{\"verdict\":\"{verdict}\"}}\n"),
    };
    fs::write(dir.join(format!("{id}.{stage}")), body).unwrap();
}

/// The base fixture with its known marks:
/// - tune-a: fail and yes at every stage (a hit everywhere).
/// - tune-b: fail and yes at report, a pass at record, fail and yes at fixround (a hit at report
///   only: the fixround mark without the record stage counts for nothing).
/// - test-c: matched no at report, fail and yes later (a miss everywhere: report is the first stage).
/// - test-d: fail and yes at report and record, matched no at fixround (a hit until fixround).
/// - test-x: a clean control; test-y: a control the reviewer blocked at report.
fn fix() -> Fix {
    let t = Tmp::new("base");
    let dir = t.raw.join("results");
    fs::create_dir_all(&dir).unwrap();
    let cases = t.raw.join("cases.jsonl");
    fs::write(&cases, base_lines().join("\n") + "\n").unwrap();
    let marks = [
        ("tune-a", "report", "fail", Some("yes")),
        ("tune-a", "record", "fail", Some("yes")),
        ("tune-a", "fixround", "fail", Some("yes")),
        ("tune-b", "report", "fail", Some("yes")),
        ("tune-b", "record", "pass", Some("yes")),
        ("tune-b", "fixround", "fail", Some("yes")),
        ("test-c", "report", "fail", Some("no")),
        ("test-c", "record", "fail", Some("yes")),
        ("test-c", "fixround", "fail", Some("yes")),
        ("test-d", "report", "fail", Some("yes")),
        ("test-d", "record", "fail", Some("yes")),
        ("test-d", "fixround", "fail", Some("no")),
        ("test-x", "report", "pass", None),
        ("test-x", "record", "pass", None),
        ("test-x", "fixround", "pass", None),
        ("test-y", "report", "fail", None),
        ("test-y", "record", "pass", None),
        ("test-y", "fixround", "pass", None),
    ];
    for (id, stage, verdict, matched) in marks {
        result(&dir, id, stage, verdict, matched);
    }
    Fix { cases, dir, _t: t }
}

struct Fix {
    cases: PathBuf,
    dir: PathBuf,
    _t: Tmp,
}

impl Fix {
    fn write_cases(&self, lines: &[String]) {
        fs::write(&self.cases, lines.join("\n") + "\n").unwrap();
    }

    fn cases_str(&self) -> &str {
        self.cases.to_str().unwrap()
    }

    fn dir_str(&self) -> &str {
        self.dir.to_str().unwrap()
    }
}

fn recall(cases: &str, results: &str, cwd: Option<&Path>) -> Output {
    let mut c = Command::new(BIN);
    c.args(["review", "recall", cases, "--results", results]).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap()
}

fn run(f: &Fix) -> Output {
    recall(f.cases_str(), f.dir_str(), None)
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

/// Every file under a root with its bytes, so a no-write check sees both a new file and a changed one.
fn snapshot(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    let mut seen = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                seen.push((p.strip_prefix(root).unwrap().to_path_buf(), fs::read(&p).unwrap()));
            }
        }
    }
    seen
}

/// The passing run of the base fixture: each printed line, in full.
#[test]
fn known_marks_print_each_line() {
    let f = fix();
    let o = run(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(
        out(&o),
        "set=tune stage=report fail_cases=2 hits=2 recall=2/2\n\
         set=tune stage=record fail_cases=2 hits=1 recall=1/2\n\
         set=tune stage=fixround fail_cases=2 hits=1 recall=1/2\n\
         set=test stage=report fail_cases=2 hits=1 recall=1/2\n\
         set=test stage=record fail_cases=2 hits=1 recall=1/2\n\
         set=test stage=fixround fail_cases=2 hits=0 recall=0/2\n\
         set=tune controls=0 false_blocks=0\n\
         set=test controls=2 false_blocks=1\n\
         cases=6 tune=2 test=4\n"
    );
    assert_eq!(err(&o), "");
}

#[test]
fn unknown_subcommand_exit_two() {
    let o = Command::new(BIN).args(["review", "replay"]).stdin(Stdio::null()).output().unwrap();
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), USAGE);
}

#[test]
fn not_valid_json_line_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[1] = "{\"id\": broken".to_string();
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case line 2: the line is not a JSON object of string fields\n");
}

#[test]
fn unknown_kind_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[2] = case_line("test-c", "odd", "test", &head('c'), Some("answer"));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-c: kind is missing or unknown\n");
}

#[test]
fn unknown_set_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[2] = case_line("test-c", "fail", "hold", &head('c'), Some("answer"));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-c: set is missing or unknown\n");
}

/// A bad head refuses the run, and neither the head nor any other field value reaches stderr.
#[test]
fn bad_head_exit_two_and_the_head_stays_private() {
    let f = fix();
    let bad_head = "ZZNOTAHEAD";
    let mut lines = base_lines();
    lines[3] = case_line("test-d", "fail", "test", bad_head, Some(&format!("an answer with {SECRET} in it")));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-d: head is missing or not 40 lowercase hex characters\n");
    assert!(!err(&o).contains(bad_head));
    assert!(!err(&o).contains(SECRET));
}

/// A head that breaks one rule only: 39 lowercase hex characters (the length rule).
#[test]
fn head_of_39_hex_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[3] = case_line("test-d", "fail", "test", &"d".repeat(39), Some("answer"));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-d: head is missing or not 40 lowercase hex characters\n");
}

/// A head that breaks one rule only: 40 hex characters in upper case (the character rule).
#[test]
fn head_of_upper_case_hex_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[3] = case_line("test-d", "fail", "test", &"ABCDEF0123".repeat(4), Some("answer"));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-d: head is missing or not 40 lowercase hex characters\n");
}

/// An id of 33 characters, all of the allowed kind (the length rule).
#[test]
fn id_of_33_characters_exit_two_named_by_line() {
    let f = fix();
    let mut lines = base_lines();
    lines[2] = case_line(&"a".repeat(33), "fail", "test", &head('c'), Some("answer"));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case line 3: the id is missing or not 1 to 32 characters of a-z 0-9 and -\n");
}

#[test]
fn bad_id_exit_two_named_by_line() {
    let f = fix();
    let mut lines = base_lines();
    lines[2] = case_line("Bad_Id", "fail", "test", &head('c'), Some("answer"));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case line 3: the id is missing or not 1 to 32 characters of a-z 0-9 and -\n");
}

#[test]
fn control_with_answer_key_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[4] = case_line("test-x", "control", "test", &head('e'), Some("an answer"));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-x: a control case must not carry an answer_key\n");
}

#[test]
fn control_result_with_matched_exit_two() {
    let f = fix();
    result(&f.dir, "test-x", "record", "pass", Some("yes"));
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-x: the record result of a control carries a matched field\n");
}

#[test]
fn duplicate_id_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[5] = case_line("test-x", "control", "test", &head('f'), None);
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-x: duplicate id\n");
}

#[test]
fn empty_answer_key_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[1] = case_line("tune-b", "fail", "tune", &head('b'), Some(""));
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case tune-b: answer_key is missing or empty\n");
}

/// A missing result file is exit 2, never a silently skipped case (skipping it would make this red).
#[test]
fn missing_result_file_exit_two() {
    let f = fix();
    fs::remove_file(f.dir.join("tune-b.fixround")).unwrap();
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case tune-b: the fixround result file is missing or unreadable\n");
}

#[test]
fn bad_verdict_exit_two() {
    let f = fix();
    result(&f.dir, "test-d", "record", "maybe", Some("yes"));
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case test-d: the record result verdict is not pass or fail\n");
}

#[test]
fn fail_result_without_matched_exit_two() {
    let f = fix();
    result(&f.dir, "tune-a", "record", "fail", None);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case tune-a: the record result has no matched field\n");
}

#[test]
fn empty_repo_exit_two() {
    let f = fix();
    let mut lines = base_lines();
    lines[1] = lines[1].replace("\"repo\":\"r\"", "\"repo\":\"\"");
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case tune-b: repo is missing or empty\n");
}

#[test]
fn matched_not_yes_or_no_exit_two() {
    let f = fix();
    result(&f.dir, "tune-a", "record", "fail", Some("maybe"));
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: case tune-a: the record result matched field is not yes or no\n");
}

#[test]
fn results_dir_that_does_not_exist_exit_two() {
    let f = fix();
    let missing = f._t.raw.join("no-such-dir");
    let o = recall(f.cases_str(), missing.to_str().unwrap(), None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the results dir is missing or not a folder\n");
}

#[test]
fn results_dir_that_is_a_file_exit_two() {
    let f = fix();
    // The cases file is a regular file: pass it where the folder belongs.
    let o = recall(f.cases_str(), f.cases_str(), None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the results dir is missing or not a folder\n");
}

#[test]
fn result_file_naming_no_case_exit_two() {
    let f = fix();
    fs::write(f.dir.join("zzz.report"), "{}\n").unwrap();
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the results dir holds a result file that names no case\n");
}

#[test]
fn tune_set_without_fail_case_exit_two() {
    let f = fix();
    let lines: Vec<String> = base_lines().into_iter().skip(2).collect();
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the tune set has no fail case\n");
}

/// A cases file whose test set has no FAIL case cannot be graded (letting it pass would make this red).
#[test]
fn test_set_without_fail_case_exit_two() {
    let f = fix();
    let lines: Vec<String> = base_lines().into_iter().filter(|l| !l.contains("\"test-c\"") && !l.contains("\"test-d\"")).collect();
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the test set has no fail case\n");
}

#[test]
fn test_set_without_control_exit_two() {
    let f = fix();
    let lines: Vec<String> = base_lines().into_iter().filter(|l| !l.contains("\"test-x\"") && !l.contains("\"test-y\"")).collect();
    f.write_cases(&lines);
    let o = run(&f);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: the test set has no control\n");
}

#[test]
fn relative_cases_path_exit_two() {
    let f = fix();
    let o = recall("cases.jsonl", f.dir_str(), Some(&f._t.raw));
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), USAGE);
}

#[test]
fn relative_results_dir_exit_two() {
    let f = fix();
    let o = recall(f.cases_str(), "results", Some(&f._t.raw));
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), USAGE);
}

#[test]
fn dot_dot_in_cases_path_exit_two() {
    let f = fix();
    let p = format!("{}/../cases.jsonl", f._t.raw.display());
    let o = recall(&p, f.dir_str(), None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), USAGE);
}

#[test]
fn dot_dot_in_results_dir_exit_two() {
    let f = fix();
    let p = format!("{}/../results", f._t.raw.display());
    let o = recall(f.cases_str(), &p, None);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), USAGE);
}

/// The verb writes nothing: the fixture folder and the working folder hold the same files, byte for
/// byte, before and after a passing run (writing a file into either would make this red).
#[test]
fn writes_no_file_in_the_fixture_or_the_working_folder() {
    let f = fix();
    let cwd = Tmp::new("cwd");
    let fixture_before = snapshot(&f._t.raw);
    let cwd_before = snapshot(&cwd.raw);
    let o = recall(f.cases_str(), f.dir_str(), Some(&cwd.raw));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(snapshot(&f._t.raw), fixture_before);
    assert_eq!(snapshot(&cwd.raw), cwd_before);
}

/// Asserts exit 2, an empty stdout and a stderr that holds the refusal's own text.
fn refused_with(o: &Output, text: &str) {
    assert_eq!(code(o), 2);
    assert_eq!(out(o), "");
    assert!(err(o).contains(text), "stderr: {}", err(o));
}

#[test]
fn cases_path_that_does_not_exist_exit_two() {
    let f = fix();
    let missing = f._t.raw.join("no-such-cases.jsonl");
    let o = recall(missing.to_str().unwrap(), f.dir_str(), None);
    refused_with(&o, "the cases file is not a readable regular file");
}

#[test]
fn cases_path_that_is_a_folder_exit_two() {
    let f = fix();
    // The results folder is a folder: pass it where the cases file belongs.
    let o = recall(f.dir_str(), f.dir_str(), None);
    refused_with(&o, "the cases file is not a readable regular file");
}

#[test]
fn cases_file_not_utf8_exit_two() {
    let f = fix();
    let mut bytes = base_lines().join("\n").into_bytes();
    bytes.extend_from_slice(&[b'\n', 0xff, b'\n']);
    fs::write(&f.cases, bytes).unwrap();
    let o = run(&f);
    refused_with(&o, "the cases file is not UTF-8 text");
}

#[test]
fn result_file_not_utf8_exit_two() {
    let f = fix();
    fs::write(f.dir.join("tune-a.record"), [0xffu8, b'\n']).unwrap();
    let o = run(&f);
    refused_with(&o, "case tune-a: the record result file is not UTF-8 text");
}

#[test]
fn result_file_not_a_json_object_exit_two() {
    let f = fix();
    fs::write(f.dir.join("tune-a.record"), "not json\n").unwrap();
    let o = run(&f);
    refused_with(&o, "case tune-a: the record result is not a JSON object of string fields");
}

/// A passing run prints ids, counts and the fixed words only: no answer-key text, no head (printing
/// the answer key would make this red).
#[test]
fn answer_key_and_heads_stay_out_of_a_passing_run() {
    let f = fix();
    let o = run(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    for text in [out(&o), err(&o)] {
        assert!(!text.contains(SECRET));
        for c in ['a', 'b', 'c', 'd', 'e', 'f'] {
            assert!(!text.contains(&head(c)), "a head reached the output");
        }
    }
}

/// tune-b holds a fail-and-yes fixround mark behind a record pass: it counts as a hit at fixround
/// only through every earlier stage (dropping the earlier-stage rule would make this red).
#[test]
fn a_fixround_hit_needs_every_earlier_stage() {
    let f = fix();
    let o = run(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let text = out(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.contains(&"set=tune stage=report fail_cases=2 hits=2 recall=2/2"));
    assert!(lines.contains(&"set=tune stage=record fail_cases=2 hits=1 recall=1/2"));
    assert!(lines.contains(&"set=tune stage=fixround fail_cases=2 hits=1 recall=1/2"));
}

/// A control the reviewer blocks is a false block, never a hit of the set's FAIL cases (counting it
/// as a hit in place of a false block would make this red).
#[test]
fn a_blocked_control_counts_as_a_false_block_not_a_hit() {
    let f = fix();
    result(&f.dir, "test-x", "report", "fail", None);
    let o = run(&f);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let text = out(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert!(lines.contains(&"set=test stage=report fail_cases=2 hits=1 recall=1/2"));
    assert!(lines.contains(&"set=test controls=2 false_blocks=2"));
}
