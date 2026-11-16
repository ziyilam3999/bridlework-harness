//! `harness friction controls-check`, process-level on scratch files. Every id and name is synthetic.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

/// The inbox of the worked case: g-1 (blocker) broken once, g-2 seen once, g-3 broken once.
fn worked(t: &Tmp) {
    t.write_inbox(&[
        hand("g-1", "2026-11-02", "", "blocker"),
        hand("g-1b", "2026-11-09", "g-1", ""),
        hand("g-2", "2026-11-02", "", ""),
        hand("g-3", "2026-11-02", "", ""),
        hand("g-3b", "2026-11-09", "g-3", ""),
    ]);
}

#[test]
fn rank_output_of_the_worked_inbox_is_unchanged() {
    let t = Tmp::new();
    worked(&t);
    let cfg = t.config();
    let o = run(&["friction", "rank", "--top", "100000", "--config", p(&cfg)]);
    assert_eq!(code(&o), 0);
    assert_eq!(
        out(&o),
        "rank group=g-1 score=54 impact=blocker occurrences=2 first=2026-11-02 last=2026-11-09\n\
         rank group=g-3 score=2 impact=unrated occurrences=2 first=2026-11-02 last=2026-11-09\n\
         rank group=g-2 score=1 impact=unrated occurrences=1 first=2026-11-02 last=2026-11-02\n\
         rank ok groups=3 listed=3\n"
    );
}

const SHAPE: &str = "<group id> | control: <pointer> | lesson: ISSUE-NNN or <group id> | not machine-checkable: <reason> | lesson: ISSUE-NNN";

/// A lessons folder holding the page ISSUE-001-x.md.
fn lessons(t: &Tmp) -> PathBuf {
    let d = t.0.join("lessons");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("ISSUE-001-x.md"), "x").unwrap();
    d
}

fn check(t: &Tmp, controls: &Path, lessons: &Path) -> Output {
    let cfg = t.config();
    run(&["friction", "controls-check", "--controls", p(controls), "--lessons-dir", p(lessons), "--config", p(&cfg)])
}

/// A valid controls line for `id`.
fn line(id: &str) -> String {
    format!("{id} | control: a guard pointer | lesson: ISSUE-001\n")
}

fn refused(ids: &str) -> String {
    format!("harness: friction refused: repeat groups with no valid line: {ids}. Add one line per group to the controls file: {SHAPE}\n")
}

/// Two rows of one group g-1.
fn twice(t: &Tmp) {
    t.write_inbox(&[hand("g-1", "2026-11-02", "", ""), hand("g-1b", "2026-11-09", "g-1", "")]);
}

/// Run on the inbox already written, with the given controls text and the page ISSUE-001-x.md.
fn with_controls(t: &Tmp, text: impl AsRef<[u8]>) -> Output {
    let c = t.file("controls.txt", text);
    let l = lessons(t);
    check(t, &c, &l)
}

fn assert_refused_empty_stdout(o: &Output, c: i32) {
    assert_eq!(code(o), c, "stderr: {}", err(o));
    assert_eq!(out(o), "");
}

// ---------------------------------------------------------------- the worked case

#[test]
fn worked_case_exact_lines() {
    let t = Tmp::new();
    worked(&t);
    let o = with_controls(&t, "g-1 | control: a guard pointer | lesson: ISSUE-001\n");
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("g-3"));
    let o = with_controls(&t, "g-1 | control: a guard pointer | lesson: ISSUE-001\ng-3 | not machine-checkable: a reason | lesson: ISSUE-001\n");
    assert_eq!(code(&o), 0);
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), "controls-check ok groups=3 repeat=2 lines=2\n");
}

// ---------------------------------------------------------------- seen twice

#[test]
fn one_occurrence_is_not_required() {
    let t = Tmp::new();
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "")]);
    let o = with_controls(&t, "");
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=0 lines=0\n");
}

#[test]
fn two_occurrences_are_required() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, "");
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("g-1"));
}

#[test]
fn a_chain_counts_once_per_row() {
    let t = Tmp::new();
    t.write_inbox(&[hand("g-1", "2026-11-02", "", ""), hand("g-1b", "2026-11-09", "g-1", ""), hand("g-1c", "2026-11-10", "g-1b", "")]);
    let o = with_controls(&t, "");
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("g-1"));
    let o = with_controls(&t, line("g-1"));
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=1\n");
}

