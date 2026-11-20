//! `harness session rotation-check`: real binary, synthetic ids, paths and words written at run time. One shared helper
//! (`check`) makes the assertions that hold for every test: whole stdout, whole stderr, exact exit code, and an equal
//! snapshot of the scratch folder before and after the run.
use std::fs;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

const SID: &str = "11111111-2222-4333-8444-555555555555";
const OTHER: &str = "aaaaaaaa-2222-4333-8444-555555555555";
const UP_SID: &str = "abcdef01-2222-4333-8444-555555555555";
const UP_NAME: &str = "ABCDEF01-2222-4333-8444-555555555555";
/// 2026-11-21 00:00:00.000 at +08:00
const M: u64 = 1_795_190_400_000;
const USAGE: &str = "usage: harness session rotation-check --log <abs file> --session-id <uuid> --transcript <abs file> --utc-offset <+HH:MM|-HH:MM> [--now-ms <n>]";
const MAXMS: &str = "253401609599000";

const NOT_COUNTED: &str = "{\"type\":\"user\"}\n";
const PLAIN6: &str = concat!(
    "{\"type\":\"user\",\"message\":{\"content\":\"hello\"}}\n",
    "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":1,\"cache_creation_input_tokens\":2,\"cache_read_input_tokens\":3,\"output_tokens\":4}}}\n",
    "{\"type\":\"system\",\"subtype\":\"init\"}\n",
    "{\"type\":\"user\",\"subtype\":\"compact_boundary\"}\n",
    "{\"type\":\"user\",\"message\":{\"content\":\"the words compact_boundary and system are text\"}}\n",
    "{\"type\":\"assistant\",\"message\":{\"id\":\"m2\",\"usage\":{\"input_tokens\":5,\"cache_creation_input_tokens\":6,\"cache_read_input_tokens\":7,\"output_tokens\":8}}}\n",
);
const MARK_A: &str = "{\"type\":\"system\",\"subtype\":\"compact_boundary\",\"uuid\":\"u1\"}\n";
const MARK_B: &str = "{\"subtype\":\"compact_boundary\",\"extra\":1,\"type\":\"system\"}\n";

