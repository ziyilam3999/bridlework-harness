//! `harness session cost`: real binary, synthetic fixtures only (no real ids, paths or names).
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn asst(id: &str, i: u64, c: u64, r: u64, o: u64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"id\":\"{id}\",\"usage\":{{\"input_tokens\":{i},\"cache_creation_input_tokens\":{c},\"cache_read_input_tokens\":{r},\"output_tokens\":{o}}}}}}}"
    )
}

fn fresh_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("scost-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn transcript(dir: &Path, body: &str) -> PathBuf {
    let f = dir.join("t.jsonl");
    fs::write(&f, body).unwrap();
    f
}

fn prices(dir: &Path, body: &str) -> PathBuf {
    let f = dir.join("prices.txt");
    fs::write(&f, body).unwrap();
    f
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(["session", "cost"]).args(args).stdin(Stdio::null()).output().unwrap()
}

fn s(p: &Path) -> &str {
    p.to_str().unwrap()
}
fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn refused(o: &Output) {
    assert_eq!(o.status.code(), Some(2), "{}", stderr(o));
    assert!(stdout(o).is_empty(), "{}", stdout(o));
}

/// One message with these usage numbers, so the amount depends on a single known fixture.
fn one(i: u64, c: u64, r: u64, o: u64) -> String {
    format!("{}\n", asst("m1", i, c, r, o))
}

