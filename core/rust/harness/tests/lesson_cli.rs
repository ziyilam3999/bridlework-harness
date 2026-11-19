//! `harness lesson lint`: real binary, synthetic fixtures only (no real ids, names, paths or text).
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// The scratch name: the tag, the process id and a sequence number.
fn scratch_name(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lesson-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)))
}

/// Make an empty folder at `d`. A process id repeats across runs and a run leaves
/// folders behind, so what is at the name is removed first (a link is removed, not followed).
/// The folder is made with a call that fails when the name exists.
fn make_scratch(d: &std::path::Path) {
    if let Ok(md) = fs::symlink_metadata(d) {
        if md.is_dir() {
            fs::remove_dir_all(d).unwrap_or_else(|e| panic!("scratch folder {} was not removed: {}", d.display(), e));
        } else {
            fs::remove_file(d).unwrap_or_else(|e| panic!("scratch entry {} was not removed: {}", d.display(), e));
        }
    }
    fs::create_dir(d).unwrap_or_else(|e| panic!("scratch folder {} was not made empty: {}", d.display(), e));
}

/// A new empty scratch folder (see `make_scratch` for why it is cleared).
fn tmp(tag: &str) -> PathBuf {
    let d = scratch_name(tag);
    make_scratch(&d);
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
    fill(&d, ticket, scope, pages);
    d
}

fn fill(d: &std::path::Path, ticket: &[u8], scope: &[u8], pages: &[(&str, &str)]) {
    fs::create_dir_all(d.join("lessons")).unwrap();
    fs::write(d.join("ticket.json"), ticket).unwrap();
    fs::write(d.join("scope.txt"), scope).unwrap();
    for (n, t) in pages {
        fs::write(d.join("lessons").join(n), t).unwrap();
    }
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

/// The cross-process reuse made on purpose: an earlier run left a page at the scratch name.
#[test]
fn a_scratch_folder_left_by_an_earlier_run_is_not_reused() {
    let d = scratch_name("reuse");
    fill(&d, default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-002-old.md", &fm("match: [nomatchtoken]"))]);
    make_scratch(&d);
    fill(&d, default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &fm("match: [example]"))]);
    let o = look(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=1 matched=ISSUE-001\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_scratch_name_that_is_a_file_or_a_link_is_replaced_by_a_new_folder() {
    let file_name = scratch_name("isfile");
    fs::write(&file_name, "x").unwrap();
    make_scratch(&file_name);
    let md = fs::symlink_metadata(&file_name).unwrap();
    assert!(md.is_dir() && !md.file_type().is_symlink());
    assert_eq!(fs::read_dir(&file_name).unwrap().count(), 0);

    let target = scratch_name("linktarget");
    make_scratch(&target);
    fs::write(target.join("keep.txt"), "kept").unwrap();
    let link_name = scratch_name("islink");
    link(&target, &link_name);
    make_scratch(&link_name);
    let md = fs::symlink_metadata(&link_name).unwrap();
    assert!(md.is_dir() && !md.file_type().is_symlink());
    assert_eq!(fs::read_dir(&link_name).unwrap().count(), 0);
    assert_eq!(fs::read_to_string(target.join("keep.txt")).unwrap(), "kept");
    let _ = fs::remove_dir_all(&file_name);
    let _ = fs::remove_dir_all(&link_name);
    let _ = fs::remove_dir_all(&target);
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

// ---- harness lesson lookup: diff mode (scratch git repos, synthetic pages) ----

fn gitcmd(dir: &std::path::Path, args: &[&str]) -> Command {
    let contact = format!("user.email=t{}scratch", '@');
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str(), "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        c.env_remove(k);
    }
    c.stdin(Stdio::null());
    c
}

fn g(dir: &std::path::Path, args: &[&str]) -> String {
    let out = gitcmd(dir, args).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn put(dir: &std::path::Path, rel: &str, bytes: &[u8]) {
    let p = dir.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, bytes).unwrap();
}

/// Write the files, remove the `del` paths, commit all (an empty commit is allowed); the new HEAD.
fn gcommit(dir: &std::path::Path, files: &[(&str, &[u8])], del: &[&str]) -> String {
    for (p, b) in files {
        put(dir, p, b);
    }
    for p in del {
        fs::remove_file(dir.join(p)).unwrap();
    }
    g(dir, &["add", "-A"]);
    g(dir, &["commit", "-q", "--allow-empty", "-m", "m"]);
    g(dir, &["rev-parse", "HEAD"])
}

fn ginit(dir: &std::path::Path) {
    fs::create_dir_all(dir).unwrap();
    g(dir, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(dir, &["config", k, v]);
    }
}

/// A scratch folder: `repo/` (two commits), `lessons/` (one page per token) and an empty `cwd/`.
struct DC {
    root: PathBuf,
    repo: PathBuf,
    from: String,
    to: String,
}

fn dc_pages(root: &std::path::Path, tokens: &[&str]) {
    fs::create_dir_all(root.join("lessons")).unwrap();
    for (i, t) in tokens.iter().enumerate() {
        fs::write(root.join("lessons").join(format!("ISSUE-{:03}-p.md", i + 1)), fm(&format!("match: [{}]", t))).unwrap();
    }
}

fn dc(a: &[(&str, &[u8])], b: &[(&str, &[u8])], del: &[&str], tokens: &[&str]) -> DC {
    let root = tmp("diff");
    let repo = root.join("repo");
    ginit(&repo);
    dc_pages(&root, tokens);
    fs::create_dir_all(root.join("cwd")).unwrap();
    let from = gcommit(&repo, a, &[]);
    let to = gcommit(&repo, b, del);
    DC { root, repo, from, to }
}

fn dargs(c: &DC, from: &str, to: &str) -> Vec<String> {
    let l = c.root.join("lessons");
    ["lesson", "lookup", l.to_str().unwrap(), "--repo-dir", c.repo.to_str().unwrap(), "--from", from, "--to", to].iter().map(|s| s.to_string()).collect()
}

fn dlook(c: &DC) -> Output {
    let a = dargs(c, &c.from, &c.to);
    let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
    run(&a)
}

/// Exit 0 and the stdout of a lookup.
fn ok_out(o: &Output) -> String {
    assert_eq!(o.status.code(), Some(0), "{}", stderr(o));
    stdout(o)
}

/// The ids after `matched=` of the last line (exit 0 asserted).
fn matched(o: &Output) -> String {
    let s = ok_out(o);
    s.lines().last().unwrap().split_once("matched=").unwrap().1.to_string()
}

fn one_file(from: &[u8], to: &[u8], token: &str) -> String {
    let c = dc(&[("a.txt", from)], &[("a.txt", to)], &[], &[token]);
    matched(&dlook(&c))
}

#[test]
fn diff_an_added_line_with_the_token_lists_the_page() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"one\nthe tokenx here\n")], &[], &["tokenx"]);
    assert_eq!(ok_out(&dlook(&c)), "lesson: ISSUE-001 p ISSUE-001-p.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn diff_the_same_change_without_the_token_lists_nothing() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"one\nthe other here\n")], &[], &["tokenx"]);
    assert_eq!(ok_out(&dlook(&c)), "lessons: scanned=1 matched=none\n");
}