struct S(PathBuf);
impl S {
    fn new() -> S {
        let d = std::env::temp_dir().join(format!("rotchk-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&d).unwrap();
        S(d)
    }
    fn p(&self, name: &str) -> String {
        self.0.join(name).to_str().unwrap().to_string()
    }
    fn log(&self) -> String {
        self.p("e.jsonl")
    }
    fn tr(&self) -> String {
        self.p(&format!("{SID}.jsonl"))
    }
    fn put(&self, name: &str, body: &str) {
        fs::write(self.p(name), body).unwrap();
    }
}
impl Drop for S {
    fn drop(&mut self) {
        let _ = fs::set_permissions(self.0.join("t000"), fs::Permissions::from_mode(0o644));
        for n in ["e.jsonl", "unreadable"] {
            let _ = fs::set_permissions(self.0.join(n), fs::Permissions::from_mode(0o644));
        }
        let _ = fs::set_permissions(self.0.join(format!("{SID}.jsonl")), fs::Permissions::from_mode(0o644));
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn hook(ts: u64, sid: &str) -> String {
    format!("{{\"v\":2,\"kind\":\"session_started\",\"ts_ms\":{ts},\"session_id\":\"{sid}\",\"source\":\"hook\"}}\n")
}
fn launch(ts: u64, sid: &str) -> String {
    format!("{{\"v\":2,\"kind\":\"session_started\",\"ts_ms\":{ts},\"session_id\":\"{sid}\",\"source\":\"launch\"}}\n")
}
fn unmanaged(ts: u64, sid: &str) -> String {
    format!("{{\"v\":2,\"kind\":\"unmanaged_session\",\"ts_ms\":{ts},\"session_id\":\"{sid}\",\"ticket\":\"t-one\",\"reason\":\"not_supervisor_launched\"}}\n")
}

/// Everything a run could change in the scratch folder, one text per entry (links are not followed).
fn snap(dir: &PathBuf, out: &mut Vec<String>) {
    let mut names: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(r) => r.map(|e| e.unwrap().path()).collect(),
        Err(_) => return,
    };
    names.sort();
    for p in names {
        let md = fs::symlink_metadata(&p).unwrap();
        let mut t = format!("{} mode={:o} len={} mtime={:?}", p.display(), md.mode(), md.len(), md.modified().unwrap());
        if md.file_type().is_symlink() {
            t.push_str(&format!(" link={:?}", fs::read_link(&p).unwrap()));
        } else if md.file_type().is_file() {
            match fs::read(&p) {
                Ok(b) => t.push_str(&format!(" bytes={b:?}")),
                Err(e) => t.push_str(&format!(" unreadable={:?}", e.kind())),
            }
        }
        out.push(t);
        if md.file_type().is_dir() {
            snap(&p, out);
        }
    }
}

/// The one helper: runs the binary and makes every assertion common to all tests.
fn check(s: &S, args: &[String], out: &str, err: &str, code: i32) {
    let (mut before, mut after) = (Vec::new(), Vec::new());
    snap(&s.0, &mut before);
    let o = Command::new(BIN).args(["session", "rotation-check"]).args(args).stdin(Stdio::null()).output().unwrap();
    snap(&s.0, &mut after);
    assert_eq!(String::from_utf8_lossy(&o.stdout), out, "stdout");
    assert_eq!(String::from_utf8_lossy(&o.stderr), err, "stderr");
    assert_eq!(o.status.code(), Some(code), "exit code");
    assert_eq!(before, after, "the scratch folder changed");
}

fn a(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|x| x.to_string()).collect()
}
/// The full call with the given moment and offset.
fn full(s: &S, now: &str, off: &str) -> Vec<String> {
    a(&["--log", &s.log(), "--session-id", SID, "--transcript", &s.tr(), "--utc-offset", off, "--now-ms", now])
}
fn uerr(msg: &str) -> String {
    format!("harness: error: {msg}\n{USAGE}\n")
}
fn rf(what: &str, why: &str) -> String {
    format!("harness: rotation-check refused: {what}: {why}\n")
}
fn start_rf(why: &str) -> String {
    rf("start record", why)
}
fn tr_rf(why: &str) -> String {
    rf("transcript", why)
}

/// A scratch with a log (given text) and a one-line transcript.
fn with_log(log: &str) -> S {
    let s = S::new();
    s.put("e.jsonl", log);
    s.put(&format!("{SID}.jsonl"), NOT_COUNTED);
    s
}
fn with_tr(t: &str) -> S {
    let s = with_log(&hook(M, SID));
    s.put(&format!("{SID}.jsonl"), t);
    s
}
/// One decision: a log with one hook row at `start`, the transcript text, the whole expected line.
fn decide(start: u64, now: u64, off: &str, t: &str, line: &str, code: i32) {
    let s = with_log(&hook(start, SID));
    s.put(&format!("{SID}.jsonl"), t);
    check(&s, &full(&s, &now.to_string(), off), &format!("{line}\n"), "", code);
}

// ---- tests of the helper ----

#[test]
fn helper_fails_on_one_extra_stdout_line() {
    let s = with_log(&hook(M - 1, SID));
    let args = full(&s, &M.to_string(), "+08:00");
    let line = "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21\nextra\n";
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&s, &args, line, "", 10)));
    assert!(r.is_err());
}
#[test]
fn helper_fails_on_one_extra_stderr_line() {
    let s = with_log(&hook(M - 1, SID));
    let args = full(&s, &M.to_string(), "+08:00");
    let line = "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21\n";
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&s, &args, line, "extra\n", 10)));
    assert!(r.is_err());
}
#[test]
fn helper_fails_on_wrong_exit_code() {
    let s = with_log(&hook(M - 1, SID));
    let args = full(&s, &M.to_string(), "+08:00");
    let line = "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21\n";
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| check(&s, &args, line, "", 0)));
    assert!(r.is_err());
}

// ---- accept check ----

