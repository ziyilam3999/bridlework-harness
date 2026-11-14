//! `harness session tokens`: real binary, synthetic fixtures only (no real ids, paths or names).
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn asst(id: &str, i: u64, c: u64, r: u64, o: u64) -> String {
    format!(
        "{{\"type\":\"assistant\",\"message\":{{\"id\":\"{id}\",\"usage\":{{\"input_tokens\":{i},\"cache_creation_input_tokens\":{c},\"cache_read_input_tokens\":{r},\"output_tokens\":{o}}}}}}}"
    )
}

/// Write `body` to a private file and run the verb on it.
fn run_on(body: &str) -> Output {
    let dir = std::env::temp_dir().join(format!("stok-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&dir).unwrap();
    let f: PathBuf = dir.join("t.jsonl");
    fs::write(&f, body).unwrap();
    let out = Command::new(BIN).args(["session", "tokens", "--transcript", f.to_str().unwrap()]).stdin(Stdio::null()).output().unwrap();
    let _ = fs::remove_dir_all(&dir);
    out
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

#[test]
fn good_fixture_prints_exact_numbers_and_counts_a_repeated_id_once() {
    let body = [
        "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}".to_string(),
        asst("m1", 10, 100, 1000, 5),
        asst("m1", 10, 100, 1000, 5),
        asst("m2", 20, 0, 3000, 7),
        "{\"type\":\"system\",\"subtype\":\"compact_boundary\"}".to_string(),
        "{\"type\":\"system\",\"subtype\":\"other\"}".to_string(),
        "{\"type\":\"system\",\"subtype\":\"compact_boundary\"}".to_string(),
    ]
    .join("\n");
    let o = run_on(&format!("{body}\n"));
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "tokens messages=2 max_context=3020 output=12 compactions=2\n");
}

#[test]
fn planted_bad_line_is_refused_and_named() {
    let body = format!("{}\nnot json at all\n{}\n", asst("m1", 1, 1, 1, 1), asst("m2", 1, 1, 1, 1));
    let o = run_on(&body);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("line 2"), "{}", stderr(&o));
}

#[test]
fn empty_and_missing_files_are_refused() {
    let o = run_on("");
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    let o = Command::new(BIN).args(["session", "tokens", "--transcript", "/nonexistent-dir-xyz/none.jsonl"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
}

#[test]
fn missing_or_non_integer_usage_is_refused() {
    let no_usage = "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\"}}\n";
    assert_eq!(run_on(no_usage).status.code(), Some(2));
    let missing_field = "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":1,\"cache_creation_input_tokens\":1,\"cache_read_input_tokens\":1}}}\n";
    assert_eq!(run_on(missing_field).status.code(), Some(2));
    let non_int = asst("m1", 1, 1, 1, 1).replace("\"output_tokens\":1", "\"output_tokens\":1.5");
    assert_eq!(run_on(&format!("{non_int}\n")).status.code(), Some(2));
    let text = asst("m1", 1, 1, 1, 1).replace("\"input_tokens\":1", "\"input_tokens\":\"1\"");
    assert_eq!(run_on(&format!("{text}\n")).status.code(), Some(2));
}

#[test]
fn session_without_a_verb_prints_the_tokens_usage() {
    let o = Command::new(BIN).args(["session"]).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("session tokens --transcript <file>"));
}