#[test]
fn diff_a_removed_line_with_the_token_lists_nothing() {
    assert_eq!(one_file(b"one\ntokenx\n", b"one\n", "tokenx"), "none");
}

#[test]
fn diff_a_context_line_with_the_token_lists_nothing() {
    let old: &[u8] = b"tokenx\nb\nc\nd\n";
    let new: &[u8] = b"tokenx\nb\nc\nd\ne\n";
    assert_eq!(one_file(old, new, "tokenx"), "none");
}

#[test]
fn diff_a_changed_path_under_a_folder_token_lists_the_page() {
    let c = dc(&[("src/a.txt", b"one\n")], &[("src/a.txt", b"two\n")], &[], &["src", "src/a.txt", "sr"]);
    assert_eq!(matched(&dlook(&c)), "ISSUE-001,ISSUE-002");
}

#[test]
fn diff_a_deleted_path_under_a_folder_token_lists_the_page() {
    let c = dc(&[("gone/x.txt", b"one\n"), ("keep.txt", b"k\n")], &[("keep.txt", b"k\n")], &["gone/x.txt"], &["gone"]);
    assert_eq!(matched(&dlook(&c)), "ISSUE-001");
}

#[test]
fn diff_an_added_line_that_looks_like_a_file_header_counts() {
    // the added text is `++ b/x tokenx`, so the patch line is `+++ b/x tokenx`
    assert_eq!(one_file(b"a\n", b"a\n++ b/x tokenx\n", "tokenx"), "ISSUE-001");
}

#[test]
fn diff_a_removed_line_that_looks_like_a_file_header_does_not_count() {
    assert_eq!(one_file(b"a\n-- b/x tokenx\n", b"a\n", "tokenx"), "none");
}

#[test]
fn diff_a_last_line_with_no_newline_counts_and_the_next_file_is_read() {
    let c = dc(
        &[("one.txt", b"x\n"), ("two.txt", b"y\n")],
        &[("one.txt", b"x\ntokenone"), ("two.txt", b"y\ntokentwo\n")],
        &[],
        &["tokenone", "tokentwo"],
    );
    assert_eq!(matched(&dlook(&c)), "ISSUE-001,ISSUE-002");
}

#[test]
fn diff_a_binary_file_counts_by_path_only() {
    let c = dc(&[("data/bin.dat", b"\0\x01\n")], &[("data/bin.dat", b"\0tokenx\n\x02")], &[], &["tokenx", "data"]);
    let o = dlook(&c);
    assert_eq!(ok_out(&o), "lesson: ISSUE-002 p ISSUE-002-p.md\nlessons: scanned=2 matched=ISSUE-002\n");
}

#[test]
fn diff_a_crlf_added_line_counts() {
    assert_eq!(one_file(b"a\r\n", b"a\r\ntokenx\r\n", "tokenx"), "ISSUE-001");
}

#[test]
fn diff_a_path_with_a_blank_is_read_whole() {
    let c = dc(&[("my dir/a b.txt", b"one\n")], &[("my dir/a b.txt", b"two\n")], &[], &["my dir", "my", "my dir/a b.txt"]);
    assert_eq!(matched(&dlook(&c)), "ISSUE-001,ISSUE-003");
}

#[test]
fn diff_two_commits_with_equal_trees_list_nothing() {
    let root = tmp("diff");
    let repo = root.join("repo");
    ginit(&repo);
    dc_pages(&root, &["tokenx", "a.txt"]);
    let from = gcommit(&repo, &[("a.txt", b"tokenx\n")], &[]);
    let to = gcommit(&repo, &[], &[]);
    assert_ne!(from, to);
    let c = DC { root, repo, from, to };
    assert_eq!(ok_out(&dlook(&c)), "lessons: scanned=2 matched=none\n");
}

