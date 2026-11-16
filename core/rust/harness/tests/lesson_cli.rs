//! `harness lesson lint`: real binary, synthetic fixtures only (no real ids, names, paths or text).
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lesson-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// A synthetic page that passes every rule (id `ISSUE-101`; the fenced block must not read as headings).
fn good_page() -> String {
    [
        "---",
        "kind: note",
        "type: lesson",
        "id: ISSUE-101",
        "status: validated",
        "updated: 2026-01-02",
        "sources: synthetic log",
        "verified: yes",
        "minimum_proven_tier: T2",
        "controls: [restart, check-log]",
        "match: [svc-a, svc-b]",
        "---",
        "# ISSUE-101: widget queue stalls",
        "",
        "## Issue and symptoms",
        "Queue stalls and widgets stop.",
        "",
        "## Context",
        "A synthetic service in a test rig.",
        "",
        "## Root cause",
        "The counter wrapped around at midnight.",
        "",
        "## Resolution",
        "Bumped the counter width and added a guard.",
        "",
        "```",
        "## Not a heading",
        "# Not a title",
        "```",
        "",
        "## Controls",
        "- restart the service",
        "- check the log",
        "",
        "## Verification",
        "Ran the synthetic load once.",
        "",
        "## Effectiveness",
        "- metric: stall-count",
        "- value: 0",
        "- date: 2026-01-03",
        "- source: synthetic dashboard",
        "",
    ]
    .join("\n")
}

