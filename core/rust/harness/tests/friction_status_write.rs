//! Ticket friction-status-write: `harness friction set-status`, the verb that rewrites a line of the inbox in
//! place, and the inbox lock the readers now share. The real binary runs as a child process on scratch inboxes.
//! The fixture (neighbour rows that are valid but not canonical, rows that carry a status) is built here; the
//! helpers and the neighbour verb run of `friction_status.rs` are reused through the `status` module. The rows,
//! pointers and deny words are synthetic. The tests hold the inbox lock with std's `File::lock` on their own open of
//! the inbox (the crate's `open_locked` is not public).
#![cfg(unix)]
#[path = "friction_status.rs"]
#[allow(dead_code, unused_imports)]
mod status;
use harness::friction::{encode_row_status, parse_row_status, Status};
use status::*;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Output, Stdio};
use std::time::{Duration, Instant};

const INBOX_CAP: usize = 4 * 1024 * 1024;

// ---------------------------------------------------------------- the fixture

/// A valid row in another key order, with inner and trailing spaces (not canonical).
fn noncanon(id: &str) -> String {
    format!("{{ \"text\" : \"a  lesson\" , \"id\":\"{id}\", \"date\": \"2026-11-01\",\"ticket\":\"t-1\", \"where-it-lives\":\"memory\" , \"breaks\":\"\" }}  ")
}

/// Eight rows: three targets (first, middle, last) and neighbours that are not canonical or carry a status.
/// Index: 0 a-1 (old hand row, a why with no cause tag), 1 n-1 (not canonical), 2 n-2 (explicit kind friction),
/// 3 p-1 (kind pattern), 4 n-3 (a status, status first), 5 the event row, 6 n-4 (guarded), 7 z-1 (last, fixed).
fn fixture_lines() -> Vec<String> {
    vec![
        row("a-1", "2026-11-01", "", "old hand", &[("why", "just because")]),
        noncanon("n-1"),
        row("n-2", "2026-11-01", "", "explicit", &[("kind", "friction")]),
        row("p-1", "2026-11-02", "", "procedure", &[("kind", "pattern")]),
        format!("{{\"status\":\"fixed\",\"status_ref\":\"{FIXED}\",\"id\":\"n-3\",\"date\":\"2026-11-02\",\"ticket\":\"t-1\",\"where-it-lives\":\"memory\",\"breaks\":\"\",\"text\":\"first status\"}}"),
        event("sig-a", "2026-w45", "2026-11-02", "high"),
        row("n-4", "2026-11-03", "", "guarded one", &[("status", "guarded"), ("status_ref", GUARDED)]),
        row("z-1", "2026-11-03", "", "last", &[("why", "no tag here"), ("status", "fixed"), ("status_ref", FIXED)]),
    ]
}

fn body(lines: &[String]) -> String {
    lines.iter().map(|l| format!("{l}\n")).collect()
}

fn fixture() -> Tmp {
    let t = Tmp::new();
    fs::write(t.inbox(), body(&fixture_lines())).unwrap();
    t
}

fn st(status: &str, r: &str) -> Status {
    Status { status: status.to_string(), status_ref: r.to_string() }
}

/// The inbox the verb must leave: line `i` re-encoded with the new status, every other line its original bytes.
fn expect_after(lines: &[String], i: usize, new: &Status) -> String {
    let (r, _) = parse_row_status(&lines[i]).unwrap();
    let mut v = lines.to_vec();
    v[i] = encode_row_status(&r, new).unwrap();
    body(&v)
}

fn set_args<'a>(cfg: &'a Path, id: &'a str, set: &'a str, r: Option<&'a str>) -> Vec<&'a str> {
    let mut a = vec!["friction", "set-status", "--id", id, "--set", set];
    if let Some(r) = r {
        a.push("--ref");
        a.push(r);
    }
    a.push("--config");
    a.push(p(cfg));
    a
}

fn set(t: &Tmp, id: &str, set: &str, r: Option<&str>) -> Output {
    let cfg = t.config();
    run(&set_args(&cfg, id, set, r))
}

fn spawn(args: &[&str]) -> Child {
    let mut c = base();
    c.args(args).env("HARNESS_PRIVACY_LIST_SECRET", DENY).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    c.spawn().unwrap()
}

/// The child is still waiting for the lock after at least 300 ms. It is polled for a full second, so a child that
/// started late under load and then finished (no lock taken) is still caught.
fn still_waiting(child: &mut Child, what: &str) {
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(100));
        assert!(child.try_wait().unwrap().is_none(), "{what} did not wait for the lock");
    }
}

fn inbox_bytes(t: &Tmp) -> Vec<u8> {
    fs::read(t.inbox()).unwrap()
}

fn copy_path(t: &Tmp) -> PathBuf {
    PathBuf::from(format!("{}.status-write", p(&t.inbox())))
}

/// A config whose inbox is spelled another way (a link to the inbox, or a path through a linked folder).
fn config_spelled(t: &Tmp, name: &str, spelled: &Path) -> PathBuf {
    let text = fs::read_to_string(t.config()).unwrap().replace(p(&t.inbox()), p(spelled));
    assert!(text.contains(p(spelled)));
    t.file(name, text)
}

/// True when this process can write into a folder whose mode forbids it (root): the permission tests stand down.
fn permissions_ignored(t: &Tmp) -> bool {
    let d = t.0.join("probe-dir");
    fs::create_dir_all(&d).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o555)).unwrap();
    let wrote = fs::write(d.join("x"), b"x").is_ok();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o755)).unwrap();
    wrote
}

/// A refusal: exit 1, the inbox byte-identical, stdout empty, one stderr line that says the status was not set and
/// names `needle`, and no recovery copy left. It carries no deny word and no path.
fn refused(t: &Tmp, before: &[u8], o: &Output, needle: &str) {
    assert_eq!(code(o), 1, "{}", err(o));
    assert!(out(o).is_empty(), "{}", out(o));
    let e = err(o);
    assert_eq!(e.lines().count(), 1, "{e}");
    assert!(e.contains("the status was not set") && e.contains(needle), "{e}");
    assert!(!e.contains(DENY) && !e.contains(p(&t.0)), "{e}");
    assert_eq!(inbox_bytes(t), before, "the inbox changed");
    assert!(!copy_path(t).exists());
}

// ---------------------------------------------------------------- the write itself