#[test]
fn diff_repo_diff_settings_do_not_change_the_list() {
    let c = dc(&[("a.txt", b"one\n"), ("b.txt", b"one\n")], &[("a.txt", b"one\ntokenx\n"), ("b.txt", b"two\n")], &[], &["tokenx", "b.txt", "nosuch"]);
    let plain = dlook(&c);
    assert_eq!(ok_out(&plain), "lesson: ISSUE-001 p ISSUE-001-p.md\nlesson: ISSUE-002 p ISSUE-002-p.md\nlessons: scanned=3 matched=ISSUE-001,ISSUE-002\n");
    let ext = c.root.join("ext.sh");
    fs::write(&ext, "#!/bin/sh\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&ext, fs::Permissions::from_mode(0o755)).unwrap();
    let color = ["al", "ways"].concat();
    for (k, v) in [
        ("color.ui", color.as_str()),
        ("diff.noprefix", "true"),
        ("diff.mnemonicPrefix", "true"),
        ("diff.external", ext.to_str().unwrap()),
        ("diff.drop.textconv", "sed s/tokenx//"),
    ] {
        g(&c.repo, &["config", k, v]);
    }
    put(&c.repo, ".git/info/attributes", b"*.txt diff=drop\n");
    let set = dlook(&c);
    assert_eq!(stdout(&plain), stdout(&set));
    assert_eq!(set.status.code(), Some(0), "{}", stderr(&set));
}

#[test]
fn diff_the_repo_token_alone_matches_nothing() {
    // the folder is named repo; the ticket mode repo token (a repo id) has no meaning here
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["repo", "example"]);
    assert_eq!(ok_out(&dlook(&c)), "lessons: scanned=2 matched=none\n");
}

fn refused2(o: &Output, want: &str) {
    assert_eq!(o.status.code(), Some(2), "{}", stderr(o));
    assert!(stdout(o).is_empty(), "{}", stdout(o));
    assert_eq!(stderr(o).lines().count(), 1, "{}", stderr(o));
    assert!(stderr(o).starts_with("harness: error: ") && stderr(o).contains(want), "{}", stderr(o));
}

#[test]
fn diff_a_base_that_is_not_an_ancestor_exits_2() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    let base = g(&c.repo, &["rev-parse", &format!("{}~1", c.to)]);
    g(&c.repo, &["checkout", "-q", "-b", "side", &base]);
    let side = gcommit(&c.repo, &[("a.txt", b"three\n")], &[]);
    let a = dargs(&c, &c.to, &side);
    let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
    refused2(&run(&a), "ancestor");
}

#[test]
fn diff_a_sha_that_is_not_a_commit_exits_2() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    let tree = g(&c.repo, &["rev-parse", &format!("{}^{{tree}}", c.to)]);
    for bad in ["0123456789012345678901234567890123456789".to_string(), tree] {
        for (f, t) in [(&bad, &c.to), (&c.from, &bad)] {
            let a = dargs(&c, f, t);
            let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
            refused2(&run(&a), "not a commit");
        }
    }
}

#[test]
fn diff_a_short_or_upper_case_sha_is_a_usage_error() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    let upper = c.to.to_uppercase();
    let short = c.to[..12].to_string();
    for (f, t) in [(&c.from, &upper), (&c.from, &short), (&upper, &c.to), (&short, &c.to)] {
        let a = dargs(&c, f, t);
        let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        let o = run(&a);
        assert_eq!(o.status.code(), Some(2));
        assert!(stdout(&o).is_empty());
        assert!(stderr(&o).starts_with("usage: harness lesson"), "{}", stderr(&o));
    }
    // a flag twice, a missing flag
    let l = c.root.join("lessons");
    let l = l.to_str().unwrap();
    for args in [
        vec!["lesson", "lookup", l, "--repo-dir", c.repo.to_str().unwrap(), "--from", &c.from, "--from", &c.from, "--to", &c.to],
        vec!["lesson", "lookup", l, "--repo-dir", c.repo.to_str().unwrap(), "--from", &c.from],
        vec!["lesson", "lookup", l, "--from", &c.from, "--to", &c.to],
    ] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{:?}", args);
        assert!(stdout(&o).is_empty(), "{:?}", args);
        assert!(stderr(&o).starts_with("usage: harness lesson") || stderr(&o).starts_with("harness: error: "), "{}", stderr(&o));
    }
}

#[test]
fn diff_from_equal_to_exits_2() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    let a = dargs(&c, &c.to, &c.to);
    let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
    refused2(&run(&a), "same commit");
}

#[test]
fn diff_with_ticket_flags_is_a_usage_error() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    fs::write(c.root.join("ticket.json"), default_ticket("t")).unwrap();
    fs::write(c.root.join("scope.txt"), "src/\n").unwrap();
    let (t, s) = (c.root.join("ticket.json"), c.root.join("scope.txt"));
    for extra in [["--ticket", t.to_str().unwrap()], ["--scope-file", s.to_str().unwrap()]] {
        let mut a = dargs(&c, &c.from, &c.to);
        a.extend(extra.iter().map(|s| s.to_string()));
        let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        let o = run(&a);
        assert_eq!(o.status.code(), Some(2), "{:?}", extra);
        assert!(stdout(&o).is_empty());
        assert!(stderr(&o).starts_with("usage: harness lesson"), "{}", stderr(&o));
    }
}

#[test]
fn diff_a_bad_page_is_refused_as_in_ticket_mode() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    fs::write(c.root.join("lessons/ISSUE-002-b.md"), "no front matter\n").unwrap();
    let o = dlook(&c);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-002-b.md: FRONT-MATTER line 1 is not ---\n");
}