#[test]
fn accept_check_record_start_then_earliest_row_is_the_start() {
    let s = S::new();
    for ts in ["1795140000000", "1795200000000"] {
        let o = Command::new(BIN).args(["session", "record-start", "--session-id", SID, "--log", &s.log(), "--ts-ms", ts]).output().unwrap();
        assert_eq!(o.status.code(), Some(0));
        assert!(o.stdout.is_empty());
    }
    s.put(&format!("{SID}.jsonl"), "{\"type\":\"user\"}\n{\"type\":\"system\",\"subtype\":\"compact_boundary\"}\n");
    check(&s, &full(&s, "1795150000000", "+08:00"), "rotation due=yes reason=compaction compactions=1 start_day=2026-11-20 now_day=2026-11-20\n", "", 10);
}

#[test]
fn usage_line_of_the_session_verb_group_names_the_verb() {
    let o = Command::new(BIN).arg("session").output().unwrap();
    assert_eq!(o.status.code(), Some(2));
    let e = String::from_utf8_lossy(&o.stderr).to_string();
    assert!(e.contains(&USAGE["usage: harness ".len()..]));
}

// ---- start record: rows ----

#[test]
fn earliest_of_three_rows_among_other_ids_is_the_start() {
    let log = format!("{}{}{}{}{}", hook(M + 5, SID), hook(M - 2, OTHER), hook(M - 1, SID), hook(M - 3, OTHER), hook(M + 9, SID));
    let s = with_log(&log);
    check(&s, &full(&s, &M.to_string(), "+08:00"), "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21\n", "", 10);
}
#[test]
fn earliest_row_is_used_with_the_later_row_written_first() {
    let s = with_log(&format!("{}{}", hook(M + 100, SID), hook(M - 1, SID)));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21\n", "", 10);
}
#[test]
fn earliest_row_is_used_with_the_earlier_row_written_first() {
    let s = with_log(&format!("{}{}", hook(M - 1, SID), hook(M + 100, SID)));
    check(&s, &full(&s, &(M + 200).to_string(), "+08:00"), "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21\n", "", 10);
}
#[test]
fn launch_row_is_ignored_for_the_start() {
    let s = with_log(&format!("{}{}", launch(M - 1, SID), hook(M, SID)));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21\n", "", 0);
}
#[test]
fn other_session_rows_are_ignored_for_the_start() {
    let s = with_log(&format!("{}{}", hook(M - 1, OTHER), hook(M, SID)));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21\n", "", 0);
}
#[test]
fn unmanaged_session_row_of_the_id_is_not_the_start() {
    let s = with_log(&format!("{}{}{}", unmanaged(M - 90_000_000, SID), hook(M - 2000, SID), hook(M - 1000, SID)));
    check(&s, &full(&s, &(M - 1).to_string(), "+08:00"), "rotation due=no reason=none compactions=0 start_day=2026-11-20 now_day=2026-11-20\n", "", 0);
}
#[test]
fn log_of_three_id_rows_and_two_other_rows_gives_the_earliest() {
    let log = format!("{}{}{}{}{}", hook(M + 3, SID), hook(M - 7, OTHER), hook(M, SID), hook(M - 9, OTHER), hook(M + 1, SID));
    let s = with_log(&log);
    check(&s, &full(&s, &(M + 86_399_999).to_string(), "+08:00"), "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21\n", "", 0);
}

// ---- start record: refusals ----