#[test]
fn one_row_changes_and_every_other_line_keeps_its_bytes() {
    let lines = fixture_lines();
    // (index, id, new status, pointer, the line grows?)
    let cases: [(usize, &str, &str, Option<&str>, i32); 7] = [
        (0, "a-1", "fixed", Some(FIXED), 1),
        (3, "p-1", "guarded", Some(GUARDED), 1),
        (5, "sig-a.2026-w45", "ticket", Some(TICKET), 1),
        (7, "z-1", "open", None, -1),
        (4, "n-3", "no-action", Some(NOACT), 0),
        (1, "n-1", "lesson", Some(LESSON), 1),
        (6, "n-4", "open", None, -1),
    ];
    for (i, id, word, r, dir) in cases {
        let t = fixture();
        let o = set(&t, id, word, r);
        assert_eq!(code(&o), 0, "{id}: {}", err(&o));
        assert_eq!(out(&o), format!("status set id={id} status={word}\n"), "{id}");
        assert!(err(&o).is_empty(), "{id}: {}", err(&o));
        let new = if word == "open" { Status::default() } else { st(word, r.unwrap()) };
        let want = expect_after(&lines, i, &new);
        let got = String::from_utf8(inbox_bytes(&t)).unwrap();
        assert_eq!(got, want, "{id}");
        let before_len = body(&lines).len();
        match dir {
            1 => assert!(got.len() > before_len, "{id} should grow"),
            -1 => assert!(got.len() < before_len, "{id} should shrink"),
            _ => {}
        }
        assert!(!copy_path(&t).exists(), "{id}: the recovery copy is gone after a good write");
        let o = run(&["friction", "lint", "--config", p(&t.config())]);
        assert_eq!(out(&o), "inbox ok rows=8\n", "{id}: {}", err(&o));
    }
}

#[test]
fn a_set_equal_to_the_stored_values_writes_nothing() {
    for (id, word, r) in [("n-3", "fixed", Some(FIXED)), ("n-1", "open", None), ("a-1", "open", None), ("n-4", "guarded", Some(GUARDED))] {
        let t = fixture();
        let before = inbox_bytes(&t);
        let o = set(&t, id, word, r);
        assert_eq!(code(&o), 0, "{id}: {}", err(&o));
        assert_eq!(out(&o), format!("status set id={id} status={word}\n"));
        assert_eq!(inbox_bytes(&t), before, "{id}");
    }
}

#[test]
fn the_same_status_with_another_pointer_replaces_the_pointer_only() {
    let lines = fixture_lines();
    let t = fixture();
    let o = set(&t, "n-3", "fixed", Some("pr:owner/repo#202"));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(String::from_utf8(inbox_bytes(&t)).unwrap(), expect_after(&lines, 4, &st("fixed", "pr:owner/repo#202")));
}

// ---------------------------------------------------------------- refusals (exit 1, the inbox untouched)

#[test]
fn no_row_with_that_id_is_refused() {
    let t = fixture();
    let before = inbox_bytes(&t);
    refused(&t, &before, &set(&t, "no-such-row", "fixed", Some(FIXED)), "no row with that id");
    assert!(!String::from_utf8_lossy(&set(&t, "no-such-row", "open", None).stderr).contains("no-such-row"));
}

#[test]
fn an_id_that_is_not_a_plain_token_is_refused_and_not_echoed() {
    let t = fixture();
    let before = inbox_bytes(&t);
    for bad in ["Bad_ID", "../x", "", "a b", "."] {
        let o = set(&t, bad, "fixed", Some(FIXED));
        refused(&t, &before, &o, "--id is not a plain token");
        if !bad.is_empty() {
            assert!(!err(&o).contains(bad), "{}", err(&o));
        }
    }
}