#[test]
fn diff_a_git_fault_wins_over_a_bad_page() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    fs::write(c.root.join("lessons/ISSUE-002-b.md"), "no front matter\n").unwrap();
    let a = dargs(&c, &c.to, &c.to);
    let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
    let o = run(&a);
    refused2(&o, "same commit");
    assert!(!stderr(&o).contains("refused"));
}

#[test]
fn diff_an_inherited_git_dir_is_ignored() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"one\ntokenx\n")], &[], &["tokenx"]);
    let other = tmp("diff-other");
    ginit(&other);
    gcommit(&other, &[("z.txt", b"z\n")], &[]);
    let a = dargs(&c, &c.from, &c.to);
    let o = Command::new(BIN).args(&a).env("GIT_DIR", other.join(".git")).stdin(Stdio::null()).output().unwrap();
    assert_eq!(ok_out(&o), "lesson: ISSUE-001 p ISSUE-001-p.md\nlessons: scanned=1 matched=ISSUE-001\n");
}

#[test]
fn diff_writes_nothing() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"one\ntokenx\n")], &[], &["tokenx"]);
    let snap = |d: &std::path::Path| {
        let mut v = Vec::new();
        listing(d, &mut v);
        v
    };
    let before = snap(&c.root);
    let a = dargs(&c, &c.from, &c.to);
    let o = Command::new(BIN).args(&a).current_dir(c.root.join("cwd")).stdin(Stdio::null()).output().unwrap();
    assert_eq!(ok_out(&o), "lesson: ISSUE-001 p ISSUE-001-p.md\nlessons: scanned=1 matched=ISSUE-001\n");
    assert_eq!(before, snap(&c.root));
    assert_eq!(fs::read_dir(c.root.join("cwd")).unwrap().count(), 0);
}

#[test]
fn diff_over_the_limit_exits_2() {
    let mut big = Vec::new();
    for _ in 0..(9 * 1024 * 1024 / 100) {
        big.extend_from_slice(&[b'x'; 99]);
        big.push(b'\n');
    }
    let c = dc(&[("big.txt", b"a\n")], &[("big.txt", &big)], &[], &["tokenx"]);
    refused2(&dlook(&c), "8 MiB");
}

// ---- diff mode: the answer rests on the two commits, not on a setting or a checked-out file ----

const HIT1: &str = "lesson: ISSUE-001 p ISSUE-001-p.md\nlessons: scanned=1 matched=ISSUE-001\n";

fn tokenx_dc() -> DC {
    dc(&[("a.txt", b"one\n")], &[("a.txt", b"one\ntokenx\n")], &[], &["tokenx"])
}

#[test]
fn diff_a_big_file_threshold_does_not_decide_the_lines() {
    let c = tokenx_dc();
    g(&c.repo, &["config", "core.bigFileThreshold", "1"]);
    assert_eq!(ok_out(&dlook(&c)), HIT1);
}

#[test]
fn diff_an_info_attributes_no_diff_does_not_decide_the_lines() {
    let c = tokenx_dc();
    put(&c.repo, ".git/info/attributes", b"*.txt -diff\n");
    assert_eq!(ok_out(&dlook(&c)), HIT1);
}

#[test]
fn diff_a_binary_diff_driver_does_not_decide_the_lines() {
    let c = tokenx_dc();
    put(&c.repo, ".git/info/attributes", b"*.txt diff=bin\n");
    g(&c.repo, &["config", "diff.bin.binary", "true"]);
    assert_eq!(ok_out(&dlook(&c)), HIT1);
}

#[test]
fn diff_a_user_attributes_file_does_not_decide_the_lines() {
    let c = tokenx_dc();
    let home = c.root.join("home");
    fs::create_dir_all(&home).unwrap();
    fs::write(home.join("attrs"), "* -diff\n").unwrap();
    fs::write(home.join(".gitconfig"), format!("[core]\n\tattributesFile = {}\n", home.join("attrs").display())).unwrap();
    let a = dargs(&c, &c.from, &c.to);
    let o = Command::new(BIN).args(&a).env("HOME", &home).env("XDG_CONFIG_HOME", &home).stdin(Stdio::null()).output().unwrap();
    assert_eq!(ok_out(&o), HIT1);
}

#[test]
fn diff_a_checked_out_gitattributes_does_not_decide_the_lines() {
    let c = tokenx_dc();
    gcommit(&c.repo, &[(".gitattributes", b"* -diff\n")], &[]);
    assert_eq!(ok_out(&dlook(&c)), HIT1);
}

#[test]
fn diff_a_nul_byte_after_the_first_8000_bytes_reads_as_text() {
    let mut new = b"tokenx\n".to_vec();
    new.extend_from_slice(&[b'x'; 8100]);
    new.extend_from_slice(b"\0\n");
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", &new)], &[], &["tokenx"]);
    assert_eq!(ok_out(&dlook(&c)), HIT1);
}

#[test]
fn diff_a_nul_byte_in_the_old_file_makes_the_file_binary() {
    let c = dc(&[("a.txt", b"\0one\n")], &[("a.txt", b"one\ntokenx\n")], &[], &["tokenx", "a.txt"]);
    assert_eq!(matched(&dlook(&c)), "ISSUE-002");
}

#[test]
fn diff_a_word_final_sigma_reads_as_in_ticket_mode() {
    // the whole-string lower case of `Α'Σ` ends in a final sigma; the per-character rule keeps the plain one
    assert_eq!(one_file(b"a\n", "Α'Σ\n".as_bytes(), "Σ"), "ISSUE-001");
}

