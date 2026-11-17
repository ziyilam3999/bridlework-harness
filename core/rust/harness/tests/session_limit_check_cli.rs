//! `harness session limit-check`: real binary, synthetic transcripts written at run time (no real ids, paths or names).
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// 2026-11-17T00:00:00Z
const NOW: &str = "1794873600000";
const OFF: &str = "+08:00";

const WEEKLY: &str = "You've hit your weekly limit · resets Nov 19 at 11am (Etc/UTC)";

fn esc(t: &str) -> String {
    t.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\t', "\\t")
}

/// One error row. `None` leaves the field out.
fn err(status: Option<u32>, error: Option<&str>, transient: Option<bool>, text: Option<&str>) -> String {
    let mut s = String::from("{\"type\":\"assistant\",\"isApiErrorMessage\":true");
    if let Some(n) = status {
        s.push_str(&format!(",\"apiErrorStatus\":{n}"));
    }
    if let Some(e) = error {
        s.push_str(&format!(",\"error\":\"{e}\""));
    }
    if let Some(t) = transient {
        s.push_str(&format!(",\"apiErrorIsTransient\":{t}"));
    }
    match text {
        Some(t) => s.push_str(&format!(",\"message\":{{\"content\":[{{\"type\":\"text\",\"text\":\"{}\"}}]}}}}", esc(t))),
        None => s.push_str(",\"message\":{\"content\":[]}}"),
    }
    s
}

fn rl(text: &str) -> String {
    err(Some(429), Some("rate_limit"), None, Some(text))
}

fn zai_text(when: &str) -> String {
    format!("Weekly/Monthly Limit Exhausted. Your limit will reset at {when} [req-test] [1310]")
}

fn zai(when: &str) -> String {
    rl(&zai_text(when))
}

