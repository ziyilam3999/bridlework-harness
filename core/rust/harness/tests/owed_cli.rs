//! Process-level tests for `harness owed add | list | close` (OW-1, ticket owed-store). Every check runs
//! the real binary as a child process and reads the real store file, so the exit-code contract (0 ok,
//! 1 a refused input, 2 usage, setup or a damaged store) is exercised end to end. Every row, marker and
//! deny word is synthetic, built at run time; the privacy list is injected on the child only, and no test
//! reads an environment variable.
#![cfg(unix)]
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_harness");

/// The head of the top-level usage line, pinned before this ticket's verbs are registered: the text of
/// cli.rs from `usage: harness --version` up to and including `lane claim|release|list|admission-check ...`.
const USAGE_HEAD: &str = "usage: harness --version | privacy classes | privacy scan|explain <mode> | friction add|lint|patterns|close-check | status <ticket-file> ... | process start|stop|check|step <ticket> ... | evidence show|check|quote <ticket> ... | workspace create <ticket-file> ... | workspace check <ticket> ... | controls approval-message|write ... | permission approval-message|write ... | session check-name|record-start ... | lesson lint <path> ... | lesson lookup <lessons-dir> --ticket <ticket-file> --scope-file <file> | lane claim|release|list|admission-check ...";

fn out(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// The bare binary prints the top-level usage line and exits 2; this ticket registers new verbs behind
/// that line, and the pinned head must survive them byte for byte.
#[test]
fn usage_line_head_is_unchanged() {
    let o = Command::new(BIN).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(out(&o).is_empty());
    assert!(err(&o).starts_with(USAGE_HEAD), "stderr: {}", err(&o));
}