#[test]
fn log_unreadable_is_refused() {
    let s = with_log(&hook(M, SID));
    fs::set_permissions(s.log(), fs::Permissions::from_mode(0o000)).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("cannot read log: permission denied"), 3);
}
#[test]
fn log_a_folder_is_refused() {
    let s = with_log("");
    fs::remove_file(s.log()).unwrap();
    fs::create_dir(s.log()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("log refused: not a regular file"), 3);
}
#[test]
fn log_a_live_symlink_is_refused() {
    let s = with_log("");
    fs::remove_file(s.log()).unwrap();
    s.put("real.jsonl", &hook(M, SID));
    symlink(s.p("real.jsonl"), s.log()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("log refused: the path is a symbolic link"), 3);
}
#[test]
fn log_a_dangling_symlink_is_refused() {
    let s = with_log("");
    fs::remove_file(s.log()).unwrap();
    symlink(s.p("nowhere.jsonl"), s.log()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("log refused: the path is a symbolic link"), 3);
}
#[test]
fn log_a_dangling_symlink_with_trailing_slash_creates_nothing() {
    let s = with_log("");
    fs::remove_file(s.log()).unwrap();
    symlink(s.p("nowhere.jsonl"), s.log()).unwrap();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[1] = format!("{}/", s.log());
    check(&s, &args, "", &start_rf("cannot read log: entity not found"), 3);
    assert!(fs::symlink_metadata(s.p("nowhere.jsonl")).is_err());
}
#[test]
fn log_missing_is_refused() {
    let s = with_log("");
    fs::remove_file(s.log()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("cannot read log: entity not found"), 3);
}
#[test]
fn log_empty_is_refused() {
    let s = with_log("");
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("no start row for the session"), 3);
}
#[test]
fn log_of_other_sessions_only_is_refused() {
    let s = with_log(&format!("{}{}", hook(M, OTHER), hook(M + 1, OTHER)));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("no start row for the session"), 3);
}
#[test]
fn log_with_only_a_launch_row_of_the_id_is_refused() {
    let s = with_log(&launch(M, SID));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("no start row for the session"), 3);
}
#[test]
fn log_with_only_an_unmanaged_row_of_the_id_is_refused() {
    let s = with_log(&unmanaged(M, SID));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("no start row for the session"), 3);
}
#[test]
fn log_with_a_corrupt_line_is_refused() {
    let s = with_log(&format!("{}not a row\n{}", hook(M, SID), hook(M, SID)));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("the log has a corrupt line or a torn tail"), 3);
}
#[test]
fn log_with_a_fractional_ts_ms_is_refused() {
    let s = with_log("{\"v\":2,\"kind\":\"session_started\",\"ts_ms\":1.5,\"session_id\":\"11111111-2222-4333-8444-555555555555\",\"source\":\"hook\"}\n");
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("the log has a corrupt line or a torn tail"), 3);
}
#[test]
fn log_ending_in_a_fragment_is_refused() {
    let s = with_log(&format!("{}{{\"v\":2,\"kind\":\"sess", hook(M, SID)));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("the log has a corrupt line or a torn tail"), 3);
}
#[test]
fn start_over_the_range_is_refused() {
    let s = with_log(&hook(253_401_609_599_001, SID));
    check(&s, &full(&s, MAXMS, "+08:00"), "", &start_rf("the start time is out of range"), 3);
}
#[test]
fn start_over_the_range_is_checked_before_after_now() {
    let s = with_log(&hook(253_401_609_599_001, SID));
    check(&s, &full(&s, "1", "+08:00"), "", &start_rf("the start time is out of range"), 3);
}
#[test]
fn start_one_ms_after_now_is_refused() {
    let s = with_log(&hook(M + 1, SID));
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("the start time is after now"), 3);
}
#[test]
fn start_equal_to_now_is_not_refused() {
    decide(M, M, "+08:00", NOT_COUNTED, "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21", 0);
}
#[test]
fn start_at_the_range_limit_without_now_ms_is_after_now() {
    let s = with_log(&hook(253_401_609_599_000, SID));
    let args = a(&["--log", &s.log(), "--session-id", SID, "--transcript", &s.tr(), "--utc-offset", "+08:00"]);
    check(&s, &args, "", &start_rf("the start time is after now"), 3);
}

// ---- transcript refusals ----