#[test]
fn four_sums_count_a_repeated_id_once() {
    let d = fresh_dir();
    let body = [
        "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}".to_string(),
        asst("m1", 10, 100, 1000, 5),
        asst("m1", 99, 99, 99, 99),
        asst("m2", 20, 0, 3000, 7),
        "{\"type\":\"system\",\"subtype\":\"compact_boundary\"}".to_string(),
    ]
    .join("\n");
    let t = transcript(&d, &format!("{body}\n"));
    let o = run(&["--transcript", s(&t), "--model", "m-1"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "cost label=- model=m-1 messages=2 input=30 cache_write=100 cache_read=4000 output=12 max_context=3020 compactions=1 usd=unpriced\n"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn priced_run_prints_the_hand_worked_amount() {
    // Tokens: input 1_000_000, cache_write 2_000_000, cache_read 4_000_000, output 500_000.
    // Prices (example numbers): 3, 3.75, 0.3, 15 dollars per million tokens.
    // By hand: 1*3 + 2*3.75 + 4*0.3 + 0.5*15 = 3 + 7.5 + 1.2 + 7.5 = 19.2 dollars -> 19.200000.
    let d = fresh_dir();
    let t = transcript(&d, &one(1_000_000, 2_000_000, 4_000_000, 500_000));
    let p = prices(&d, "# example prices, not real\n\nm-1 3 3.75 0.3 15\nother 9 9 9 9\n");
    let o = run(&["--transcript", s(&t), "--model", "m-1", "--prices", s(&p), "--label", "run:1"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "cost label=run:1 model=m-1 messages=1 input=1000000 cache_write=2000000 cache_read=4000000 output=500000 max_context=7000000 compactions=0 usd=19.200000\n"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn rounding_is_half_up_on_the_millionth_of_a_dollar() {
    // One input token at price 0.5 dollars per million tokens is 0.5 millionths of a dollar: half rounds up -> 0.000001.
    // At price 0.499999 it is 0.499999 millionths -> rounds down -> 0.000000.
    // Three input tokens at 1.5 is 4.5 millionths -> 0.000005; at 1.499999 it is 4.499997 -> 0.000004.
    let d = fresh_dir();
    let cases = [(1, "0.5", "0.000001"), (1, "0.499999", "0.000000"), (3, "1.5", "0.000005"), (3, "1.499999", "0.000004")];
    for (tokens, price, want) in cases {
        let t = transcript(&d, &one(tokens, 0, 0, 0));
        let p = prices(&d, &format!("m-1 {price} 0 0 0\n"));
        let o = run(&["--transcript", s(&t), "--model", "m-1", "--prices", s(&p)]);
        assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
        assert!(stdout(&o).trim_end().ends_with(&format!("usd={want}")), "{price}: {}", stdout(&o));
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn unpriced_without_prices_and_for_an_unknown_model() {
    let d = fresh_dir();
    let t = transcript(&d, &one(1, 1, 1, 1));
    let p = prices(&d, "known 1 1 1 1\n");
    let o = run(&["--transcript", s(&t), "--model", "known"]);
    assert_eq!(o.status.code(), Some(0));
    assert!(stdout(&o).ends_with("usd=unpriced\n"));
    let o = run(&["--transcript", s(&t), "--model", "unknown", "--prices", s(&p)]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).ends_with("usd=unpriced\n"));
    // The model id must equal the listed one exactly.
    let o = run(&["--transcript", s(&t), "--model", "Known", "--prices", s(&p)]);
    assert!(stdout(&o).ends_with("usd=unpriced\n"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn price_file_refusals() {
    let d = fresh_dir();
    let t = transcript(&d, &one(1, 1, 1, 1));
    let bad = [
        ("a 1 2 3\n", "line 1"),
        ("# c\na 1 2 3 x\n", "line 2"),
        ("a 1 2 3 -4\n", "line 1"),
        // A leading plus sign is not a plain decimal, although number parsing alone would take it.
        ("a 1 2 3 +2\n", "line 1"),
        ("a +1 2 3 4\n", "line 1"),
        ("a 1 2 3 .5\n", "line 1"),
        // Ten digits before the point is one more than the limit of nine.
        ("a 1 2 3 1234567890\n", "line 1"),
        ("a 1 2 3 1234567890.5\n", "line 1"),
        ("a 1 2 3 1e3\n", "line 1"),
        ("a 1 2 3 0.1234567\n", "line 1"),
        ("a 1 2 3 4 5\n", "line 1"),
        ("a 1 2 3 4\n\na 5 6 7 8\n", "line 3"),
    ];
    for (body, want) in bad {
        let p = prices(&d, body);
        let o = run(&["--transcript", s(&t), "--model", "a", "--prices", s(&p)]);
        refused(&o);
        assert!(stderr(&o).contains(want), "{body:?}: {}", stderr(&o));
    }
    // A symbolic link to a good file.
    let good = prices(&d, "a 1 1 1 1\n");
    let link = d.join("link.txt");
    symlink(&good, &link).unwrap();
    refused(&run(&["--transcript", s(&t), "--model", "a", "--prices", s(&link)]));
    // A directory.
    refused(&run(&["--transcript", s(&t), "--model", "a", "--prices", s(&d)]));
    // Too large: a comment line of 70000 bytes.
    let big = prices(&d, &format!("#{}\na 1 1 1 1\n", "x".repeat(70_000)));
    let o = run(&["--transcript", s(&t), "--model", "a", "--prices", s(&big)]);
    refused(&o);
    assert!(stderr(&o).contains("larger"), "{}", stderr(&o));
    // Missing.
    refused(&run(&["--transcript", s(&t), "--model", "a", "--prices", s(&d.join("none.txt"))]));
    // A byte that is not valid UTF-8, even inside a comment line.
    let p = d.join("bytes.txt");
    fs::write(&p, b"# \xff\na 1 1 1 1\n").unwrap();
    let o = run(&["--transcript", s(&t), "--model", "a", "--prices", s(&p)]);
    refused(&o);
    assert!(stderr(&o).contains("UTF-8"), "{}", stderr(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_price_with_nine_digits_before_the_point_is_accepted() {
    // By hand: 1000000 input tokens at 999999999 dollars per million tokens is 999999999 dollars,
    // so the amount is 999999999.000000 (nine digits before the point, which the limit allows).
    let d = fresh_dir();
    let t = transcript(&d, &one(1_000_000, 0, 0, 0));
    let p = prices(&d, "a 999999999 0 0 0\n");
    let o = run(&["--transcript", s(&t), "--model", "a", "--prices", s(&p)]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert!(stdout(&o).ends_with("usd=999999999.000000\n"), "{}", stdout(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn bad_model_and_bad_label_are_refused() {
    let d = fresh_dir();
    let t = transcript(&d, &one(1, 1, 1, 1));
    let long = "a".repeat(129);
    for m in ["", "a b", "a=b", "a\nb", "é", long.as_str()] {
        refused(&run(&["--transcript", s(&t), "--model", m]));
        refused(&run(&["--transcript", s(&t), "--model", "ok", "--label", m]));
    }
    let max = "a".repeat(128);
    let o = run(&["--transcript", s(&t), "--model", &max, "--label", &max]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let o = run(&["--transcript", s(&t)]);
    refused(&o);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_model_or_label_that_starts_with_a_dash_is_refused() {
    // `--label --out <file>` must not take `--out` as the label and drop the append silently.
    let d = fresh_dir();
    let t = transcript(&d, &one(1, 1, 1, 1));
    let out = d.join("log.txt");
    refused(&run(&["--transcript", s(&t), "--model", "a", "--label", "--out", s(&out)]));
    refused(&run(&["--transcript", s(&t), "--model", "--out", s(&out)]));
    refused(&run(&["--transcript", s(&t), "--model", "-a"]));
    refused(&run(&["--transcript", s(&t), "--model", "a", "--label", "-"]));
    assert!(!out.exists());
    // A dash inside the value is fine.
    let o = run(&["--transcript", s(&t), "--model", "a-b", "--label", "x-y"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn transcript_refusals_and_sum_overflow_exit_2() {
    let d = fresh_dir();
    let bad = transcript(&d, &format!("{}\nnot json\n", asst("m1", 1, 1, 1, 1)));
    let o = run(&["--transcript", s(&bad), "--model", "a"]);
    refused(&o);
    assert!(stderr(&o).contains("line 2"), "{}", stderr(&o));
    let empty = transcript(&d, "");
    refused(&run(&["--transcript", s(&empty), "--model", "a"]));
    refused(&run(&["--transcript", s(&d.join("none.jsonl")), "--model", "a"]));
    let max = u64::MAX;
    for body in [
        format!("{}\n{}\n", asst("m1", max, 0, 0, 0), asst("m2", 1, 0, 0, 0)),
        format!("{}\n{}\n", asst("m1", 0, max, 0, 0), asst("m2", 0, 1, 0, 0)),
        format!("{}\n{}\n", asst("m1", 0, 0, max, 0), asst("m2", 0, 0, 1, 0)),
        format!("{}\n{}\n", asst("m1", 0, 0, 0, max), asst("m2", 0, 0, 0, 1)),
    ] {
        let t = transcript(&d, &body);
        refused(&run(&["--transcript", s(&t), "--model", "a"]));
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn out_creates_appends_and_keeps_the_first_line() {
    let d = fresh_dir();
    let t = transcript(&d, &one(1, 2, 3, 4));
    let out = d.join("log.txt");
    let o1 = run(&["--transcript", s(&t), "--model", "a", "--label", "one", "--out", s(&out)]);
    assert_eq!(o1.status.code(), Some(0), "{}", stderr(&o1));
    assert_eq!(fs::read_to_string(&out).unwrap(), stdout(&o1));
    let o2 = run(&["--transcript", s(&t), "--model", "a", "--label", "two", "--out", s(&out)]);
    assert_eq!(o2.status.code(), Some(0), "{}", stderr(&o2));
    assert_eq!(fs::read_to_string(&out).unwrap(), format!("{}{}", stdout(&o1), stdout(&o2)));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn out_file_without_a_final_newline_gets_the_line_glued_on() {
    // The verb appends the line and a newline; it does not add a newline before it.
    let d = fresh_dir();
    let t = transcript(&d, &one(1, 2, 3, 4));
    let out = d.join("log.txt");
    fs::write(&out, "partial").unwrap();
    let o = run(&["--transcript", s(&t), "--model", "a", "--out", s(&out)]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(fs::read_to_string(&out).unwrap(), format!("partial{}", stdout(&o)));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn out_refuses_a_symbolic_link_a_directory_and_a_relative_path() {
    let d = fresh_dir();
    let t = transcript(&d, &one(1, 2, 3, 4));
    let target = d.join("target.txt");
    fs::write(&target, "keep\n").unwrap();
    let link = d.join("link.txt");
    symlink(&target, &link).unwrap();
    refused(&run(&["--transcript", s(&t), "--model", "a", "--out", s(&link)]));
    assert_eq!(fs::read_to_string(&target).unwrap(), "keep\n");
    refused(&run(&["--transcript", s(&t), "--model", "a", "--out", s(&d)]));
    let o = Command::new(BIN)
        .current_dir(&d)
        .args(["session", "cost", "--transcript", s(&t), "--model", "a", "--out", "rel.txt"])
        .output()
        .unwrap();
    refused(&o);
    assert!(!d.join("rel.txt").exists());
    // A failed write (the folder does not exist) exits 2 and says so.
    let o = run(&["--transcript", s(&t), "--model", "a", "--out", s(&d.join("no-dir").join("x.txt"))]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("could not be written"), "{}", stderr(&o));
    // The line was printed before the append failed, and it says unpriced although the exit code is 2.
    assert!(stdout(&o).ends_with("usd=unpriced\n"), "{}", stdout(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn nothing_is_appended_after_a_refused_input() {
    let d = fresh_dir();
    let out = d.join("log.txt");
    fs::write(&out, "first\n").unwrap();
    let bad = transcript(&d, "not json\n");
    refused(&run(&["--transcript", s(&bad), "--model", "a", "--out", s(&out)]));
    let good = transcript(&d, &one(1, 1, 1, 1));
    let p = prices(&d, "a 1 2\n");
    refused(&run(&["--transcript", s(&good), "--model", "a", "--prices", s(&p), "--out", s(&out)]));
    refused(&run(&["--transcript", s(&good), "--model", "bad model", "--out", s(&out)]));
    assert_eq!(fs::read_to_string(&out).unwrap(), "first\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn session_tokens_output_is_unchanged() {
    let d = fresh_dir();
    let t = transcript(&d, &format!("{}\n{}\n{}\n", asst("m1", 10, 100, 1000, 5), asst("m1", 10, 100, 1000, 5), asst("m2", 20, 0, 3000, 7)));
    let o = Command::new(BIN).args(["session", "tokens", "--transcript", s(&t)]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(stdout(&o), "tokens messages=2 max_context=3020 output=12 compactions=0\n");
    let _ = fs::remove_dir_all(&d);
}
