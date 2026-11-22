//! Process-level tests for `harness ticket body-cites`. The real binary runs as a child against a scratch git
//! repo built with local git only (no network): an old commit, a base commit and a head commit. Each exit 2 test
//! checks one stderr line and an empty stdout. No test waits on a clock.
#![cfg(unix)]
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const USAGE: &str = "usage: harness ticket body-cites --repo-dir <absolute-dir> --head <full-sha> --base <full-sha> --body-file <absolute-file> [--cite-ok \"<cite or token>\"]...";
static SEQ: AtomicUsize = AtomicUsize::new(0);

/// Head `src/a.rs`: line 1 `fn alpha`, 2 blank, 3 `fn beta`, 4 blank, 5 `fn gamma` (five lines).
const HEAD_A: &str = "fn alpha() {}\n\nfn beta() {}\n\nfn gamma() {}\n";

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("bcites-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

fn gitcmd(dir: &Path, args: &[&str]) -> Command {
    let contact = format!("user.email=t{}scratch", '@');
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "user.name=t", "-c", contact.as_str(), "-c", "core.autocrlf=false", "-c", "commit.gpgsign=false"]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR", "GIT_OBJECT_DIRECTORY"] {
        c.env_remove(k);
    }
    c.stdin(Stdio::null());
    c
}

fn g(dir: &Path, args: &[&str]) -> String {
    let out = gitcmd(dir, args).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?} {:?}", args, String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn commit_all(dir: &Path, files: &[(&str, &str)]) -> String {
    for (p, b) in files {
        let f = dir.join(p);
        fs::create_dir_all(f.parent().unwrap()).unwrap();
        fs::write(f, b).unwrap();
    }
    g(dir, &["add", "-A"]);
    g(dir, &["commit", "-q", "-m", "m"]);
    g(dir, &["rev-parse", "HEAD"])
}

fn init(t: &Tmp, name: &str) -> PathBuf {
    let dir = t.0.join(name);
    fs::create_dir_all(&dir).unwrap();
    g(&dir, &["init", "-q"]);
    for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
        g(&dir, &["config", k, v]);
    }
    dir
}

struct Case {
    t: Tmp,
    dir: PathBuf,
    old: String,
    base: String,
    head: String,
}

struct Out {
    code: i32,
    stdout: String,
    stderr: String,
}

/// Old commit (`src/a.rs` line 1 is `fn alpha`), base commit (`src/a.rs` is `fn base_line`, a `docs` folder, a
/// link `lnk`), head commit (`src/a.rs` is HEAD_A, a new `src/new.rs`).
fn case() -> Case {
    let t = Tmp::new();
    let dir = init(&t, "repo");
    let old = commit_all(&dir, &[("src/a.rs", "fn alpha() {}\n")]);
    symlink("src/a.rs", dir.join("lnk")).unwrap();
    let base = commit_all(&dir, &[("src/a.rs", "fn base_line() {}\n"), ("docs/d.md", "x\n")]);
    let head = commit_all(&dir, &[("src/a.rs", HEAD_A), ("src/new.rs", "fn fresh() {}\n")]);
    Case { t, dir, old, base, head }
}

fn cite(sha: &str, path: &str, rng: &str, frag: &str) -> String {
    format!("{sha}:{path}:{rng} \"{frag}\"")
}

fn run_args(args: &[&str], env: &[(&str, &str)]) -> Out {
    let mut c = Command::new(BIN);
    c.args(["ticket", "body-cites"]).args(args).stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let o = c.output().unwrap();
    Out { code: o.status.code().unwrap(), stdout: String::from_utf8_lossy(&o.stdout).into_owned(), stderr: String::from_utf8_lossy(&o.stderr).into_owned() }
}

fn run_full(c: &Case, body: &[u8], extra: &[&str], env: &[(&str, &str)]) -> Out {
    let f = c.t.0.join("body.md");
    fs::write(&f, body).unwrap();
    let mut a = vec!["--repo-dir", c.dir.to_str().unwrap(), "--head", c.head.as_str(), "--base", c.base.as_str(), "--body-file", f.to_str().unwrap()];
    a.extend_from_slice(extra);
    run_args(&a, env)
}

fn run(c: &Case, body: &str) -> Out {
    run_full(c, body.as_bytes(), &[], &[])
}

fn lines(o: &Out) -> Vec<&str> {
    o.stdout.lines().collect()
}