/// One planted bad page per rule word: (rule word, file name, page text).
fn cases() -> Vec<(&'static str, String, String)> {
    let name = |tail: &str| format!("ISSUE-101-{}.md", tail);
    let mut v: Vec<(&'static str, String, String)> = Vec::new();
    let mut add = |word: &'static str, page: String| v.push((word, name(&word.to_lowercase()), page));
    // FRONT-MATTER
    add("FRONT-MATTER", good_page().replacen("---\n", "", 1));
    add("FRONT-MATTER", good_page().replace("verified: yes\n", "verified: yes\nnot a pair\n"));
    // SHARED-FIELD
    add("SHARED-FIELD", good_page().replace("verified: yes\n", ""));
    add("SHARED-FIELD", good_page().replace("updated: 2026-01-02", "updated: 2026-13-02"));
    add("SHARED-FIELD", good_page().replace("verified: yes", "verified: maybe"));
    // TYPE
    add("TYPE", good_page().replace("type: lesson", "type: note"));
    // ID
    add("ID", good_page().replace("id: ISSUE-101", "id: ISSUE-202"));
    add("ID", good_page().replace("id: ISSUE-101", "id: LESSON-101"));
    // STATUS
    add("STATUS", good_page().replace("status: validated", "status: draft"));
    // TIER
    add("TIER", good_page().replace("minimum_proven_tier: T2\n", ""));
    add("TIER", good_page().replace("minimum_proven_tier: T2", "minimum_proven_tier:"));
    add("TIER", good_page().replace("minimum_proven_tier: T2", "minimum_proven_tier: two"));
    // CONTROLS
    add("CONTROLS", good_page().replace("controls: [restart, check-log]", "controls: restart"));
    add("CONTROLS", good_page().replace("controls: [restart, check-log]\n", ""));
    // MATCH
    add("MATCH", good_page().replace("match: [svc-a, svc-b]", "match: []"));
    add("MATCH", good_page().replace("match: [svc-a, svc-b]", "match: [svc-a, ]"));
    // TITLE
    add("TITLE", good_page().replace("# ISSUE-101: widget queue stalls", "## no title here"));
    add("TITLE", good_page().replace("# ISSUE-101: widget queue stalls", "# ISSUE-101: first\n\n# ISSUE-101: second"));
    // SECTIONS
    add("SECTIONS", good_page().replace("## Verification\n", ""));
    add("SECTIONS", good_page().replace("## Context\n", "## Root cause\n"));
    add("SECTIONS", good_page().replace("## Context\n", "## Temp\n").replace("## Root cause\n", "## Context\n").replace("## Temp\n", "## Root cause\n"));
    add("SECTIONS", good_page().replace("## Effectiveness\n", "## Extra\n\nsome text\n\n## Effectiveness\n"));
    add("SECTIONS", good_page().replace("## Context\nA synthetic service in a test rig.\n", "## Context\n"));
    // ROOT-CAUSE
    add("ROOT-CAUSE", good_page().replace("The counter wrapped around at midnight.", "Queue stalls and widgets stop."));
    // EFFECTIVENESS
    add("EFFECTIVENESS", good_page().replace("- value: 0", "- value:"));
    add("EFFECTIVENESS", good_page().replace("- date: 2026-01-03", "- date: 2026-02-31"));
    add("EFFECTIVENESS", good_page().replace("- source: synthetic dashboard\n", ""));
    // VALIDATED-CONTROL
    add("VALIDATED-CONTROL", good_page().replace("controls: [restart, check-log]", "controls: []"));
    add("VALIDATED-CONTROL", good_page().replace("controls: [restart, check-log]", "controls: [ ]"));
    v
}

#[test]
fn good_page_passes_with_the_exact_ok_line() {
    let d = tmp("good");
    let f = d.join("ISSUE-101-pass.md");
    fs::write(&f, good_page()).unwrap();
    let o = run(&["lesson", "lint", f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");

    let second = d.join("ISSUE-303-other.md");
    fs::write(&second, good_page().replace("ISSUE-101", "ISSUE-303")).unwrap();
    let o = run(&["lesson", "lint", f.to_str().unwrap(), second.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=2\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn every_rule_word_is_refused() {
    for (word, name, page) in cases() {
        let d = tmp("bad");
        let f = d.join(&name);
        fs::write(&f, &page).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{}: {}", word, stderr(&o));
        let out = stdout(&o);
        assert!(out.contains(&format!(": {} ", word)), "{} not in {:?}", word, out);
        let _ = fs::remove_dir_all(&d);
    }
}

/// Each refused page names the right rule on the right file name, with the detail that names the problem.
#[test]
fn details_name_the_problem() {
    let check = |page: String, want: &[&str]| {
        let d = tmp("detail");
        fs::write(d.join("ISSUE-101-x.md"), page).unwrap();
        let o = run(&["lesson", "lint", d.join("ISSUE-101-x.md").to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1));
        let out = stdout(&o);
        for w in want {
            assert!(out.contains(w), "{:?} not in {:?}", w, out);
        }
        assert!(out.lines().all(|l| l.starts_with("ISSUE-101-x.md: ")), "{}", out);
        let _ = fs::remove_dir_all(&d);
    };
    check(good_page().replace("## Verification\n", ""), &["SECTIONS the heading Verification is missing"]);
    check(good_page().replace("## Context\n", "## Root cause\n"), &["SECTIONS the heading Root cause appears 2 times"]);
    check(good_page().replace("## Effectiveness\n", "## Extra\n\nsome text\n\n## Effectiveness\n"), &["SECTIONS the heading Extra is not one of the seven"]);
    let swapped = good_page().replace("## Context\n", "## Temp\n").replace("## Root cause\n", "## Context\n").replace("## Temp\n", "## Root cause\n");
    check(swapped, &["SECTIONS the heading Context is out of order"]);
    check(good_page().replace("## Context\nA synthetic service in a test rig.\n", "## Context\n"), &["SECTIONS the heading Context has no text"]);
    check(good_page().replace("match: [svc-a, svc-b]", "match: []"), &["MATCH match is an empty list"]);
    check(good_page().replace("match: [svc-a, svc-b]", "match: [svc-a, ]"), &["MATCH match has an empty token"]);
    check(
        good_page().replace("minimum_proven_tier: T2", "minimum_proven_tier: two"),
        &["TIER minimum_proven_tier two is not one of T1, T2, T3, T4, unproven, unmeasured"],
    );
}

/// The six legal `minimum_proven_tier` values each pass; any other value is refused with the value named.
#[test]
fn tier_values_are_checked() {
    let d = tmp("tier");
    for good in ["T1", "T2", "T3", "T4", "unproven", "unmeasured"] {
        let f = d.join("ISSUE-101-g.md");
        fs::write(&f, good_page().replace("minimum_proven_tier: T2", &format!("minimum_proven_tier: {}", good))).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", good, stdout(&o), stderr(&o));
        assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    }
    for bad in ["two", "t2", "T0", "T5", "\"T2\"", "synthetic-model"] {
        let f = d.join("ISSUE-101-b.md");
        fs::write(&f, good_page().replace("minimum_proven_tier: T2", &format!("minimum_proven_tier: {}", bad))).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{}: {}", bad, stderr(&o));
        let out = stdout(&o);
        assert!(
            out.contains(&format!("TIER minimum_proven_tier {} is not one of T1, T2, T3, T4, unproven, unmeasured", bad)),
            "{}: {}",
            bad,
            out
        );
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn validated_with_no_reusable_control_words_passes() {
    let d = tmp("words");
    let page = good_page().replace("controls: [restart, check-log]", "controls: []").replace("- restart the service\n- check the log\n", "There is no reusable control for this one.\n");
    fs::write(d.join("ISSUE-101-w.md"), page).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn problems_come_in_path_order_and_every_page_is_checked() {
    let d = tmp("order");
    fs::write(d.join("ISSUE-501-a.md"), good_page().replace("ISSUE-101", "ISSUE-501").replace("type: lesson", "type: note")).unwrap();
    fs::write(d.join("ISSUE-502-b.md"), good_page().replace("ISSUE-101", "ISSUE-502").replace("status: validated", "status: draft")).unwrap();
    let late = tmp("late").join("ISSUE-500-z.md");
    fs::write(&late, good_page().replace("ISSUE-101", "ISSUE-500").replace("updated: 2026-01-02", "updated: nope")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap(), late.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1));
    let out = stdout(&o);
    let l: Vec<&str> = out.lines().collect();
    assert_eq!(l.len(), 3, "{}", out);
    assert!(l[0].starts_with("ISSUE-501-a.md: TYPE"), "{}", out);
    assert!(l[1].starts_with("ISSUE-502-b.md: STATUS"), "{}", out);
    assert!(l[2].starts_with("ISSUE-500-z.md: SHARED-FIELD"), "{}", out);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn duplicate_ids_across_pages_are_refused() {
    let d = tmp("dup");
    fs::write(d.join("ISSUE-707-a.md"), good_page().replace("ISSUE-101", "ISSUE-707")).unwrap();
    fs::write(d.join("ISSUE-707-b.md"), good_page().replace("ISSUE-101", "ISSUE-707")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).contains("DUPLICATE-ID"), "{}", stdout(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn directory_lists_only_bad_pages_and_skips_other_files() {
    let d = tmp("dir");
    fs::write(d.join("ISSUE-404-good.md"), good_page().replace("ISSUE-101", "ISSUE-404")).unwrap();
    fs::write(
        d.join("ISSUE-405-bad.md"),
        good_page().replace("ISSUE-101", "ISSUE-405").replace("status: validated", "status: draft"),
    )
    .unwrap();
    fs::write(d.join("README.md"), "not a lesson page").unwrap();
    fs::create_dir_all(d.join("nested")).unwrap();
    fs::write(d.join("nested/ISSUE-406-deep.md"), good_page().replace("ISSUE-101", "ISSUE-406").replace("type: lesson", "type: note")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let out = stdout(&o);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 1, "one problem line expected, got {:?}", out);
    assert!(lines[0].contains("405") && lines[0].contains("STATUS"), "{}", out);
    assert!(!out.contains("406") && !out.contains("README"), "{}", out);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn missing_path_and_empty_directory_exit_2() {
    let o = run(&["lesson", "lint", "/nonexistent-dir-xyz/nope.md"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(!stderr(&o).is_empty());

    let d = tmp("empty");
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(!stderr(&o).is_empty());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_file_that_is_not_utf8_exits_2() {
    let d = tmp("utf8");
    let f = d.join("ISSUE-808-bin.md");
    fs::write(&f, b"---\nid: ISSUE-808\n\xff\xfe\n---\n").unwrap();
    let o = run(&["lesson", "lint", f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(!stderr(&o).is_empty());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn controls_with_an_empty_token_is_refused() {
    let d = tmp("ctrlempty");
    for bad in ["[,]", "[restart,,check-log]", "[restart, ]"] {
        let f = d.join("ISSUE-101-e.md");
        fs::write(&f, good_page().replace("controls: [restart, check-log]", &format!("controls: {}", bad))).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{}: {}", bad, stderr(&o));
        assert!(stdout(&o).contains("CONTROLS controls has an empty token"), "{}: {}", bad, stdout(&o));
    }
    // an empty list ([] or [ ]) with the no-reusable-control words still passes
    for good in ["[]", "[ ]"] {
        let f = d.join("ISSUE-101-g.md");
        fs::write(
            &f,
            good_page()
                .replace("controls: [restart, check-log]", &format!("controls: {}", good))
                .replace("- restart the service\n- check the log\n", "There is no reusable control for this one.\n"),
        )
        .unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", good, stdout(&o), stderr(&o));
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_section_of_only_fence_lines_is_refused() {
    let d = tmp("fence");
    // the only non-blank lines of the section are the fence lines themselves
    let bad = d.join("ISSUE-101-f.md");
    fs::write(&bad, good_page().replace("A synthetic service in a test rig.", "```\n```")).unwrap();
    let o = run(&["lesson", "lint", bad.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).contains("SECTIONS the heading Context has no text"), "{}", stdout(&o));
    // a fenced block with at least one non-blank line inside it still counts as text
    let good = d.join("ISSUE-101-t.md");
    fs::write(&good, good_page().replace("A synthetic service in a test rig.", "```\na synthetic rig\n```")).unwrap();
    let o = run(&["lesson", "lint", good.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_same_page_reached_twice_is_checked_once() {
    let d = tmp("twice");
    let f = d.join("ISSUE-101-t.md");
    fs::write(&f, good_page()).unwrap();
    let o = run(&["lesson", "lint", f.to_str().unwrap(), f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    // the same page through the file and through the directory that holds it
    let o = run(&["lesson", "lint", f.to_str().unwrap(), d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    // two different files with the same id are still refused
    let g = d.join("ISSUE-101-u.md");
    fs::write(&g, good_page()).unwrap();
    let o = run(&["lesson", "lint", f.to_str().unwrap(), g.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).contains("DUPLICATE-ID"), "{}", stdout(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn an_unreadable_page_shaped_entry_in_a_directory_exits_2() {
    // a broken symbolic link with a page-shaped name is no longer skipped silently
    let d = tmp("broken");
    fs::write(d.join("ISSUE-606-good.md"), good_page().replace("ISSUE-101", "ISSUE-606")).unwrap();
    std::os::unix::fs::symlink(d.join("nowhere-607.md"), d.join("ISSUE-607-broken.md")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("ISSUE-607-broken.md"), "{}", stderr(&o));
    // a sub-folder with such a name is still skipped
    let d2 = tmp("folder");
    fs::write(d2.join("ISSUE-608-good.md"), good_page().replace("ISSUE-101", "ISSUE-608")).unwrap();
    fs::create_dir_all(d2.join("ISSUE-609-dir.md")).unwrap();
    let o = run(&["lesson", "lint", d2.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&d2);
}

/// A blank other than an ASCII space or tab around the tier value is not trimmed.
#[test]
fn non_space_blanks_around_the_tier_are_refused() {
    let d = tmp("blank");
    for bad in ["T2\u{00a0}", "\u{00a0}T2", "T2\u{2003}"] {
        let f = d.join("ISSUE-101-b.md");
        fs::write(&f, good_page().replace("minimum_proven_tier: T2", &format!("minimum_proven_tier: {}", bad))).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{}: {}{}", bad, stdout(&o), stderr(&o));
        assert!(stdout(&o).contains("TIER minimum_proven_tier"), "{}: {}", bad, stdout(&o));
    }
    for good in [" T2", "\tT2\t", " T2 "] {
        let f = d.join("ISSUE-101-g.md");
        fs::write(&f, good_page().replace("minimum_proven_tier: T2", &format!("minimum_proven_tier: {}", good))).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(0), "{}: {}{}", good, stdout(&o), stderr(&o));
        assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    }
    let _ = fs::remove_dir_all(&d);
}

/// A front matter key written twice is refused, whichever line holds the good value.
#[test]
fn a_repeated_front_matter_key_is_refused() {
    let d = tmp("repeat");
    for (tag, two) in [
        ("gg", "status: validated\nstatus: validated\n"),
        ("bg", "status: draft\nstatus: validated\n"),
        ("gb", "status: validated\nstatus: draft\n"),
    ] {
        let f = d.join(format!("ISSUE-101-{}.md", tag));
        fs::write(&f, good_page().replace("status: validated\n", two)).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{}: {}{}", tag, stdout(&o), stderr(&o));
        assert!(stdout(&o).contains("FRONT-MATTER line 6 repeats the key status"), "{}: {}", tag, stdout(&o));
    }
    let _ = fs::remove_dir_all(&d);
}

/// A key that is, or holds, a blank or a control character is not key: value.
#[test]
fn a_key_with_a_blank_or_control_character_is_refused() {
    let d = tmp("key");
    // an added line whose key is only a blank, holds a control character, or holds a space
    for bad in ["\u{a0}: junk", "k\u{1b}ey: junk", "bad key: junk"] {
        let f = d.join("ISSUE-101-k.md");
        fs::write(&f, good_page().replace("sources: synthetic log\n", &format!("sources: synthetic log\n{}\n", bad))).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{}: {}{}", bad, stdout(&o), stderr(&o));
        let out = stdout(&o);
        assert_eq!(out.lines().count(), 1, "{}: {}", bad, out);
        assert!(out.contains("FRONT-MATTER line 8 is not key: value"), "{}: {}", bad, out);
    }
    // a NBSP in front of a repeated key name is not that key
    let f = d.join("ISSUE-101-r.md");
    fs::write(&f, good_page().replace("status: validated\n", "status: validated\n\u{a0}status: draft\n")).unwrap();
    let o = run(&["lesson", "lint", f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}{}", stdout(&o), stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("FRONT-MATTER line 6 is not key: value"), "{}", out);
    assert!(!out.contains("repeats"), "{}", out);
    let _ = fs::remove_dir_all(&d);
}

/// Control characters planted in a value are printed escaped; stdout holds none of them.
#[test]
fn control_characters_in_a_value_are_escaped() {
    let d = tmp("ctrl");
    let f = d.join("ISSUE-101-c.md");
    fs::write(&f, good_page().replace("status: validated", "status: valid\u{1b}\u{7}ated\u{9b}")).unwrap();
    let o = run(&["lesson", "lint", f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("STATUS status valid\\u{1b}\\u{7}ated\\u{9b} is not"), "{}", out);
    assert!(out.chars().all(|c| !c.is_control() || c == '\n'), "{:?}", out);
    let _ = fs::remove_dir_all(&d);
}

/// A blank other than a plain space (U+00A0, U+2028) is escaped in a problem line too.
#[test]
fn blanks_that_are_not_control_characters_are_escaped() {
    let d = tmp("blankesc");
    for (bad, escd) in [("valid\u{a0}ated", "valid\\u{a0}ated"), ("valid\u{2028}ated", "valid\\u{2028}ated")] {
        let f = d.join("ISSUE-101-b.md");
        fs::write(&f, good_page().replace("status: validated", &format!("status: {}", bad))).unwrap();
        let o = run(&["lesson", "lint", f.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{}: {}{}", bad, stdout(&o), stderr(&o));
        let out = stdout(&o);
        assert!(out.contains(&format!("STATUS status {} is not", escd)), "{}: {}", bad, out);
        assert!(!out.contains(bad), "{}: {}", bad, out);
    }
    let _ = fs::remove_dir_all(&d);
}

/// A control character in a path argument is escaped in the stderr message.
#[test]
fn a_control_character_in_a_path_is_escaped_on_stderr() {
    let d = tmp("escpath");
    let f = d.join("no\u{1b}pe.md");
    let o = run(&["lesson", "lint", f.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    let err = stderr(&o);
    assert!(err.contains("\\u{1b}"), "{}", err);
    assert!(!err.contains('\u{1b}'), "{}", err);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn usage_errors_print_the_usage_text() {
    for args in [vec!["lesson"], vec!["lesson", "frobnicate"], vec!["lesson", "lint"]] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{:?}", args);
        assert!(stdout(&o).is_empty(), "{:?}", args);
        assert!(stderr(&o).contains("lesson lint <path>"), "{:?}: {}", args, stderr(&o));
    }
    // The top-level usage names the verb too.
    let o = run(&[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("lesson lint"), "{}", stderr(&o));
}

// ---- harness lesson lookup ----

fn ticket_json(repo: &str, title: &str, criteria: &[&str], objective: &str, group: &str) -> String {
    let crit: Vec<String> = criteria.iter().map(|c| format!("\"{}\"", c)).collect();
    format!(
        "{{\"id\":\"T-1\",\"repo_id\":\"{}\",\"title\":\"{}\",\"group\":\"{}\",\"size\":\"S\",\"objective\":\"{}\",\"acceptance_criteria\":[{}],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}",
        repo,
        title,
        group,
        objective,
        crit.join(",")
    )
}

fn default_ticket(title: &str) -> String {
    ticket_json("example", title, &["checks/hello.sh passes"], "obj", "grp")
}

/// A scratch folder with `lessons/`, `ticket.json`, `scope.txt` and the given pages.
fn setup(ticket: &[u8], scope: &[u8], pages: &[(&str, &str)]) -> PathBuf {
    let d = tmp("look");
    fs::create_dir_all(d.join("lessons")).unwrap();
    fs::write(d.join("ticket.json"), ticket).unwrap();
    fs::write(d.join("scope.txt"), scope).unwrap();
    for (n, t) in pages {
        fs::write(d.join("lessons").join(n), t).unwrap();
    }
    d
}

fn look(d: &std::path::Path) -> Output {
    let (l, t, s) = (d.join("lessons"), d.join("ticket.json"), d.join("scope.txt"));
    run(&["lesson", "lookup", l.to_str().unwrap(), "--ticket", t.to_str().unwrap(), "--scope-file", s.to_str().unwrap()])
}

fn fm(body: &str) -> String {
    format!("---\n{}\n---\n", body)
}

/// One page `match: [<token>]` under a ticket with the given title: does it match?
fn hit_in(title: &str, token: &str) -> bool {
    hit_with(&default_ticket(title), token)
}

/// Same, with a title only: the one criterion is `zzz`.
fn hit_title(title: &str, token: &str) -> bool {
    hit_with(&ticket_json("example", title, &["zzz"], "obj", "grp"), token)
}

fn hit_with(ticket: &str, token: &str) -> bool {
    let d = setup(ticket.as_bytes(), b"src/\n",&[("ISSUE-001-a.md", &fm(&format!("match: [{}]", token)))]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0), "{}: {}", token, stderr(&o));
    let out = stdout(&o);
    let _ = fs::remove_dir_all(&d);
    match out.as_str() {
        "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n" => true,
        "lessons: scanned=1 matched=none\n" => false,
        other => panic!("{}: {:?}", token, other),
    }
}

fn hit(token: &str) -> bool {
    hit_in("Say hello", token)
}

#[test]
fn heading_line_is_printed_trimmed() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a-b.md", &(fm("match: [example]") + "text\n# \u{a0} My head \u{3000}\n"))]);
    let o = look(&d);
    assert_eq!(stdout(&o), "lesson: ISSUE-001 My head ISSUE-001-a-b.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn heading_comes_from_the_name_when_there_is_no_heading_line() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a-b-c.md", &(fm("match: [example]") + "## sub\n#nospace\n"))]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 a b c ISSUE-001-a-b-c.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn heading_of_only_hash_and_blank_comes_from_the_name() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-x-y.md", &(fm("match: [example]") + "# \n# later\n"))]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 x y ISSUE-001-x-y.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn the_first_heading_line_wins() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &(fm("match: [example]") + "# first\n# second\n"))]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 first ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn a_heading_line_inside_a_code_fence_is_the_heading() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &(fm("match: [example]") + "```\n# fenced\n```\n# real\n"))]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 fenced ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn a_hash_line_in_the_front_matter_refuses_the_page() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]\n# head"))]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-a.md: FRONT-MATTER line 3 is not key: value\n");
}

#[test]
fn a_ticket_that_is_not_utf8_is_refused() {
    let d = setup(b"\xff\xfe{}", b"src/\n", &[("ISSUE-001-a.md", &fm("match: [x]"))]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).starts_with("harness: ticket refused: ticket is not utf-8"), "{}", stderr(&o));
}

#[test]
fn a_ticket_over_the_reader_limit_is_refused() {
    let big = vec![b' '; 1024 * 1024 + 8];
    let d = setup(&big, b"src/\n", &[("ISSUE-001-a.md", &fm("match: [x]"))]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).starts_with("harness: ticket refused: ticket is over 1 MiB"), "{}", stderr(&o));
}

#[test]
fn a_ticket_file_that_cannot_be_read_exits_2() {
    let d = setup(b"", b"src/\n", &[("ISSUE-001-a.md", &fm("match: [x]"))]);
    fs::remove_file(d.join("ticket.json")).unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).starts_with("harness: error: "), "{}", stderr(&o));
}

#[test]
fn objective_and_group_are_not_looked_at() {
    let t = ticket_json("example", "Say hello", &["checks/hello.sh passes"], "tokenword", "tokenword");
    let d = setup(t.as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [tokenword]"))]);
    assert_eq!(stdout(&look(&d)), "lessons: scanned=1 matched=none\n");
}

#[test]
fn a_bad_scope_file_exits_2() {
    let page = fm("match: [x]");
    for (tag, bytes) in [("empty", &b""[..]), ("not utf8", b"\xff\n"), ("blank line", b"src/\n   \n"), ("dotdot", b"a/../b\n")] {
        let d = setup(default_ticket("t").as_bytes(), bytes, &[("ISSUE-001-a.md", &page)]);
        let o = look(&d);
        assert_eq!(o.status.code(), Some(2), "{}", tag);
        assert!(stdout(&o).is_empty(), "{}", tag);
        assert!(stderr(&o).starts_with("harness: error: "), "{}: {}", tag, stderr(&o));
    }
    let d = setup(default_ticket("t").as_bytes(), b"", &[("ISSUE-001-a.md", &page)]);
    fs::remove_file(d.join("scope.txt")).unwrap();
    assert_eq!(look(&d).status.code(), Some(2));
}

#[test]
fn the_scope_reader_message_is_shown() {
    let d = setup(default_ticket("t").as_bytes(), b"   \n", &[("ISSUE-001-a.md", &fm("match: [x]"))]);
    assert!(stderr(&look(&d)).contains("a scope line holds only whitespace"));
}

#[test]
fn other_entries_are_skipped_and_not_counted() {
    let page = fm("match: [example]");
    let d = setup(
        default_ticket("t").as_bytes(),
        b"src/\n",
        &[("ISSUE-001-a.md", &page), ("README.md", &page), ("notes.md", &page)],
    );
    fs::create_dir_all(d.join("lessons/tests")).unwrap();
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn two_pages_with_one_id_are_both_listed() {
    let page = fm("match: [example]");
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-b.md", &page), ("ISSUE-001-a.md", &page)]);
    assert_eq!(
        stdout(&look(&d)),
        "lesson: ISSUE-001 a ISSUE-001-a.md\nlesson: ISSUE-001 b ISSUE-001-b.md\nlessons: scanned=2 matched=ISSUE-001,ISSUE-001\n"
    );
}

#[test]
fn a_missing_lessons_folder_exits_2() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[]);
    fs::remove_dir_all(d.join("lessons")).unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("does not exist"), "{}", stderr(&o));
}

#[test]
fn a_file_given_as_the_lessons_folder_exits_2() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[]);
    fs::remove_dir_all(d.join("lessons")).unwrap();
    fs::write(d.join("lessons"), "x").unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("is a file"), "{}", stderr(&o));
}

#[test]
fn a_page_that_is_not_utf8_exits_2() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[]);
    fs::write(d.join("lessons/ISSUE-001-a.md"), b"---\n\xff\n---\n").unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("ISSUE-001-a.md is not valid UTF-8"), "{}", stderr(&o));
}

#[test]
fn a_folder_with_no_page_exits_2() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("README.md", "x")]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).contains("no lesson page"), "{}", stderr(&o));
}

#[test]
fn a_control_character_in_a_file_name_is_escaped() {
    let page = fm("match: [example]");
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a\u{1b}b.md", &page)]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(stdout(&o), "lesson: ISSUE-001 a\\u{1b}b ISSUE-001-a\\u{1b}b.md\nlessons: scanned=1 matched=ISSUE-001\n");
    fs::write(d.join("lessons/ISSUE-001-a\u{1b}b.md"), "no front matter\n").unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-a\\u{1b}b.md: FRONT-MATTER line 1 is not ---\n");
    // a newline in the name is a page too
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a\nb.md", &page)]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 a\\u{a}b ISSUE-001-a\\u{a}b.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn a_page_whose_line_1_is_not_the_front_matter_start_is_refused() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", "match: [example]\n---\n")]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-a.md: FRONT-MATTER line 1 is not ---\n");
}

#[test]
fn a_repeated_match_key_is_refused() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [nothing]\nmatch: [example]"))]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-a.md: FRONT-MATTER line 3 repeats the key match\n");
}

#[test]
fn a_page_with_no_match_key_is_counted_and_matches_nothing() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("title: example"))]);
    assert_eq!(stdout(&look(&d)), "lessons: scanned=1 matched=none\n");
}

#[test]
fn only_the_front_matter_rules_are_checked() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", "---\nmatch: [example]\n---\n")]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn an_empty_token_after_the_quotes_are_removed_is_refused() {
    for m in ["[\"\"]", "[example, '']"] {
        let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm(&format!("match: {}", m)))]);
        let o = look(&d);
        assert_eq!(o.status.code(), Some(1), "{}", m);
        assert!(stdout(&o).is_empty());
        assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-a.md: MATCH match has a token with no text between its quotes\n");
    }
}

#[test]
fn word_boundary_hello_matches_the_title_hellox_hello() {
    assert!(hit_title("hellox hello", "hello"));
    assert!(!hit_title("hellox", "hello"));
}

#[test]
fn word_matches_overlapping_starts() {
    assert!(hit_title("xab ab ab", "ab ab"));
}

#[test]
fn a_letter_before_the_token_is_a_word_character() {
    assert!(!hit_title("\u{e9}hello", "hello"));
    assert!(hit_title("-hello-", "hello"));
}

#[test]
fn letter_case_is_ignored_beyond_ascii() {
    assert!(hit_title("un \u{e9}t\u{e9}", "\u{c9}T\u{c9}"));
}

#[test]
fn token_hell_does_not_match() {
    assert!(!hit("hell"));
}
#[test]
fn token_quoted_upper_hello_matches() {
    assert!(hit("\"HELLO\""));
}
#[test]
fn token_hello_sh_matches() {
    assert!(hit("hello.sh"));
}
#[test]
fn token_sr_does_not_match() {
    assert!(!hit("sr"));
}
#[test]
fn token_upper_src_does_not_match() {
    assert!(!hit("SRC"));
}
#[test]
fn token_capital_example_does_not_match() {
    assert!(!hit("Example"));
    assert!(hit("example"));
}
#[test]
fn token_src_matches_by_path() {
    assert!(hit("src"));
}
#[test]
fn token_src_lib_does_not_match() {
    assert!(!hit("src/lib"));
}
#[test]
fn token_slash_does_not_match() {
    assert!(!hit("/"));
}
#[test]
fn one_letter_token_s_needs_a_word_of_its_own() {
    assert!(!hit("s"));
    assert!(hit_in("Say s hello", "s"));
}
#[test]
fn token_say_hello_matches_as_one_phrase() {
    assert!(hit("say hello"));
}
#[test]
fn a_match_line_below_the_closing_marker_is_not_read() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", "---\ntitle: x\n---\nmatch: [example]\n")]);
    assert_eq!(stdout(&look(&d)), "lessons: scanned=1 matched=none\n");
}
#[test]
fn an_indented_match_line_is_read() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("  match: [example]"))]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n");
}
#[test]
fn a_match_key_with_a_blank_before_the_colon_is_read() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match : [example]"))]);
    assert_eq!(stdout(&look(&d)), "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn the_worked_case() {
    let pages = [
        ("ISSUE-901-alpha.md", fm("match: [nosuch-repo, example]")),
        ("ISSUE-902-beta.md", fm("match: [src/]") + "# ISSUE-902: beta page\n"),
        ("ISSUE-903-gamma.md", fm("match: [\"HELLO\"]")),
        ("ISSUE-904-delta.md", fm("match: [hell]")),
        ("ISSUE-906-zeta.md", fm("match: [sr, che, other-repo]")),
        ("ISSUE-907-eta.md", "---\ntitle: x\n---\nmatch: [example]\n".to_string()),
        ("ISSUE-908-theta.md", fm("match: [passes]")),
        ("ISSUE-909-iota-two.md", fm("match: [example]")),
        ("notes.md", fm("match: [example]")),
        ("README.md", "readme\n".to_string()),
    ];
    let refs: Vec<(&str, &str)> = pages.iter().map(|(n, t)| (*n, t.as_str())).collect();
    let d = setup(default_ticket("Say hello").as_bytes(), b"src/\n", &refs);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "lesson: ISSUE-901 alpha ISSUE-901-alpha.md\nlesson: ISSUE-902 ISSUE-902: beta page ISSUE-902-beta.md\nlesson: ISSUE-903 gamma ISSUE-903-gamma.md\nlesson: ISSUE-908 theta ISSUE-908-theta.md\nlesson: ISSUE-909 iota two ISSUE-909-iota-two.md\nlessons: scanned=8 matched=ISSUE-901,ISSUE-902,ISSUE-903,ISSUE-908,ISSUE-909\n"
    );
    assert!(stderr(&o).is_empty());
    fs::write(d.join("ticket.json"), ticket_json("example", "Nothing here", &["zzz"], "o", "g")).unwrap();
    fs::write(d.join("scope.txt"), "docs/\n").unwrap();
    let o = look(&d);
    assert_eq!(
        stdout(&o),
        "lesson: ISSUE-901 alpha ISSUE-901-alpha.md\nlesson: ISSUE-909 iota two ISSUE-909-iota-two.md\nlessons: scanned=8 matched=ISSUE-901,ISSUE-909\n"
    );
}

#[test]
fn exit_2_wins_over_exit_1() {
    let no_fm = "no front matter\n";
    // a bad page and a missing scope file
    let d = setup(default_ticket("t").as_bytes(), b"", &[("ISSUE-001-a.md", no_fm)]);
    fs::remove_file(d.join("scope.txt")).unwrap();
    // a ticket the reader refuses and a missing scope file
    let e = setup(b"{}", b"", &[("ISSUE-001-a.md", &fm("match: [x]"))]);
    fs::remove_file(e.join("scope.txt")).unwrap();
    // a page with line 1 not --- and a page that is not UTF-8
    let f = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", no_fm)]);
    fs::write(f.join("lessons/ISSUE-002-b.md"), b"\xff\xfe").unwrap();
    for (tag, dir) in [("page+scope", d), ("ticket+scope", e), ("page+utf8", f)] {
        let o = look(&dir);
        assert_eq!(o.status.code(), Some(2), "{}", tag);
        assert!(stdout(&o).is_empty(), "{}", tag);
        let err = stderr(&o);
        assert!(err.starts_with("harness: error: ") && !err.contains("refused"), "{}: {}", tag, err);
    }
}

#[test]
fn lookup_usage_errors_exit_2() {
    let cases: [(&[&str], &str); 8] = [
        (&["--ticket", "t", "--scope-file", "s"], "usage: harness lesson"),
        (&["a", "b", "--ticket", "t", "--scope-file", "s"], "folder"),
        (&["a", "--scope-file", "s"], "--ticket"),
        (&["a", "--ticket", "t"], "--scope-file"),
        (&["a", "--ticket", "t", "--ticket", "u", "--scope-file", "s"], "--ticket given twice"),
        (&["a", "--ticket", "t", "--scope-file", "s", "--scope-file", "s"], "--scope-file given twice"),
        (&["a", "--ticket", "t", "--scope-file"], "--scope-file needs a value"),
        (&["a", "--ticket", "t", "--scope-file", "s", "--bogus", "x"], "--bogus"),
    ];
    for (args, want) in cases {
        let mut full = vec!["lesson", "lookup"];
        full.extend_from_slice(args);
        let o = run(&full);
        assert_eq!(o.status.code(), Some(2), "{:?}", args);
        assert!(stdout(&o).is_empty(), "{:?}", args);
        assert!(stderr(&o).contains(want), "{:?}: {}", args, stderr(&o));
    }
}

#[test]
fn usage_names_both_verbs() {
    let o = run(&["lesson"]);
    let err = stderr(&o);
    assert!(err.starts_with("usage: harness lesson"), "{}", err);
    assert!(err.contains("lesson lint <path>") && err.contains("lesson lookup <lessons-dir>"), "{}", err);
    let o = run(&[]);
    let err = stderr(&o);
    assert!(err.contains("lesson lint") && err.contains("lesson lookup") && err.contains("lane claim|release|list|admission-check"), "{}", err);
}

// ---- lookup refusals: walk ----

fn link(target: &std::path::Path, at: &std::path::Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

fn mkfifo(p: &std::path::Path) {
    let st = Command::new("mkfifo").arg(p).status().unwrap();
    assert!(st.success());
}

/// `look`, but the child is killed after 10 seconds and the test fails (a FIFO must not be opened).
fn look_timed(d: &std::path::Path) -> Output {
    let (l, t, s) = (d.join("lessons"), d.join("ticket.json"), d.join("scope.txt"));
    let mut c = Command::new(BIN)
        .args(["lesson", "lookup", l.to_str().unwrap(), "--ticket", t.to_str().unwrap(), "--scope-file", s.to_str().unwrap()])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while c.try_wait().unwrap().is_none() {
        if start.elapsed() > std::time::Duration::from_secs(10) {
            let _ = c.kill();
            let _ = c.wait();
            panic!("lookup did not finish in 10 seconds");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    c.wait_with_output().unwrap()
}

/// A folder with the good page ISSUE-001-a.md; `make` adds the bad entry; exit 2, stdout empty, stderr is `line`.
fn refused_entry(make: impl FnOnce(&std::path::Path), line: &str) {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
    make(&d);
    let o = look_timed(&d);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert!(stdout(&o).is_empty(), "{}", stdout(&o));
    assert_eq!(stderr(&o), format!("{}\n", line));
    let _ = fs::remove_dir_all(&d);
}

const GOOD_OUT: &str = "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n";

fn digit_line(n: &str) -> String {
    format!("harness: error: the entry {} does not have exactly 3 digits after ISSUE-", n)
}

#[test]
fn a_link_to_a_page_file_is_refused() {
    refused_entry(
        |d| {
            fs::write(d.join("outside.md"), fm("match: [example]")).unwrap();
            link(&d.join("outside.md"), &d.join("lessons/ISSUE-002-b.md"));
        },
        "harness: error: the entry ISSUE-002-b.md is a symbolic link",
    );
}

#[test]
fn a_dangling_link_is_refused() {
    refused_entry(|d| link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-b.md")), "harness: error: the entry ISSUE-002-b.md is a symbolic link");
}

#[test]
fn a_link_to_a_folder_is_refused() {
    refused_entry(
        |d| {
            fs::create_dir_all(d.join("sub")).unwrap();
            link(&d.join("sub"), &d.join("lessons/ISSUE-002-b.md"));
        },
        "harness: error: the entry ISSUE-002-b.md is a symbolic link",
    );
}

#[test]
fn a_name_with_one_digit_is_refused() {
    refused_entry(|d| fs::write(d.join("lessons/ISSUE-1-b.md"), "x").unwrap(), &digit_line("ISSUE-1-b.md"));
}

#[test]
fn a_name_with_two_digits_is_refused() {
    refused_entry(|d| fs::write(d.join("lessons/ISSUE-02-b.md"), "x").unwrap(), &digit_line("ISSUE-02-b.md"));
}

#[test]
fn a_name_with_four_digits_is_refused() {
    refused_entry(|d| fs::write(d.join("lessons/ISSUE-0003-c.md"), "x").unwrap(), &digit_line("ISSUE-0003-c.md"));
}

#[test]
fn a_fifo_is_refused_without_a_hang() {
    refused_entry(|d| mkfifo(&d.join("lessons/ISSUE-002-b.md")), "harness: error: the entry ISSUE-002-b.md is not a regular file");
}

#[test]
fn a_control_character_in_a_bad_entry_name_is_escaped() {
    refused_entry(
        |d| link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-a\u{1b}b.md")),
        "harness: error: the entry ISSUE-002-a\\u{1b}b.md is a symbolic link",
    );
}

#[test]
fn a_newline_in_a_bad_entry_name_is_escaped() {
    refused_entry(
        |d| link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-a\nb.md")),
        "harness: error: the entry ISSUE-002-a\\u{a}b.md is a symbolic link",
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_name_that_is_not_utf8_is_refused() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;
    refused_entry(
        |d| fs::write(d.join("lessons").join(OsStr::from_bytes(b"ISSUE-002-\xff.md")), "x").unwrap(),
        "harness: error: the entry ISSUE-002-\u{fffd}.md has a name that is not valid UTF-8",
    );
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
    fs::write(d.join("lessons").join(OsStr::from_bytes(b"\xff.md")), "x").unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), GOOD_OUT);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_link_with_four_digits_gives_the_digit_line() {
    refused_entry(|d| link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-0002-b.md")), &digit_line("ISSUE-0002-b.md"));
}

#[test]
fn a_link_to_a_fifo_is_refused_without_a_hang() {
    refused_entry(
        |d| {
            mkfifo(&d.join("fifo"));
            link(&d.join("fifo"), &d.join("lessons/ISSUE-002-b.md"));
        },
        "harness: error: the entry ISSUE-002-b.md is a symbolic link",
    );
}

#[test]
fn a_link_loop_is_refused() {
    refused_entry(
        |d| link(std::path::Path::new("ISSUE-002-b.md"), &d.join("lessons/ISSUE-002-b.md")),
        "harness: error: the entry ISSUE-002-b.md is a symbolic link",
    );
}

#[test]
fn a_bad_entry_next_to_a_page_that_is_not_utf8_gives_only_the_entry_line() {
    refused_entry(
        |d| {
            fs::write(d.join("lessons/ISSUE-003-c.md"), b"---\n\xff\n---\n").unwrap();
            link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-b.md"));
        },
        "harness: error: the entry ISSUE-002-b.md is a symbolic link",
    );
}

#[test]
fn a_page_named_folder_is_skipped() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
    fs::create_dir_all(d.join("lessons/ISSUE-002-dir.md")).unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), GOOD_OUT);
}

#[test]
fn a_folder_with_a_two_digit_name_is_skipped() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
    fs::create_dir_all(d.join("lessons/ISSUE-02-x.md")).unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), GOOD_OUT);
}

#[test]
fn names_that_are_not_page_shaped_stay_skipped() {
    // one folder per name: a case-insensitive file system would merge issue-001-x.md and ISSUE-001-x.MD
    for n in ["README.md", "notes.md", "ISSUE-001.md", "ISSUE-001-a.txt", "ISSUE-abc-x.md", "issue-001-x.md", "ISSUE-001-x.MD", ".ISSUE-001-x.md"] {
        let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
        link(&d.join("nowhere.md"), &d.join("lessons").join(n));
        let o = look(&d);
        assert_eq!(o.status.code(), Some(0), "{}: {}", n, stderr(&o));
        assert_eq!(stdout(&o), GOOD_OUT, "{}", n);
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn several_bad_entries_come_in_name_order() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-003-d.md", &fm("match: [example]"))]);
    link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-001-a.md"));
    fs::write(d.join("lessons/ISSUE-0001-b.md"), "x").unwrap();
    mkfifo(&d.join("lessons/ISSUE-002-c.md"));
    let o = look_timed(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert_eq!(
        stderr(&o),
        format!(
            "{}\nharness: error: the entry ISSUE-001-a.md is a symbolic link\nharness: error: the entry ISSUE-002-c.md is not a regular file\n",
            digit_line("ISSUE-0001-b.md")
        )
    );
}

#[test]
fn bad_entries_are_sorted_not_given_in_folder_order() {
    // APFS lists these names out of byte order (ISSUE-005-f.md second), so a missing sort goes red on macOS too
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-003-d.md", &fm("match: [example]"))]);
    let names = ["ISSUE-005-f.md", "ISSUE-004-e.md", "ISSUE-002-c.md", "ISSUE-001-a.md", "ISSUE-0001-b.md"];
    for n in names {
        link(&d.join("nowhere.md"), &d.join("lessons").join(n));
    }
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    let mut want: Vec<&str> = names.to_vec();
    want.sort();
    let want: String = want
        .iter()
        .map(|n| if n.starts_with("ISSUE-0001") { format!("{}\n", digit_line(n)) } else { format!("harness: error: the entry {} is a symbolic link\n", n) })
        .collect();
    assert_eq!(stderr(&o), want);
}

#[test]
fn bad_entries_sort_by_the_raw_name_not_the_escaped_text() {
    // raw: the byte 0x0a sorts before '0'; escaped: the text \u{a} sorts after '0'
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
    link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-a0.md"));
    link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-a\nb.md"));
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert_eq!(
        stderr(&o),
        "harness: error: the entry ISSUE-002-a\\u{a}b.md is a symbolic link\nharness: error: the entry ISSUE-002-a0.md is a symbolic link\n"
    );
}

#[test]
fn only_bad_entries_give_no_no_page_line() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[]);
    link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-001-a.md"));
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: the entry ISSUE-001-a.md is a symbolic link\n");
}

#[test]
fn a_bad_entry_wins_over_a_bad_front_matter() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", "no front matter\n")]);
    link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-b.md"));
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: error: the entry ISSUE-002-b.md is a symbolic link\n");
}

