//! `harness lesson replay`: real binary, synthetic fixtures only (no real ids, names, paths or text).
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// The scratch name: the tag, the process id and a sequence number.
fn scratch_name(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("lrep-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)))
}

/// Make an empty folder at `d`. A process id repeats across runs and a run leaves
/// folders behind, so what is at the name is removed first (a link is removed, not followed).
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

fn fm(body: &str) -> String {
    format!("---\n{}\n---\n", body)
}

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

/// A ticket whose repo id, title and one criterion hit the token `example` and friends.
fn hello_ticket() -> String {
    ticket_json("example", "Say hello", &["checks/hello.sh passes"], "obj", "grp")
}

/// A ticket that hits none of the synthetic pages.
fn other_ticket() -> String {
    ticket_json("other-repo", "Nothing here", &["zzz"], "o", "g")
}

/// A replay scratch folder: `lessons/`, `tickets/`, `scopes/` and an empty `cwd/`.
fn rsetup(pages: &[(&str, String)], tickets: &[(&str, String)], scopes: &[(&str, String)]) -> PathBuf {
    let d = tmp("replay");
    fs::create_dir_all(d.join("lessons")).unwrap();
    for (n, t) in pages {
        fs::write(d.join("lessons").join(n), t).unwrap();
    }
    fs::create_dir_all(d.join("tickets")).unwrap();
    for (n, t) in tickets {
        fs::write(d.join("tickets").join(n), t).unwrap();
    }
    fs::create_dir_all(d.join("scopes")).unwrap();
    for (n, t) in scopes {
        fs::write(d.join("scopes").join(n), t).unwrap();
    }
    fs::create_dir_all(d.join("cwd")).unwrap();
    d
}

fn replay_in(d: &std::path::Path) -> Output {
    let (l, t, s) = (d.join("lessons"), d.join("tickets"), d.join("scopes"));
    run(&["lesson", "replay", l.to_str().unwrap(), "--tickets", t.to_str().unwrap(), "--scopes", s.to_str().unwrap()])
}

/// The ticket-mode lookup over one ticket of the fixture.
fn lookup_one(d: &std::path::Path, id: &str) -> Output {
    let t = d.join("tickets").join(format!("{}.json", id));
    let s = d.join("scopes").join(format!("{}.paths", id));
    run(&["lesson", "lookup", d.join("lessons").to_str().unwrap(), "--ticket", t.to_str().unwrap(), "--scope-file", s.to_str().unwrap()])
}

/// One good page that hits the hello ticket.
fn one_page() -> Vec<(&'static str, String)> {
    vec![("ISSUE-001-a.md", fm("match: [example]"))]
}

fn link(target: &std::path::Path, at: &std::path::Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// The shared fixture: a retired page whose token hits a ticket, a page that hits no ticket, and a ticket
/// with no scope file (skipped_no_scope=1 on the last line).
fn fixture() -> PathBuf {
    rsetup(
        &[
            ("ISSUE-101-a.md", fm("match: [example]")),
            ("ISSUE-102-b.md", fm("match: [hello]")),
            ("ISSUE-103-c.md", fm("match: [zzzmiss]")),
            ("ISSUE-104-d.md", fm("status: retired\nmatch: [example]")),
        ],
        &[("aa-1.json", hello_ticket()), ("bb-2.json", other_ticket()), ("cc-3.json", hello_ticket())],
        &[("aa-1.paths", "src/\n".to_string()), ("bb-2.paths", "docs/\n".to_string())],
    )
}

const FIXTURE_OUT: &str = "replay: ticket=aa-1 offered=2 pages=ISSUE-101,ISSUE-102\n\
replay: ticket=bb-2 offered=0 pages=none\n\
page: ISSUE-101 hits=1\n\
page: ISSUE-102 hits=1\n\
page: ISSUE-103 hits=0\n\
page: ISSUE-104 hits=0\n\
replay ok tickets=2 skipped_no_scope=1 pages=4 offered_total=2 offered_max=2 zero=1 retired=1\n";

#[test]
fn the_fixture_prints_the_exact_lines() {
    let d = fixture();
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), FIXTURE_OUT);
    let _ = fs::remove_dir_all(&d);
}