fn refused(o: &Out) {
    assert_eq!(o.code, 2, "stderr: {}", o.stderr);
    assert_eq!(o.stdout, "");
    assert_eq!(o.stderr.lines().count(), 1, "stderr: {}", o.stderr);
}

// Mutant: accept an unused --cite-ok, or drop a check below, and the named test turns red.

#[test]
fn all_good_body_exits_0_every_line_ok() {
    // Red if a good cite is flagged: any reason check that fires on a clean cite (sha prefix, range, blank, case).
    let c = case();
    let c1 = cite(&c.head, "src/a.rs", "1", "fn alpha");
    let c2 = cite(&c.head[..7], "src/a.rs", "1-3", "fn beta");
    let c3 = cite(&c.base, "src/a.rs", "1", "base_line");
    let o = run(&c, &format!("Intro `{c1}` and ({c2}).\nAnd {c3}\n"));
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert_eq!(lines(&o), vec![format!("OK {c1}"), format!("OK {c2}"), format!("OK {c3}"), "cites=3 ok=3 flag=0 unparsed=0".to_string()]);
}

#[test]
fn range_with_blank_first_line_is_blank_line() {
    // Mutant: drop the blank check of the first line of a range -> this turns red.
    let c = case();
    let x = cite(&c.head, "src/a.rs", "2-3", "fn beta");
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG blank-line {x}"), "cites=1 ok=0 flag=1 unparsed=0".to_string()]);
}

#[test]
fn range_with_blank_last_line_only_is_blank_line() {
    // Mutant: drop the blank check of the last line of a range -> this turns red.
    let c = case();
    let x = cite(&c.head, "src/a.rs", "3-4", "fn beta");
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG blank-line {x}"), "cites=1 ok=0 flag=1 unparsed=0".to_string()]);
}

#[test]
fn range_past_the_last_line_is_out_of_range_not_blank_line() {
    // Mutant: read a line past the end as blank (or not check m) -> this turns red.
    let c = case();
    let x1 = cite(&c.head, "src/a.rs", "5-6", "fn gamma");
    let x2 = cite(&c.head, "src/a.rs", "0", "fn alpha");
    let x3 = cite(&c.head, "src/a.rs", "4-3", "fn beta");
    let x4 = cite(&c.head, "src/a.rs", "6", "fn gamma");
    let o = run(&c, &format!("{x1}\n{x2}\n{x3}\n{x4}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(
        lines(&o),
        vec![format!("FLAG out-of-range {x1}"), format!("FLAG out-of-range {x2}"), format!("FLAG out-of-range {x3}"), format!("FLAG out-of-range {x4}"), "cites=4 ok=0 flag=4 unparsed=0".to_string()]
    );
}

#[test]
fn a_final_line_with_no_newline_counts() {
    let t = Tmp::new();
    let dir = init(&t, "repo");
    let base = commit_all(&dir, &[("f.txt", "one\ntwo")]);
    let head = commit_all(&dir, &[("g.txt", "x\n")]);
    let f = t.0.join("b.md");
    let ok = cite(&head[..10], "f.txt", "2", "two");
    let past = cite(&base, "f.txt", "3", "two");
    // base is the first commit; head's tree holds f.txt too.
    fs::write(&f, format!("{ok}\n{past}\n")).unwrap();
    let o = run_args(&["--repo-dir", dir.to_str().unwrap(), "--head", &head, "--base", &base, "--body-file", f.to_str().unwrap()], &[]);
    assert_eq!(lines(&o), vec![format!("OK {ok}"), format!("FLAG out-of-range {past}"), "cites=2 ok=1 flag=1 unparsed=0".to_string()]);
}

#[test]
fn directory_and_symlink_are_not_a_file() {
    // Mutant: read a tree or a link as a file -> this turns red.
    let c = case();
    let d = cite(&c.base, "docs", "1", "x");
    let l = cite(&c.head, "lnk", "1", "fn alpha");
    let o = run(&c, &format!("{d}\n{l}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG not-a-file {d}"), format!("FLAG not-a-file {l}"), "cites=2 ok=0 flag=2 unparsed=0".to_string()]);
}