struct Dir(PathBuf);
impl Dir {
    fn new() -> Dir {
        let d = std::env::temp_dir().join(format!("lchk-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
    fn names(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.0).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        v.sort();
        v
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run_bytes(body: &[u8], args: &[&str]) -> Output {
    let d = Dir::new();
    let f = d.0.join("t.jsonl");
    fs::write(&f, body).unwrap();
    let mut c = Command::new(BIN);
    c.args(["session", "limit-check", "--transcript", f.to_str().unwrap()]).args(args).stdin(Stdio::null());
    c.output().unwrap()
}

fn lines(rows: &[String]) -> String {
    format!("{}\n", rows.join("\n"))
}

/// Run with the main clock and offset.
fn go(rows: &[String], provider: &str) -> Output {
    run_bytes(lines(rows).as_bytes(), &["--provider", provider, "--utc-offset", OFF, "--now-ms", NOW])
}

/// Run with the main clock and no offset.
fn go_no_offset(rows: &[String], provider: &str) -> Output {
    run_bytes(lines(rows).as_bytes(), &["--provider", provider, "--now-ms", NOW])
}

fn go_at(rows: &[String], provider: &str, now: &str, off: &str) -> Output {
    run_bytes(lines(rows).as_bytes(), &["--provider", provider, "--utc-offset", off, "--now-ms", now])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn errs(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn expect(o: &Output, code: i32, prefix: &str) {
    assert_eq!(o.status.code(), Some(code), "stdout={} stderr={}", out(o), errs(o));
    assert!(out(o).starts_with(prefix), "want prefix {prefix:?}, got {:?}", out(o));
    assert!(out(o).ends_with('\n') && out(o).matches('\n').count() == 1, "one line: {:?}", out(o));
    assert!(errs(o).is_empty(), "stderr: {}", errs(o));
}

fn limit(provider: &str, scope: &str, code: &str, resets: &str) -> String {
    format!("class=limit provider={provider} scope={scope} code={code} resets={resets} text=")
}

fn refused(o: &Output, code: i32, stderr_start: &str) {
    assert_eq!(o.status.code(), Some(code), "stdout={} stderr={}", out(o), errs(o));
    assert!(out(o).is_empty(), "stdout: {}", out(o));
    assert!(errs(o).starts_with(stderr_start), "stderr: {}", errs(o));
}

#[test]
fn claude_weekly_with_a_date_is_a_limit() {
    let o = go(&[rl(WEEKLY)], "claude");
    expect(&o, 0, &limit("claude", "provider", "weekly", "2026-11-19T03:00:00Z"));
}

#[test]
fn claude_weekly_with_a_clock_only_takes_the_next_occurrence() {
    let o = go(&[rl("You've hit your weekly limit · resets 11am (Etc/UTC)")], "claude");
    expect(&o, 0, &limit("claude", "provider", "weekly", "2026-11-17T03:00:00Z"));
}

#[test]
fn claude_session_limit_is_a_limit() {
    let t = "You've hit your session limit · resets 12:30am (Etc/UTC) · progress saved";
    expect(&go(&[rl(t)], "claude"), 0, &limit("claude", "provider", "session", "2026-11-17T16:30:00Z"));
    let t = "You've hit your session limit · resets 10:40pm (Etc/UTC)";
    expect(&go(&[rl(t)], "claude"), 0, &limit("claude", "provider", "session", "2026-11-17T14:40:00Z"));
}

#[test]
fn fable_cap_marks_fable_only() {
    let hit = "You've hit your Fable limit · resets Nov 19 at 11am (Etc/UTC)";
    expect(&go(&[rl(hit)], "claude"), 0, &limit("claude", "fable", "model", "2026-11-19T03:00:00Z"));
    let reached = "You've reached your Fable 5 limit. Run /usage-credits to continue";
    expect(&go(&[rl(reached)], "claude"), 0, &limit("claude", "fable", "model", "unknown"));
}

#[test]
fn another_model_cap_marks_the_whole_of_claude() {
    let t = "You've reached your Opus limit · resets Nov 19 at 11am (Etc/UTC)";
    expect(&go(&[rl(t)], "claude"), 0, &limit("claude", "provider", "model", "2026-11-19T03:00:00Z"));
}

#[test]
fn zai_1310_is_a_limit_with_its_reset() {
    expect(&go(&[zai("2026-11-20 23:35:47")], "zai"), 0, &limit("zai", "provider", "1310", "2026-11-20T15:35:47Z"));
    let t = "Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-11-20 23:35:47";
    expect(&go(&[rl(t)], "zai"), 0, &limit("zai", "provider", "1310", "2026-11-20T15:35:47Z"));
}

#[test]
fn zai_1113_is_a_limit_with_no_reset() {
    let t = "Insufficient balance or no resource package [1113] [req-test]";
    expect(&go(&[rl(t)], "zai"), 0, &limit("zai", "provider", "1113", "unknown"));
}

#[test]
fn zai_1308_is_unknown() {
    let t = "Usage limit reached for 5 hour. Your limit will reset at 2026-11-17 12:00:00 [1308] [req-test]";
    expect(&go(&[rl(t)], "zai"), 5, "class=unknown provider=- scope=- code=429 resets=unknown text=");
}

#[test]
fn non_limit_errors_are_other() {
    let cases = [
        (err(Some(403), Some("authentication_failed"), None, Some("Please run /login")), "403"),
        (err(Some(529), Some("server_error"), None, Some("Overloaded")), "529"),
        (err(None, Some("server_error"), None, Some("Request timed out")), "-"),
        (err(Some(404), Some("model_not_found"), None, Some("There's an issue with the selected model")), "404"),
        (err(Some(400), Some("invalid_request"), None, Some("Prompt is too long")), "400"),
        (err(None, Some("max_output_tokens"), None, Some("Output token maximum exceeded")), "-"),
    ];
    for (row, code) in cases {
        let o = go(&[row], "claude");
        expect(&o, 4, &format!("class=other provider=- scope=- code={code} resets=unknown text="));
    }
}

#[test]
fn autocompact_text_with_the_word_limit_is_other() {
    let t = "Autocompact is thrashing: the context refilled to the limit";
    let o = go(&[err(Some(400), Some("invalid_request"), None, Some(t))], "claude");
    expect(&o, 4, "class=other provider=- scope=- code=400 resets=unknown text=Autocompact");
}

#[test]
fn an_unmatched_429_is_unknown() {
    let o = go(&[rl("Too many requests")], "claude");
    expect(&o, 5, "class=unknown provider=- scope=- code=429 resets=unknown text=Too many");
}

#[test]
fn a_provider_mismatch_is_unknown() {
    expect(&go(&[rl(WEEKLY)], "zai"), 5, "class=unknown provider=- scope=- code=429");
    expect(&go(&[zai("2026-11-20 23:35:47")], "claude"), 5, "class=unknown provider=- scope=- code=429");
}

#[test]
fn a_limit_text_without_429_and_rate_limit_is_unknown() {
    let o = go(&[err(Some(403), Some("rate_limit"), None, Some(WEEKLY))], "claude");
    expect(&o, 5, "class=unknown provider=- scope=- code=403");
    let o = go(&[err(Some(429), Some("server_error"), None, Some(WEEKLY))], "claude");
    expect(&o, 5, "class=unknown provider=- scope=- code=429");
    let o = go(&[err(Some(403), Some("authentication_failed"), None, Some(WEEKLY))], "claude");
    expect(&o, 5, "class=unknown provider=- scope=- code=403");
}

#[test]
fn a_transient_row_is_not_a_limit() {
    let t = zai_text("2026-11-20 23:35:47");
    let o = go(&[err(Some(429), Some("rate_limit"), Some(true), Some(t.as_str()))], "zai");
    expect(&o, 4, "class=other provider=- scope=- code=429 resets=unknown text=");
}

#[test]
fn a_seat_that_recovered_exits_6() {
    let ok = "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"done\"}]}}".to_string();
    let o = go(&[zai("2026-11-20 23:35:47"), ok], "zai");
    refused(&o, 6, "harness: limit-check: reason=recovered\n");
}

#[test]
fn a_transcript_with_no_error_row_exits_6() {
    let rows = ["{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}".to_string(), "{\"type\":\"assistant\",\"isApiErrorMessage\":false}".to_string()];
    refused(&go(&rows, "claude"), 6, "harness: limit-check: reason=no-error-row\n");
}

#[test]
fn only_the_last_error_row_counts() {
    let other = err(Some(403), Some("authentication_failed"), None, Some("Please run /login"));
    let o = go(&[zai("2026-11-20 23:35:47"), other.clone()], "zai");
    expect(&o, 4, "class=other");
    let o = go(&[other, zai("2026-11-20 23:35:47")], "zai");
    expect(&o, 0, "class=limit");
}

#[test]
fn a_reset_in_the_past_is_unknown() {
    expect(&go(&[zai("2026-11-16 23:35:47")], "zai"), 0, &limit("zai", "provider", "1310", "unknown"));
    let t = "You've hit your weekly limit · resets Nov 12 at 11am (Etc/UTC)";
    expect(&go(&[rl(t)], "claude"), 0, &limit("claude", "provider", "weekly", "unknown"));
}

#[test]
fn a_reset_more_than_8_days_ahead_is_unknown() {
    expect(&go(&[zai("2026-11-25 08:00:01")], "zai"), 0, &limit("zai", "provider", "1310", "unknown"));
}

#[test]
fn a_reset_exactly_8_days_ahead_is_kept() {
    expect(&go(&[zai("2026-11-25 08:00:00")], "zai"), 0, &limit("zai", "provider", "1310", "2026-11-25T00:00:00Z"));
}

#[test]
fn claude_reset_across_new_year_takes_the_next_year() {
    let t = "You've hit your weekly limit · resets Jan 2 at 11am (Etc/UTC)";
    let o = go_at(&[rl(t)], "claude", "1798588800000", OFF);
    expect(&o, 0, &limit("claude", "provider", "weekly", "2027-01-02T03:00:00Z"));
}

#[test]
fn claude_reset_without_utc_offset_is_unknown() {
    expect(&go_no_offset(&[rl(WEEKLY)], "claude"), 0, &limit("claude", "provider", "weekly", "unknown"));
}

#[test]
fn bad_reset_words_are_unknown() {
    for w in ["Feb 30 at 11am", "13pm", "Foo 3 at 11am", "Nov 19 at 0am", "Nov 19 at 11:5am"] {
        let t = format!("You've hit your weekly limit · resets {w} (Etc/UTC)");
        expect(&go(&[rl(&t)], "claude"), 0, &limit("claude", "provider", "weekly", "unknown"));
    }
    let t = "You've reached your Fable limit. Run /usage-credits to continue";
    expect(&go(&[rl(t)], "claude"), 0, &limit("claude", "fable", "model", "unknown"));
}

#[test]
fn an_unparsable_line_exits_3_and_names_the_line() {
    let body = format!("{}\nnot json at all\n{}\n", zai("2026-11-20 23:35:47"), zai("2026-11-20 23:35:47"));
    let o = run_bytes(body.as_bytes(), &["--provider", "zai", "--now-ms", NOW]);
    refused(&o, 3, "harness: limit-check refused: reason=unreadable");
    assert!(errs(&o).contains("line 2"), "{}", errs(&o));
    let body = format!("{}\n{{\"type\":\"user\"", zai("2026-11-20 23:35:47"));
    let o = run_bytes(body.as_bytes(), &["--provider", "zai", "--now-ms", NOW]);
    refused(&o, 3, "harness: limit-check refused: reason=unreadable");
    assert!(errs(&o).contains("line 2"), "{}", errs(&o));
}

#[test]
fn a_missing_empty_or_folder_transcript_exits_3() {
    let o = Command::new(BIN).args(["session", "limit-check", "--transcript", "/nonexistent-dir-xyz/none.jsonl", "--provider", "zai"]).output().unwrap();
    refused(&o, 3, "harness: limit-check refused: reason=unreadable");
    refused(&run_bytes(b"", &["--provider", "zai"]), 3, "harness: limit-check refused: reason=unreadable");
    let d = Dir::new();
    let o = Command::new(BIN).args(["session", "limit-check", "--transcript", d.0.to_str().unwrap(), "--provider", "zai"]).output().unwrap();
    refused(&o, 3, "harness: limit-check refused: reason=unreadable");
    let mut body = lines(&[zai("2026-11-20 23:35:47")]).into_bytes();
    body.extend_from_slice(b"{\"x\":\"\xff\"}\n");
    refused(&run_bytes(&body, &["--provider", "zai"]), 3, "harness: limit-check refused: reason=unreadable");
}

#[test]
fn usage_refusals_exit_2() {
    let d = Dir::new();
    let f = d.0.join("t.jsonl");
    fs::write(&f, lines(&[zai("2026-11-20 23:35:47")])).unwrap();
    let f = f.to_str().unwrap();
    let cases: Vec<Vec<&str>> = vec![
        vec![],
        vec!["--provider", "zai"],
        vec!["--transcript", f],
        vec!["--transcript", f, "--transcript", f, "--provider", "zai"],
        vec!["--transcript", f, "--provider", "zai", "--provider", "zai"],
        vec!["--transcript", f, "--provider", "zai", "--utc-offset", "+08:00", "--utc-offset", "+08:00"],
        vec!["--transcript", f, "--provider", "zai", "--now-ms", "1", "--now-ms", "1"],
        vec!["--transcript", f, "--provider"],
        vec!["--transcript", f, "--provider", "zai", "--now-ms"],
        vec!["--transcript", f, "--provider", "zai", "--bogus", "x"],
        vec!["--transcript", f, "--provider=zai"],
        vec!["--transcript", f, "--provider", "zai", "stray"],
        vec!["--transcript", f, "--provider", "openai"],
        vec!["--transcript", f, "--provider", "Claude"],
        vec!["--transcript", f, "--provider", ""],
        vec!["--transcript", f, "--provider", "zai", "--utc-offset", "+8"],
        vec!["--transcript", f, "--provider", "zai", "--utc-offset", "08:00"],
        vec!["--transcript", f, "--provider", "zai", "--utc-offset", "+24:00"],
        vec!["--transcript", f, "--provider", "zai", "--utc-offset", "+15:00"],
        vec!["--transcript", f, "--provider", "zai", "--utc-offset", "+08:60"],
        vec!["--transcript", f, "--provider", "zai", "--utc-offset", ""],
        vec!["--transcript", f, "--provider", "zai", "--now-ms", ""],
        vec!["--transcript", f, "--provider", "zai", "--now-ms", "-1"],
        vec!["--transcript", f, "--provider", "zai", "--now-ms", "12x"],
        vec!["--transcript", f, "--provider", "zai", "--now-ms", "1.5"],
    ];
    for c in cases {
        let o = Command::new(BIN).arg("session").arg("limit-check").args(&c).output().unwrap();
        refused(&o, 2, "usage: harness session limit-check --transcript");
    }
    // the offsets that pass
    for ok in ["+08:00", "-05:30", "+14:00", "-00:00"] {
        let o = run_bytes(lines(&[zai("2026-11-20 23:35:47")]).as_bytes(), &["--provider", "zai", "--utc-offset", ok, "--now-ms", NOW]);
        assert_eq!(o.status.code(), Some(0), "{ok}: {}", errs(&o));
    }
}

#[test]
fn usage_names_the_verb() {
    let o = Command::new(BIN).arg("session").output().unwrap();
    refused(&o, 2, "usage:");
    assert!(errs(&o).contains("session limit-check --transcript"), "{}", errs(&o));
}

#[test]
fn the_text_part_is_one_line_and_short() {
    let t = format!("Odd\ntext\twith controls {}", "x".repeat(200));
    let o = go(&[err(Some(403), Some("authentication_failed"), None, Some(t.as_str()))], "claude");
    assert_eq!(o.status.code(), Some(4));
    let s = out(&o);
    assert_eq!(s.matches('\n').count(), 1);
    let text = s.trim_end().split_once(" text=").unwrap().1;
    assert_eq!(text.chars().count(), 80);
    assert!(text.starts_with("Odd text with controls x"), "{text}");
    // a cut on a character boundary
    let t = "é".repeat(100);
    let o = go(&[err(Some(403), Some("authentication_failed"), None, Some(t.as_str()))], "claude");
    let s = out(&o);
    assert_eq!(s.trim_end().split_once(" text=").unwrap().1.chars().count(), 80);
}

#[test]
fn the_verb_writes_no_file() {
    let d = Dir::new();
    let f = d.0.join("t.jsonl");
    let before = d.names();
    let cases = [
        (lines(&[zai("2026-11-20 23:35:47")]), 0),
        (lines(&[err(Some(403), Some("authentication_failed"), None, Some("Please run /login"))]), 4),
        (lines(&[rl("Too many requests")]), 5),
    ];
    for (body, code) in cases {
        fs::write(&f, &body).unwrap();
        let before_run = d.names();
        assert_eq!(before_run.len(), before.len() + 1);
        let o = Command::new(BIN).args(["session", "limit-check", "--transcript", f.to_str().unwrap(), "--provider", "zai", "--now-ms", NOW]).output().unwrap();
        assert_eq!(o.status.code(), Some(code));
        assert_eq!(d.names(), before_run);
        assert_eq!(fs::read_to_string(&f).unwrap(), body);
    }
}

#[test]
fn a_limit_text_in_a_tool_result_is_not_an_error_row() {
    let t = format!("{WEEKLY} {{\"isApiErrorMessage\": true}}");
    let row = format!("{{\"type\":\"user\",\"message\":{{\"content\":[{{\"type\":\"tool_result\",\"content\":\"{}\"}}]}}}}", esc(&t));
    let o = go(&[row], "claude");
    refused(&o, 6, "harness: limit-check: reason=no-error-row\n");
}

#[test]
fn rows_after_the_error_that_are_not_assistant_do_not_recover() {
    let rows = [
        rl("Insufficient balance or no resource package [1113] [req-test]"),
        "{\"type\":\"user\",\"message\":{\"content\":\"go on\"}}".to_string(),
        "{\"type\":\"system\",\"subtype\":\"info\"}".to_string(),
        "{\"costUSD\":0.5,\"durationMs\":10}".to_string(),
    ];
    expect(&go(&rows, "zai"), 0, &limit("zai", "provider", "1113", "unknown"));
}

#[test]
fn crlf_line_ends_are_read() {
    let body = format!("{}\r\n{}\r\n", "{\"type\":\"user\"}", zai("2026-11-20 23:35:47"));
    let o = run_bytes(body.as_bytes(), &["--provider", "zai", "--now-ms", NOW]);
    expect(&o, 0, &limit("zai", "provider", "1310", "2026-11-20T15:35:47Z"));
}

#[test]
fn a_byte_order_mark_exits_3_on_line_1() {
    let body = format!("\u{feff}{}\n", zai("2026-11-20 23:35:47"));
    let o = run_bytes(body.as_bytes(), &["--provider", "zai", "--now-ms", NOW]);
    refused(&o, 3, "harness: limit-check refused: reason=unreadable");
    assert!(errs(&o).contains("line 1"), "{}", errs(&o));
}

#[test]
fn a_long_line_is_read() {
    let big = format!("{{\"type\":\"user\",\"message\":{{\"content\":\"{}\"}}}}", "x".repeat(2_000_000));
    let o = go(&[big, zai("2026-11-20 23:35:47")], "zai");
    expect(&o, 0, &limit("zai", "provider", "1310", "2026-11-20T15:35:47Z"));
}

#[test]
fn a_claude_text_with_no_model_name_is_unknown() {
    let o = go(&[rl("You've hit your limit · resets 3pm (Etc/UTC)")], "claude");
    expect(&o, 5, "class=unknown provider=- scope=- code=429");
}

#[test]
fn feb_29_is_read_only_in_a_leap_year() {
    let t = "You've hit your weekly limit · resets Feb 29 at 11am (Etc/UTC)";
    let o = go_at(&[rl(t)], "claude", "1835222400000", OFF);
    expect(&o, 0, &limit("claude", "provider", "weekly", "2028-02-29T03:00:00Z"));
    expect(&go(&[rl(t)], "claude"), 0, &limit("claude", "provider", "weekly", "unknown"));
}

#[test]
fn a_negative_offset_crosses_the_date() {
    let t = "You've hit your session limit · resets 11pm (Etc/UTC)";
    let o = go_at(&[rl(t)], "claude", "1794880800000", "-05:00");
    expect(&o, 0, &limit("claude", "provider", "session", "2026-11-17T04:00:00Z"));
}