#[test]
fn an_event_group_is_named_by_its_signature() {
    let t = Tmp::new();
    t.write_inbox(&[event("sig-a", "2026-w45", "2026-11-02", ""), event("sig-a", "2026-w46", "2026-11-09", "")]);
    let o = with_controls(&t, "");
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("sig-a"));
    let o = with_controls(&t, line("sig-a"));
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=1\n");
}

#[test]
fn a_line_for_an_event_row_id_covers_nothing() {
    let t = Tmp::new();
    t.write_inbox(&[event("sig-a", "2026-w45", "2026-11-02", ""), event("sig-a", "2026-w46", "2026-11-09", "")]);
    let o = with_controls(&t, line("sig-a.2026-w45"));
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("sig-a"));
}

#[test]
fn a_pattern_broken_once_is_not_required() {
    let t = Tmp::new();
    t.write_inbox(&[pattern("p-1", "2026-11-01", ""), hand("b-1", "2026-11-02", "p-1", "")]);
    let o = with_controls(&t, "");
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=0 lines=0\n");
}

fn broken_twice(t: &Tmp) {
    t.write_inbox(&[pattern("p-1", "2026-11-01", ""), hand("b-1", "2026-11-02", "p-1", ""), hand("b-2", "2026-11-03", "p-1", "")]);
}

#[test]
fn a_pattern_broken_twice_is_required() {
    let t = Tmp::new();
    broken_twice(&t);
    let o = with_controls(&t, "");
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("p-1"));
}

#[test]
fn a_pattern_nothing_broke_is_not_counted() {
    let t = Tmp::new();
    t.write_inbox(&[pattern("p-1", "2026-11-01", "")]);
    let o = with_controls(&t, "");
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "controls-check ok groups=0 repeat=0 lines=0\n");
}

#[test]
fn a_line_for_the_pattern_root_covers_a_pattern_broken_twice() {
    let t = Tmp::new();
    broken_twice(&t);
    let o = with_controls(&t, line("p-1"));
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=1\n");
}

#[test]
fn a_breaks_cycle_refuses() {
    let t = Tmp::new();
    t.write_inbox(&[hand("g-1", "2026-11-02", "g-2", ""), hand("g-2", "2026-11-03", "g-1", "")]);
    let o = with_controls(&t, "");
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), "rank: breaks cycle through g-1\n");
}

#[test]
fn a_member_line_does_not_cover_the_group() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, line("g-1b"));
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("g-1"));
}

#[test]
fn a_line_for_another_group_does_not_cover() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, line("zz-9"));
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("g-1"));
    assert!(!err(&o).contains("zz-9"));
}

#[test]
fn two_missing_groups_are_listed_in_byte_order() {
    let t = Tmp::new();
    t.write_inbox(&[
        hand("g-2", "2026-11-02", "", ""),
        hand("g-2b", "2026-11-09", "g-2", ""),
        hand("g-10", "2026-11-02", "", ""),
        hand("g-10b", "2026-11-09", "g-10", ""),
    ]);
    let o = with_controls(&t, "");
    assert_eq!(err(&o), refused("g-10, g-2"));
}

// ---------------------------------------------------------------- the disagreements with the wrapper

#[test]
fn bad_line_refuses_even_for_a_group_seen_once() {
    let t = Tmp::new();
    t.write_inbox(&[hand("g-1", "2026-11-02", "", "")]);
    let o = with_controls(&t, "not a line\n");
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("controls line 1 is malformed"), "{}", err(&o));
}

#[test]
fn lone_cr_refuses() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, "g-1 | control: x\r| lesson: ISSUE-001\n");
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("controls line 1 has a control character"), "{}", err(&o));
}

#[test]
fn crlf_line_is_read() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, "g-1 | control: x | lesson: ISSUE-001\r\n");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=1\n");
}

#[test]
fn escape_in_line_refuses() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, "g-1 | control: x\u{1b}y | lesson: ISSUE-001\n");
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("has a control character"), "{}", err(&o));
}

#[test]
fn form_feed_in_line_refuses() {
    let t = Tmp::new();
    twice(&t);
    for c in ["\u{0c}", "\u{85}"] {
        let o = with_controls(&t, format!("g-1 | control: x{c}y | lesson: ISSUE-001\n"));
        assert_refused_empty_stdout(&o, 1);
        assert!(err(&o).contains("has a control character"), "{}", err(&o));
    }
}