#[test]
fn diff_a_relative_or_dot_dot_repo_dir_is_a_usage_error() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["a.txt"]);
    fs::create_dir_all(c.root.join("cwd")).unwrap();
    let l = c.root.join("lessons");
    let dd = format!("{}/cwd/../repo", c.root.to_str().unwrap());
    // run in the root, where `repo` is the real repo; the dot-dot path also reaches it
    for repo in ["repo", dd.as_str()] {
        let o = Command::new(BIN)
            .args(["lesson", "lookup", l.to_str().unwrap(), "--repo-dir", repo, "--from", &c.from, "--to", &c.to])
            .current_dir(&c.root)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{}", repo);
        assert!(stdout(&o).is_empty());
        assert!(stderr(&o).starts_with("usage: harness lesson lint"), "{}", stderr(&o));
    }
}

#[test]
fn diff_a_plain_folder_inside_a_repo_is_refused() {
    let c = dc(&[("sub/a.txt", b"one\n")], &[("sub/a.txt", b"one\ntokenx\n")], &[], &["tokenx"]);
    let inner = DC { root: c.root.clone(), repo: c.repo.join("sub"), from: c.from.clone(), to: c.to.clone() };
    refused2(&dlook(&inner), "top folder");
}

#[test]
fn diff_a_marker_inside_a_hunk_is_skipped() {
    // old side has no final newline and more lines follow: `-b`, the marker, `+b`, `+tokenx`
    assert_eq!(one_file(b"a\nb", b"a\nb\ntokenx\n", "tokenx"), "ISSUE-001");
}

#[test]
fn diff_a_gitlink_bump_counts_by_path_only() {
    let root = tmp("diff");
    let repo = root.join("repo");
    ginit(&repo);
    let (s1, s2) = ("1".repeat(40), "2".repeat(40));
    dc_pages(&root, &["subproject", s2.as_str(), "sub"]);
    gcommit(&repo, &[("a.txt", b"one\n")], &[]);
    let link = |sha: &str| {
        g(&repo, &["update-index", "--add", "--cacheinfo", &format!("160000,{},sub", sha)]);
        g(&repo, &["commit", "-q", "-m", "m"]);
        g(&repo, &["rev-parse", "HEAD"])
    };
    let from = link(&s1);
    let to = link(&s2);
    let c = DC { root, repo, from, to };
    assert_eq!(matched(&dlook(&c)), "ISSUE-003");
}

// ---- status: a retired page is skipped, an unknown status is refused ----

/// A page that hits the default ticket (token `example`), with the given status line text.
fn spage(status_line: &str) -> String {
    fm(&format!("{}\nmatch: [example]", status_line))
}

/// Ticket-mode lookup over the given pages (default ticket, scope src/).
fn slook(pages: &[(&str, &str)]) -> Output {
    look(&setup(default_ticket("t").as_bytes(), b"src/\n", pages))
}

const A_ONLY: &str = "lesson: ISSUE-001 a ISSUE-001-a.md\n";

fn status_refusal(detail: &str) -> String {
    format!("harness: lesson lookup refused: ISSUE-001-a.md: STATUS status {} is not unvalidated, validated or retired\n", detail)
}

#[test]
fn lookup_a_retired_page_that_hits_is_not_listed() {
    let o = slook(&[("ISSUE-001-a.md", &spage("status: retired"))]);
    assert_eq!(ok_out(&o), "lessons: scanned=1 matched=none retired=1\n");
}

#[test]
fn lookup_the_same_page_unvalidated_or_validated_is_listed() {
    for s in ["unvalidated", "validated"] {
        let o = slook(&[("ISSUE-001-a.md", &spage(&format!("status: {}", s)))]);
        assert_eq!(ok_out(&o), format!("{}lessons: scanned=1 matched=ISSUE-001\n", A_ONLY), "{}", s);
    }
}

#[test]
fn lookup_a_page_with_no_status_that_hits_is_listed() {
    let o = slook(&[("ISSUE-001-a.md", &fm("match: [example]"))]);
    assert_eq!(ok_out(&o), format!("{}lessons: scanned=1 matched=ISSUE-001\n", A_ONLY));
}

#[test]
fn lookup_a_status_key_in_other_letter_case_is_not_read() {
    let o = slook(&[("ISSUE-001-a.md", &spage("Status: retired"))]);
    assert_eq!(ok_out(&o), format!("{}lessons: scanned=1 matched=ISSUE-001\n", A_ONLY));
}

#[test]
fn lookup_a_retired_page_and_an_active_page_count_in_scanned() {
    let o = slook(&[("ISSUE-001-a.md", &spage("status: retired")), ("ISSUE-002-b.md", &spage("status: validated"))]);
    assert_eq!(ok_out(&o), "lesson: ISSUE-002 b ISSUE-002-b.md\nlessons: scanned=2 matched=ISSUE-002 retired=1\n");
}

#[test]
fn lookup_a_retired_page_that_does_not_hit_counts_as_retired() {
    let miss = fm("status: retired\nmatch: [nosuch]");
    let none = fm("status: retired");
    let o = slook(&[("ISSUE-001-a.md", &miss), ("ISSUE-002-b.md", &none)]);
    assert_eq!(ok_out(&o), "lessons: scanned=2 matched=none retired=2\n");
}