#[test]
fn a_missing_scope_file_wins_over_a_bad_entry() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
    link(&d.join("nowhere.md"), &d.join("lessons/ISSUE-002-b.md"));
    fs::remove_file(d.join("scope.txt")).unwrap();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(2));
    assert!(stdout(&o).is_empty());
    assert!(stderr(&o).starts_with("harness: error: "), "{}", stderr(&o));
    assert!(!stderr(&o).contains("ISSUE-002-b"), "{}", stderr(&o));
}

// ---- lookup refusals: front matter ----

/// One page ISSUE-001-a.md: exit 1, stdout empty, stderr is exactly one refusal line per detail.
fn refused_page(page: &str, details: &[&str]) {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", page)]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).is_empty(), "{}", stdout(&o));
    let want: String = details.iter().map(|x| format!("harness: lesson lookup refused: ISSUE-001-a.md: {}\n", x)).collect();
    assert_eq!(stderr(&o), want);
    let _ = fs::remove_dir_all(&d);
}

const NOT_OPEN: &str = "FRONT-MATTER line 1 is not ---";

#[test]
fn an_empty_page_is_refused() {
    refused_page("", &[NOT_OPEN]);
}

#[test]
fn a_page_of_only_body_text_is_refused() {
    refused_page("# a\ntext\n", &[NOT_OPEN]);
}