#[test]
fn transcript_name_with_another_uuid_is_refused() {
    let s = with_log(&hook(M, SID));
    s.put(&format!("{OTHER}.jsonl"), NOT_COUNTED);
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[5] = s.p(&format!("{OTHER}.jsonl"));
    check(&s, &args, "", &tr_rf("the file name does not match the session id"), 3);
}
#[test]
fn event_log_as_the_transcript_is_refused_by_name() {
    let s = with_log(&hook(M, SID));
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[5] = s.log();
    check(&s, &args, "", &tr_rf("the file name does not match the session id"), 3);
}
#[test]
fn transcript_name_with_the_id_in_uppercase_is_refused() {
    let s = with_log(&hook(M, UP_SID));
    s.put(&format!("{UP_NAME}.jsonl"), NOT_COUNTED);
    let args = a(&["--log", &s.log(), "--session-id", UP_SID, "--transcript", &s.p(&format!("{UP_NAME}.jsonl")), "--utc-offset", "+08:00", "--now-ms", &M.to_string()]);
    check(&s, &args, "", &tr_rf("the file name does not match the session id"), 3);
}
#[test]
fn transcript_missing_is_refused() {
    let s = with_log(&hook(M, SID));
    fs::remove_file(s.tr()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("the transcript cannot be opened: entity not found"), 3);
}
#[test]
fn transcript_unreadable_is_refused() {
    let s = with_log(&hook(M, SID));
    fs::set_permissions(s.tr(), fs::Permissions::from_mode(0o000)).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("the transcript cannot be opened: permission denied"), 3);
}
#[test]
fn transcript_a_folder_is_refused() {
    let s = with_log(&hook(M, SID));
    fs::remove_file(s.tr()).unwrap();
    fs::create_dir(s.tr()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("the transcript is not a regular file"), 3);
}
#[test]
fn transcript_a_live_symlink_is_refused() {
    let s = with_log(&hook(M, SID));
    s.put("real.jsonl", NOT_COUNTED);
    fs::remove_file(s.tr()).unwrap();
    symlink(s.p("real.jsonl"), s.tr()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("the transcript is a symbolic link"), 3);
}
#[test]
fn transcript_a_dangling_symlink_is_refused() {
    let s = with_log(&hook(M, SID));
    fs::remove_file(s.tr()).unwrap();
    symlink(s.p("nowhere.jsonl"), s.tr()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("the transcript is a symbolic link"), 3);
}
#[test]
fn transcript_live_symlink_with_trailing_slash_creates_nothing() {
    let s = with_log(&hook(M, SID));
    s.put("real.jsonl", NOT_COUNTED);
    fs::remove_file(s.tr()).unwrap();
    symlink(s.p("real.jsonl"), s.tr()).unwrap();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[5] = format!("{}/", s.tr());
    check(&s, &args, "", &tr_rf("the transcript cannot be opened: not a directory"), 3);
}
#[test]
fn transcript_dangling_symlink_with_trailing_slash_creates_nothing() {
    let s = with_log(&hook(M, SID));
    fs::remove_file(s.tr()).unwrap();
    symlink(s.p("nowhere.jsonl"), s.tr()).unwrap();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[5] = format!("{}/", s.tr());
    check(&s, &args, "", &tr_rf("the transcript cannot be opened: entity not found"), 3);
    assert!(fs::symlink_metadata(s.p("nowhere.jsonl")).is_err());
}
#[test]
fn transcript_a_fifo_is_refused_without_a_hang() {
    let s = with_log(&hook(M, SID));
    fs::remove_file(s.tr()).unwrap();
    assert!(Command::new("mkfifo").arg(s.tr()).status().unwrap().success());
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("the transcript is not a regular file"), 3);
}
#[test]
fn transcript_empty_is_refused() {
    let s = with_tr("");
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("the transcript is empty"), 3);
}
#[test]
fn transcript_line_three_not_json_is_refused() {
    let s = with_tr("{\"type\":\"user\"}\n{\"type\":\"user\"}\nnot json\n{\"type\":\"user\"}\n");
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("line 3 is not valid JSON"), 3);
}
#[test]
fn transcript_assistant_line_without_usage_is_refused() {
    let s = with_tr("{\"type\":\"user\"}\n{\"type\":\"assistant\",\"message\":{\"id\":\"m1\"}}\n");
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("line 2: assistant message has no usage"), 3);
}
#[test]
fn transcript_with_a_cut_last_line_is_refused() {
    let s = with_tr("{\"type\":\"user\"}\n{\"type\":\"user\"}\n{\"type\":\"assis");
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("line 3 is not valid JSON"), 3);
}
#[test]
fn marker_word_in_a_refused_transcript_is_not_printed() {
    let s = with_tr("{\"type\":\"user\",\"message\":{\"content\":\"zebrafish\"}}\nnot json zebrafish\n");
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &tr_rf("line 2 is not valid JSON"), 3);
}
#[test]
fn marker_word_in_a_decided_transcript_is_not_printed() {
    let t = "{\"type\":\"user\",\"message\":{\"content\":\"zebrafish\"}}\n";
    decide(M, M, "+08:00", t, "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21", 0);
}