#[test]
fn a_missing_inbox_is_refused_and_not_created() {
    let t = Tmp::new();
    let o = set(&t, "a-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("no row with that id") && err(&o).contains("the status was not set"), "{}", err(&o));
    assert_eq!(err(&o).lines().count(), 1);
    assert!(!t.inbox().exists(), "set-status must not create the inbox");
}

#[test]
fn a_damaged_inbox_is_refused_by_the_same_read_as_add() {
    let lines = fixture_lines();
    // a torn tail
    let t = fixture();
    let mut b = inbox_bytes(&t);
    b.extend_from_slice(b"{\"id\":\"half");
    fs::write(t.inbox(), &b).unwrap();
    refused(&t, &b, &set(&t, "a-1", "fixed", Some(FIXED)), "torn tail at line 9");
    // a bad line
    let mut bad = lines.clone();
    bad[2] = "not json".to_string();
    fs::write(t.inbox(), body(&bad)).unwrap();
    let b = inbox_bytes(&t);
    refused(&t, &b, &set(&t, "a-1", "fixed", Some(FIXED)), "line 3");
    // a CRLF line
    let mut crlf = body(&lines);
    crlf = crlf.replacen("\n", "\r\n", 1);
    fs::write(t.inbox(), &crlf).unwrap();
    let b = inbox_bytes(&t);
    refused(&t, &b, &set(&t, "p-1", "fixed", Some(FIXED)), "line 1");
    // a duplicate id
    let mut dup = lines.clone();
    dup.push(lines[0].clone());
    fs::write(t.inbox(), body(&dup)).unwrap();
    let b = inbox_bytes(&t);
    let o = set(&t, "p-1", "fixed", Some(FIXED));
    refused(&t, &b, &o, "line 9");
    assert!(err(&o).contains("duplicate"), "{}", err(&o));
    // a stored privacy hit, in a row other than the target
    let mut hit = lines.clone();
    hit[1] = row("n-1", "2026-11-01", "", &format!("has {DENY} inside"), &[]);
    fs::write(t.inbox(), body(&hit)).unwrap();
    let b = inbox_bytes(&t);
    let o = set(&t, "p-1", "fixed", Some(FIXED));
    refused(&t, &b, &o, "field text has a privacy hit");
    assert!(err(&o).contains("line 2"), "{}", err(&o));
}

#[test]
fn a_privacy_hit_in_the_new_ref_is_refused_by_class_and_never_echoed() {
    let t = fixture();
    let before = inbox_bytes(&t);
    let r = format!("cmd:{DENY} is the reason");
    let o = set(&t, "a-1", "no-action", Some(&r));
    refused(&t, &before, &o, "field status_ref has a privacy hit");
    // a hit also wins over a pointer the status rules would refuse: the scan comes first
    let o = set(&t, "a-1", "fixed", Some(&format!("session:{DENY}")));
    refused(&t, &before, &o, "field status_ref has a privacy hit");
}

#[test]
fn a_pair_the_status_rules_refuse_is_refused() {
    let t = fixture();
    let before = inbox_bytes(&t);
    for (word, r, needle) in [
        ("fixed", Some("cmd:x"), "status_ref"),
        ("fixed", None, "status_ref is missing"),
        ("guarded", Some(FIXED), "status_ref"),
        ("ticket", Some(LESSON), "status_ref"),
        ("no-action", Some(FIXED), "status_ref"),
        ("done", Some(FIXED), "--set must be"),
        ("", None, "--set must be"),
        ("open", Some(FIXED), "--ref is not allowed with --set open"),
        ("open", Some(""), "--ref is not allowed with --set open"),
    ] {
        let o = set(&t, "a-1", word, r);
        refused(&t, &before, &o, needle);
        if let Some(r) = r.filter(|r| !r.is_empty()) {
            assert!(!err(&o).contains(r), "the ref is echoed: {}", err(&o));
        }
    }
}

#[test]
fn a_new_line_over_the_line_cap_is_refused() {
    let t = Tmp::new();
    let text = "t".repeat(14_500);
    t.write_inbox(&[row("big-1", "2026-11-02", "", &text, &[]), hand("s-2", "2026-11-02", "", "")]);
    let before = inbox_bytes(&t);
    let r = format!("cmd:{}", "x".repeat(1996));
    refused(&t, &before, &set(&t, "big-1", "no-action", Some(&r)), "cap");
    // a short pointer fits
    let o = set(&t, "big-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 0, "{}", err(&o));
}

/// An inbox of `target` plus fat rows, `CAP - 100` bytes in all.
fn near_cap(t: &Tmp) -> Vec<String> {
    let tgt = hand("tg-1", "2026-11-02", "", "");
    let rem = INBOX_CAP - 100 - (tgt.len() + 1);
    let mut n = rem / 16_000;
    let mut pad = rem - n * 16_000;
    if pad < 200 {
        n -= 1;
        pad += 16_000;
    }
    let fat = |id: &str, total: usize| {
        let base = row(id, "2026-11-02", "", "", &[]).len() + 1;
        row(id, "2026-11-02", "", &"x".repeat(total - base), &[])
    };
    let mut lines = vec![tgt];
    for i in 0..n {
        lines.push(fat(&format!("f-{i}"), 16_000));
    }
    lines.push(fat("pad-1", pad));
    t.write_inbox(&lines);
    assert_eq!(inbox_bytes(t).len(), INBOX_CAP - 100);
    lines
}

#[test]
fn a_new_inbox_over_the_inbox_cap_is_refused_and_one_at_the_edge_is_kept() {
    let t = Tmp::new();
    near_cap(&t);
    let before = inbox_bytes(&t);
    let big = format!("cmd:{}", "x".repeat(400));
    refused(&t, &before, &set(&t, "tg-1", "no-action", Some(&big)), "cap");
    // the same inbox grown by less than 100 bytes is written
    let o = set(&t, "tg-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(inbox_bytes(&t).len() <= INBOX_CAP);
}

#[test]
fn a_hard_linked_inbox_is_refused_and_both_names_keep_their_bytes() {
    let t = fixture();
    let alias = t.0.join("alias.jsonl");
    fs::hard_link(t.inbox(), &alias).unwrap();
    let before = inbox_bytes(&t);
    refused(&t, &before, &set(&t, "a-1", "fixed", Some(FIXED)), "hard link");
    assert_eq!(fs::read(&alias).unwrap(), before);
}

// ---------------------------------------------------------------- setup and usage errors (exit 2)

fn exit_two_unchanged(t: &Tmp, o: &Output, before: &[u8]) {
    assert_eq!(code(o), 2, "{}", err(o));
    assert!(out(o).is_empty(), "{}", out(o));
    assert_eq!(inbox_bytes(t), before);
    assert!(!copy_path(t).exists(), "no recovery copy may be left by a setup error");
}

#[test]
fn setup_errors_exit_two_and_leave_the_inbox_alone() {
    let t = fixture();
    let before = inbox_bytes(&t);
    let cfg = t.config();
    // no config
    let o = run(&["friction", "set-status", "--id", "a-1", "--set", "fixed", "--ref", FIXED]);
    exit_two_unchanged(&t, &o, &before);
    // no privacy list
    let mut c = base();
    c.args(set_args(&cfg, "a-1", "fixed", Some(FIXED))).stdin(Stdio::null());
    let o = c.output().unwrap();
    exit_two_unchanged(&t, &o, &before);
    // over the cap at read
    let big = vec![b'x'; INBOX_CAP + 1];
    fs::write(t.inbox(), &big).unwrap();
    exit_two_unchanged(&t, &set(&t, "a-1", "fixed", Some(FIXED)), &big);
    // not utf-8
    let bad = vec![0xff, b'\n'];
    fs::write(t.inbox(), &bad).unwrap();
    exit_two_unchanged(&t, &set(&t, "a-1", "fixed", Some(FIXED)), &bad);
    // the inbox path is a folder
    fs::remove_file(t.inbox()).unwrap();
    fs::create_dir(t.inbox()).unwrap();
    let o = set(&t, "a-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty());
}

#[test]
fn a_read_only_inbox_and_a_folder_that_is_not_writable_exit_two() {
    let t = fixture();
    if permissions_ignored(&t) {
        return;
    }
    let before = inbox_bytes(&t);
    let _ = t.config();
    // a read-only file
    fs::set_permissions(t.inbox(), fs::Permissions::from_mode(0o444)).unwrap();
    exit_two_unchanged(&t, &set(&t, "a-1", "fixed", Some(FIXED)), &before);
    fs::set_permissions(t.inbox(), fs::Permissions::from_mode(0o644)).unwrap();
    // a folder that is not writable: the recovery copy cannot be made, so nothing is written
    let cfg = t.config();
    fs::set_permissions(&t.0, fs::Permissions::from_mode(0o555)).unwrap();
    let o = run(&set_args(&cfg, "a-1", "fixed", Some(FIXED)));
    fs::set_permissions(&t.0, fs::Permissions::from_mode(0o755)).unwrap();
    exit_two_unchanged(&t, &o, &before);
}

#[test]
fn usage_errors_exit_two_with_the_usage_line() {
    let t = fixture();
    let before = inbox_bytes(&t);
    let cfg = t.config();
    let c = p(&cfg);
    let cases: Vec<Vec<&str>> = vec![
        vec!["friction", "set-status", "--set", "fixed", "--ref", FIXED, "--config", c],
        vec!["friction", "set-status", "--id", "a-1", "--ref", FIXED, "--config", c],
        vec!["friction", "set-status", "--id", "a-1", "--set", "fixed", "--bogus", "x", "--config", c],
        vec!["friction", "set-status", "--id", "a-1", "--id", "a-1", "--set", "open", "--config", c],
        vec!["friction", "set-status", "--id", "a-1", "--set", "open", "--set", "open", "--config", c],
        vec!["friction", "set-status", "--id", "a-1", "--set", "open", "--config", c, "--config", c],
        vec!["friction", "set-status", "--id", "a-1", "--set", "open", "extra", "--config", c],
        vec!["friction", "set-status", "--id", "a-1", "--set", "open", "--config"],
        vec!["friction", "set-status"],
    ];
    for args in cases {
        let o = run(&args);
        assert_eq!(code(&o), 2, "{args:?}: {}", err(&o));
        assert!(out(&o).is_empty());
        assert!(err(&o).starts_with("usage: harness friction"), "{args:?}: {}", err(&o));
        assert_eq!(inbox_bytes(&t), before);
    }
}

// ---------------------------------------------------------------- the lock (a)-(d)

#[test]
fn the_write_keeps_the_inode_and_follows_a_symlink() {
    let lines = fixture_lines();
    let t = fixture();
    let m0 = fs::metadata(t.inbox()).unwrap();
    let o = set(&t, "a-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let m1 = fs::metadata(t.inbox()).unwrap();
    assert_eq!((m0.dev(), m0.ino()), (m1.dev(), m1.ino()), "the inbox was replaced, not rewritten");
    // a symlinked file spelling of the inbox path: the real file gets the new line
    let link = t.0.join("link.jsonl");
    std::os::unix::fs::symlink(t.inbox(), &link).unwrap();
    let cfg = config_spelled(&t, "link-config.yaml", &link);
    let o = run(&set_args(&cfg, "p-1", "guarded", Some(GUARDED)));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "the link is still a link");
    let mut want = lines.clone();
    let (r0, _) = parse_row_status(&want[0]).unwrap();
    want[0] = encode_row_status(&r0, &st("fixed", FIXED)).unwrap();
    let (r3, _) = parse_row_status(&want[3]).unwrap();
    want[3] = encode_row_status(&r3, &st("guarded", GUARDED)).unwrap();
    assert_eq!(String::from_utf8(inbox_bytes(&t)).unwrap(), body(&want));
    assert_eq!(fs::read_to_string(&link).unwrap(), body(&want));
    let m2 = fs::metadata(t.inbox()).unwrap();
    assert_eq!(m1.ino(), m2.ino());
}

#[test]
fn a_set_status_waits_for_the_writer_that_holds_the_lock_and_then_works_on_the_new_bytes() {
    let lines = fixture_lines();
    let t = fixture();
    let before = inbox_bytes(&t);
    let cfg = t.config();
    let mut h = fs::OpenOptions::new().read(true).append(true).open(t.inbox()).unwrap();
    h.lock().unwrap();
    let mut child = spawn(&set_args(&cfg, "a-1", "fixed", Some(FIXED)));
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(inbox_bytes(&t), before, "the verb wrote while the lock was held");
    still_waiting(&mut child, "set-status");
    assert_eq!(inbox_bytes(&t), before, "the verb wrote while the lock was held");
    let late = row("late-1", "2026-11-05", "", "late", &[]);
    h.write_all(format!("{late}\n").as_bytes()).unwrap();
    h.sync_all().unwrap();
    h.unlock().unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 0, "{}", err(&o));
    let want = format!("{}{late}\n", expect_after(&lines, 0, &st("fixed", FIXED)));
    assert_eq!(String::from_utf8(inbox_bytes(&t)).unwrap(), want, "the appended row or the new status is missing");
}

#[test]
fn a_lock_held_past_the_wait_makes_set_status_exit_two_busy() {
    let t = fixture();
    let before = inbox_bytes(&t);
    let h = fs::OpenOptions::new().read(true).append(true).open(t.inbox()).unwrap();
    h.lock().unwrap();
    let started = Instant::now();
    let o = set(&t, "a-1", "fixed", Some(FIXED));
    assert!(started.elapsed() >= Duration::from_secs(9), "the verb gave up early");
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(err(&o).contains("the inbox is busy"), "{}", err(&o));
    assert!(out(&o).is_empty());
    assert_eq!(inbox_bytes(&t), before);
    h.unlock().unwrap();
    assert!(!copy_path(&t).exists());
}

#[test]
fn a_reader_waits_for_the_writer_lock_and_then_reads_the_whole_inbox() {
    let t = fixture();
    let cfg = t.config();
    let mut h = fs::OpenOptions::new().read(true).append(true).open(t.inbox()).unwrap();
    h.lock().unwrap();
    let mut child = spawn(&["friction", "lint", "--config", p(&cfg)]);
    std::thread::sleep(Duration::from_millis(300));
    still_waiting(&mut child, "lint");
    h.write_all(format!("{}\n", row("late-1", "2026-11-05", "", "late", &[])).as_bytes()).unwrap();
    h.unlock().unwrap();
    let o = child.wait_with_output().unwrap();
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "inbox ok rows=9\n");
}

#[test]
fn readers_share_the_lock_with_each_other() {
    let t = fixture();
    let cfg = t.config();
    let h = fs::File::open(t.inbox()).unwrap();
    h.lock_shared().unwrap();
    let started = Instant::now();
    let o = run(&["friction", "lint", "--config", p(&cfg)]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(started.elapsed() < Duration::from_secs(5));
    h.unlock().unwrap();
}

// ---------------------------------------------------------------- the recovery copy (e), (g), (h)

/// A scratch folder with the files the fourteen verbs below need.
struct Verbs {
    t: Tmp,
}

impl Verbs {
    fn new() -> Verbs {
        let t = Tmp::new();
        t.write_inbox(&plain_or_statused(false));
        Verbs { t }
    }

    /// (name, args, the exit code a refusal gives): fourteen verbs, three that write (set-status, add, sweep) and
    /// eleven that read.
    fn calls(&self) -> Vec<(&'static str, Vec<String>, i32)> {
        let t = &self.t;
        let cfg = p(&t.config()).to_string();
        let note = p(&t.file("note.md", "lesson: none\n")).to_string();
        let controls = p(&t.file("controls.txt", "g-1 | control: a guard pointer | lesson: ISSUE-101\n")).to_string();
        let lessons = t.0.join("lessons");
        fs::create_dir_all(&lessons).unwrap();
        fs::write(lessons.join("ISSUE-101-a.md"), "---\nmatch: [example]\n---\n").unwrap();
        let tickets = t.0.join("tickets");
        fs::create_dir_all(&tickets).unwrap();
        fs::write(
            tickets.join("aa-1.json"),
            "{\"id\":\"T-1\",\"repo_id\":\"example\",\"title\":\"Say hello\",\"group\":\"grp\",\"size\":\"S\",\"objective\":\"obj\",\"acceptance_criteria\":[\"checks/hello.sh passes\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}",
        )
        .unwrap();
        let scopes = t.0.join("scopes");
        fs::create_dir_all(&scopes).unwrap();
        fs::write(scopes.join("aa-1.paths"), "src/\n").unwrap();
        let log = p(&t.file("events.jsonl", format!("{}\n", "{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":1794182400000,\"run_id\":\"run-1\",\"ticket\":\"193\",\"repo\":\"example-repo\",\"reason\":\"replan-threshold\",\"role\":\"executor\",\"model\":\"worker\"}"))).to_string();
        let state = p(&t.file("state.jsonl", "")).to_string();
        let handoff = p(&t.handoff()).to_string();
        let out_file = p(&t.0.join("page.md")).to_string();
        let s = |v: &str| v.to_string();
        vec![
            ("set-status", vec![s("friction"), s("set-status"), s("--id"), s("g-1"), s("--set"), s("open"), s("--config"), cfg.clone()], 1),
            (
                "add",
                ["friction", "add", "--id", "new-1", "--date", "2026-11-05", "--ticket", "t-1", "--where-it-lives", "memory", "--why", "cause: system", "--text", "x", "--config", cfg.as_str()]
                    .iter()
                    .map(|a| s(a))
                    .collect(),
                1,
            ),
            ("sweep", vec![s("friction"), s("sweep"), s("--log"), log, s("--week"), s("2026-w46"), s("--config"), cfg.clone()], 1),
            ("lint", vec![s("friction"), s("lint"), s("--config"), cfg.clone()], 1),
            ("open", vec![s("friction"), s("open"), s("--config"), cfg.clone()], 1),
            ("patterns", vec![s("friction"), s("patterns"), s("--config"), cfg.clone()], 1),
            ("close-check", vec![s("friction"), s("close-check"), note, s("--ticket"), s("t-1"), s("--config"), cfg.clone()], 2),
            ("rank", vec![s("friction"), s("rank"), s("--config"), cfg.clone()], 1),
            (
                "triage",
                ["friction", "triage", "--week", "2026-w46", "--state", state.as_str(), "--handoff-dir", handoff.as_str(), "--as-of", "2026-11-12T00:00:00Z", "--config", cfg.as_str()]
                    .iter()
                    .map(|a| s(a))
                    .collect(),
                1,
            ),
            (
                "page",
                ["friction", "page", "--week", "2026-w46", "--state", state.as_str(), "--out", out_file.as_str(), "--config", cfg.as_str()].iter().map(|a| s(a)).collect(),
                1,
            ),
            (
                "controls-check",
                ["friction", "controls-check", "--controls", controls.as_str(), "--lessons-dir", p(&lessons), "--config", cfg.as_str()].iter().map(|a| s(a)).collect(),
                1,
            ),
            (
                "bind",
                ["friction", "bind", "--state", state.as_str(), "--group", "g-1", "--task-id", "task-1", "--task-session", "sess-1", "--config", cfg.as_str()].iter().map(|a| s(a)).collect(),
                1,
            ),
            ("suggest", vec![s("friction"), s("suggest"), s("--text"), s("lesson"), s("--config"), cfg.clone()], 1),
            (
                "lesson replay",
                [
                    "lesson",
                    "replay",
                    p(&lessons),
                    "--tickets",
                    p(&tickets),
                    "--scopes",
                    p(&scopes),
                    "--config",
                    cfg.as_str(),
                    "--controls",
                    controls.as_str(),
                ]
                .iter()
                .map(|a| s(a))
                .collect(),
                1,
            ),
        ]
    }
}

#[test]
fn a_planted_recovery_copy_makes_the_fourteen_listed_verbs_refuse_with_their_damaged_inbox_code() {
    // The calls are well formed: on a scratch folder with no copy, none of them prints the recovery-copy line
    // (this loop does not check their exit codes).
    let plain = Verbs::new();
    for (name, args, _) in &plain.calls() {
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let o = run(&a);
        assert!(!err(&o).contains("recovery copy"), "{name}: {}", err(&o));
    }
    let v = Verbs::new();
    let calls = v.calls();
    fs::write(copy_path(&v.t), b"a partial copy").unwrap();
    let before = inbox_bytes(&v.t);
    for (name, args, want) in &calls {
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let o = run(&a);
        assert_eq!(code(&o), *want, "{name}: {}", err(&o));
        assert!(out(&o).is_empty(), "{name}: stdout {}", out(&o));
        let e = err(&o);
        assert_eq!(e.lines().count(), 1, "{name}: {e}");
        assert!(e.contains("status-write recovery copy"), "{name}: {e}");
        assert!(!e.contains(p(&v.t.0)), "{name}: a path leaked: {e}");
        assert_eq!(inbox_bytes(&v.t), before, "{name}");
    }
    assert_eq!(fs::read(copy_path(&v.t)).unwrap(), b"a partial copy", "the copy is left as it was");
}

#[test]
fn a_recovery_copy_beside_a_missing_inbox_stops_close_check() {
    let t = Tmp::new();
    fs::write(copy_path(&t), b"x").unwrap();
    let note = t.file("note.md", "lesson: none\n");
    let o = run(&["friction", "close-check", p(&note), "--ticket", "t-1", "--config", p(&t.config())]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(err(&o).contains("status-write recovery copy") && out(&o).is_empty(), "{}", err(&o));
}

/// A recovery copy planted beside an inbox that does not exist: the verb gives the one recovery line and the exit
/// code it gives with the inbox present, the copy is left as it was and no inbox file is made.
fn recovery_beside_missing_inbox(name: &str, want: i32, build: impl Fn(&Tmp, &str) -> Vec<String>) {
    let t = Tmp::new();
    fs::write(copy_path(&t), b"a partial copy").unwrap();
    let cfg = t.config();
    let args = build(&t, p(&cfg));
    let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let o = run(&a);
    assert_eq!(code(&o), want, "{name}: {}", err(&o));
    assert!(out(&o).is_empty(), "{name}: stdout {}", out(&o));
    let e = err(&o);
    assert_eq!(e.lines().count(), 1, "{name}: {e}");
    assert!(e.contains("status-write recovery copy"), "{name}: {e}");
    assert!(!e.contains(p(&t.0)), "{name}: a path leaked: {e}");
    assert!(!t.inbox().exists(), "{name}: the inbox was created");
    assert_eq!(fs::read(copy_path(&t)).unwrap(), b"a partial copy", "{name}: the copy changed");
}

fn strings(a: &[&str]) -> Vec<String> {
    a.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_read_verb_with_a_recovery_copy_and_no_inbox_gives_the_recovery_line() {
    recovery_beside_missing_inbox("lint", 1, |_, c| strings(&["friction", "lint", "--config", c]));
    recovery_beside_missing_inbox("open", 1, |_, c| strings(&["friction", "open", "--config", c]));
    recovery_beside_missing_inbox("patterns", 1, |_, c| strings(&["friction", "patterns", "--config", c]));
    recovery_beside_missing_inbox("rank", 1, |_, c| strings(&["friction", "rank", "--config", c]));
    recovery_beside_missing_inbox("set-status", 1, |_, c| strings(&["friction", "set-status", "--id", "a-1", "--set", "open", "--config", c]));
    recovery_beside_missing_inbox("close-check", 2, |t, c| {
        let note = p(&t.file("note.md", "lesson: none\n")).to_string();
        strings(&["friction", "close-check", &note, "--ticket", "t-1", "--config", c])
    });
}

#[test]
fn add_with_a_recovery_copy_and_no_inbox_gives_the_recovery_line_and_creates_no_inbox() {
    recovery_beside_missing_inbox("add", 1, |_, c| {
        strings(&["friction", "add", "--id", "new-1", "--date", "2026-11-05", "--ticket", "t-1", "--where-it-lives", "memory", "--why", "cause: system", "--text", "x", "--config", c])
    });
}

#[test]
fn sweep_with_a_recovery_copy_and_no_inbox_gives_the_recovery_line_and_creates_no_inbox() {
    recovery_beside_missing_inbox("sweep", 1, |t, c| {
        let log = t.file("events.jsonl", "{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":1794182400000,\"run_id\":\"run-1\",\"ticket\":\"193\",\"repo\":\"example-repo\",\"reason\":\"replan-threshold\",\"role\":\"executor\",\"model\":\"worker\"}\n");
        strings(&["friction", "sweep", "--log", p(&log), "--week", "2026-w46", "--config", c])
    });
}

#[test]
fn the_recovery_copy_is_found_through_every_spelling_of_the_inbox_path() {
    let t = fixture();
    // The real file lives in `real/`; the copy is planted beside it.
    let real = t.0.join("real");
    fs::create_dir_all(&real).unwrap();
    fs::rename(t.inbox(), real.join("inbox.jsonl")).unwrap();
    fs::write(real.join("inbox.jsonl.status-write"), b"x").unwrap();
    // a symlinked file spelling (the link sits in another folder than the file)
    let link = t.0.join("link.jsonl");
    std::os::unix::fs::symlink(real.join("inbox.jsonl"), &link).unwrap();
    // a symlinked folder spelling
    let dlink = t.0.join("dlink");
    std::os::unix::fs::symlink(&real, &dlink).unwrap();
    let text = fs::read_to_string(t.config()).unwrap().replace(p(&t.inbox()), p(&dlink.join("inbox.jsonl")));
    let cfg_folder = t.file("folder-config.yaml", text);
    let text = fs::read_to_string(t.config()).unwrap().replace(p(&t.inbox()), p(&link));
    let cfg_file = t.file("file-config.yaml", text);
    let text = fs::read_to_string(t.config()).unwrap().replace(p(&t.inbox()), p(&real.join("inbox.jsonl")));
    let cfg_real = t.file("real-config.yaml", text);
    for (name, cfg) in [("folder link", &cfg_folder), ("file link", &cfg_file), ("real path", &cfg_real)] {
        let o = run(&["friction", "lint", "--config", p(cfg)]);
        assert_eq!(code(&o), 1, "{name}: {}", err(&o));
        assert!(err(&o).contains("status-write recovery copy"), "{name}: {}", err(&o));
    }
}

/// Bind for a group that already has a state row reads no inbox, but it still refuses while a recovery copy sits
/// beside the inbox, and writes nothing. The state row comes from a real triage run.
#[test]
fn bind_for_a_group_with_a_state_row_refuses_while_a_recovery_copy_exists() {
    let v = Verbs::new();
    let calls = v.calls();
    let call = |name: &str| -> Vec<String> { calls.iter().find(|c| c.0 == name).unwrap().1.clone() };
    let triage = call("triage");
    let a: Vec<&str> = triage.iter().map(|s| s.as_str()).collect();
    let o = run(&a);
    assert_eq!(code(&o), 0, "triage: {}", err(&o));
    let state = v.t.0.join("state.jsonl");
    let state_before = fs::read(&state).unwrap();
    assert!(String::from_utf8_lossy(&state_before).contains("\"g-1\""), "triage proposed no g-1 row: {}", String::from_utf8_lossy(&state_before));
    let bind = call("bind");
    let b: Vec<&str> = bind.iter().map(|s| s.as_str()).collect();
    fs::write(copy_path(&v.t), b"a partial copy").unwrap();
    let o = run(&b);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).is_empty(), "{}", out(&o));
    assert_eq!(err(&o).lines().count(), 1, "{}", err(&o));
    assert!(err(&o).contains("status-write recovery copy"), "{}", err(&o));
    assert_eq!(fs::read(&state).unwrap(), state_before, "bind wrote the state file");
    // the same call binds once the copy is moved aside: the fixture is a real state-row case
    fs::remove_file(copy_path(&v.t)).unwrap();
    let o = run(&b);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(fs::read(&state).unwrap().len() > state_before.len(), "bind wrote no row without the copy");
}

/// An inbox spelled through a dangling symlink: the copy is looked for beside the file the link points to, so a
/// copy there refuses `add` and `sweep` before they create the target (the other verbs find a missing inbox).
#[test]
fn an_inbox_behind_a_dangling_symlink_finds_the_copy_beside_the_target_and_creates_no_target() {
    let t = fixture();
    let real = t.0.join("real");
    fs::create_dir_all(&real).unwrap();
    fs::write(real.join("inbox.jsonl.status-write"), b"a partial copy").unwrap();
    let link = t.0.join("dangling.jsonl");
    std::os::unix::fs::symlink(real.join("inbox.jsonl"), &link).unwrap();
    refuse_everywhere(&t, &link, Some(&real.join("inbox.jsonl")));
    // a relative link, and a chain of two links, reach the same copy
    let rel = t.0.join("rel.jsonl");
    std::os::unix::fs::symlink("real/inbox.jsonl", &rel).unwrap();
    refuse_everywhere(&t, &rel, Some(&real.join("inbox.jsonl")));
    let chain = t.0.join("chain.jsonl");
    std::os::unix::fs::symlink(&rel, &chain).unwrap();
    refuse_everywhere(&t, &chain, Some(&real.join("inbox.jsonl")));
}

/// An inbox path whose recovery path cannot be built (a link loop) counts as a copy, not as "no copy": close-check,
/// which treats a path that does not exist as a fresh install, refuses with the recovery line instead. The verbs
/// that open the inbox stop on the loop with a setup error and create nothing.
#[test]
fn an_inbox_path_whose_recovery_path_cannot_be_built_refuses() {
    let t = fixture();
    let a = t.0.join("loop-a.jsonl");
    let b = t.0.join("loop-b.jsonl");
    std::os::unix::fs::symlink(&b, &a).unwrap();
    std::os::unix::fs::symlink(&a, &b).unwrap();
    let cfg = config_spelled(&t, "loop-config.yaml", &a);
    let note = p(&t.file("note.md", "lesson: none\n")).to_string();
    let o = run(&["friction", "close-check", &note, "--ticket", "t-1", "--config", p(&cfg)]);
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(out(&o).is_empty() && err(&o).lines().count() == 1, "{}", err(&o));
    assert!(err(&o).contains("status-write recovery copy") && !err(&o).contains(p(&t.0)), "{}", err(&o));
    for args in [
        vec!["friction", "add", "--id", "new-1", "--date", "2026-11-05", "--ticket", "t-1", "--where-it-lives", "memory", "--why", "cause: system", "--text", "x", "--config", p(&cfg)],
        vec!["friction", "lint", "--config", p(&cfg)],
        vec!["friction", "set-status", "--id", "a-1", "--set", "open", "--config", p(&cfg)],
    ] {
        let o = run(&args);
        assert_ne!(code(&o), 0, "{}: {}", args[1], out(&o));
        assert!(out(&o).is_empty(), "{}: {}", args[1], out(&o));
    }
    assert!(fs::symlink_metadata(&a).unwrap().file_type().is_symlink() && fs::metadata(&a).is_err());
}

/// Run the five verbs that meet a missing or odd inbox path against `link` as the inbox: each refuses with the
/// recovery line (its damaged-inbox code), and no file named `target` is created.
fn refuse_everywhere(t: &Tmp, link: &Path, target: Option<&Path>) {
    let cfg = config_spelled(t, "spelled-config.yaml", link);
    let note = p(&t.file("note.md", "lesson: none\n")).to_string();
    let log = p(&t.file("events.jsonl", "{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":1794182400000,\"run_id\":\"run-1\",\"ticket\":\"193\",\"repo\":\"example-repo\",\"reason\":\"replan-threshold\",\"role\":\"executor\",\"model\":\"worker\"}\n")).to_string();
    let c = p(&cfg).to_string();
    let verbs: Vec<(&str, Vec<String>, i32)> = vec![
        ("add", strings(&["friction", "add", "--id", "new-1", "--date", "2026-11-05", "--ticket", "t-1", "--where-it-lives", "memory", "--why", "cause: system", "--text", "x", "--config", &c]), 1),
        ("sweep", strings(&["friction", "sweep", "--log", &log, "--week", "2026-w46", "--config", &c]), 1),
        ("set-status", strings(&["friction", "set-status", "--id", "a-1", "--set", "open", "--config", &c]), 1),
        ("lint", strings(&["friction", "lint", "--config", &c]), 1),
        ("close-check", strings(&["friction", "close-check", &note, "--ticket", "t-1", "--config", &c]), 2),
    ];
    for (name, args, want) in &verbs {
        let a: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        let o = run(&a);
        assert_eq!(code(&o), *want, "{name}: {}", err(&o));
        assert!(out(&o).is_empty(), "{name}: stdout {}", out(&o));
        let e = err(&o);
        assert_eq!(e.lines().count(), 1, "{name}: {e}");
        assert!(e.contains("status-write recovery copy"), "{name}: {e}");
        assert!(!e.contains(p(&t.0)), "{name}: a path leaked: {e}");
        if let Some(target) = target {
            assert!(!target.exists(), "{name}: the dangling target was created");
            let copy = PathBuf::from(format!("{}.status-write", p(target)));
            assert_eq!(fs::read(copy).unwrap(), b"a partial copy", "{name}: the copy changed");
        }
        assert!(fs::metadata(link).is_err(), "{name}: the link now names a file");
    }
}

#[test]
fn a_crash_while_a_reader_waits_is_seen_by_the_reader() {
    let t = fixture();
    let cfg = t.config();
    let h = fs::OpenOptions::new().read(true).write(true).open(t.inbox()).unwrap();
    h.lock().unwrap();
    let mut child = spawn(&["friction", "lint", "--config", p(&cfg)]);
    std::thread::sleep(Duration::from_millis(300));
    still_waiting(&mut child, "lint");
    // a crashed writer: the copy exists, the inbox is empty, the descriptor closes
    fs::write(copy_path(&t), body(&fixture_lines())).unwrap();
    h.set_len(0).unwrap();
    drop(h);
    let o = child.wait_with_output().unwrap();
    assert_ne!(code(&o), 0, "lint read an emptied inbox as ok: {}", out(&o));
    assert!(String::from_utf8_lossy(&o.stderr).contains("status-write recovery copy"), "{}", String::from_utf8_lossy(&o.stderr));
    assert!(out(&o).is_empty());
}

#[test]
fn a_planted_symlink_or_folder_at_the_recovery_path_stops_set_status_before_any_write() {
    let before = body(&fixture_lines()).into_bytes();
    // a regular file there is a recovery copy: the read refuses it (exit 1)
    let t = fixture();
    fs::write(copy_path(&t), b"planted").unwrap();
    let o = set(&t, "a-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(inbox_bytes(&t), before);
    assert_eq!(fs::read(copy_path(&t)).unwrap(), b"planted");
    // a symlink (to a file, and dangling) and a folder: the exclusive create refuses (exit 2)
    for kind in ["file-link", "dangling", "folder"] {
        let t = fixture();
        let target = t.0.join("victim.txt");
        fs::write(&target, b"victim").unwrap();
        match kind {
            "file-link" => std::os::unix::fs::symlink(&target, copy_path(&t)).unwrap(),
            "dangling" => std::os::unix::fs::symlink(t.0.join("nowhere"), copy_path(&t)).unwrap(),
            _ => fs::create_dir(copy_path(&t)).unwrap(),
        }
        let o = set(&t, "a-1", "fixed", Some(FIXED));
        assert_eq!(code(&o), 2, "{kind}: {}", err(&o));
        assert!(out(&o).is_empty(), "{kind}");
        assert_eq!(inbox_bytes(&t), before, "{kind}");
        assert_eq!(fs::read(&target).unwrap(), b"victim", "{kind}: the link target was written");
        assert!(!t.0.join("nowhere").exists(), "{kind}: the dangling target was created");
    }
}

#[test]
fn a_good_write_keeps_the_inbox_permission_bits_and_leaves_no_recovery_copy() {
    let t = fixture();
    fs::set_permissions(t.inbox(), fs::Permissions::from_mode(0o640)).unwrap();
    let o = set(&t, "a-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert!(!copy_path(&t).exists());
    assert_eq!(fs::metadata(t.inbox()).unwrap().permissions().mode() & 0o7777, 0o640);
}

/// The copy a killed write leaves has the inbox's permission bits. The binary runs under a file size limit
/// (`ulimit -f`, 512-byte blocks) smaller than the inbox, so the write of the old inbox into the copy is stopped
/// by the kernel (SIGXFSZ) after the copy was created: the copy exists, partial, and is read here. This is also the
/// first crash window (while the copy is written: the inbox whole, the copy partial).
#[test]
fn a_copy_left_by_a_killed_write_has_the_inbox_permission_bits_and_the_inbox_is_whole() {
    let t = fixture();
    // 0o664 is not what a 022 umask gives a new file, so only the explicit bits pass this.
    fs::set_permissions(t.inbox(), fs::Permissions::from_mode(0o664)).unwrap();
    let before = inbox_bytes(&t);
    assert!(before.len() > 1024);
    let cfg = t.config();
    let mut c = std::process::Command::new("sh");
    c.arg("-c")
        .arg("ulimit -c 0; ulimit -f 1; exec \"$0\" \"$@\"")
        .arg(BIN)
        .args(set_args(&cfg, "a-1", "fixed", Some(FIXED)))
        .env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_ne!(code(&o), 0, "the write finished under the size limit: {}", out(&o));
    assert!(out(&o).is_empty(), "{}", out(&o));
    let copy = copy_path(&t);
    let m = fs::metadata(&copy).expect("the killed write left no copy");
    assert!(m.len() < before.len() as u64, "the copy is not partial");
    assert_eq!(m.permissions().mode() & 0o7777, 0o664, "the copy does not carry the inbox bits");
    assert_eq!(inbox_bytes(&t), before, "the inbox changed before the copy was whole");
    let o = run(&["friction", "lint", "--config", p(&cfg)]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("status-write recovery copy"), "{}", err(&o));
}

// ---------------------------------------------------------------- races (f)

fn statuses(t: &Tmp) -> Vec<(String, String)> {
    String::from_utf8(inbox_bytes(t))
        .unwrap()
        .lines()
        .map(|l| {
            let (r, s) = parse_row_status(l).unwrap_or_else(|e| panic!("a line does not parse: {e}"));
            (r.id, s.word().to_string())
        })
        .collect()
}

fn lint_ok(t: &Tmp, rows: usize) {
    let o = run(&["friction", "lint", "--config", p(&t.config())]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), format!("inbox ok rows={rows}\n"));
}

fn both(a: &[&str], b: &[&str]) -> (Output, Output) {
    let ca = spawn(a);
    let cb = spawn(b);
    (ca.wait_with_output().unwrap(), cb.wait_with_output().unwrap())
}

fn word_of(t: &Tmp, id: &str) -> String {
    statuses(t).into_iter().find(|(i, _)| i == id).map(|(_, w)| w).unwrap()
}

const ROUNDS: usize = 50;

#[test]
fn add_and_set_status_race_without_losing_either() {
    let t = Tmp::new();
    let cfg = t.config();
    let c = p(&cfg);
    for round in 0..ROUNDS {
        t.write_inbox(&plain_or_statused(false));
        let id = format!("new-{round}");
        let add_args = ["friction", "add", "--id", id.as_str(), "--date", "2026-11-05", "--ticket", "t-1", "--where-it-lives", "memory", "--why", "cause: system", "--text", "x", "--config", c];
        let set_args = ["friction", "set-status", "--id", "g-2", "--set", "fixed", "--ref", FIXED, "--config", c];
        let (a, s) = if round % 2 == 0 { both(&add_args, &set_args) } else {
            let (s, a) = both(&set_args, &add_args);
            (a, s)
        };
        assert_eq!(code(&a), 0, "round {round}: add: {}", err(&a));
        assert_eq!(code(&s), 0, "round {round}: set-status: {}", err(&s));
        assert_eq!(word_of(&t, "g-2"), "fixed", "round {round}");
        assert_eq!(word_of(&t, &id), "open", "round {round}");
        lint_ok(&t, 7);
    }
}

#[test]
fn two_set_status_on_different_rows_race_without_losing_either() {
    let t = Tmp::new();
    let cfg = t.config();
    let c = p(&cfg);
    for round in 0..ROUNDS {
        t.write_inbox(&plain_or_statused(false));
        let one = ["friction", "set-status", "--id", "g-2", "--set", "fixed", "--ref", FIXED, "--config", c];
        let two = ["friction", "set-status", "--id", "g-4", "--set", "no-action", "--ref", NOACT, "--config", c];
        let (a, b) = both(&one, &two);
        assert_eq!(code(&a), 0, "round {round}: {}", err(&a));
        assert_eq!(code(&b), 0, "round {round}: {}", err(&b));
        assert_eq!(word_of(&t, "g-2"), "fixed", "round {round}");
        assert_eq!(word_of(&t, "g-4"), "no-action", "round {round}");
        lint_ok(&t, 6);
    }
}

#[test]
fn set_status_and_sweep_race_without_losing_either() {
    let t = Tmp::new();
    let cfg = t.config();
    let c = p(&cfg);
    let log = t.file("events.jsonl", "{\"v\":2,\"kind\":\"replan_required\",\"ts_ms\":1794182400000,\"run_id\":\"run-1\",\"ticket\":\"193\",\"repo\":\"example-repo\",\"reason\":\"replan-threshold\",\"role\":\"executor\",\"model\":\"worker\"}\n");
    for round in 0..ROUNDS {
        t.write_inbox(&plain_or_statused(false));
        let sweep = ["friction", "sweep", "--log", p(&log), "--week", "2026-w46", "--config", c];
        let setting = ["friction", "set-status", "--id", "g-3", "--set", "lesson", "--ref", LESSON, "--config", c];
        let (a, b) = if round % 2 == 0 { both(&sweep, &setting) } else {
            let (b, a) = both(&setting, &sweep);
            (a, b)
        };
        assert_eq!(code(&a), 0, "round {round}: sweep: {}", err(&a));
        assert_eq!(code(&b), 0, "round {round}: set-status: {}", err(&b));
        assert_eq!(word_of(&t, "g-3"), "lesson", "round {round}");
        assert_eq!(statuses(&t).len(), 7, "round {round}: the swept row is missing");
        lint_ok(&t, 7);
    }
}

// ---------------------------------------------------------------- the neighbours

#[test]
fn the_read_only_verbs_and_triage_print_the_same_after_one_status_set() {
    let a = Tmp::new();
    a.write_inbox(&plain_or_statused(false));
    let b = Tmp::new();
    b.write_inbox(&plain_or_statused(false));
    let o = set(&b, "g-2", "fixed", Some(FIXED));
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_ne!(inbox_bytes(&a), inbox_bytes(&b));
    let (ra, rb) = (neighbours_in(&a), neighbours_in(&b));
    assert_eq!(ra.len(), rb.len());
    for (x, y) in ra.iter().zip(rb.iter()) {
        assert_eq!(x.0, y.0);
        assert_eq!(x, y, "{} differs", x.0);
    }
    assert!(ra.iter().any(|r| r.0 == "triage") && ra.iter().any(|r| r.0.starts_with("handoff/")), "triage proposed nothing");
}

#[test]
fn open_changes_only_in_the_target_line_and_the_counts() {
    let t = fixture();
    let cfg = t.config();
    let before = out(&run(&["friction", "open", "--config", p(&cfg)]));
    assert!(before.ends_with("rows open=5 fixed=2 guarded=1 ticket=0 lesson=0 no-action=0 total=8\n"), "{before}");
    let o = set(&t, "a-1", "fixed", Some(FIXED));
    assert_eq!(code(&o), 0, "{}", err(&o));
    let after = out(&run(&["friction", "open", "--config", p(&cfg)]));
    assert!(after.ends_with("rows open=4 fixed=3 guarded=1 ticket=0 lesson=0 no-action=0 total=8\n"), "{after}");
    let keep = |s: &str| -> Vec<String> { s.lines().filter(|l| l.starts_with("open ") && !l.starts_with("open id=a-1 ")).map(String::from).collect() };
    assert!(before.contains("open id=a-1 "), "{before}");
    assert!(!after.contains("open id=a-1 "), "{after}");
    assert_eq!(keep(&before), keep(&after));
}