#[test]
fn a_first_line_with_a_trailing_blank_is_refused() {
    refused_page("--- \nmatch: [example]\n---\n", &[NOT_OPEN]);
}

#[test]
fn a_byte_order_mark_before_the_first_line_is_refused() {
    refused_page("\u{feff}---\nmatch: [example]\n---\n", &[NOT_OPEN]);
}

#[test]
fn a_page_with_no_closing_line_is_refused() {
    refused_page("---\nmatch: [example]\n", &["FRONT-MATTER no closing ---"]);
}

#[test]
fn a_closing_line_with_a_trailing_blank_is_refused() {
    refused_page("---\nmatch: [example]\n--- \n", &["FRONT-MATTER no closing ---"]);
}

#[test]
fn a_blank_line_inside_the_front_matter_is_refused() {
    refused_page("---\nmatch: [example]\n\n---\n", &["FRONT-MATTER line 3 is not key: value"]);
}

#[test]
fn a_key_with_a_blank_inside_is_refused() {
    refused_page("---\nma tch: [example]\n---\n", &["FRONT-MATTER line 2 is not key: value"]);
}

#[test]
fn a_repeated_other_key_is_refused() {
    refused_page("---\ntitle: a\ntitle: b\nmatch: [example]\n---\n", &["FRONT-MATTER line 3 repeats the key title"]);
}