#[test]
fn strict_cite_without_fragment_and_with_curly_quotes_is_unparsed() {
    // Mutant: take a cite with no fragment, or with curly quotes, as strict -> this turns red.
    let c = case();
    let h = &c.head;
    let o = run(&c, &format!("{h}:src/a.rs:1\n{h}:src/a.rs:5 \u{201c}fn gamma\u{201d}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(
        lines(&o),
        vec![format!("FLAG unparsed {h}:src/a.rs:1"), format!("FLAG unparsed {h}:src/a.rs:5"), "cites=0 ok=0 flag=0 unparsed=2".to_string()]
    );
}

#[test]
fn fragment_in_other_case_is_ok() {
    // Mutant: compare the fragment with case -> this turns red.
    let c = case();
    let x = cite(&c.head, "src/a.rs", "5", "FN GaMmA");
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(o.code, 0);
    assert_eq!(lines(&o), vec![format!("OK {x}"), "cites=1 ok=1 flag=0 unparsed=0".to_string()]);
}

#[test]
fn inherited_git_dir_changes_nothing() {
    // Mutant: let GIT_DIR reach the git calls -> the other repo answers and this turns red.
    let c = case();
    let other = init(&c.t, "other");
    commit_all(&other, &[("src/a.rs", "zzz\n")]);
    let body = format!("{}\n{}\n", cite(&c.head, "src/a.rs", "1", "fn alpha"), cite(&c.base, "src/a.rs", "1", "nope"));
    let plain = run(&c, &body);
    let other_git = other.join(".git");
    let with_env = run_full(&c, body.as_bytes(), &[], &[("GIT_DIR", other_git.to_str().unwrap())]);
    assert_eq!(plain.code, 1);
    assert_eq!(with_env.code, plain.code);
    assert_eq!(with_env.stdout, plain.stdout);
}

#[test]
fn fragment_not_on_the_range_is_fragment_absent() {
    let c = case();
    let x = cite(&c.head, "src/a.rs", "1", "fn beta");
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG fragment-absent {x}"), "cites=1 ok=0 flag=1 unparsed=0".to_string()]);
}

#[test]
fn a_cite_can_carry_two_reasons_in_the_fixed_order() {
    let c = case();
    let x = cite(&c.head, "src/a.rs", "2", "zzz");
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(lines(&o)[0], format!("FLAG blank-line,fragment-absent {x}"));
}

#[test]
fn path_not_in_that_commit_is_file_missing() {
    let c = case();
    let x1 = cite(&c.head, "src/gone.rs", "1", "x");
    let x2 = cite(&c.base, "src/new.rs", "1", "fn fresh");
    let o = run(&c, &format!("{x1}\n{x2}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG file-missing {x1}"), format!("FLAG file-missing {x2}"), "cites=2 ok=0 flag=2 unparsed=0".to_string()]);
}

#[test]
fn cite_on_an_older_commit_is_sha_not_head_or_base() {
    // Mutant: accept any sha -> the old commit's file holds the line, so this turns red.
    let c = case();
    let x = cite(&c.old, "src/a.rs", "1", "fn alpha");
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG sha-not-head-or-base {x}"), "cites=1 ok=0 flag=1 unparsed=0".to_string()]);
}

#[test]
fn cite_on_base_that_checks_is_ok() {
    // Mutant: read every cite at --head -> the base text is not there and this turns red.
    let c = case();
    let x = cite(&c.base, "src/a.rs", "1", "base_line");
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(o.code, 0);
    assert_eq!(lines(&o), vec![format!("OK {x}"), "cites=1 ok=1 flag=0 unparsed=0".to_string()]);
}

#[test]
fn bare_line_number_and_name_with_dot_are_unparsed() {
    // Mutant: exit 0 with unparsed > 0 -> this turns red.
    let c = case();
    let o = run(&c, "see :12 and a.rs:12 and src/x:3-4\n");
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec!["FLAG unparsed :12", "FLAG unparsed a.rs:12", "FLAG unparsed src/x:3-4", "cites=0 ok=0 flag=0 unparsed=3"]);
}

#[test]
fn clock_time_is_not_unparsed() {
    // Mutant: count clock times -> this turns red.
    let c = case();
    let o = run(&c, "at 16:35 UTC, (09:05) and 10:30:45\n");
    assert_eq!(o.code, 0);
    assert_eq!(lines(&o), vec!["cites=0 ok=0 flag=0 unparsed=0"]);
}

