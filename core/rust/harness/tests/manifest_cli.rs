//! Process-level tests for `harness manifest init|add|remove|list|owns|verify` (P1B-02 part B). Every check runs the
//! real binary as a child process and reads the real file, so the exit-code contract (0 ok / owned, 1 damage or not
//! owned, 2 usage error or refusal) and "a refused write leaves the file byte-identical" are both exercised end to end.
//! The command reads no environment variable: every input arrives as a flag, so nothing is cleared for the child.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("mcli-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    /// A scratch layout: `R` and `O` are sibling folders (the root and the outsider), `D` holds the manifest.
    fn fence(&self) -> (PathBuf, PathBuf, PathBuf) {
        let r = self.0.join("R");
        let o = self.0.join("O");
        let d = self.0.join("D");
        fs::create_dir_all(&r).unwrap();
        fs::create_dir_all(&o).unwrap();
        fs::create_dir_all(&d).unwrap();
        (r, o, d)
    }

    fn file(&self, d: &Path, ticket: &str) -> PathBuf {
        d.join(format!("{ticket}.jsonl"))
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str], cwd: Option<&Path>) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// init + add one temp_dir entry, the fixture most tests start from.
fn seeded(t: &Tmp, ticket: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (r, o, d) = t.fence();
    let i = run(&["manifest", "init", ticket, "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap(), "--ts-ms", "1000"], None);
    assert_eq!(code(&i), 0, "init: {}", err(&i));
    let a = run(
        &["manifest", "add", ticket, "--dir", d.to_str().unwrap(), "--class", "temp_dir", "--id", "a", "--path", &r.join("x").to_string_lossy(), "--ts-ms", "1001"],
        None,
    );
    assert_eq!(code(&a), 0, "add: {}", err(&a));
    (r, o, d)
}

/// A refused write must leave the file byte-identical: capture before, run, compare after.
fn refuses_leaving_file_untouched(args: &[&str], file: &Path) {
    let before = fs::read(file).unwrap();
    let o = run(args, None);
    assert_ne!(code(&o), 0, "expected a refusal, got exit 0; stdout: {}", out(&o));
    let after = fs::read(file).unwrap();
    assert_eq!(before, after, "a refused write changed the manifest");
}

// ---------------------------------------------------------------- happy paths

/// AC 7: init, add, list, owns, remove round-trip across separate invocations.
#[test]
fn add_then_remove_round_trips() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    let l = run(&["manifest", "list", "t-1", "--dir", d.to_str().unwrap()], None);
    assert_eq!(code(&l), 0);
    assert_eq!(out(&l), format!("a\ttemp_dir\t{}\n", r.join("x").display()), "list prints id, class and the stored resolved path");

    let q = run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--path", &r.join("x/y").to_string_lossy()], None);
    assert_eq!(code(&q), 0, "a path inside a live entry is owned: {}", err(&q));

    let rm = run(&["manifest", "remove", "t-1", "--dir", d.to_str().unwrap(), "--id", "a", "--ts-ms", "1002"], None);
    assert_eq!(code(&rm), 0, "remove: {}", err(&rm));
    let l2 = run(&["manifest", "list", "t-1", "--dir", d.to_str().unwrap()], None);
    assert_eq!(code(&l2), 0);
    assert_eq!(out(&l2), "", "an emptied manifest lists nothing");
    let q2 = run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--path", &r.join("x/y").to_string_lossy()], None);
    assert_ne!(code(&q2), 0, "a removed entry owns nothing any more");
    // a second remove of the now-dead id is refused
    refuses_leaving_file_untouched(
        &["manifest", "remove", "t-1", "--dir", d.to_str().unwrap(), "--id", "a", "--ts-ms", "1003"],
        &t.file(&d, "t-1"),
    );
    let v = run(&["manifest", "verify", "t-1", "--dir", d.to_str().unwrap()], None);
    assert_eq!(code(&v), 0, "verify of a clean file: {}", err(&v));
}

/// AC 8: process and port entries survive the writing invocation; `owns` answers by exact pid and port only.
#[test]
fn process_and_port_entries_survive_restart() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    assert_eq!(
        code(&run(&["manifest", "init", "t-1", "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap()], None)),
        0
    );
    let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = sleeper.id();
    let mut other = Command::new("sleep").arg("30").spawn().unwrap();
    let other_pid = other.id();

    let a = run(&["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "process", "--id", "p-1", "--pid", &pid.to_string()], None);
    assert_eq!(code(&a), 0, "process add: {}", err(&a));
    let b = run(&["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "port", "--id", "n-1", "--port", "8080"], None);
    assert_eq!(code(&b), 0, "port add: {}", err(&b));

    // fresh invocations, after the writing processes have long gone: both entries are still there.
    let l = run(&["manifest", "list", "t-1", "--dir", d.to_str().unwrap()], None);
    assert_eq!(code(&l), 0);
    let text = out(&l);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "both entries live: {:?}", lines);
    let proc_line = lines.iter().find(|l| l.contains("p-1")).expect("process entry listed");
    let mut f = proc_line.split('\t').collect::<Vec<&str>>();
    let pid_start = if f.len() == 4 { f.pop().unwrap() } else { panic!("process entry must print four fields: {proc_line}") };
    assert_eq!(f, vec!["p-1", "process", &pid.to_string()], "id, class, pid");
    assert!(!pid_start.is_empty() && pid_start.bytes().all(|b| b.is_ascii_digit()), "pid_start_ms is decimal: {pid_start}");
    let port_line = *lines.iter().find(|l| l.starts_with("n-1\t")).expect("port entry listed");
    assert_eq!(port_line, "n-1\tport\t8080", "the port line is exactly three fields (id, class, port), never a fourth");

    assert_eq!(code(&run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--pid", &pid.to_string()], None)), 0, "the listed pid is owned");
    assert_eq!(code(&run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--port", "8080"], None)), 0, "the listed port is owned");
    assert_ne!(code(&run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--pid", &other_pid.to_string()], None)), 0, "an unlisted pid is not owned");

    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let _ = other.kill();
    let _ = other.wait();
}

/// The default manifest dir (`.harness/manifests/` in the working folder) is used when --dir is absent.
#[test]
fn default_dir_is_the_working_folder_state_dir() {
    let t = Tmp::new();
    let r = t.0.join("R");
    fs::create_dir_all(&r).unwrap();
    let cwd = t.0.join("cwd");
    fs::create_dir_all(&cwd).unwrap();
    assert_eq!(code(&run(&["manifest", "init", "t-1", "--root", r.to_str().unwrap()], Some(&cwd))), 0);
    let expected = cwd.join(".harness/manifests/t-1.jsonl");
    assert!(expected.exists(), "init wrote the default manifest file");
    let l = run(&["manifest", "verify", "t-1"], Some(&cwd));
    assert_eq!(code(&l), 0, "verify reads the default dir: {}", err(&l));
    // an empty --dir is a usage error, never a read of the working folder itself
    fs::copy(&expected, cwd.join("t-1.jsonl")).unwrap();
    let e = run(&["manifest", "list", "t-1", "--dir", ""], Some(&cwd));
    assert_eq!(code(&e), 2, "an empty --dir value is refused: {}", err(&e));
    assert_eq!(out(&e), "", "the stray file in the working folder was not listed");
}

// ---------------------------------------------------------------- the fence (AC 6)

/// AC 6: a path outside every root is refused outright, through `..`, and through a symlink that points out.
#[test]
fn outside_the_fence_is_refused() {
    let t = Tmp::new();
    let (r, o, d) = seeded(&t, "t-1");
    let file = t.file(&d, "t-1");
    let outside = o.join("x");
    std::os::unix::fs::symlink(&o, r.join("link")).unwrap();

    for p in [outside.to_string_lossy().into_owned(), format!("{}/../O/x", r.display()), format!("{}/link/x", r.display())] {
        refuses_leaving_file_untouched(
            &["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "temp_dir", "--id", "z", "--path", &p],
            &file,
        );
    }
    let l = run(&["manifest", "list", "t-1", "--dir", d.to_str().unwrap()], None);
    assert_eq!(code(&l), 0);
    assert_eq!(out(&l), format!("a\ttemp_dir\t{}\n", r.join("x").display()), "no refused entry ever landed in the file");
}

/// A symlink that does not resolve (dangling link) is refused, never stored as the path the caller means.
#[test]
fn dangling_link_is_refused() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    std::os::unix::fs::symlink(&r.join("missing-target"), r.join("dangling")).unwrap();
    refuses_leaving_file_untouched(
        &["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "temp_dir", "--id", "z", "--path", &r.join("dangling/f").to_string_lossy()],
        &t.file(&d, "t-1"),
    );
    // the query half of the same rule: a path that does not resolve answers not owned, never an error-free guess
    let q = run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--path", &r.join("dangling").to_string_lossy()], None);
    assert_eq!(code(&q), 1, "an unresolvable owns query is not owned: {}", err(&q));
}

/// Odd but legal shapes are accepted: a hex-like id and ticket are plain tokens, and a path may contain spaces.
#[test]
fn odd_but_legal_shapes_are_accepted() {
    let t = Tmp::new();
    let root = t.0.join("R dir");
    fs::create_dir_all(&root).unwrap();
    let d = t.0.join("D");
    fs::create_dir_all(&d).unwrap();
    assert_eq!(code(&run(&["manifest", "init", "0xbeef", "--dir", d.to_str().unwrap(), "--root", root.to_str().unwrap()], None)), 0, "a hex-like ticket is a token");
    let target = root.join("x y");
    let a = run(&["manifest", "add", "0xbeef", "--dir", d.to_str().unwrap(), "--class", "evidence", "--id", "0xdead", "--path", &target.to_string_lossy()], None);
    assert_eq!(code(&a), 0, "a hex-like id and a spaced path are legal: {}", err(&a));
    let l = run(&["manifest", "list", "0xbeef", "--dir", d.to_str().unwrap()], None);
    assert_eq!(out(&l), format!("0xdead\tevidence\t{}\n", target.display()));
    let q = run(&["manifest", "owns", "0xbeef", "--dir", d.to_str().unwrap(), "--path", &target.join("inner").to_string_lossy()], None);
    assert_eq!(code(&q), 0, "the spaced path owns its interior");
}

/// A `..` or `.` after a part that does not exist yet is refused, never folded: the stored path must be the one the
/// caller will create.
#[test]
fn dot_after_a_missing_part_is_refused() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    for tail in ["missing/../y", "missing/./y"] {
        refuses_leaving_file_untouched(
            &["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "temp_dir", "--id", "z", "--path", &format!("{}/{tail}", r.display())],
            &t.file(&d, "t-1"),
        );
    }
}

/// A relative --path is refused on add, and on owns it is a usage error (the answer would be about the wrong folder).
#[test]
fn relative_path_is_refused() {
    let t = Tmp::new();
    let (_r, _o, d) = seeded(&t, "t-1");
    refuses_leaving_file_untouched(
        &["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "temp_dir", "--id", "z", "--path", "R/x"],
        &t.file(&d, "t-1"),
    );
    let q = run(&["manifest", "owns", "t-1", "--dir", d.to_str().unwrap(), "--path", "R/x"], None);
    assert_eq!(code(&q), 2, "a relative owns query is a usage error, not an answer: {}", err(&q));
}

/// The manifest dir itself may not resolve inside one of the roots: the record must survive removal of the roots.
#[test]
fn dir_inside_a_root_is_refused() {
    let t = Tmp::new();
    let (r, _o, _d) = t.fence();
    let inside = r.join("man");
    fs::create_dir_all(&inside).unwrap();
    let i = run(&["manifest", "init", "t-1", "--dir", inside.to_str().unwrap(), "--root", r.to_str().unwrap()], None);
    assert_eq!(code(&i), 2, "the manifest file must live outside the roots");
    assert!(!inside.join("t-1.jsonl").exists(), "the refused init created nothing");
}

// ---------------------------------------------------------------- bad commands (AC 9)

/// No init yet: add and remove are refused, list/verify say error, owns answers not owned.
#[test]
fn operations_before_init_are_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    let dir = d.to_str().unwrap();
    for args in [
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "a", "--path", &r.join("x").to_string_lossy().into_owned()],
        vec!["manifest", "remove", "t-1", "--dir", dir, "--id", "a"],
    ] {
        let o = run(&args, None);
        assert_eq!(code(&o), 2, "before init every write is refused: {}", err(&o));
    }
    assert!(!t.file(&d, "t-1").exists(), "a refused write created no file");
    assert_eq!(code(&run(&["manifest", "list", "t-1", "--dir", dir], None)), 2, "a missing manifest is not a manifest");
    assert_eq!(code(&run(&["manifest", "verify", "t-1", "--dir", dir], None)), 2);
    assert_eq!(code(&run(&["manifest", "owns", "t-1", "--dir", dir, "--pid", "1234"], None)), 1, "a missing manifest owns nothing");
}

/// A second init is refused and leaves the file byte-identical.
#[test]
fn second_init_is_refused() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    let file = t.file(&d, "t-1");
    let before = fs::read(&file).unwrap();
    let i = run(&["manifest", "init", "t-1", "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap()], None);
    assert_eq!(code(&i), 2, "second init: {}", err(&i));
    assert_eq!(fs::read(&file).unwrap(), before, "the refused init rewrote nothing");
}

/// An unknown class (the container class the spec defers) is refused.
#[test]
fn unknown_class_is_refused() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    refuses_leaving_file_untouched(
        &["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "container", "--id", "z", "--path", &r.join("x2").to_string_lossy()],
        &t.file(&d, "t-1"),
    );
}

/// A duplicate live id, and the same path/pid/port under a new id, are all refused.
#[test]
fn duplicate_targets_are_refused() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    let dir = d.to_str().unwrap();
    let file = t.file(&d, "t-1");
    // same id, different path
    refuses_leaving_file_untouched(&["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "a", "--path", &r.join("other").to_string_lossy()], &file);
    // same path, new id
    refuses_leaving_file_untouched(&["manifest", "add", "t-1", "--dir", dir, "--class", "log", "--id", "b", "--path", &r.join("x").to_string_lossy()], &file);

    let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = sleeper.id();
    assert_eq!(code(&run(&["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "p-1", "--pid", &pid.to_string()], None)), 0);
    // same pid, new id
    refuses_leaving_file_untouched(&["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "p-2", "--pid", &pid.to_string()], &file);
    // same port, new id
    assert_eq!(code(&run(&["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "n-1", "--port", "8080"], None)), 0);
    refuses_leaving_file_untouched(&["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "n-2", "--port", "8080"], &file);
    let _ = sleeper.kill();
    let _ = sleeper.wait();
}

/// Bad roots: `/`, a relative dir, a missing dir, a file, and no --root at all; each creates nothing.
#[test]
fn bad_roots_are_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    let dir = d.to_str().unwrap();
    let plain_file = t.0.join("plain");
    fs::write(&plain_file, b"x").unwrap();
    for root in ["/", "relative/dir", &t.0.join("nope").to_string_lossy().into_owned(), &plain_file.to_string_lossy().into_owned()] {
        let ticket = format!("tk-{}", root.len()); // distinct ticket per case so no earlier file can matter
        let i = run(&["manifest", "init", &ticket, "--dir", dir, "--root", root], None);
        assert_eq!(code(&i), 2, "root {root:?} must be refused: {}", err(&i));
        assert!(!t.file(&d, &ticket).exists(), "root {root:?}: the refused init created the manifest anyway");
    }
    let n = run(&["manifest", "init", "t-1", "--dir", dir], None);
    assert_eq!(code(&n), 2, "init with no --root: {}", err(&n));
    // a good root still works after all those refusals
    assert_eq!(code(&run(&["manifest", "init", "t-1", "--dir", dir, "--root", r.to_str().unwrap()], None)), 0);
}

/// `--class` must match the target flag: every mismatched or missing pair is a usage error.
#[test]
fn class_must_match_the_target_flag() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    let dir = d.to_str().unwrap();
    let p = r.join("x3").to_string_lossy().into_owned();
    let mut sleeper = Command::new("sleep").arg("30").spawn().unwrap();
    let live_pid = sleeper.id().to_string();
    for args in [
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "z", "--pid", "123"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "z", "--port", "80"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--path", &p],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--port", "80"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--path", &p],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--pid", "123"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "z", "--path", &p, "--pid", "1234"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--port", "81", "--pid", "1234"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "z"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--id", "z", "--path", &p],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--path", &p],
    ] {
        let o = run(&args, None);
        assert_eq!(code(&o), 2, "expected a usage error for {:?}", args);
        assert!(!out(&o).contains("temp_dir"), "a usage error prints no listing");
    }
    let o = run(&["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--pid", &live_pid, "--path", &p], None);
    assert_eq!(code(&o), 2, "a stray --path alongside a valid --pid is still a mismatch");
    let _ = sleeper.kill();
    let _ = sleeper.wait();
    let l = run(&["manifest", "list", "t-1", "--dir", dir], None);
    assert_eq!(out(&l).lines().count(), 1, "none of the mismatched adds landed");
}

/// pid bounds (0, 1) and port bounds (0, 65536), and non-integer or empty values, are refused.
#[test]
fn pid_and_port_bounds_are_refused() {
    let t = Tmp::new();
    let (_r, _o, d) = seeded(&t, "t-1");
    let dir = d.to_str().unwrap();
    for bad_pid in ["0", "1"] {
        let o = run(&["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--pid", bad_pid], None);
        assert_eq!(code(&o), 2, "pid {bad_pid}: {}", err(&o));
    }
    for bad_port in ["0", "65536"] {
        let o = run(&["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--port", bad_port], None);
        assert_eq!(code(&o), 2, "port {bad_port}: {}", err(&o));
    }
    for args in [
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--pid", "12a"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--pid", "-5"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--pid", ""],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "process", "--id", "z", "--pid", "99999999999999999999"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--port", "80 80"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--port", "0x2000"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "port", "--id", "z", "--port", " 80"],
    ] {
        let o = run(&args, None);
        assert_eq!(code(&o), 2, "expected a usage error for {:?}", args);
    }
    let l = run(&["manifest", "list", "t-1", "--dir", dir], None);
    assert_eq!(out(&l).lines().count(), 1);
}

/// A dead pid is refused: its start time cannot be read, so it can never be signed out.
#[test]
fn dead_pid_is_refused() {
    let t = Tmp::new();
    let (_r, _o, d) = seeded(&t, "t-1");
    let mut dead = Command::new("sleep").arg("30").spawn().unwrap();
    let pid = dead.id();
    let _ = dead.kill();
    let _ = dead.wait();
    let o = run(&["manifest", "add", "t-1", "--dir", d.to_str().unwrap(), "--class", "process", "--id", "z", "--pid", &pid.to_string()], None);
    assert_eq!(code(&o), 2, "a dead pid has no readable start time: {}", err(&o));
}

/// Non-token tickets and ids, odd shapes (whitespace, unicode digits, uppercase, two positionals) are refused before
/// anything is touched.
#[test]
fn non_token_values_are_refused() {
    let t = Tmp::new();
    let (r, _o, d) = t.fence();
    let dir = d.to_str().unwrap();
    for bad in ["..", "T-1", "t 1", "t/1", "١٢٣", ""] {
        let o = run(&["manifest", "init", bad, "--dir", dir, "--root", r.to_str().unwrap()], None);
        assert_eq!(code(&o), 2, "ticket {bad:?} must be refused: {}", err(&o));
        assert!(!t.file(&d, bad).exists(), "ticket {bad:?}: a file was touched anyway");
    }
    assert_eq!(code(&run(&["manifest", "init", "t-1", "--dir", dir, "--root", r.to_str().unwrap()], None)), 0);
    let file = t.file(&d, "t-1");
    for bad in ["..", "A B", "id/1", "١٢٣", ""] {
        refuses_leaving_file_untouched(&["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", bad, "--path", &r.join("x").to_string_lossy()], &file);
    }
    let two = run(&["manifest", "init", "t-1", "t-2", "--dir", dir, "--root", r.to_str().unwrap()], None);
    assert_eq!(code(&two), 2, "two positionals are a usage error");
}

/// Option hygiene: unknown flags, flags without a value, flags given twice, options that do not belong to the
/// subcommand, and a bad --ts-ms are all usage errors.
#[test]
fn option_hygiene_is_enforced() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    let dir = d.to_str().unwrap();
    for args in [
        vec!["manifest"],
        vec!["manifest", "init", "t-x", "--dir", dir, "--root", r.to_str().unwrap(), "--root"],
        vec!["manifest", "bogus", "t-1", "--dir", dir],
        vec!["manifest", "list", "t-1", "--dir"],
        vec!["manifest", "list", "t-1", "--nope", "x"],
        vec!["manifest", "list", "t-1", "--dir", dir, "--nope"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "z", "--path", &r.join("q").to_string_lossy().into_owned(), "--ts-ms"],
        vec!["manifest", "list", "t-1", "--dir", dir, "--class", "temp_dir"],
        vec!["manifest", "list", "t-1", "--dir", dir, "--id", "a"],
        vec!["manifest", "list", "t-1", "--dir", dir, "--path", &r.join("x").to_string_lossy().into_owned()],
        vec!["manifest", "list"],
        vec!["manifest", "add", "t-1", "--dir", dir, "--ts-ms", "abc", "--class", "temp_dir", "--id", "z", "--path", &r.join("q").to_string_lossy().into_owned()],
        vec!["manifest", "add", "t-1", "--dir", dir, "--ts-ms", "-1", "--class", "temp_dir", "--id", "z", "--path", &r.join("q").to_string_lossy().into_owned()],
        vec!["manifest", "add", "t-1", "--dir", dir, "--dir", dir, "--class", "temp_dir", "--id", "z", "--path", &r.join("q").to_string_lossy().into_owned()],
        vec!["manifest", "list", "t-1", "--dir", ""],
        vec!["manifest", "owns", "t-1", "--dir", dir, "--class", "port", "--pid", "123"],
        vec!["manifest", "owns", "t-1", "--dir", dir],
        vec!["manifest", "owns", "t-1", "--dir", dir, "--pid", "123", "--port", "80"],
        vec!["manifest", "owns", "t-1", "--dir", dir, "--path", &r.join("x").to_string_lossy().into_owned(), "--port", "80"],
        vec!["manifest", "owns", "t-1", "--dir", dir, "--pid", "abc"],
    ] {
        let o = run(&args, None);
        assert_eq!(code(&o), 2, "expected a usage error for {:?}", args);
    }
    let l = run(&["manifest", "list", "t-1", "--dir", dir], None);
    assert_eq!(code(&l), 0);
    assert_eq!(out(&l).lines().count(), 1, "none of that ever wrote");

    // a --root with no value is a usage error and creates no manifest file
    assert!(!t.file(&d, "t-x").exists(), "the refused init with a valueless trailing --root created a manifest anyway");

    // remove with no --id is a usage error and a refused write, never a defaulted removal
    let rm_no_id = run(&["manifest", "remove", "t-1", "--dir", dir], None);
    assert_eq!(code(&rm_no_id), 2, "remove with no --id: {}", err(&rm_no_id));
    refuses_leaving_file_untouched(&["manifest", "remove", "t-1", "--dir", dir], &t.file(&d, "t-1"));

    // a missing ticket is refused having created nothing, in a dir that holds no manifest yet
    let d2 = t.0.join("D2");
    fs::create_dir_all(&d2).unwrap();
    let i = run(&["manifest", "init", "--dir", d2.to_str().unwrap(), "--root", r.to_str().unwrap()], None);
    assert_eq!(code(&i), 2, "a ticket is required: {}", err(&i));
    assert_eq!(fs::read_dir(&d2).unwrap().count(), 0, "the refused init created nothing");
}

// ---------------------------------------------------------------- damage fails closed (AC 10)

/// A torn tail: list exits 1, add is refused having written nothing, owns answers not owned even for a path that was
/// owned before the damage. Replacing the fragment with a complete garbage line makes verify exit 1 too.
#[test]
fn damage_fails_closed() {
    let t = Tmp::new();
    let (r, _o, d) = seeded(&t, "t-1");
    let dir = d.to_str().unwrap();
    let file = t.file(&d, "t-1");
    let owned = r.join("x/y").to_string_lossy().into_owned();

    // a final fragment with no newline
    let mut bytes = fs::read(&file).unwrap();
    bytes.extend_from_slice(b"{\"v\":1,\"op\":\"add\",\"ticket\":\"t-1\"");
    fs::write(&file, &bytes).unwrap();

    let l = run(&["manifest", "list", "t-1", "--dir", dir], None);
    assert_eq!(code(&l), 1, "a torn tail is reported: {}", err(&l));
    assert_eq!(out(&l), "", "a damaged manifest lists nothing");
    assert_ne!(code(&run(&["manifest", "owns", "t-1", "--dir", dir, "--path", &owned], None)), 0, "a damaged manifest owns nothing");
    refuses_leaving_file_untouched(&["manifest", "add", "t-1", "--dir", dir, "--class", "temp_dir", "--id", "z", "--path", &r.join("w").to_string_lossy()], &file);
    assert_eq!(code(&run(&["manifest", "verify", "t-1", "--dir", dir], None)), 1);

    // a complete garbage line instead: corruption, same fail-closed answers
    let mut bytes = fs::read(&file).unwrap();
    bytes.extend_from_slice(b"garbage\n");
    fs::write(&file, &bytes).unwrap();
    assert_eq!(code(&run(&["manifest", "verify", "t-1", "--dir", dir], None)), 1, "verify of corruption");
    let l2 = run(&["manifest", "list", "t-1", "--dir", dir], None);
    assert_eq!(code(&l2), 1, "list of corruption: {}", err(&l2));
    assert_ne!(code(&run(&["manifest", "owns", "t-1", "--dir", dir, "--path", &owned], None)), 0);
    refuses_leaving_file_untouched(&["manifest", "remove", "t-1", "--dir", dir, "--id", "a"], &file);
}