#[test]
fn a_match_value_that_is_not_a_list_is_refused() {
    refused_page("---\nmatch: example\n---\n", &["MATCH match example is not an inline list [..]"]);
}

#[test]
fn an_empty_match_list_is_refused() {
    refused_page("---\nmatch: []\n---\n", &["MATCH match is an empty list"]);
}

#[test]
fn a_match_list_with_an_empty_part_is_refused() {
    refused_page("---\nmatch: [a,,b]\n---\n", &["MATCH match has an empty token"]);
}

#[test]
fn two_problems_on_one_page_give_two_lines() {
    refused_page("---\nmatch: [a,,b]\n\n---\n", &["FRONT-MATTER line 3 is not key: value", "MATCH match has an empty token"]);
}

#[test]
fn bad_pages_come_in_name_order_after_the_ticket_line() {
    let d = setup(b"{}", b"src/\n", &[("ISSUE-002-b.md", "x\n"), ("ISSUE-001-a.md", "y\n")]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    let err = stderr(&o);
    let l: Vec<&str> = err.lines().collect();
    assert_eq!(l.len(), 3, "{}", err);
    assert!(l[0].starts_with("harness: ticket refused: "), "{}", err);
    assert_eq!(l[1], format!("harness: lesson lookup refused: ISSUE-001-a.md: {}", NOT_OPEN));
    assert_eq!(l[2], format!("harness: lesson lookup refused: ISSUE-002-b.md: {}", NOT_OPEN));
}

#[test]
fn a_good_page_next_to_a_bad_page_prints_nothing_on_stdout() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]")), ("ISSUE-002-b.md", "x\n")]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty(), "{}", stdout(&o));
    assert_eq!(stderr(&o), format!("harness: lesson lookup refused: ISSUE-002-b.md: {}\n", NOT_OPEN));
}