#[test]
fn lookup_with_no_retired_page_prints_no_retired_field() {
    let o = slook(&[("ISSUE-001-a.md", &spage("status: validated")), ("ISSUE-002-b.md", &spage("status: unvalidated"))]);
    let out = ok_out(&o);
    assert!(!out.contains("retired"), "{}", out);
    assert!(out.ends_with("lessons: scanned=2 matched=ISSUE-001,ISSUE-002\n"), "{}", out);
}

#[test]
fn lookup_a_status_with_blanks_around_it_is_read() {
    let o = slook(&[("ISSUE-001-a.md", &spage("status:  \t retired \t  "))]);
    assert_eq!(ok_out(&o), "lessons: scanned=1 matched=none retired=1\n");
}

#[test]
fn lookup_a_crlf_retired_page_is_not_listed() {
    let page = "---\r\nstatus: retired\r\nmatch: [example]\r\n---\r\n# t\r\n";
    let o = slook(&[("ISSUE-001-a.md", page)]);
    assert_eq!(ok_out(&o), "lessons: scanned=1 matched=none retired=1\n");
}

#[test]
fn lookup_an_unknown_status_is_refused() {
    for (line, detail) in [("status: draft", "draft"), ("status: Retired", "Retired"), ("status: \"retired\"", "\"retired\""), ("status:", "")] {
        let o = slook(&[("ISSUE-001-a.md", &spage(line))]);
        assert_eq!(o.status.code(), Some(1), "{}", line);
        assert!(stdout(&o).is_empty(), "{}", line);
        assert_eq!(stderr(&o), status_refusal(detail), "{}", line);
    }
}

#[test]
fn lookup_an_unknown_status_on_a_page_with_no_match_key_is_refused() {
    let o = slook(&[("ISSUE-001-a.md", &fm("status: draft"))]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), status_refusal("draft"));
}

#[test]
fn lookup_a_retired_page_with_a_bad_match_is_still_refused() {
    let o = slook(&[("ISSUE-001-a.md", &fm("status: retired\nmatch: []"))]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-a.md: MATCH match is an empty list\n");
}

#[test]
fn lookup_an_unknown_status_and_a_bad_match_come_in_the_lint_order() {
    let o = slook(&[("ISSUE-001-a.md", &fm("status: draft\nmatch: []"))]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), format!("{}harness: lesson lookup refused: ISSUE-001-a.md: MATCH match is an empty list\n", status_refusal("draft")));
}

#[test]
fn lookup_a_repeated_status_key_gives_both_lines() {
    let o = slook(&[("ISSUE-001-a.md", &spage("status: validated\nstatus: draft"))]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(
        stderr(&o),
        format!("harness: lesson lookup refused: ISSUE-001-a.md: FRONT-MATTER line 3 repeats the key status\n{}", status_refusal("draft"))
    );
}

#[test]
fn lookup_and_lint_print_the_same_status_line() {
    for (i, v) in ["draft", "\"retired\"", "a\u{1b}b"].iter().enumerate() {
        let page = spage(&format!("status: {}", v));
        let d = setup(default_ticket("t").as_bytes(), b"src/\n", &[("ISSUE-001-a.md", &page)]);
        let l = run(&["lesson", "lint", d.join("lessons/ISSUE-001-a.md").to_str().unwrap()]);
        let lint_line = stdout(&l).lines().find_map(|x| x.strip_prefix("ISSUE-001-a.md: STATUS ").map(|r| r.to_string())).unwrap();
        let o = look(&d);
        assert_eq!(o.status.code(), Some(1), "{}", i);
        assert_eq!(stderr(&o), format!("harness: lesson lookup refused: ISSUE-001-a.md: STATUS {}\n", lint_line), "{}", i);
    }
}

/// A diff-mode scratch folder whose one page ISSUE-001-p.md is replaced by `page` (it hits: token tokenx).
fn dc_with_page(page: &str) -> DC {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"one\ntokenx\n")], &[], &["tokenx"]);
    fs::write(c.root.join("lessons/ISSUE-001-p.md"), page).unwrap();
    c
}

#[test]
fn diff_a_retired_page_that_hits_is_not_listed() {
    let c = dc_with_page(&fm("status: retired\nmatch: [tokenx]"));
    assert_eq!(ok_out(&dlook(&c)), "lessons: scanned=1 matched=none retired=1\n");
}

#[test]
fn diff_the_same_page_unvalidated_is_listed() {
    let c = dc_with_page(&fm("status: unvalidated\nmatch: [tokenx]"));
    assert_eq!(ok_out(&dlook(&c)), HIT1);
}

#[test]
fn diff_an_unknown_status_is_refused_as_in_ticket_mode() {
    let c = dc_with_page(&fm("status: draft\nmatch: [tokenx]"));
    let o = dlook(&c);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-p.md: STATUS status draft is not unvalidated, validated or retired\n");
}

#[test]
fn diff_a_git_fault_wins_over_an_unknown_status() {
    let c = dc_with_page(&fm("status: draft\nmatch: [tokenx]"));
    let a = dargs(&c, &c.to, &c.to);
    let a: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
    let o = run(&a);
    refused2(&o, "same commit");
    assert!(!stderr(&o).contains("refused"));
}

// ---- the always key: one rule sentence, no match token ----

/// A rule sentence of a synthetic always page.
const RULE: &str = "Read the whole error text before the fix.";

/// A full good page with the given id whose `match` line is replaced by `always: <rule>`.
fn apage(id: &str, rule: &str) -> String {
    good_page().replace("ISSUE-101", id).replace("match: [svc-a, svc-b]", &format!("always: {}", rule))
}