#[test]
fn cite_ok_clears_a_flag() {
    let c = case();
    let x = cite(&c.head, "src/a.rs", "1", "nope");
    let o = run_full(&c, format!("{x}\n").as_bytes(), &["--cite-ok", &x], &[]);
    assert_eq!(o.code, 0, "{}", o.stdout);
    assert_eq!(lines(&o), vec![format!("CLEARED {x}"), "cites=1 ok=0 flag=0 unparsed=0".to_string()]);
    let o = run_full(&c, b"see a.rs:12\n", &["--cite-ok", "a.rs:12"], &[]);
    assert_eq!(o.code, 0);
    assert_eq!(lines(&o), vec!["CLEARED a.rs:12", "cites=0 ok=0 flag=0 unparsed=0"]);
}

#[test]
fn cite_ok_that_matches_nothing_exits_1() {
    // Mutant: accept an unused --cite-ok -> this turns red.
    let c = case();
    let x = cite(&c.head, "src/a.rs", "1", "fn alpha");
    let o = run_full(&c, format!("{x}\n").as_bytes(), &["--cite-ok", "zzz", "--cite-ok", &x], &[]);
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("OK {x}"), "FLAG cite-ok-unused zzz".to_string(), format!("FLAG cite-ok-unused {x}"), "cites=1 ok=1 flag=0 unparsed=0".to_string()]);
}

#[test]
fn empty_body_exits_0() {
    let c = case();
    let o = run(&c, "");
    assert_eq!(o.code, 0);
    assert_eq!(lines(&o), vec!["cites=0 ok=0 flag=0 unparsed=0"]);
}

#[test]
fn a_tab_before_the_quote_is_strict_and_printed_as_written() {
    let c = case();
    let x = format!("{}:src/a.rs:1\t\"fn alpha\"", c.head);
    let o = run(&c, &format!("{x}\n"));
    assert_eq!(o.code, 0);
    assert_eq!(lines(&o)[0], format!("OK {x}"));
}

#[test]
fn body_that_is_a_link_is_exit_2() {
    let c = case();
    let real = c.t.0.join("real.md");
    fs::write(&real, "x\n").unwrap();
    let link = c.t.0.join("link.md");
    symlink(&real, &link).unwrap();
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head, "--base", &c.base, "--body-file", link.to_str().unwrap()], &[]);
    refused(&o);
    // Mutant: drop the link check -> the run says "not a regular file" and this turns red.
    assert_eq!(o.stderr.trim_end(), "harness: error: the body file is a link");
}

#[test]
fn body_that_is_a_directory_or_missing_is_exit_2() {
    let c = case();
    let d = c.t.0.join("adir");
    fs::create_dir_all(&d).unwrap();
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head, "--base", &c.base, "--body-file", d.to_str().unwrap()], &[]);
    refused(&o);
    // Mutant: drop the regular-file check -> the folder read fails with another text and this turns red.
    assert_eq!(o.stderr.trim_end(), "harness: error: the body file is not a regular file");
    let m = c.t.0.join("missing.md");
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head, "--base", &c.base, "--body-file", m.to_str().unwrap()], &[]);
    refused(&o);
}

#[test]
fn body_over_1048576_bytes_is_exit_2() {
    let c = case();
    refused(&run_full(&c, &vec![b'a'; 1_048_577], &[], &[]));
}

#[test]
fn body_not_utf8_is_exit_2() {
    let c = case();
    refused(&run_full(&c, b"abc \xff\xfe\n", &[], &[]));
}

#[test]
fn relative_repo_dir_is_exit_2() {
    let c = case();
    let f = c.t.0.join("b.md");
    fs::write(&f, "x\n").unwrap();
    let o = run_args(&["--repo-dir", "repo", "--head", &c.head, "--base", &c.base, "--body-file", f.to_str().unwrap()], &[]);
    refused(&o);
    // Mutant: accept a relative --repo-dir -> the run refuses later with another text and this turns red.
    assert_eq!(o.stderr.trim_end(), USAGE);
}

#[test]
fn repo_dir_that_is_not_a_work_tree_is_exit_2() {
    let c = case();
    let plain = c.t.0.join("plain");
    fs::create_dir_all(&plain).unwrap();
    let f = c.t.0.join("b.md");
    fs::write(&f, "x\n").unwrap();
    let o = run_args(&["--repo-dir", plain.to_str().unwrap(), "--head", &c.head, "--base", &c.base, "--body-file", f.to_str().unwrap()], &[]);
    refused(&o);
    // Mutant: drop the work tree check -> the run refuses on the commit check instead and this turns red.
    assert_eq!(o.stderr.trim_end(), "harness: error: the repo dir is not a git work tree");
}