#[test]
fn a_crlf_front_matter_is_read() {
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", "---\r\nmatch: [example]\r\n---\r\n")]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), GOOD_OUT);
}

// ---- lookup refusals: lint comparison, nothing written, nothing printed ----

#[test]
fn lint_and_lookup_agree_on_the_front_matter() {
    let bad: [(&str, &str); 13] = [
        ("ISSUE-001-empty.md", ""),
        ("ISSUE-002-body.md", "# a\ntext\n"),
        ("ISSUE-003-trail.md", "--- \nmatch: [example]\n---\n"),
        ("ISSUE-004-bom.md", "\u{feff}---\nmatch: [example]\n---\n"),
        ("ISSUE-005-noclose.md", "---\nmatch: [example]\n"),
        ("ISSUE-006-closetrail.md", "---\nmatch: [example]\n--- \n"),
        ("ISSUE-007-blankline.md", "---\nmatch: [example]\n\n---\n"),
        ("ISSUE-008-keyblank.md", "---\nma tch: [example]\n---\n"),
        ("ISSUE-009-repeat.md", "---\ntitle: a\ntitle: b\nmatch: [example]\n---\n"),
        ("ISSUE-010-notlist.md", "---\nmatch: example\n---\n"),
        ("ISSUE-011-emptylist.md", "---\nmatch: []\n---\n"),
        ("ISSUE-012-emptytok.md", "---\nmatch: [a,,b]\n---\n"),
        ("ISSUE-013-two.md", "---\nmatch: [a,,b]\n\n---\n"),
    ];
    let keep = |l: &&str| {
        let rest = l.split_once(": ").map(|x| x.1).unwrap_or("");
        rest.starts_with("FRONT-MATTER") || rest.starts_with("MATCH")
    };
    let mut pages: Vec<(&str, &str)> = bad.to_vec();
    pages.push(("ISSUE-014-good.md", "---\nmatch: [example]\n---\n"));
    let d = setup(default_ticket("t").as_bytes(), b"src/\n", &pages);
    let lint = run(&["lesson", "lint", d.join("lessons").to_str().unwrap()]);
    let so = stdout(&lint);
    let lint_lines: Vec<&str> = so.lines().filter(keep).collect();
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    let se = stderr(&o);
    let look_lines: Vec<&str> = se.lines().map(|l| l.strip_prefix("harness: lesson lookup refused: ").unwrap()).collect();
    assert!(!look_lines.is_empty());
    assert_eq!(lint_lines, look_lines);
    // the two empty-token pages: lint prints no FRONT-MATTER or MATCH line, lookup refuses each
    let d = setup(
        default_ticket("t").as_bytes(),
        b"src/\n",
        &[("ISSUE-001-dq.md", "---\nmatch: [\"\"]\n---\n"), ("ISSUE-002-sq.md", "---\nmatch: ['']\n---\n")],
    );
    let lint = run(&["lesson", "lint", d.join("lessons").to_str().unwrap()]);
    let so = stdout(&lint);
    assert!(!so.lines().any(|l| keep(&l)), "{}", so);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(1));
    let want = "harness: lesson lookup refused: ISSUE-001-dq.md: MATCH match has a token with no text between its quotes\nharness: lesson lookup refused: ISSUE-002-sq.md: MATCH match has a token with no text between its quotes\n";
    assert_eq!(stderr(&o), want);
}