// ---- order of checks ----

#[test]
fn bad_log_and_bad_transcript_give_the_start_record_line() {
    let s = with_log("");
    fs::remove_file(s.tr()).unwrap();
    check(&s, &full(&s, &M.to_string(), "+08:00"), "", &start_rf("no start row for the session"), 3);
}
#[test]
fn wrong_name_and_unreadable_file_give_the_name_line() {
    let s = with_log(&hook(M, SID));
    s.put("other.jsonl", NOT_COUNTED);
    fs::set_permissions(s.p("other.jsonl"), fs::Permissions::from_mode(0o000)).unwrap();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[5] = s.p("other.jsonl");
    check(&s, &args, "", &tr_rf("the file name does not match the session id"), 3);
    fs::set_permissions(s.p("other.jsonl"), fs::Permissions::from_mode(0o644)).unwrap();
}
#[test]
fn usage_error_comes_before_a_bad_log() {
    let s = S::new();
    let args = a(&["--log", &s.log(), "--session-id", SID, "--transcript", &s.tr()]);
    check(&s, &args, "", &uerr("--utc-offset is required"), 2);
}

// ---- usage ----

#[test]
fn unknown_flag_is_bad_arguments() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args.extend(a(&["--bogus", "x"]));
    check(&s, &args, "", &uerr("bad arguments"), 2);
}
#[test]
fn flag_with_no_value_is_bad_arguments() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args.push("--now-ms".to_string());
    args.remove(args.len() - 3);
    args.remove(args.len() - 2);
    check(&s, &args, "", &uerr("bad arguments"), 2);
}
#[test]
fn flag_given_twice_is_bad_arguments() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args.extend(a(&["--utc-offset", "+08:00"]));
    check(&s, &args, "", &uerr("bad arguments"), 2);
}
#[test]
fn stray_argument_is_bad_arguments() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args.push("stray".to_string());
    check(&s, &args, "", &uerr("bad arguments"), 2);
}
#[test]
fn log_is_required() {
    let s = S::new();
    check(&s, &a(&["--session-id", SID, "--transcript", &s.tr(), "--utc-offset", "+08:00"]), "", &uerr("--log is required"), 2);
}
#[test]
fn session_id_is_required() {
    let s = S::new();
    check(&s, &a(&["--log", &s.log(), "--transcript", &s.tr(), "--utc-offset", "+08:00"]), "", &uerr("--session-id is required"), 2);
}
#[test]
fn transcript_is_required() {
    let s = S::new();
    check(&s, &a(&["--log", &s.log(), "--session-id", SID, "--utc-offset", "+08:00"]), "", &uerr("--transcript is required"), 2);
}
#[test]
fn utc_offset_is_required() {
    let s = with_log(&hook(M, SID));
    check(&s, &a(&["--log", &s.log(), "--session-id", SID, "--transcript", &s.tr(), "--now-ms", &M.to_string()]), "", &uerr("--utc-offset is required"), 2);
}
#[test]
fn required_flags_are_checked_in_order() {
    let s = S::new();
    check(&s, &a(&["--utc-offset", "+08:00"]), "", &uerr("--log is required"), 2);
}
#[test]
fn log_must_be_absolute() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[1] = "e.jsonl".to_string();
    check(&s, &args, "", &uerr("--log must be an absolute path"), 2);
}
#[test]
fn transcript_must_be_absolute() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[5] = format!("{SID}.jsonl");
    check(&s, &args, "", &uerr("--transcript must be an absolute path"), 2);
}
#[test]
fn session_id_in_uppercase_is_refused() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[3] = UP_NAME.to_string();
    check(&s, &args, "", &uerr("--session-id must be a uuid"), 2);
}
#[test]
fn session_id_not_a_uuid_is_refused() {
    let s = S::new();
    let mut args = full(&s, &M.to_string(), "+08:00");
    args[3] = "not-a-uuid".to_string();
    check(&s, &args, "", &uerr("--session-id must be a uuid"), 2);
}