#[test]
fn refusal_prints_no_line_text() {
    let t = Tmp::new();
    twice(&t);
    let tok = "zzsynthtokenzz";
    for text in [
        format!("g-9 | control: {tok} | lesson: ISSUE-001\n"),
        format!("g-1 | control: {tok} | lesson: ISSUE-002\n"),
        format!("g-1 | control: {tok} | lesson: ISSUE-1\n"),
        format!("{tok} {tok}\n"),
    ] {
        let o = with_controls(&t, text);
        assert_eq!(code(&o), 1);
        assert!(!out(&o).contains(tok) && !err(&o).contains(tok), "{}", err(&o));
    }
}

// ---------------------------------------------------------------- hostile input

#[test]
fn controls_missing() {
    let t = Tmp::new();
    twice(&t);
    let l = lessons(&t);
    let o = check(&t, &t.0.join("nope.txt"), &l);
    assert_refused_empty_stdout(&o, 2);
}

#[test]
fn controls_is_a_folder() {
    let t = Tmp::new();
    twice(&t);
    let l = lessons(&t);
    let d = t.0.join("cdir");
    fs::create_dir_all(&d).unwrap();
    assert_refused_empty_stdout(&check(&t, &d, &l), 2);
}

#[test]
fn controls_over_1_mib() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, vec![b'#'; 1024 * 1024 + 1]);
    assert_refused_empty_stdout(&o, 2);
}

#[test]
fn lessons_dir_missing() {
    let t = Tmp::new();
    twice(&t);
    let c = t.file("controls.txt", "");
    assert_refused_empty_stdout(&check(&t, &c, &t.0.join("nolessons")), 2);
}

#[test]
fn lessons_path_is_a_file() {
    let t = Tmp::new();
    twice(&t);
    let c = t.file("controls.txt", "");
    let f = t.file("lessons-file", "x");
    assert_refused_empty_stdout(&check(&t, &c, &f), 2);
}

#[test]
fn inbox_missing() {
    let t = Tmp::new();
    let o = with_controls(&t, "");
    assert_refused_empty_stdout(&o, 2);
}

#[test]
fn controls_not_utf8() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, [0xffu8, 0xfe, b'\n']);
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("not utf-8"), "{}", err(&o));
}

/// One controls line for g-1 over a lessons folder the caller built.
fn refuse_with_lessons(t: &Tmp, l: &Path, text: &str) -> Output {
    let c = t.file("controls.txt", text);
    check(t, &c, l)
}

fn bare_lessons(t: &Tmp) -> PathBuf {
    let d = t.0.join("lessons");
    fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn lesson_page_is_a_symlink() {
    let t = Tmp::new();
    twice(&t);
    let d = bare_lessons(&t);
    fs::write(d.join("other.md"), "x").unwrap();
    std::os::unix::fs::symlink(d.join("other.md"), d.join("ISSUE-001-x.md")).unwrap();
    let o = refuse_with_lessons(&t, &d, &line("g-1"));
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("no regular file"), "{}", err(&o));
}

#[test]
fn lesson_page_is_a_folder() {
    let t = Tmp::new();
    twice(&t);
    let d = bare_lessons(&t);
    fs::create_dir_all(d.join("ISSUE-001-x.md")).unwrap();
    let o = refuse_with_lessons(&t, &d, &line("g-1"));
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("no regular file"), "{}", err(&o));
}

#[test]
fn lesson_page_lowercase() {
    let t = Tmp::new();
    twice(&t);
    let d = bare_lessons(&t);
    fs::write(d.join("issue-001-x.md"), "x").unwrap();
    let o = refuse_with_lessons(&t, &d, &line("g-1"));
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("no regular file"), "{}", err(&o));
}

#[test]
fn lesson_page_in_a_subfolder() {
    let t = Tmp::new();
    twice(&t);
    let d = bare_lessons(&t);
    fs::create_dir_all(d.join("sub")).unwrap();
    fs::write(d.join("sub").join("ISSUE-001-x.md"), "x").unwrap();
    let o = refuse_with_lessons(&t, &d, &line("g-1"));
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("no regular file"), "{}", err(&o));
}

#[test]
fn two_digit_lesson_number() {
    let t = Tmp::new();
    twice(&t);
    let d = bare_lessons(&t);
    fs::write(d.join("ISSUE-01-x.md"), "x").unwrap();
    let o = refuse_with_lessons(&t, &d, "g-1 | control: x | lesson: ISSUE-01\n");
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("malformed"), "{}", err(&o));
}