#[test]
fn lint_an_always_page_passes_and_a_200_byte_rule_passes() {
    let d = tmp("alw-good");
    fs::write(d.join("ISSUE-101-a.md"), apage("ISSUE-101", RULE)).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    // 200 bytes pass; a retired always page passes too (it just does not count toward the cap)
    fs::write(d.join("ISSUE-102-b.md"), apage("ISSUE-102", &"x".repeat(200))).unwrap();
    fs::write(d.join("ISSUE-103-c.md"), apage("ISSUE-103", RULE).replace("status: validated", "status: retired")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=3\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lint_an_empty_always_value_is_refused() {
    for rule in ["", "   "] {
        let d = tmp("alw-empty");
        fs::write(d.join("ISSUE-101-e.md"), apage("ISSUE-101", rule)).unwrap();
        let o = run(&["lesson", "lint", d.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{:?}: {}", rule, stderr(&o));
        assert_eq!(stdout(&o), "ISSUE-101-e.md: ALWAYS always is empty\n", "{:?}", rule);
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn lint_an_always_value_over_200_bytes_is_refused() {
    let d = tmp("alw-len");
    fs::write(d.join("ISSUE-101-l.md"), apage("ISSUE-101", &"x".repeat(201))).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "ISSUE-101-l.md: ALWAYS always is over 200 bytes\n");
    // 200 bytes pass
    fs::write(d.join("ISSUE-101-l.md"), apage("ISSUE-101", &"x".repeat(200))).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lint_an_always_value_with_a_tab_a_nbsp_or_a_control_character_is_refused() {
    for bad in ["one\ttwo", "one\u{a0}two", "one\u{7}two"] {
        let d = tmp("alw-blank");
        fs::write(d.join("ISSUE-101-b.md"), apage("ISSUE-101", bad)).unwrap();
        let o = run(&["lesson", "lint", d.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(1), "{:?}: {}", bad, stderr(&o));
        assert_eq!(
            stdout(&o),
            "ISSUE-101-b.md: ALWAYS always holds a control character or a blank other than a space\n",
            "{:?}",
            bad
        );
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn lint_a_page_with_always_and_match_is_refused() {
    let d = tmp("alw-both");
    let page = apage("ISSUE-101", RULE).replace(&format!("always: {}", RULE), &format!("always: {}\nmatch: [svc-a]", RULE));
    fs::write(d.join("ISSUE-101-m.md"), page).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "ISSUE-101-m.md: ALWAYS always and match on one page\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lint_always_is_checked_on_a_retired_page() {
    let d = tmp("alw-ret");
    fs::write(d.join("ISSUE-101-r.md"), apage("ISSUE-101", "").replace("status: validated", "status: retired")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "ISSUE-101-r.md: ALWAYS always is empty\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lint_six_always_pages_give_one_cap_line() {
    let d = tmp("alw-cap6");
    for i in 1..=6 {
        let id = format!("ISSUE-{}", 100 + i);
        fs::write(d.join(format!("{}-a.md", id)), apage(&id, RULE)).unwrap();
    }
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "ISSUE-106-a.md: ALWAYS-CAP more than 5 pages carry always\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lint_seven_always_pages_give_two_cap_lines() {
    let d = tmp("alw-cap7");
    for i in 1..=7 {
        let id = format!("ISSUE-{}", 100 + i);
        fs::write(d.join(format!("{}-a.md", id)), apage(&id, RULE)).unwrap();
    }
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "ISSUE-106-a.md: ALWAYS-CAP more than 5 pages carry always\nISSUE-107-a.md: ALWAYS-CAP more than 5 pages carry always\n"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lint_five_always_pages_and_a_retired_one_pass() {
    let d = tmp("alw-cap5r");
    for i in 1..=5 {
        let id = format!("ISSUE-{}", 100 + i);
        fs::write(d.join(format!("{}-a.md", id)), apage(&id, RULE)).unwrap();
    }
    fs::write(d.join("ISSUE-106-a.md"), apage("ISSUE-106", RULE).replace("status: validated", "status: retired")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), "lesson-lint ok pages=6\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lint_the_cap_line_comes_after_the_always_line_of_that_page() {
    let d = tmp("alw-capord");
    for i in 1..=5 {
        let id = format!("ISSUE-{}", 100 + i);
        fs::write(d.join(format!("{}-a.md", id)), apage(&id, RULE)).unwrap();
    }
    // the sixth page has a bad always value: its ALWAYS line first, the cap line directly after it
    fs::write(d.join("ISSUE-106-a.md"), apage("ISSUE-106", "")).unwrap();
    let o = run(&["lesson", "lint", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "ISSUE-106-a.md: ALWAYS always is empty\nISSUE-106-a.md: ALWAYS-CAP more than 5 pages carry always\n"
    );
    let _ = fs::remove_dir_all(&d);
}

// ---- the always key: lookup, both modes ----

/// A ticket-mode lookup over `n` always pages (ids ISSUE-001..ISSUE-00n, no status key) plus extra pages.
fn alw_look(n: usize, extra: &[(&str, &str)]) -> Output {
    let mut pages: Vec<(String, String)> = (1..=n).map(|i| (format!("ISSUE-{:03}-a.md", i), fm("always: One habit page."))).collect();
    for (name, text) in extra {
        pages.push((name.to_string(), text.to_string()));
    }
    let refs: Vec<(&str, &str)> = pages.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    look(&setup(default_ticket("t").as_bytes(), b"src/\n", &refs))
}

#[test]
fn lookup_always_lines_come_before_the_lesson_lines() {
    let o = slook(&[
        ("ISSUE-001-a.md", &fm("match: [example]")),
        ("ISSUE-002-b.md", &fm("always: One habit page.")),
        ("ISSUE-003-c.md", &fm(&format!("always: {}", RULE))),
    ]);
    assert_eq!(
        ok_out(&o),
        "always: ISSUE-002 One habit page.\nalways: ISSUE-003 Read the whole error text before the fix.\nlesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=3 matched=ISSUE-001 always=ISSUE-002,ISSUE-003\n"
    );
}

#[test]
fn lookup_a_retired_always_page_prints_no_line_and_counts_as_retired() {
    let o = slook(&[
        ("ISSUE-001-a.md", &fm("match: [example]")),
        ("ISSUE-002-b.md", &fm("status: retired\nalways: One habit page.")),
    ]);
    assert_eq!(ok_out(&o), "lesson: ISSUE-001 a ISSUE-001-a.md\nlessons: scanned=2 matched=ISSUE-001 retired=1\n");
}

#[test]
fn lookup_the_always_field_stands_before_the_retired_field() {
    let o = slook(&[
        ("ISSUE-002-b.md", &fm("always: One habit page.")),
        ("ISSUE-003-c.md", &fm("status: retired\nmatch: [example]")),
    ]);
    assert_eq!(ok_out(&o), "always: ISSUE-002 One habit page.\nlessons: scanned=2 matched=none always=ISSUE-002 retired=1\n");
}

#[test]
fn diff_always_lines_come_before_the_lesson_lines() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"one\ntokenx\n")], &[], &["tokenx"]);
    fs::write(c.root.join("lessons/ISSUE-002-q.md"), fm("always: One habit page.")).unwrap();
    fs::write(c.root.join("lessons/ISSUE-004-s.md"), fm(&format!("always: {}", RULE))).unwrap();
    assert_eq!(
        ok_out(&dlook(&c)),
        "always: ISSUE-002 One habit page.\nalways: ISSUE-004 Read the whole error text before the fix.\nlesson: ISSUE-001 p ISSUE-001-p.md\nlessons: scanned=3 matched=ISSUE-001 always=ISSUE-002,ISSUE-004\n"
    );
}

#[test]
fn diff_a_retired_always_page_prints_no_line_and_counts_as_retired() {
    let c = dc_with_page(&fm("status: retired\nalways: One habit page."));
    assert_eq!(ok_out(&dlook(&c)), "lessons: scanned=1 matched=none retired=1\n");
}

#[test]
fn lookup_a_page_the_always_rule_refuses_is_refused() {
    refused_page("---\nalways:\n---\n", &["ALWAYS always is empty"]);
}

#[test]
fn lookup_a_retired_page_with_a_bad_always_value_is_refused() {
    refused_page("---\nstatus: retired\nalways:\n---\n", &["ALWAYS always is empty"]);
}

#[test]
fn lookup_a_page_with_always_and_match_is_refused() {
    refused_page("---\nmatch: [example]\nalways: One habit page.\n---\n", &["ALWAYS always and match on one page"]);
}

#[test]
fn lookup_problems_on_one_page_come_in_the_order_status_match_always() {
    refused_page(
        "---\nstatus: draft\nmatch: []\nalways:\n---\n",
        &[
            "STATUS status draft is not unvalidated, validated or retired",
            "MATCH match is an empty list",
            "ALWAYS always is empty",
        ],
    );
}

#[test]
fn diff_a_page_the_always_rule_refuses_is_refused_as_in_ticket_mode() {
    let c = dc_with_page("---\nalways:\n---\n");
    let o = dlook(&c);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-001-p.md: ALWAYS always is empty\n");
}

#[test]
fn lookup_six_always_pages_are_refused_with_the_cap_line() {
    let o = alw_look(6, &[]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ALWAYS-CAP 6 pages carry always, at most 5\n");
}

#[test]
fn lookup_five_always_pages_and_a_retired_one_pass() {
    let o = alw_look(5, &[("ISSUE-006-b.md", fm("status: retired\nalways: One habit page.").as_str())]);
    assert_eq!(
        ok_out(&o),
        "always: ISSUE-001 One habit page.\nalways: ISSUE-002 One habit page.\nalways: ISSUE-003 One habit page.\nalways: ISSUE-004 One habit page.\nalways: ISSUE-005 One habit page.\nlessons: scanned=6 matched=none always=ISSUE-001,ISSUE-002,ISSUE-003,ISSUE-004,ISSUE-005 retired=1\n"
    );
}

#[test]
fn lookup_six_pages_with_one_bad_value_print_the_page_line_then_the_cap_line() {
    let o = alw_look(5, &[("ISSUE-006-b.md", fm("always:").as_str())]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stdout(&o).is_empty());
    assert_eq!(
        stderr(&o),
        "harness: lesson lookup refused: ISSUE-006-b.md: ALWAYS always is empty\nharness: lesson lookup refused: ALWAYS-CAP 6 pages carry always, at most 5\n"
    );
}

#[test]
fn diff_six_always_pages_are_refused_with_the_cap_line() {
    let c = dc(&[("a.txt", b"one\n")], &[("a.txt", b"two\n")], &[], &["tokenx"]);
    fs::write(c.root.join("lessons/ISSUE-001-p.md"), fm("always: One habit page.")).unwrap();
    for i in 2..=6 {
        fs::write(c.root.join("lessons").join(format!("ISSUE-{:03}-q.md", i)), fm("always: One habit page.")).unwrap();
    }
    let o = dlook(&c);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert!(stdout(&o).is_empty());
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ALWAYS-CAP 6 pages carry always, at most 5\n");
}