const OFF_MSG: &str = "--utc-offset must be +HH:MM or -HH:MM (HH 00 to 14, MM 00 to 59)";
fn bad_offset(off: &str) {
    let s = S::new();
    check(&s, &full(&s, &M.to_string(), off), "", &uerr(OFF_MSG), 2);
}
#[test]
fn offset_plus_8_colon_00_is_refused() {
    bad_offset("+8:00");
}
#[test]
fn offset_utc_is_refused() {
    bad_offset("UTC");
}
#[test]
fn offset_zone_name_is_refused() {
    bad_offset("Asia/Hong_Kong");
}
#[test]
fn offset_plus_15_is_refused() {
    bad_offset("+15:00");
}
#[test]
fn offset_minutes_60_is_refused() {
    bad_offset("+08:60");
}
#[test]
fn offset_without_colon_is_refused() {
    bad_offset("+0800");
}
#[test]
fn offset_empty_is_refused() {
    bad_offset("");
}

const NOW_MSG: &str = "--now-ms must be digits, at most 253401609599000";
fn bad_now(now: &str) {
    let s = S::new();
    check(&s, &full(&s, now, "+08:00"), "", &uerr(NOW_MSG), 2);
}
#[test]
fn now_ms_empty_is_refused() {
    bad_now("");
}
#[test]
fn now_ms_negative_is_refused() {
    bad_now("-1");
}
#[test]
fn now_ms_fraction_is_refused() {
    bad_now("1.5");
}
#[test]
fn now_ms_of_forty_digits_is_refused() {
    bad_now("1234567890123456789012345678901234567890");
}
#[test]
fn now_ms_over_the_limit_is_refused() {
    bad_now("253401609599001");
}
#[test]
fn now_ms_at_the_limit_is_accepted() {
    decide(M, 253_401_609_599_000, "+08:00", NOT_COUNTED, "rotation due=yes reason=day compactions=0 start_day=2026-11-21 now_day=9999-12-24", 10);
}
#[test]
fn two_bad_flags_give_the_first_line() {
    let s = S::new();
    let mut args = full(&s, "x", "UTC");
    args[3] = "bad".to_string();
    check(&s, &args, "", &uerr("--session-id must be a uuid"), 2);
}
#[test]
fn bad_offset_and_bad_now_give_the_offset_line() {
    let s = S::new();
    check(&s, &full(&s, "x", "UTC"), "", &uerr(OFF_MSG), 2);
}

// ---- decision: day ----

