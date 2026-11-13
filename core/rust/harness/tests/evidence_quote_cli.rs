//! Tests for `harness evidence quote` and the scanned output path (P1B-10 / #3698): evidence never
//! leaves private state unscanned. Every check drives the real binary over real files. Planted
//! sensitive lines come from `harness fixtures` at run time (never typed), the privacy list is the
//! fixtures' own synthetic list injected as `HARNESS_PRIVACY_LIST_SECRET`, and every inherited list
//! variable is stripped. Needs a debug build (fixtures are not built into release binaries).
#![cfg(unix)]
use harness::evidence::{QUOTE_TRUNCATED_NOTE, STREAM_CAP, TRUNCATION_MARKER};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("evq-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn base() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_CONFIG")
        .env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET");
    c
}

fn text_of(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap().trim_end().to_string()
}

fn fixture(args: &[&str]) -> String {
    let mut v = vec!["fixtures"];
    v.extend_from_slice(args);
    let o = base().args(v).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "fixtures {args:?}");
    text_of(&o)
}

/// The synthetic list text, fetched once per call (cheap).
fn list() -> String {
    String::from_utf8(base().args(["fixtures", "list"]).output().unwrap().stdout).unwrap()
}

fn run_with(list: Option<&str>, args: &[&str]) -> Output {
    let mut c = base();
    if let Some(l) = list {
        c.env("HARNESS_PRIVACY_LIST_SECRET", l);
    }
    let mut child = c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let deadline = Instant::now() + Duration::from_millis(20_000);
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("the command did not return within 20000 ms: {args:?}");
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn run(args: &[&str]) -> Output {
    run_with(Some(&list()), args)
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

struct Planted {
    _t: Tmp,
    d: PathBuf,
    rd: PathBuf,
    ticket: String,
    run: String,
}

/// Start a real program under `ticket`, wait for its record, and return the run's coordinates.
fn plant(ticket: &str) -> Planted {
    let t = Tmp::new();
    let r = t.0.join("R");
    let d = t.0.join("D");
    fs::create_dir_all(&r).unwrap();
    fs::create_dir_all(&d).unwrap();
    let (r, d) = (r.canonicalize().unwrap(), d.canonicalize().unwrap());
    let i = run(&["manifest", "init", ticket, "--dir", d.to_str().unwrap(), "--root", r.to_str().unwrap()]);
    assert_eq!(code(&i), 0, "init: {}", err(&i));
    let s = run(&[
        "process", "start", ticket, "--id", "a", "--cwd", r.to_str().unwrap(), "--dir", d.to_str().unwrap(), "--", "/bin/sh", "-c",
        "exit 0",
    ]);
    assert_eq!(code(&s), 0, "start: {} {}", out(&s), err(&s));
    let l = run(&["manifest", "list", ticket, "--dir", d.to_str().unwrap()]);
    let listed = out(&l);
    let line = listed.lines().find(|x| x.starts_with("a.") && x.ends_with("/exit")).expect("exit entry listed");
    let id = line.split('\t').next().unwrap();
    let stamp = id.trim_end_matches(".exit");
    let rd = PathBuf::from(line.split('\t').nth(2).unwrap()).parent().unwrap().to_path_buf();
    let deadline = Instant::now() + Duration::from_millis(8000);
    while !rd.join("exit").exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert!(rd.join("exit").exists(), "no exit record");
    Planted { _t: t, d, rd, ticket: ticket.to_string(), run: stamp.to_string() }
}

impl Planted {
    fn set_stream(&self, name: &str, body: impl AsRef<[u8]>) {
        fs::write(self.rd.join(name), body).unwrap();
    }

    fn quote_args<'a>(&'a self, extra: &[&'a str]) -> Vec<&'a str> {
        let mut v = vec!["evidence", "quote", self.ticket.as_str(), "--run", self.run.as_str(), "--manifest-dir", self.d.to_str().unwrap()];
        v.extend_from_slice(extra);
        v
    }

    fn quote(&self, extra: &[&str]) -> Output {
        run(&self.quote_args(extra))
    }
}

fn assert_refused_for_class(o: &Output, class: &str, needle: &str) {
    assert_eq!(code(o), 1, "stdout {:?} stderr {:?}", out(o), err(o));
    assert_eq!(out(o), "", "a refusal prints nothing on stdout");
    let e = err(o);
    assert!(e.contains(class), "stderr names the class {class}: {e:?}");
    assert!(e.contains("stdout:1"), "stderr names the line number: {e:?}");
    assert!(!e.contains(needle), "stderr holds no byte of the needle: {e:?}");
}

#[test]
fn clean_line_is_quoted() {
    let p = plant("evq-1");
    p.set_stream("stdout", "hello world\nsecond line\n");
    let o = p.quote(&["--stream", "stdout"]);
    assert_eq!(code(&o), 0, "{} {}", out(&o), err(&o));
    assert_eq!(out(&o), "hello world\nsecond line\n");
    let o = p.quote(&["--stream", "stdout", "--last", "1"]);
    assert_eq!(out(&o), "second line\n", "--last counts whole lines");
}

#[test]
fn home_path_line_is_refused() {
    let p = plant("evq-2");
    let line = fixture(&["line", "home-raw"]);
    let marker = fixture(&["marker", "home-raw"]);
    p.set_stream("stdout", format!("{line}\n"));
    let o = p.quote(&["--stream", "stdout"]);
    assert_refused_for_class(&o, "home-path", &marker);
    assert!(!err(&o).contains(&line));
}

#[test]
fn deny_token_line_is_refused() {
    let p = plant("evq-3");
    let line = fixture(&["line", "deny-profile"]);
    let marker = fixture(&["marker", "deny-profile"]);
    p.set_stream("stdout", format!("{line}\n"));
    let o = p.quote(&["--stream", "stdout"]);
    assert_refused_for_class(&o, "deny-token", &marker);
}

#[test]
fn quote_without_list_fails_closed() {
    let p = plant("evq-4");
    p.set_stream("stdout", "hello world\n");
    let o = run_with(None, &p.quote_args(&["--stream", "stdout"]));
    assert_eq!(code(&o), 2, "{} {}", out(&o), err(&o));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("no privacy list configured"));
}

#[test]
fn quote_refuses_unsigned_run_fifo_symlink() {
    let p = plant("evq-5");
    p.set_stream("stdout", "hello world\n");
    // A run the manifest never signed out.
    let o = run(&["evidence", "quote", "evq-5", "--run", "nope.123", "--stream", "stdout", "--manifest-dir", p.d.to_str().unwrap()]);
    assert_eq!((code(&o), out(&o)), (2, String::new()), "unsigned run: {}", err(&o));
    // Bad --last values.
    for bad in ["0", "201", "abc", "-1"] {
        let o = p.quote(&["--stream", "stdout", "--last", bad]);
        assert_eq!((code(&o), out(&o)), (2, String::new()), "--last {bad}: {}", err(&o));
    }
    let o = p.quote(&["--stream", "other"]);
    assert_eq!((code(&o), out(&o)), (2, String::new()), "bad stream");
    // A FIFO at the stream name is never opened (the run would hang a reader that opened it).
    fs::remove_file(p.rd.join("stdout")).unwrap();
    let mk = Command::new("mkfifo").arg(p.rd.join("stdout")).status().unwrap();
    assert!(mk.success());
    let o = p.quote(&["--stream", "stdout"]);
    assert_eq!((code(&o), out(&o)), (2, String::new()), "fifo: {}", err(&o));
    // A symlink at the stream name is never followed, even to a clean file.
    fs::remove_file(p.rd.join("stdout")).unwrap();
    let target: &Path = &p.d.join("target.txt");
    fs::write(target, "hello world\n").unwrap();
    std::os::unix::fs::symlink(target, p.rd.join("stdout")).unwrap();
    let o = p.quote(&["--stream", "stdout"]);
    assert_eq!((code(&o), out(&o)), (2, String::new()), "symlink: {}", err(&o));
}

#[test]
fn show_and_check_without_list_fail_closed() {
    let p = plant("evq-6");
    for verb in ["show", "check"] {
        let o = run_with(None, &["evidence", verb, "evq-6", "--manifest-dir", p.d.to_str().unwrap()]);
        assert_eq!(code(&o), 2, "{verb}: {} {}", out(&o), err(&o));
        assert_eq!(out(&o), "", "{verb} prints nothing without a list");
        assert!(err(&o).contains("no privacy list configured"));
    }
}

#[test]
fn check_output_hit_fails_closed() {
    // A ticket id equal to a token the list flags: the rendered text itself has a hit.
    let token = fixture(&["marker", "deny-profile"]);
    let p = plant(&token);
    for verb in ["check", "show"] {
        let o = run(&["evidence", verb, &token, "--manifest-dir", p.d.to_str().unwrap()]);
        assert_eq!(code(&o), 2, "{verb}: {} {}", out(&o), err(&o));
        assert_eq!(out(&o), "", "{verb} prints nothing on a hit");
        assert!(err(&o).contains("deny-token"));
        assert!(!err(&o).contains(&token), "the refusal never echoes the token");
    }
}

#[test]
fn cut_fragment_is_never_quoted() {
    let p = plant("evq-7a");
    let line = fixture(&["line", "home-raw"]);
    let marker = fixture(&["marker", "home-raw"]);
    let cut = 10; // the window start lands this many bytes into the fixture line
    let full = format!("{line}\n");
    let a: String = "a\n".repeat(100);
    let b_len = 65_536 - (full.len() - cut);
    let b = format!("{}\n", "p".repeat(b_len - 1));
    let body = format!("{a}{full}{b}");
    assert_eq!(body.len() - 65_536, a.len() + cut, "the window starts inside the fixture line");
    p.set_stream("stdout", &body);
    let o = p.quote(&["--stream", "stdout"]);
    assert_eq!(code(&o), 0, "{} {}", out(&o), err(&o));
    let fragment = &line[cut..];
    assert!(!out(&o).contains(fragment), "no byte of the cut fragment's tail");
    assert!(!out(&o).contains(&marker[3..]), "no byte of the marker tail");
}

#[test]
fn truncated_stream_cut_line_is_never_quoted() {
    let p = plant("evq-7b");
    let line = fixture(&["line", "home-raw"]);
    let marker = fixture(&["marker", "home-raw"]);
    let cut_line = &line[..line.len() - 3]; // cut mid-line, no newline
    let head_len = STREAM_CAP as usize - cut_line.len();
    let mut head = String::new();
    for _ in 0..head_len / 100 {
        head.push_str(&format!("{}\n", "x".repeat(99)));
    }
    let r = head_len % 100;
    if r > 0 {
        head.push_str(&format!("{}\n", "y".repeat(r - 1)));
    }
    let mut body: Vec<u8> = head.into_bytes();
    body.extend_from_slice(cut_line.as_bytes());
    assert_eq!(body.len() as u64, STREAM_CAP);
    body.extend_from_slice(TRUNCATION_MARKER);
    p.set_stream("stdout", &body);
    let o = p.quote(&["--stream", "stdout", "--last", "3"]);
    assert_eq!(code(&o), 0, "{} {}", out(&o), err(&o));
    let text = out(&o);
    assert!(!text.contains(&marker[3..]) && !text.contains(&cut_line[5..]), "no byte of the cut line: {text:?}");
    assert!(text.ends_with(&format!("{QUOTE_TRUNCATED_NOTE}\n")), "the fixed note closes the quote: {text:?}");
    assert_eq!(text.lines().count(), 4, "three whole lines plus the note");

    // A stream whose last line has no newline: that line is not quoted.
    p.set_stream("stdout", "finished line\nstill being writ");
    let o = p.quote(&["--stream", "stdout"]);
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o), "finished line\n");

    // Non-UTF-8 whole lines are never printed lossily.
    p.set_stream("stdout", b"bad \xff\xfe line\n");
    let o = p.quote(&["--stream", "stdout"]);
    assert_eq!((code(&o), out(&o)), (2, String::new()));
}
