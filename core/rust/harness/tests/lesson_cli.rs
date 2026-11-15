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

/// A non-space blank around the tier value is not trimmed; ASCII spaces and tabs are.
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

/// Control bytes planted in a value are printed escaped; stdout holds none of them.
#[test]
fn control_bytes_in_a_value_are_escaped() {
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