#[test]
fn day_changes_at_local_midnight_plus_8() {
    decide(M - 1, M, "+08:00", NOT_COUNTED, "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21", 10);
}
#[test]
fn day_does_not_change_before_local_midnight_plus_8() {
    decide(M - 1, M - 1, "+08:00", NOT_COUNTED, "rotation due=no reason=none compactions=0 start_day=2026-11-20 now_day=2026-11-20", 0);
}
#[test]
fn day_holds_to_the_last_ms_of_the_local_day() {
    decide(M, M + 86_399_999, "+08:00", NOT_COUNTED, "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21", 0);
}
#[test]
fn day_changes_one_local_day_later() {
    decide(M, M + 86_400_000, "+08:00", NOT_COUNTED, "rotation due=yes reason=day compactions=0 start_day=2026-11-21 now_day=2026-11-22", 10);
}
#[test]
fn utc_date_change_without_local_day_change_is_not_due() {
    decide(1_795_219_199_999, 1_795_219_200_000, "+08:00", NOT_COUNTED, "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21", 0);
}
#[test]
fn day_boundary_at_minus_5() {
    decide(1_795_237_199_999, 1_795_237_200_000, "-05:00", NOT_COUNTED, "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21", 10);
}
#[test]
fn day_boundary_at_plus_5_30() {
    decide(1_795_199_399_999, 1_795_199_400_000, "+05:30", NOT_COUNTED, "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21", 10);
}
#[test]
fn day_boundary_at_plus_14() {
    decide(1_795_168_799_999, 1_795_168_800_000, "+14:00", NOT_COUNTED, "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21", 10);
}
#[test]
fn day_boundary_at_minus_12() {
    decide(1_795_262_399_999, 1_795_262_400_000, "-12:00", NOT_COUNTED, "rotation due=yes reason=day compactions=0 start_day=2026-11-20 now_day=2026-11-21", 10);
}
#[test]
fn moment_near_the_epoch_at_minus_12() {
    decide(0, 0, "-12:00", NOT_COUNTED, "rotation due=no reason=none compactions=0 start_day=1969-12-31 now_day=1969-12-31", 0);
}

// ---- decision: compactions ----

#[test]
fn zero_markers_same_day_is_not_due() {
    decide(M, M + 1000, "+08:00", PLAIN6, "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21", 0);
}
#[test]
fn one_marker_same_day_is_due() {
    let t = format!("{PLAIN6}{MARK_A}");
    decide(M, M + 1000, "+08:00", &t, "rotation due=yes reason=compaction compactions=1 start_day=2026-11-21 now_day=2026-11-21", 10);
}
#[test]
fn two_markers_among_six_lines_same_day_is_due() {
    let p: Vec<&str> = PLAIN6.lines().collect();
    let t = format!("{}\n{MARK_A}{}\n{}\n{MARK_B}{}\n", p[0], p[1], p[2], p[3]);
    assert_eq!(t.lines().count(), 6);
    decide(M, M + 1000, "+08:00", &t, "rotation due=yes reason=compaction compactions=2 start_day=2026-11-21 now_day=2026-11-21", 10);
}
#[test]
fn two_markers_with_a_day_change_is_due_by_both() {
    let t = format!("{PLAIN6}{MARK_B}{MARK_A}");
    decide(M, M + 86_400_000, "+08:00", &t, "rotation due=yes reason=day+compaction compactions=2 start_day=2026-11-21 now_day=2026-11-22", 10);
}
#[test]
fn marker_with_the_other_key_order_counts() {
    let t = format!("{PLAIN6}{MARK_B}");
    decide(M, M, "+08:00", &t, "rotation due=yes reason=compaction compactions=1 start_day=2026-11-21 now_day=2026-11-21", 10);
}
#[test]
fn user_line_with_the_subtype_does_not_count() {
    let t = "{\"type\":\"user\",\"subtype\":\"compact_boundary\"}\n{\"type\":\"user\",\"message\":{\"content\":\"compact_boundary\"}}\n";
    decide(M, M, "+08:00", t, "rotation due=no reason=none compactions=0 start_day=2026-11-21 now_day=2026-11-21", 0);
}

// ---- source: no write calls ----

fn hit(text: &str, needle: &str) -> bool {
    text.contains(needle)
}

#[test]
fn source_holds_no_write_or_child_process_call() {
    let needles: Vec<String> = [("File", "::create"), ("Open", "Options"), ("fs::", "write"), ("fs::", "remove"), ("fs::", "rename"), ("fs::", "create_dir"), ("fs::", "copy"), ("set_", "permissions"), ("Com", "mand")]
        .iter()
        .map(|(x, y)| format!("{x}{y}"))
        .collect();
    let sample = needles.join(" ");
    for n in &needles {
        assert!(hit(&sample, n), "sample must hit {n}");
    }
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/limit_check/rotation_check.rs");
    let src = fs::read_to_string(path).expect("source file is missing");
    assert!(!src.is_empty());
    for n in &needles {
        assert!(!hit(&src, n), "source holds {n}");
    }
}