/// One line per path under `p`: its type, and for a file its size and bytes, for a link its target.
fn listing(p: &std::path::Path, out: &mut Vec<String>) {
    let md = fs::symlink_metadata(p).unwrap();
    if md.file_type().is_symlink() {
        out.push(format!("{} link -> {:?}", p.display(), fs::read_link(p).unwrap()));
    } else if md.is_dir() {
        out.push(format!("{} dir", p.display()));
        let mut kids: Vec<PathBuf> = fs::read_dir(p).unwrap().map(|e| e.unwrap().path()).collect();
        kids.sort();
        for k in kids {
            listing(&k, out);
        }
    } else {
        out.push(format!("{} file {} {:?}", p.display(), md.len(), fs::read(p).unwrap()));
    }
}

#[test]
fn lookup_writes_nothing() {
    let cases: [(&str, i32); 3] = [("good", 0), ("nofm", 1), ("link", 2)];
    for (tag, code) in cases {
        let page = if tag == "nofm" { "no front matter\n".to_string() } else { fm("match: [example]") };
        let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &page)]);
        fs::create_dir_all(d.join("cwd")).unwrap();
        if tag == "link" {
            link(&d.join("lessons/ISSUE-001-a.md"), &d.join("lessons/ISSUE-002-b.md"));
        }
        let snap = |d: &std::path::Path| {
            let mut v = Vec::new();
            listing(d, &mut v);
            v
        };
        let before = snap(&d);
        let (l, t, s) = (d.join("lessons"), d.join("ticket.json"), d.join("scope.txt"));
        let o = Command::new(BIN)
            .args(["lesson", "lookup", l.to_str().unwrap(), "--ticket", t.to_str().unwrap(), "--scope-file", s.to_str().unwrap()])
            .current_dir(d.join("cwd"))
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(code), "{}: {}", tag, stderr(&o));
        assert_eq!(before, snap(&d), "{}", tag);
        assert_eq!(fs::read_dir(d.join("cwd")).unwrap().count(), 0, "{}", tag);
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn nothing_of_a_page_or_the_ticket_is_printed() {
    const M: &str = "zqmarkword";
    let ticket = ticket_json("example", &format!("{} title", M), &[&format!("{} criterion", M), "checks/x"], &format!("{} obj", M), &format!("{} grp", M));
    let page = fm(&format!("title: {m}\nsources: {m}\nmatch: [example]", m = M)) + &format!("# head\n{m} body\n```\n{m} fenced\n```\n## {m} section\n", m = M);
    let d = setup(ticket.as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &page)]);
    let check = |tag: &str, code: i32| {
        let o = look(&d);
        assert_eq!(o.status.code(), Some(code), "{}: {}", tag, stderr(&o));
        assert!(!stdout(&o).contains(M) && !stderr(&o).contains(M), "{}: {}{}", tag, stdout(&o), stderr(&o));
        o
    };
    let o = check("exit 0", 0);
    assert!(stdout(&o).starts_with("lesson: ISSUE-001 head ISSUE-001-a.md\n"), "{}", stdout(&o));
    fs::write(d.join("lessons/ISSUE-002-b.md"), fm(&format!("{} has no colon\nmatch: [example]", M))).unwrap();
    fs::write(d.join("lessons/ISSUE-003-c.md"), fm(&format!("sources: {m}\nsources: {m}\nmatch: [example]", m = M))).unwrap();
    let o = check("exit 1", 1);
    assert_eq!(stderr(&o).lines().count(), 2, "{}", stderr(&o));
    fs::write(d.join("target.md"), format!("{} target\n", M)).unwrap();
    link(&d.join("target.md"), &d.join("lessons/ISSUE-004-d.md"));
    let o = check("exit 2", 2);
    assert_eq!(stderr(&o), "harness: error: the entry ISSUE-004-d.md is a symbolic link\n");
}
