//! Process-level tests for the argv UTF-8 gate in `main.rs` (ticket 148). `std::env::args` panics on an
//! argument that is not valid UTF-8, so before the fix every verb died with exit 101 before it started.
//! The contract under test: the first non-UTF-8 argument is refused with exactly one stderr line in the
//! `harness: error: ...` style naming its 1-based position (counted from the first argument after the
//! program name), nothing on stdout, exit 2 — never a panic. Valid argv keeps its byte-identical output.
//! The bad bytes are synthetic (`0xFF 0xFE`), never real data.
#![cfg(unix)]
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::{Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_harness");

/// Synthetic invalid-UTF-8 bytes (0xFF 0xFE): not valid in any UTF-8 sequence, and not real data.
fn bad() -> OsString {
    OsString::from_vec(vec![0xFF, 0xFE])
}

fn run(args: &[OsString]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn strs(items: &[&str]) -> Vec<OsString> {
    items.iter().map(|s| OsString::from(*s)).collect()
}

/// The refusal contract: exit exactly 2 (not 101), stdout empty, stderr exactly one line, no panic.
fn assert_refused(o: &Output, label: &str) {
    let code = o.status.code().unwrap_or(-1);
    assert_ne!(code, 101, "{label}: must not panic (exit 101), got {code}");
    assert_eq!(code, 2, "{label}: exit code must be 2, got {code}");
    assert!(o.stdout.is_empty(), "{label}: stdout must be empty, got {:?}", String::from_utf8_lossy(&o.stdout));
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(!stderr.contains("panicked"), "{label}: stderr must not mention a panic, got {stderr:?}");
    assert!(stderr.ends_with('\n'), "{label}: stderr line must be newline-terminated, got {stderr:?}");
    assert_eq!(stderr.lines().count(), 1, "{label}: stderr must be exactly one line, got {stderr:?}");
}

/// AC-1: a non-UTF-8 argument is refused the same way whatever the verb is, because the gate sits before
/// `cli::run`. On origin/master every one of these panics with exit 101 before the verb starts.
#[test]
fn bad_arg_refused_on_every_verb() {
    let verbs: Vec<(&str, Vec<OsString>)> = vec![
        ("--version", strs(&["--version"])),
        ("privacy classes", strs(&["privacy", "classes"])),
        ("events list", strs(&["events", "list", "--log"])),
        ("manifest list", strs(&["manifest", "list"])),
        ("status", strs(&["status"])),
    ];
    for (label, mut prefix) in verbs {
        prefix.push(bad());
        assert_refused(&run(&prefix), label);
    }
}

/// AC-2: the position is named 1-based from the first argument after the program name
/// (`events`(1) `list`(2) `--log`(3) `<bad>`(4)), in the `harness: error:` style, and the raw
/// bytes are not echoed — not even lossily (no U+FFFD).
#[test]
fn bad_arg_position_named() {
    let o = &run(&[OsString::from("events"), OsString::from("list"), OsString::from("--log"), bad()]);
    assert_refused(o, "events list --log <bad>");
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(stderr.starts_with("harness: error: "), "stderr must use the harness error style, got {stderr:?}");
    assert!(stderr.contains("argument 4"), "stderr must name argument 4, got {stderr:?}");
    assert!(!stderr.contains('\u{FFFD}'), "stderr must not echo the bad bytes lossily, got {stderr:?}");
    assert!(!o.stderr.iter().any(|&b| b == 0xFF || b == 0xFE), "stderr must not contain the raw bytes");
}

/// AC-3: the verb itself being non-UTF-8 is refused too, naming position 1.
#[test]
fn bad_verb_refused() {
    let o = &run(&[bad()]);
    assert_refused(o, "<bad> alone");
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(stderr.contains("argument 1"), "stderr must name argument 1, got {stderr:?}");
}

/// AC-4 (guard): valid argv is untouched — `--version` still exits 0 with the library's version line.
#[test]
fn version_unchanged() {
    let o = run(&strs(&["--version"]));
    assert_eq!(o.status.code(), Some(0), "--version must still exit 0");
    assert_eq!(o.stdout, format!("{}\n", harness::version_line()).as_bytes(), "--version stdout must stay the version line");
}

/// AC-5 (guard): `privacy classes` stays byte-identical — one line per entry of `classes::ALL`.
#[test]
fn privacy_classes_unchanged() {
    let o = run(&strs(&["privacy", "classes"]));
    assert_eq!(o.status.code(), Some(0), "privacy classes must still exit 0");
    let mut expected = String::new();
    for c in harness::classes::ALL {
        expected.push_str(c);
        expected.push('\n');
    }
    assert_eq!(o.stdout, expected.as_bytes(), "privacy classes stdout must stay one line per class");
}

/// AC-6 (guard): valid usage errors are untouched — `events` with no sub-verb and the bare binary both
/// still print their existing usage line on stderr and exit 2.
#[test]
fn usage_error_unchanged() {
    let o = run(&strs(&["events"]));
    assert_eq!(o.status.code(), Some(2), "events with no sub-verb must still exit 2");
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(stderr.starts_with("usage: harness events append"), "events usage line must stay, got {stderr:?}");

    let o = run(&[]);
    assert_eq!(o.status.code(), Some(2), "no arguments must still exit 2");
    let stderr = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(
        stderr.starts_with("usage: harness --version | privacy classes |"),
        "top-level usage line must stay, got {stderr:?}"
    );
}