#[test]
fn empty_fragment_is_not_a_strict_cite_so_it_is_unparsed() {
    // Mutant: take an empty fragment as strict -> this prints OK or a cite count and turns red.
    let c = case();
    let h = &c.head;
    let o = run(&c, &format!("{h}:src/a.rs:1 \"\"\n"));
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG unparsed {h}:src/a.rs:1"), "cites=0 ok=0 flag=0 unparsed=1".to_string()]);
}

#[test]
fn a_flag_given_twice_is_exit_2() {
    // Mutant: let a second --head replace the first -> the run succeeds and this turns red.
    let c = case();
    let f = c.t.0.join("b.md");
    fs::write(&f, "x\n").unwrap();
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head, "--head", &c.head, "--base", &c.base, "--body-file", f.to_str().unwrap()], &[]);
    refused(&o);
    assert_eq!(o.stderr.trim_end(), USAGE);
}

#[test]
fn blob_over_1048576_bytes_and_blob_not_utf8_are_not_a_file() {
    // Mutants: read a blob over the size limit; read a blob that is not UTF-8 as text -> this turns red.
    let t = Tmp::new();
    let dir = init(&t, "repo");
    let big = format!("fn big() {{}}\n{}", "a".repeat(1_048_576));
    let exact = format!("fn edge() {{}}\n{}", "a".repeat(1_048_576 - 13));
    assert_eq!(exact.len(), 1_048_576);
    let base = commit_all(&dir, &[("big.txt", big.as_str()), ("edge.txt", exact.as_str())]);
    fs::write(dir.join("bin.txt"), b"fn bin() {}\n\xff\xfe\n").unwrap();
    let head = commit_all(&dir, &[("ok.txt", "x\n")]);
    let big_c = cite(&base, "big.txt", "1", "fn big");
    let edge_c = cite(&base, "edge.txt", "1", "fn edge");
    let bin_c = cite(&head, "bin.txt", "1", "fn bin");
    let f = t.0.join("b.md");
    fs::write(&f, format!("{big_c}\n{edge_c}\n{bin_c}\n")).unwrap();
    let o = run_args(&["--repo-dir", dir.to_str().unwrap(), "--head", &head, "--base", &base, "--body-file", f.to_str().unwrap()], &[]);
    assert_eq!(o.code, 1);
    assert_eq!(lines(&o), vec![format!("FLAG not-a-file {big_c}"), format!("OK {edge_c}"), format!("FLAG not-a-file {bin_c}"), "cites=3 ok=1 flag=2 unparsed=0".to_string()]);
}

#[test]
fn short_head_is_exit_2() {
    let c = case();
    let f = c.t.0.join("b.md");
    fs::write(&f, "x\n").unwrap();
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head[..12], "--base", &c.base, "--body-file", f.to_str().unwrap()], &[]);
    refused(&o);
}

#[test]
fn head_that_is_not_a_commit_is_exit_2() {
    let c = case();
    let f = c.t.0.join("b.md");
    fs::write(&f, "x\n").unwrap();
    let zero = "0".repeat(40);
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &zero, "--base", &c.base, "--body-file", f.to_str().unwrap()], &[]);
    refused(&o);
}

#[test]
fn base_that_is_not_a_commit_is_exit_2() {
    let c = case();
    let f = c.t.0.join("b.md");
    fs::write(&f, "x\n").unwrap();
    let tree = g(&c.dir, &["rev-parse", "HEAD^{tree}"]);
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head, "--base", &tree, "--body-file", f.to_str().unwrap()], &[]);
    refused(&o);
}

#[test]
fn unknown_flag_and_missing_flag_are_exit_2() {
    let c = case();
    let f = c.t.0.join("b.md");
    fs::write(&f, "x\n").unwrap();
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head, "--base", &c.base, "--body-file", f.to_str().unwrap(), "--nope", "1"], &[]);
    refused(&o);
    assert_eq!(o.stderr.trim_end(), USAGE);
    let o = run_args(&["--repo-dir", c.dir.to_str().unwrap(), "--head", &c.head, "--body-file", f.to_str().unwrap()], &[]);
    refused(&o);
}