/// The pages= list of each replay line equals the matched= list of the lookup over that ticket.
#[test]
fn replay_lists_what_lookup_lists() {
    let d = fixture();
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    for tid in ["aa-1", "bb-2"] {
        let lo = lookup_one(&d, tid);
        assert_eq!(lo.status.code(), Some(0), "{}: {}", tid, stderr(&lo));
        // the receipt may end with the optional retired= field, so only the first word is matched=
        let matched = stdout(&lo).lines().last().unwrap().split_once("matched=").unwrap().1.split(' ').next().unwrap().to_string();
        let line = out.lines().find(|l| l.starts_with(&format!("replay: ticket={} ", tid))).unwrap();
        let listed = line.split_once("pages=").unwrap().1.to_string();
        assert_eq!(listed, matched, "{}: {} vs {}", tid, listed, matched);
        // the retired page is skipped as in lookup, so it is not offered
        assert!(!listed.contains("ISSUE-104"), "{}: {}", tid, listed);
    }
    let last = out.lines().last().unwrap();
    assert!(last.contains("skipped_no_scope=1"), "{}", last);
    assert!(last.ends_with("retired=1"), "{}", last);
    let _ = fs::remove_dir_all(&d);
}

/// The worked example: two pages, two tickets.
#[test]
fn two_pages_two_tickets_print_the_exact_lines() {
    let d = rsetup(
        &[("ISSUE-201-a.md", fm("match: [example]")), ("ISSUE-202-b.md", fm("match: [nope]"))],
        &[("aa-1.json", hello_ticket()), ("bb-2.json", other_ticket())],
        &[("aa-1.paths", "src/\n".to_string()), ("bb-2.paths", "docs/\n".to_string())],
    );
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "replay: ticket=aa-1 offered=1 pages=ISSUE-201\n\
replay: ticket=bb-2 offered=0 pages=none\n\
page: ISSUE-201 hits=1\n\
page: ISSUE-202 hits=0\n\
replay ok tickets=2 skipped_no_scope=0 pages=2 offered_total=1 offered_max=1 zero=1\n"
    );
    let _ = fs::remove_dir_all(&d);
}

/// A retired page whose token hits a ticket is not offered, still gets its page line with hits=0, and is
/// counted once in retired=<r>.
#[test]
fn retired_page_is_not_offered_and_counted_once() {
    let d = rsetup(
        &[("ISSUE-301-a.md", fm("status: retired\nmatch: [example]")), ("ISSUE-302-b.md", fm("match: [example]"))],
        &[("aa-1.json", hello_ticket())],
        &[("aa-1.paths", "src/\n".to_string())],
    );
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "replay: ticket=aa-1 offered=1 pages=ISSUE-302\n\
page: ISSUE-301 hits=0\n\
page: ISSUE-302 hits=1\n\
replay ok tickets=1 skipped_no_scope=0 pages=2 offered_total=1 offered_max=1 zero=0 retired=1\n"
    );
    let _ = fs::remove_dir_all(&d);
}

/// A ticket with no scope file is skipped: no replay line of its own, tickets=<n> leaves it out, and
/// skipped_no_scope counts it (the zz-9 ticket offers nothing, so a run of it would print offered=0).
#[test]
fn ticket_with_no_scope_file_is_skipped_not_offered_zero() {
    let d = rsetup(
        one_page().as_slice(),
        &[("aa-1.json", hello_ticket()), ("zz-9.json", other_ticket())],
        &[("aa-1.paths", "src/\n".to_string())],
    );
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(
        stdout(&o),
        "replay: ticket=aa-1 offered=1 pages=ISSUE-001\n\
page: ISSUE-001 hits=1\n\
replay ok tickets=1 skipped_no_scope=1 pages=1 offered_total=1 offered_max=1 zero=0\n"
    );
    let _ = fs::remove_dir_all(&d);
}

/// A page offered to no ticket is still listed in the page lines, with hits=0.
#[test]
fn page_with_no_hits_gets_a_page_line() {
    let d = rsetup(
        &[("ISSUE-501-a.md", fm("match: [example]")), ("ISSUE-502-b.md", fm("match: [nope]"))],
        &[("aa-1.json", hello_ticket())],
        &[("aa-1.paths", "src/\n".to_string())],
    );
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    let out = stdout(&o);
    assert!(out.contains("page: ISSUE-502 hits=0\n"), "{}", out);
    let _ = fs::remove_dir_all(&d);
}

// ---- refusals ----

/// A page the lookup refuses refuses the replay with the lookup's own line, and no partial count is printed.
#[test]
fn a_page_the_lookup_refuses_refuses_the_replay() {
    let d = rsetup(
        &[("ISSUE-701-a.md", "no front matter\n".to_string())],
        &[("aa-1.json", hello_ticket())],
        &[("aa-1.paths", "src/\n".to_string())],
    );
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: lesson lookup refused: ISSUE-701-a.md: FRONT-MATTER line 1 is not ---\n");
    let _ = fs::remove_dir_all(&d);
}

/// A lessons folder the lookup's folder walk refuses exits 2 with the walk's own line.
#[test]
fn a_lessons_folder_the_walk_refuses_exits_2() {
    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
    fs::write(d.join("outside.md"), fm("match: [example]")).unwrap();
    link(&d.join("outside.md"), &d.join("lessons/ISSUE-802-b.md"));
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: error: the entry ISSUE-802-b.md is a symbolic link\n");
    let _ = fs::remove_dir_all(&d);
}