#[test]
fn empty_pointer() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, "g-1 | control:  | lesson: ISSUE-001\n");
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).contains("malformed"), "{}", err(&o));
}

#[test]
fn bom_before_first_line() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, format!("\u{feff}{}", line("g-1")));
    assert_refused_empty_stdout(&o, 1);
    assert_eq!(err(&o), refused("g-1"));
}

#[test]
fn inbox_lint_problem() {
    let t = Tmp::new();
    t.write_inbox(&["not a row".to_string()]);
    let o = with_controls(&t, "");
    assert_refused_empty_stdout(&o, 1);
    assert!(err(&o).starts_with("harness: inbox line 1: "), "{}", err(&o));
}

fn args_only(args: &[&str]) -> Output {
    let mut a = vec!["friction", "controls-check"];
    a.extend_from_slice(args);
    run(&a)
}

#[test]
fn option_given_twice() {
    let o = args_only(&["--controls", "zzval1", "--controls", "zzval2"]);
    assert_refused_empty_stdout(&o, 2);
    assert!(err(&o).contains("--controls was given twice"), "{}", err(&o));
    assert!(!err(&o).contains("zzval"));
}

#[test]
fn option_with_no_value() {
    let o = args_only(&["--lessons-dir"]);
    assert_refused_empty_stdout(&o, 2);
    assert!(err(&o).contains("--lessons-dir is missing its value"), "{}", err(&o));
}

#[test]
fn unknown_option() {
    let o = args_only(&["--bogus", "zzval1"]);
    assert_refused_empty_stdout(&o, 2);
    assert!(err(&o).contains("an unknown option"), "{}", err(&o));
    assert!(!err(&o).contains("zzval1") && !err(&o).contains("--bogus"));
}

#[test]
fn only_controls_given() {
    let o = args_only(&["--controls", "zzval1"]);
    assert_refused_empty_stdout(&o, 2);
    assert!(err(&o).contains("--lessons-dir is required"), "{}", err(&o));
    assert!(!err(&o).contains("zzval1"));
}

#[test]
fn only_lessons_dir_given() {
    let o = args_only(&["--lessons-dir", "zzval1"]);
    assert_refused_empty_stdout(&o, 2);
    assert!(err(&o).contains("--controls is required"), "{}", err(&o));
    assert!(!err(&o).contains("zzval1"));
}

// ---------------------------------------------------------------- input that passes

#[test]
fn empty_controls_with_no_repeat_group() {
    let t = Tmp::new();
    t.write_inbox(&[hand("g-1", "2026-11-02", "", ""), hand("g-2", "2026-11-02", "", "")]);
    let o = with_controls(&t, "");
    assert_eq!(code(&o), 0);
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), "controls-check ok groups=2 repeat=0 lines=0\n");
}

#[test]
fn empty_inbox() {
    let t = Tmp::new();
    t.write_inbox(&[]);
    let o = with_controls(&t, "");
    assert_eq!(code(&o), 0);
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), "controls-check ok groups=0 repeat=0 lines=0\n");
}

#[test]
fn comment_and_blank_lines_are_skipped() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, format!("# a comment\n\n   \n\t\n{}", line("g-1")));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=1\n");
}

#[test]
fn duplicate_valid_lines_both_count() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, format!("{}{}", line("g-1"), line("g-1")));
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=2\n");
}

#[test]
fn blanks_and_a_tab_in_the_pointer() {
    let t = Tmp::new();
    twice(&t);
    let o = with_controls(&t, "  \tg-1 | control: a\tguard | lesson: ISSUE-001  \t\n");
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(err(&o), "");
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=1\n");
}

#[test]
fn controls_is_a_symlink_to_a_valid_file() {
    let t = Tmp::new();
    twice(&t);
    let real = t.file("real.txt", line("g-1"));
    let link = t.0.join("link.txt");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let l = lessons(&t);
    let o = check(&t, &link, &l);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "controls-check ok groups=1 repeat=1 lines=1\n");
}

// ---------------------------------------------------------------- usage

#[test]
fn usage_lists_the_new_verb_and_keeps_the_old_ones() {
    for args in [&["friction"][..], &["friction", "no-such-verb"][..]] {
        let o = run(args);
        assert_refused_empty_stdout(&o, 2);
        let e = err(&o);
        assert!(e.contains("friction controls-check --controls <file> --lessons-dir <dir> [--config <file>]"), "{e}");
        assert!(e.contains("friction rank") && e.contains("friction triage"), "{e}");
    }
}
