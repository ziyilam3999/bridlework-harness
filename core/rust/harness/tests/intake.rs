//! Process-level smoke for `harness ticket intake` (P1A-06b). Every check runs the real binary as a child process against scratch repos
//! under the system temp folder (learned from `std::env::temp_dir()`, never beside the checkout). Variables are set only on the child;
//! this file never reads one. Each refusal has a planted failing twin: the same run with one thing changed that a wrong build gets wrong.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const MANIFEST: &str = env!("CARGO_MANIFEST_DIR");
const MAX: usize = 1024 * 1024;
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// True when `p` lies under `root` (both made canonical when they exist).
fn is_under(root: &Path, p: &Path) -> bool {
    let r = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let q = p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    q.starts_with(r)
}

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let root = std::env::temp_dir();
        let p = root.join(format!("hintake-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        let p = p.canonicalize().unwrap();
        // AC 9: scratch lives under the system temp folder, never beside the checkout.
        assert!(is_under(&root, &p), "scratch must be under the temp folder");
        assert!(!is_under(Path::new(MANIFEST), &p), "scratch must not be beside the checkout");
        Tmp(p)
    }

    fn dir(&self, name: &str) -> PathBuf {
        let d = self.0.join(name);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(&self, name: &str, body: impl AsRef<[u8]>) -> PathBuf {
        let f = self.0.join(name);
        fs::write(&f, body).unwrap();
        f
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One git call in a scratch repo: no hooks, no inherited repo-redirecting names, no stdin, fixed identity.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "scratch git call failed");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A scratch repo with one tracked file and one commit whose message is `msg`, so different repos get different heads.
fn repo(t: &Tmp, name: &str, msg: &str) -> PathBuf {
    let d = t.dir(name);
    git(&d, &["init", "-q"]);
    fs::write(d.join("f.txt"), msg).unwrap();
    git(&d, &["add", "f.txt"]);
    git(&d, &["commit", "-q", "-m", msg]);
    d.canonicalize().unwrap()
}

fn head(d: &Path) -> String {
    git(d, &["rev-parse", "HEAD"])
}

/// The valid control ticket as ordered (key, raw JSON value) pairs, for repo id `repo_id`.
fn base(repo_id: &str) -> Vec<(String, String)> {
    let p = |k: &str, v: &str| (k.to_string(), v.to_string());
    vec![
        p("id", "\"T-1\""),
        p("repo_id", &format!("\"{repo_id}\"")),
        p("title", "\"A title\""),
        p("group", "\"grp\""),
        p("size", "\"M\""),
        p("objective", "\"An objective\""),
        p("acceptance_criteria", "[\"one\"]"),
        p("dependencies", "[]"),
        p("risk", "\"normal\""),
        p("owner_role", "\"execution_verification\""),
        p("evidence_links", "[]"),
    ]
}

fn render(pairs: &[(String, String)]) -> String {
    let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

fn ticket_for(repo_id: &str) -> String {
    render(&base(repo_id))
}

fn without(repo_id: &str, key: &str) -> String {
    render(&base(repo_id).into_iter().filter(|(k, _)| k != key).collect::<Vec<_>>())
}

fn with_value(repo_id: &str, key: &str, raw: &str) -> String {
    render(&base(repo_id).into_iter().map(|(k, v)| if k == key { (k, raw.to_string()) } else { (k, v) }).collect::<Vec<_>>())
}

/// Run `harness ticket intake` from `cwd`. `child_env` is set on the child only.
fn intake_in(cwd: &Path, args: &[&str], child_env: &[(&str, &Path)]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("ticket").arg("intake").args(args).current_dir(cwd).stdin(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    for (k, v) in child_env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn map_arg(id: &str, d: &Path) -> String {
    format!("{id}={}", d.to_str().unwrap())
}

/// Run intake on `file` with the given map entries, from `cwd`.
fn go(cwd: &Path, file: &Path, entries: &[(&str, &Path)]) -> Output {
    let maps: Vec<String> = entries.iter().map(|(i, d)| map_arg(i, d)).collect();
    let mut a: Vec<&str> = vec![file.to_str().unwrap()];
    for m in &maps {
        a.push("--repo-map");
        a.push(m);
    }
    intake_in(cwd, &a, &[])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap()
}

/// AC 10 helper: true when `needle` appears in stdout or stderr text. The test below checks it against a known-leaking string first.
fn leaks(stdout: &str, stderr: &str, needle: &str) -> bool {
    stdout.contains(needle) || stderr.contains(needle)
}

fn leaks_o(o: &Output, needle: &str) -> bool {
    leaks(&out(o), &err(o), needle)
}

fn refused(o: &Output) {
    assert_eq!(code(o), 1, "refusal is exit 1; stderr: {}", err(o));
    assert!(out(o).is_empty(), "a refusal prints nothing on stdout: {}", out(o));
}

// AC 10 twin: the leak helper really does catch a message that echoes a folder.
#[test]
fn the_leak_helper_catches_a_message_that_echoes_the_folder() {
    let dir = "/synthetic-folder-0xA1";
    assert!(leaks("", &format!("repo not found at {dir}"), dir), "a leaking stderr must be caught");
    assert!(leaks(&format!("ok {dir}"), "", dir), "a leaking stdout must be caught");
    assert!(!leaks("ticket=T-1 repo_id=b head=abc", "repo not found for repo id `b`", dir), "a clean pair passes");
}

// AC 9 twin: the location helper rejects a path beside the checkout and accepts one under the temp folder.
#[test]
fn the_scratch_location_check_rejects_a_folder_beside_the_checkout() {
    let beside = Path::new(MANIFEST).join("scratch-beside");
    assert!(!is_under(&std::env::temp_dir(), &beside), "a folder beside the checkout is not under the temp folder");
    let t = Tmp::new();
    assert!(is_under(&std::env::temp_dir(), &t.0), "the scratch folder is under the temp folder");
}

// AC 1: the ticket's repo is used, not the folder the process runs in. Also AC 9 and AC 10 on the accepted run.
#[test]
fn acts_on_the_ticket_repo_not_the_current_folder() {
    let t = Tmp::new();
    let (a, b) = (repo(&t, "a", "commit in a"), repo(&t, "b", "commit in b"));
    let (sha_a, sha_b) = (head(&a), head(&b));
    assert_ne!(sha_a, sha_b, "the two scratch repos must differ");
    let tf = t.file("tb.json", ticket_for("b"));
    let o = go(&a, &tf, &[("a", &a), ("b", &b)]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("ticket=T-1 repo_id=b head={sha_b}\n"), "one line naming the ticket, the repo id and b's head");
    assert!(!out(&o).contains(&sha_a), "twin: a's head must not appear");
    assert!(!leaks_o(&o, b.to_str().unwrap()) && !leaks_o(&o, a.to_str().unwrap()), "AC 10: no folder in the output");
    // Mirror: a ticket for a, run from b, reports a's head.
    let ta = t.file("ta.json", ticket_for("a"));
    let m = go(&b, &ta, &[("a", &a), ("b", &b)]);
    assert_eq!(code(&m), 0);
    assert!(out(&m).contains(&sha_a) && !out(&m).contains(&sha_b), "mirror run reports a's head");
    // An inherited repo-redirecting variable on the child (set on the child only) does not move the check off b.
    let a_git = a.join(".git");
    let r = intake_in(&a, &[tf.to_str().unwrap(), "--repo-map", &map_arg("b", &b)], &[("GIT_DIR", &a_git)]);
    assert_eq!(code(&r), 0, "stderr: {}", err(&r));
    assert!(out(&r).contains(&sha_b) && !out(&r).contains(&sha_a), "an inherited GIT_DIR must not redirect the repo");
}

// AC 2: a ticket with no repo id is refused (exit 1, stdout empty, stderr names the field); the restored key is accepted.
#[test]
fn a_ticket_with_no_repo_id_is_refused() {
    let t = Tmp::new();
    let b = repo(&t, "b", "in b");
    let bad = t.file("bad.json", without("b", "repo_id"));
    let o = go(&b, &bad, &[("b", &b)]);
    refused(&o);
    assert!(err(&o).contains("repo_id"), "stderr names the field: {}", err(&o));
    assert!(!leaks_o(&o, b.to_str().unwrap()), "no folder in the output");
    let good = t.file("good.json", ticket_for("b"));
    assert_eq!(code(&go(&b, &good, &[("b", &b)])), 0, "twin: the key restored is accepted");
}

// AC 3: an id missing from the map is refused by name; adding it flips the result; a same-named folder is not a map.
#[test]
fn an_unknown_repo_id_is_refused_and_a_folder_name_is_not_a_map() {
    let t = Tmp::new();
    let (b, c) = (repo(&t, "b", "in b"), repo(&t, "c", "in c"));
    let tf = t.file("tc.json", ticket_for("c"));
    let o = go(&b, &tf, &[("b", &b)]);
    refused(&o);
    assert!(err(&o).contains("`c`"), "stderr names the id: {}", err(&o));
    assert_eq!(code(&go(&b, &tf, &[("b", &b), ("c", &c)])), 0, "twin A: with c mapped it is accepted");
    // Twin B: run from a real repo whose folder is named `c`; the map still lacks `c`.
    let o2 = go(&c, &tf, &[("b", &b)]);
    refused(&o2);
    assert!(err(&o2).contains("`c`"), "twin B: still refused: {}", err(&o2));
}

// AC 4: broken tickets are refused and the refusal names the field; the valid control is accepted.
#[test]
fn broken_tickets_are_refused_and_name_the_field() {
    let t = Tmp::new();
    let b = repo(&t, "b", "in b");
    let valid = ticket_for("b");
    let mut dup = base("b");
    dup.push(("id".to_string(), "\"T-2\"".to_string()));
    let mut unknown = base("b");
    unknown.push(("surprise".to_string(), "\"x\"".to_string()));
    let mut non_utf8 = valid.clone().into_bytes();
    let pos = non_utf8.windows(8).position(|w| w == b"A title\"").unwrap();
    non_utf8[pos] = 0xff;
    let mut oversize = valid.clone().into_bytes();
    oversize.extend(std::iter::repeat(b' ').take(MAX + 1));
    let cases: Vec<(&str, Vec<u8>, &str)> = vec![
        ("unknown key", render(&unknown).into_bytes(), "surprise"),
        ("duplicate key", render(&dup).into_bytes(), "id"),
        ("size not S/M/L", with_value("b", "size", "\"XL\"").into_bytes(), "size"),
        ("trailing text", format!("{valid} trailing").into_bytes(), "trailing"),
        ("not utf-8", non_utf8, "utf-8"),
        ("over 1 MiB", oversize, "1 MiB"),
        ("path as repo id", with_value("b", "repo_id", "\"../b\"").into_bytes(), "repo_id"),
    ];
    for (label, body, field) in &cases {
        let tf = t.file("broken.json", body);
        let o = go(&b, &tf, &[("b", &b)]);
        refused(&o);
        assert!(err(&o).contains(field), "{label}: stderr must name `{field}`: {}", err(&o));
        assert!(!leaks_o(&o, b.to_str().unwrap()), "{label}: no folder in the output");
    }
    let tf = t.file("valid.json", &valid);
    assert_eq!(code(&go(&b, &tf, &[("b", &b)])), 0, "twin: the valid control ticket is accepted");
}

/// Feed `total` bytes (a marker first, then filler) to a ticket-file path that is a named pipe, and report how many bytes the
/// reader took before it let go. A reader with a read cap stops near the cap; one without it drains everything.
fn run_on_pipe(t: &Tmp, total: usize, marker: &str) -> (Output, usize, PathBuf) {
    let fifo = t.0.join("pipe-ticket.json");
    let made = Command::new("mkfifo").arg(&fifo).status().expect("mkfifo is available").success();
    assert!(made, "mkfifo made the pipe");
    let (tx, rx) = std::sync::mpsc::channel::<usize>();
    let (path, head) = (fifo.clone(), marker.as_bytes().to_vec());
    std::thread::spawn(move || {
        use std::io::Write;
        let mut w = fs::OpenOptions::new().write(true).open(&path).unwrap();
        let mut sent = 0usize;
        let mut chunk = head;
        chunk.resize(64 * 1024, b' ');
        while sent < total {
            if w.write_all(&chunk).is_err() {
                break;
            }
            sent += chunk.len();
            chunk.iter_mut().for_each(|b| *b = b' ');
        }
        let _ = tx.send(sent);
    });
    let b = t.dir("pipe-cwd");
    let o = intake_in(&b, &[fifo.to_str().unwrap()], &[]);
    let sent = match rx.recv_timeout(std::time::Duration::from_secs(5)) {
        Ok(n) => n,
        Err(_) => {
            // The command never opened the pipe, so the writer is still waiting for a reader: let it go, then fail loudly.
            drop(fs::File::open(&fifo));
            usize::MAX
        }
    };
    (o, sent, fifo)
}

// The 1 MiB read cap: an oversize ticket is refused (exit 1, the size reason), the reader stops reading near the cap instead of
// draining the whole file, and neither the file's content nor its path reaches any output. Twin: a small file on the same path shape
// (an under-cap stream) is read to its end, so the early stop comes from the cap and not from the pipe.
#[test]
fn an_oversize_ticket_is_refused_and_read_only_up_to_the_cap() {
    let marker = "PLANTED-TICKET-CONTENT-MARKER";
    let t = Tmp::new();
    let total = 16 * MAX;
    let (o, sent, fifo) = run_on_pipe(&t, total, marker);
    refused(&o);
    assert!(err(&o).contains("1 MiB"), "stderr must give the size reason: {}", err(&o));
    assert!(!leaks_o(&o, marker), "no file content in the output");
    assert!(!leaks_o(&o, fifo.to_str().unwrap()), "no file path in the output");
    assert!(!leaks_o(&o, "pipe-ticket"), "no file name in the output");
    assert!(sent != usize::MAX, "the command opened the ticket file");
    assert!(sent <= 2 * MAX, "the reader took {sent} of {total} bytes: the read cap is gone");
    // Twin: the same run shape with the whole stream under the cap is read to the end (the writer is never cut short).
    let t2 = Tmp::new();
    let (o2, sent2, _) = run_on_pipe(&t2, MAX / 2, marker);
    refused(&o2);
    assert_eq!(sent2, MAX / 2, "twin: an under-cap stream is fully read");
}

// AC 5 and AC 10: a folder that is not the top of a repo is refused naming only the id, never the folder; the top level is accepted.
#[test]
fn a_folder_that_is_not_a_repo_top_is_refused_without_naming_it() {
    let t = Tmp::new();
    let b = repo(&t, "b", "in b");
    let sub = b.join("subdir");
    fs::create_dir_all(&sub).unwrap();
    let plain = t.dir("plain");
    let missing = t.0.join("nothing-here");
    let a_file = t.file("just-a-file.txt", "x");
    let empty_repo = t.dir("empty-repo");
    git(&empty_repo, &["init", "-q"]);
    let tf = t.file("tb.json", ticket_for("b"));
    for (label, dir) in [("missing", &missing), ("plain", &plain), ("subfolder", &sub), ("file", &a_file), ("no commit", &empty_repo)] {
        let o = go(&b, &tf, &[("b", dir)]);
        refused(&o);
        assert!(err(&o).contains("`b`"), "{label}: names the id: {}", err(&o));
        assert!(!leaks_o(&o, dir.to_str().unwrap()), "{label}: the refusal must not echo the folder: {}", err(&o));
    }
    assert_eq!(code(&go(&plain, &tf, &[("b", &b)])), 0, "twin: the top level of the repo is accepted");
}

/// Every file name under `d` (the `.git` folder included), sorted.
fn listing(d: &Path) -> Vec<String> {
    fn walk(d: &Path, base: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            if p.is_dir() {
                walk(&p, base, out);
            }
        }
    }
    let mut v = Vec::new();
    walk(d, d, &mut v);
    v.sort();
    v
}

// AC 6: an accepted run leaves the repo exactly as it was: clean status, same head, no new file. Twin: a dirty repo stays exactly as dirty.
#[test]
fn intake_is_read_only() {
    let t = Tmp::new();
    let b = repo(&t, "b", "in b");
    let tf = t.file("tb.json", ticket_for("b"));
    let (files, sha) = (listing(&b), head(&b));
    let o = go(&t.0, &tf, &[("b", &b)]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(git(&b, &["status", "--porcelain"]), "", "the status is still empty");
    assert_eq!(head(&b), sha, "HEAD is unchanged");
    assert_eq!(listing(&b), files, "no new file appeared in the repo");
    // Twin: make b dirty (an edit and an untracked file); intake neither cleans nor complains.
    fs::write(b.join("f.txt"), "edited").unwrap();
    fs::write(b.join("untracked.txt"), "u").unwrap();
    let (dirty, dirty_files) = (git(&b, &["status", "--porcelain"]), listing(&b));
    assert!(!dirty.is_empty(), "the twin repo really is dirty");
    let o2 = go(&t.0, &tf, &[("b", &b)]);
    assert_eq!(code(&o2), 0, "a dirty repo is still accepted; stderr: {}", err(&o2));
    assert!(err(&o2).is_empty(), "and nothing is said about it: {}", err(&o2));
    assert_eq!(git(&b, &["status", "--porcelain"]), dirty, "the dirt is exactly as it was");
    assert_eq!(listing(&b), dirty_files);
    assert_eq!(fs::read_to_string(b.join("f.txt")).unwrap(), "edited");
    assert_eq!(head(&b), sha);
}

// AC 7: usage errors exit 2, not 1; stdout empty. Twins: the valid command exits 0 and a ticket refusal exits 1.
#[test]
fn usage_errors_exit_2_and_a_refusal_exits_1() {
    let t = Tmp::new();
    let b = repo(&t, "b", "in b");
    let tf = t.file("tb.json", ticket_for("b"));
    let (f, bm) = (tf.to_str().unwrap(), map_arg("b", &b));
    let missing = t.0.join("no-such-ticket.json");
    let missing_s = missing.to_str().unwrap();
    let dir_as_file = t.dir("a-folder");
    let dir_as_file_s = dir_as_file.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("no file argument", vec!["--repo-map", &bm]),
        ("no arguments at all", vec![]),
        ("missing ticket file", vec![missing_s, "--repo-map", &bm]),
        ("ticket path is a folder", vec![dir_as_file_s, "--repo-map", &bm]),
        ("map without =", vec![f, "--repo-map", "b"]),
        ("map with a relative dir", vec![f, "--repo-map", "b=relative/dir"]),
        ("map id not a valid repo id", vec![f, "--repo-map", "Bad=/abs/dir"]),
        ("map id twice", vec![f, "--repo-map", &bm, "--repo-map", &bm]),
        ("--repo-map without a value", vec![f, "--repo-map"]),
        ("unknown option", vec![f, "--nope", "--repo-map", &bm]),
        ("two ticket files", vec![f, f, "--repo-map", &bm]),
    ];
    for (label, args) in &cases {
        let o = intake_in(&t.0, args, &[]);
        assert_eq!(code(&o), 2, "{label}: stderr: {}", err(&o));
        assert!(out(&o).is_empty(), "{label}: stdout must be empty: {}", out(&o));
        assert!(!leaks_o(&o, missing_s) && !leaks_o(&o, b.to_str().unwrap()), "{label}: no folder in the output: {}", err(&o));
    }
    // Wrong subcommand words are also usage errors.
    for args in [vec!["ticket"], vec!["ticket", "nope"]] {
        let o = Command::new(BIN).args(&args).stdin(Stdio::null()).output().unwrap();
        assert_eq!(code(&o), 2, "{args:?}");
        assert!(out(&o).is_empty(), "{args:?}");
    }
    assert_eq!(code(&intake_in(&t.0, &[f, "--repo-map", &bm], &[])), 0, "twin: the valid command exits 0");
    let bad = t.file("norepo.json", without("b", "repo_id"));
    assert_eq!(code(&intake_in(&t.0, &[bad.to_str().unwrap(), "--repo-map", &bm], &[])), 1, "twin: a ticket refusal exits 1, not 2");
}

/// True when the `[dependencies]` section of a Cargo manifest text holds no entry.
fn deps_empty(manifest: &str) -> bool {
    let mut in_deps = false;
    for line in manifest.lines().map(|l| l.trim()) {
        if line.starts_with('[') {
            in_deps = line == "[dependencies]";
        } else if in_deps && !line.is_empty() && !line.starts_with('#') {
            return false;
        }
    }
    true
}

// AC 8: std only, and the test lint accepts this file but refuses a planted file that reads the environment.
#[test]
fn std_only_and_the_test_lint_has_teeth() {
    let manifest = fs::read_to_string(Path::new(MANIFEST).join("Cargo.toml")).unwrap();
    assert!(manifest.contains("[dependencies]"), "the manifest keeps its dependencies section");
    assert!(deps_empty(&manifest), "[dependencies] must stay empty");
    assert!(!deps_empty("[dependencies]\nserde = \"1\"\n"), "twin: the checker catches an added dependency");
    let lint = Path::new(MANIFEST).join("../../../ci/lint-rust-tests.sh");
    let run = |p: &Path| Command::new("bash").arg(&lint).arg(p).stdin(Stdio::null()).output().unwrap();
    let t = Tmp::new();
    let planted = t.file("planted.rs", format!("fn reads() {{ let _ = std::env::{}(\"X\"); }}\n", "var"));
    let o = run(&planted);
    assert_eq!(code(&o), 1, "twin: a test file that reads the environment is refused; stderr: {}", err(&o));
    let real = Path::new(MANIFEST).join("tests/intake.rs");
    let o = run(&real);
    assert_eq!(code(&o), 0, "this file passes the lint; out: {} err: {}", out(&o), err(&o));
}