/// A ticket file the ticket reader refuses: exit 1, one line naming the file name only. The refused ticket
/// sorts after a good one, so a partial count would have printed the good ticket's line first.
#[test]
fn refusal_prints_no_partial_count() {
    let d = rsetup(
        one_page().as_slice(),
        &[("aa-1.json", hello_ticket()), ("zz-9.json", "{\"no\": \"ticket\"}".to_string())],
        &[("aa-1.paths", "src/\n".to_string()), ("zz-9.paths", "src/\n".to_string())],
    );
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: lesson replay refused: zz-9.json\n");
    let _ = fs::remove_dir_all(&d);
}

/// A scope file parse_scope refuses: exit 1, one line naming the file name only.
#[test]
fn a_scope_file_parse_scope_refuses_is_named() {
    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "   \n".to_string())]);
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: lesson replay refused: aa-1.paths\n");
    let _ = fs::remove_dir_all(&d);
}

/// A ticket file that is a symbolic link is refused with its file name.
#[test]
fn a_symlinked_ticket_file_is_refused() {
    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
    fs::remove_file(d.join("tickets/aa-1.json")).unwrap();
    fs::write(d.join("away.json"), hello_ticket()).unwrap();
    link(&d.join("away.json"), &d.join("tickets/aa-1.json"));
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: lesson replay refused: aa-1.json\n");
    let _ = fs::remove_dir_all(&d);
}

/// A ticket file over the reader limit is refused with its file name.
#[test]
fn a_ticket_over_the_reader_limit_is_refused() {
    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
    fs::write(d.join("tickets/aa-1.json"), vec![b' '; 1024 * 1024 + 8]).unwrap();
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: lesson replay refused: aa-1.json\n");
    let _ = fs::remove_dir_all(&d);
}

/// A scope file that is a symbolic link is refused with its file name.
#[test]
fn a_symlinked_scope_file_is_refused() {
    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
    fs::remove_file(d.join("scopes/aa-1.paths")).unwrap();
    fs::write(d.join("away.paths"), "src/\n").unwrap();
    link(&d.join("away.paths"), &d.join("scopes/aa-1.paths"));
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: lesson replay refused: aa-1.paths\n");
    let _ = fs::remove_dir_all(&d);
}

/// A scope file over the reader limit is refused with its file name.
#[test]
fn a_scope_file_over_the_reader_limit_is_refused() {
    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
    fs::write(d.join("scopes/aa-1.paths"), vec![b' '; 1024 * 1024 + 8]).unwrap();
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert_eq!(stderr(&o), "harness: lesson replay refused: aa-1.paths\n");
    let _ = fs::remove_dir_all(&d);
}

// ---- exit 2: usage and folder errors ----

#[test]
fn replay_usage_errors_exit_2() {
    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
    let (l, t, s) = (
        d.join("lessons").to_str().unwrap().to_string(),
        d.join("tickets").to_str().unwrap().to_string(),
        d.join("scopes").to_str().unwrap().to_string(),
    );
    let dd = format!("{}/cwd/../lessons", d.to_str().unwrap());
    // a relative or dot-dot lessons folder, tickets folder or scopes folder
    for args in [
        vec!["lesson", "replay", "lessons", "--tickets", &t, "--scopes", &s],
        vec!["lesson", "replay", &dd, "--tickets", &t, "--scopes", &s],
        vec!["lesson", "replay", &l, "--tickets", "tickets", "--scopes", &s],
        vec!["lesson", "replay", &l, "--tickets", &dd, "--scopes", &s],
        vec!["lesson", "replay", &l, "--tickets", &t, "--scopes", "scopes"],
        vec!["lesson", "replay", &l, "--tickets", &t, "--scopes", &dd],
    ] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{:?}", args);
        assert_eq!(stdout(&o), "", "{:?}", args);
        assert!(stderr(&o).starts_with("usage: harness lesson"), "{:?}: {}", args, stderr(&o));
    }
    // a flag given twice, an unknown option, a missing value, a missing lessons folder argument
    for (args, want) in [
        (vec!["lesson", "replay", &l, "--tickets", &t, "--tickets", &t, "--scopes", &s], "--tickets given twice"),
        (vec!["lesson", "replay", &l, "--tickets", &t, "--scopes", &s, "--scopes", &s], "--scopes given twice"),
        (vec!["lesson", "replay", &l, "--tickets", &t, "--scopes", &s, "--bogus", "x"], "--bogus"),
        (vec!["lesson", "replay", &l, "--tickets", &t, "--scopes"], "--scopes needs a value"),
        (vec!["lesson", "replay", &l, "--scopes", &s], "--tickets is required"),
        (vec!["lesson", "replay", &l, "--tickets", &t], "--scopes is required"),
        (vec!["lesson", "replay", "--tickets", &t, "--scopes", &s], "usage: harness lesson"),
        (vec!["lesson", "replay", &l, &l, "--tickets", &t, "--scopes", &s], "more than one lessons folder"),
    ] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{:?}", args);
        assert_eq!(stdout(&o), "", "{:?}", args);
        assert!(stderr(&o).contains(want), "{:?}: {}", args, stderr(&o));
    }
    // the usage text of harness lesson names the verb
    let o = run(&["lesson"]);
    assert!(stderr(&o).contains("lesson replay <lessons-dir> --tickets"), "{}", stderr(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn replay_a_missing_folder_exits_2() {
    for gone in ["lessons", "tickets", "scopes"] {
        let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
        fs::remove_dir_all(d.join(gone)).unwrap();
        let o = replay_in(&d);
        assert_eq!(o.status.code(), Some(2), "{}", gone);
        assert_eq!(stdout(&o), "", "{}", gone);
        assert!(stderr(&o).starts_with("harness: error: "), "{}: {}", gone, stderr(&o));
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn replay_a_tickets_folder_with_no_ticket_file_exits_2() {
    for tickets in [vec![("README.md", "x".to_string())], vec![]] {
        let d = rsetup(one_page().as_slice(), tickets.as_slice(), &[("aa-1.paths", "src/\n".to_string())]);
        let o = replay_in(&d);
        assert_eq!(o.status.code(), Some(2), "{:?}", tickets);
        assert_eq!(stdout(&o), "");
        assert!(stderr(&o).contains("holds no ticket file"), "{}", stderr(&o));
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn replay_no_ticket_left_after_the_skip_exits_2() {
    let d = rsetup(
        one_page().as_slice(),
        &[("aa-1.json", hello_ticket()), ("bb-2.json", other_ticket())],
        &[],
    );
    let o = replay_in(&d);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    assert_eq!(stdout(&o), "");
    assert!(stderr(&o).contains("no ticket left"), "{}", stderr(&o));
    let _ = fs::remove_dir_all(&d);
}

// ---- read only, same answer twice ----

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

fn snap(d: &std::path::Path) -> Vec<String> {
    let mut v = Vec::new();
    listing(d, &mut v);
    v
}

/// Run the replay in `cwd`: the three folders stay byte-identical, nothing is created in them or in `cwd`.
fn run_read_only(d: &std::path::Path, want: i32) -> Output {
    let before = snap(d);
    let (l, t, s) = (d.join("lessons"), d.join("tickets"), d.join("scopes"));
    let o = Command::new(BIN)
        .args(["lesson", "replay", l.to_str().unwrap(), "--tickets", t.to_str().unwrap(), "--scopes", s.to_str().unwrap()])
        .current_dir(d.join("cwd"))
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(want), "{}{}", stdout(&o), stderr(&o));
    assert_eq!(before, snap(d), "the folders changed");
    assert_eq!(fs::read_dir(d.join("cwd")).unwrap().count(), 0, "the current folder gained a file");
    o
}

#[test]
fn replay_writes_nothing_and_prints_the_same_twice() {
    // a passing run
    let d = fixture();
    let first = run_read_only(&d, 0);
    assert_eq!(stdout(&first), FIXTURE_OUT);
    let second = replay_in(&d);
    assert_eq!(stdout(&second), stdout(&first), "the same input twice");
    let _ = fs::remove_dir_all(&d);

    // one refusal per exit code: a refused page (1), a refused ticket file (1), a refused lessons walk (2)
    let d = rsetup(
        &[("ISSUE-901-a.md", "no front matter\n".to_string())],
        &[("aa-1.json", hello_ticket())],
        &[("aa-1.paths", "src/\n".to_string())],
    );
    run_read_only(&d, 1);
    let _ = fs::remove_dir_all(&d);

    let d = rsetup(
        one_page().as_slice(),
        &[("aa-1.json", "{\"no\": \"ticket\"}".to_string())],
        &[("aa-1.paths", "src/\n".to_string())],
    );
    run_read_only(&d, 1);
    let _ = fs::remove_dir_all(&d);

    let d = rsetup(one_page().as_slice(), &[("aa-1.json", hello_ticket())], &[("aa-1.paths", "src/\n".to_string())]);
    fs::write(d.join("outside.md"), fm("match: [example]")).unwrap();
    link(&d.join("outside.md"), &d.join("lessons/ISSUE-902-b.md"));
    run_read_only(&d, 2);
    let _ = fs::remove_dir_all(&d);
}
