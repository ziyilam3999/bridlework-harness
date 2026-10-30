//! Engine tests. Every marker is built at run time from fragments; the list is injected as text, never read from the real list env.
use harness::detect::classes_for_line;
use harness::overlay;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn j(parts: &[&str]) -> String {
    parts.concat()
}

const DENY: &str = "zzdenyzz";
const REG: &str = "zzregzz";

fn list() -> overlay::List {
    overlay::parse(&format!("# synthetic\n{}\nregulated:{}\n", DENY, REG)).unwrap()
}

fn classes(line: &str) -> Vec<&'static str> {
    classes_for_line(line, &list())
}

fn has(line: &str, class: &str) -> bool {
    classes(line).contains(&class)
}

fn email() -> String {
    j(&["bob", "@", "example", ".invalid"])
}

#[test]
fn nine_classes() {
    assert_eq!(harness::classes::ALL.len(), 9);
    assert!(harness::classes::is_class("home-path"));
    assert!(!harness::classes::is_class("other"));
}

#[test]
fn deny_token_and_regulated_tag() {
    assert!(has("x ZzDenyZz y", "deny-token"));
    assert!(!has("x ZzDenyZz y", "regulated-id"));
    assert!(has("x ZZREGZZ y", "regulated-id"));
    assert!(!has("x ZZREGZZ y", "deny-token"));
    assert!(classes("nothing here").is_empty());
}

#[test]
fn deny_token_in_decoded_forms() {
    assert!(has("a%7ADenyzz", "deny-token") || has(&j(&["%7a", "zdenyzz"]), "deny-token"));
    assert!(has(&j(&["\\x7a", "zdenyzz"]), "deny-token"));
}

#[test]
fn email_class() {
    assert!(has(&format!("contact {}", email()), "email"));
    assert!(!has(&j(&["12345+bob", "@", "users.", "noreply.github.com"]), "email"));
    assert!(!has("a @ b and name@host", "email"));
}

#[test]
fn coauthor_class() {
    let l = format!("{}{}: Bob <{}>", "Co-authored", "-by", email());
    assert!(has(&l, "coauthor-personal"));
    let ok = j(&["Co-authored", "-by: Bot <1+bot", "@", "users.", "noreply.github.com>"]);
    assert!(!has(&ok, "coauthor-personal"));
}

#[test]
fn home_path_five_forms() {
    let raw = j(&["see /Us", "ers/", "tester", "/proj"]);
    let pct = j(&["see %2FUs", "ers%2F", "tester", "%2Fproj"]);
    let esc = j(&["see \\/Us", "ers\\/", "tester"]);
    let hexesc = j(&["see \\x2fUs", "ers\\x2f", "tester"]);
    let dash = j(&["dir -Us", "ers-", "tester", "-proj"]);
    let under = j(&["dir _Us", "ers_", "tester", "_proj"]);
    let lower = j(&["see /ho", "me/", "tester", "/proj"]);
    for l in [&raw, &pct, &esc, &hexesc, &dash, &under, &lower] {
        assert!(has(l, "home-path"), "missed: form of length {}", l.len());
    }
    assert!(!has(&j(&["my-ho", "me-page and some/ho", "me/dir"]), "home-path"));
    assert!(!has("the users guide", "home-path"));
}

#[test]
fn credential_shapes_and_clean_controls() {
    let a = j(&["AK", "IA", "ABCDEFGHIJKLMNOP"]);
    let g = j(&["gh", "p_", &"A1".repeat(15)]);
    let jwt = j(&["ey", "J", "hbGciOiJIUzI1NiIs", ".", "eyJzdWIiOiIxMjM0", ".", "SflKxwRJSMeKKF2QT4"]);
    let pem = j(&["-----BE", "GIN RSA PRI", "VATE KEY-----"]);
    let asg = j(&["my_set", "ting = \"", "Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Q", "\""]);
    for l in [&a, &g, &jwt, &pem, &asg] {
        assert!(has(l, "credential"));
    }
    assert!(!has("API_KEY=$MY_VAR", "credential"));
    assert!(!has("API_KEY=${MY_VAR}", "credential"));
    assert!(!has("key: ${{ secrets.THING }}", "credential"));
    assert!(!has("about 40 tokens per ticket", "credential"));
    assert!(!has("value = [redacted]", "credential"));
    assert!(!has(&format!("checksum = \"{}\"", "ab12".repeat(16)), "credential"));
    assert!(!has("see https://example.invalid/path/with/long/segments", "credential"));
}

#[test]
fn session_link_class() {
    let url = j(&["https://clau", "de.ai/code/", "session_01Abc"]);
    let trailer = j(&["Claude-", "Session: something"]);
    assert!(has(&url, "session-link"));
    assert!(has(&trailer, "session-link"));
    assert!(!has(&j(&["https://clau", "de.ai/download"]), "session-link"));
}

#[test]
fn baseline_number_class() {
    assert!(has(&j(&["base", "line.", "latency_ms: 123"]), "baseline-number"));
    assert!(has(&j(&["BASE", "LINE.", "p95 = 7"]), "baseline-number"));
    assert!(!has(&j(&["base", "line.", "latency_ms: n/a"]), "baseline-number"));
    assert!(!has("latency 123", "baseline-number"));
    assert!(!has("a baseline of 123 samples", "baseline-number"));
}

#[test]
fn synthetic_class() {
    assert!(has(&j(&["Synthetic", "-Test", "-Marker"]), "synthetic-test"));
    assert!(!has("synthetic data", "synthetic-test"));
}

#[test]
fn list_parsing_rules() {
    assert!(overlay::parse("").is_err());
    assert!(overlay::parse("  \n\n# only a comment\n").is_err());
    assert!(overlay::parse("regulated:\n").is_err());
    assert_eq!(overlay::parse("a\n# c\n\nregulated: b\n").unwrap().entries.len(), 2);
}

// ---- binary tests ----

const BIN: &str = env!("CARGO_BIN_EXE_harness");

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("hc-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn base_cmd() -> Command {
    let mut c = Command::new(BIN);
    c.env_remove("HARNESS_PRIVACY_LIST")
        .env_remove("HARNESS_PRIVACY_OLD_LIST")
        .env_remove("HARNESS_PRIVACY_LIST_SECRET")
        .env_remove("HARNESS_PRIVACY_TESTMODE");
    c
}

fn run_text(cmd: &mut Command, input: &str) -> (i32, String, String) {
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut ch = cmd.spawn().unwrap();
    ch.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    let o = ch.wait_with_output().unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

fn text_cmd(secret: &str, extra: &[&str]) -> Command {
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST_SECRET", secret).args(["privacy", "scan", "--text"]).args(extra);
    c
}

fn shape_ok(line: &str, text_mode: bool) -> bool {
    let parts: Vec<&str> = line.split(':').collect();
    if text_mode {
        parts.len() == 2 && !parts[0].is_empty() && parts[0].bytes().all(|c| c.is_ascii_lowercase() || c == b'_' || c == b'.') && !parts[1].is_empty() && parts[1].bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    } else {
        parts.len() == 3 && !parts[0].is_empty() && !parts[1].is_empty() && parts[1].bytes().all(|c| c.is_ascii_digit()) && !parts[2].is_empty() && parts[2].bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
    }
}

#[test]
fn version_and_classes() {
    let o = base_cmd().arg("--version").output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).starts_with("harness "));
    let o = base_cmd().args(["privacy", "classes"]).output().unwrap();
    assert_eq!(o.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&o.stdout).lines().count(), 9);
}

#[test]
fn text_mode_exit_codes_and_shape() {
    let (rc, out, _) = run_text(&mut text_cmd(DENY, &[]), "all clean here\n");
    assert_eq!((rc, out.as_str()), (0, ""));
    let (rc, out, _) = run_text(&mut text_cmd(DENY, &["--surface", "pr_title"]), &format!("hello {}\n", email()));
    assert_eq!(rc, 1);
    assert_eq!(out, "pr_title:email\n");
    for l in out.lines() {
        assert!(shape_ok(l, true));
    }
    assert!(!out.contains(&email()));
}

#[test]
fn fail_closed_on_list_problems() {
    // no list configured
    let (rc, out, _) = run_text(base_cmd().args(["privacy", "scan", "--text"]), "x\n");
    assert_eq!((rc, out.as_str()), (2, ""));
    // empty and whitespace-only secret
    for s in ["", "  \n\t\n"] {
        let (rc, out, _) = run_text(&mut text_cmd(s, &[]), "x\n");
        assert_eq!((rc, out.as_str()), (2, ""));
    }
    // unreadable path
    let d = tmp("missing");
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST", d.join("absent.txt")).args(["privacy", "scan", "--text"]);
    let (rc, out, _) = run_text(&mut c, "x\n");
    assert_eq!((rc, out.as_str()), (2, ""));
    // an empty secret wins over a good path (still an error)
    let good = d.join("good.txt");
    std::fs::write(&good, format!("{}\n", DENY)).unwrap();
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST", &good).env("HARNESS_PRIVACY_LIST_SECRET", "").args(["privacy", "scan", "--text"]);
    let (rc, out, _) = run_text(&mut c, "x\n");
    assert_eq!((rc, out.as_str()), (2, ""));
    // path alone works
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST", &good).args(["privacy", "scan", "--text"]);
    let (rc, _, _) = run_text(&mut c, &format!("a {} b\n", DENY));
    assert_eq!(rc, 1);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn drift_check() {
    let d = tmp("drift");
    let a = d.join("a.txt");
    let b = d.join("b.txt");
    std::fs::write(&a, format!("{}\n", DENY)).unwrap();
    std::fs::write(&b, format!("{}\n", DENY)).unwrap();
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST", &a).env("HARNESS_PRIVACY_OLD_LIST", &b).args(["privacy", "scan", "--text"]);
    assert_eq!(run_text(&mut c, "x\n").0, 0);
    std::fs::write(&b, format!("{}\nextra\n", DENY)).unwrap();
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST", &a).env("HARNESS_PRIVACY_OLD_LIST", &b).args(["privacy", "scan", "--text"]);
    let (rc, out, _) = run_text(&mut c, "x\n");
    assert_eq!((rc, out.as_str()), (2, ""));
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST", &a).env("HARNESS_PRIVACY_OLD_LIST", d.join("nope.txt")).args(["privacy", "scan", "--text"]);
    assert_eq!(run_text(&mut c, "x\n").0, 2);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn testmode_only_admits_synthetic() {
    let m = j(&["Synthetic", "-Test", "-Marker"]);
    let (rc, out, _) = run_text(&mut text_cmd(DENY, &[]), &format!("{}\n", m));
    assert_eq!((rc, out.as_str()), (1, "text:synthetic-test\n"));
    let (rc, out, _) = run_text(&mut text_cmd(DENY, &["--testmode", "synthetic-test"]), &format!("{}\n", m));
    assert_eq!((rc, out.as_str()), (0, ""));
    let (rc, _, _) = run_text(&mut text_cmd(DENY, &["--testmode", "synthetic-test"]), &format!("{} {}\n", m, email()));
    assert_eq!(rc, 1);
    let (rc, _, _) = run_text(&mut text_cmd(DENY, &["--testmode", "email"]), &format!("{}\n", m));
    assert_eq!(rc, 1);
}

#[test]
fn explain_prints_placeholder_only() {
    let mut c = base_cmd();
    c.env("HARNESS_PRIVACY_LIST_SECRET", DENY).args(["privacy", "explain", "--text"]);
    let (rc, out, _) = run_text(&mut c, &format!("x {}\n", email()));
    assert_eq!(rc, 1);
    assert!(out.contains("value=[redacted]"));
    assert!(out.contains("class=email"));
    assert!(!out.contains(&email()));
}

// ---- git-backed modes ----

fn git(dir: &PathBuf, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", j(&["1+t", "@", "users.", "noreply.example"]))
        .env("GIT_COMMITTER_NAME", "Tester")
        .env("GIT_COMMITTER_EMAIL", j(&["1+t", "@", "users.", "noreply.example"]))
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git failed: {:?}", args);
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn scan_in(dir: &PathBuf, args: &[&str]) -> (i32, String, String) {
    let o = base_cmd()
        .current_dir(dir)
        .env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .args(["privacy", "scan"])
        .args(args)
        .output()
        .unwrap();
    (o.status.code().unwrap(), String::from_utf8_lossy(&o.stdout).into_owned(), String::from_utf8_lossy(&o.stderr).into_owned())
}

#[test]
fn git_modes() {
    let d = tmp("git");
    git(&d, &["init", "-q"]);
    std::fs::write(d.join("clean.txt"), "nothing to see\n").unwrap();
    git(&d, &["add", "clean.txt"]);
    // staged clean
    assert_eq!(scan_in(&d, &["--staged"]).0, 0);
    git(&d, &["commit", "-q", "-m", "first"]);
    let (rc, out, err) = scan_in(&d, &["--range", "HEAD"]);
    assert_eq!((rc, out.as_str()), (0, ""));
    assert!(err.contains("scanned_commits=1"));
    let (rc, _, err) = scan_in(&d, &["--tree"]);
    assert_eq!(rc, 0);
    assert!(err.contains("files=1"));
    // a dirty file and a dirty message
    std::fs::write(d.join("f.txt"), format!("line one\ncontact {}\n", email())).unwrap();
    git(&d, &["add", "f.txt"]);
    let (rc, out, _) = scan_in(&d, &["--staged"]);
    assert_eq!((rc, out.as_str()), (1, "f.txt:2:email\n"));
    git(&d, &["commit", "-q", "-m", &format!("note {}", email())]);
    let (rc, out, err) = scan_in(&d, &["--range", "HEAD"]);
    assert_eq!(rc, 1);
    assert!(err.contains("scanned_commits=2"));
    assert!(out.lines().any(|l| l == "f.txt:2:email"));
    assert!(out.lines().any(|l| l.contains(".message:1:email")));
    for l in out.lines() {
        assert!(shape_ok(l, false), "bad shape");
    }
    assert!(!out.contains(&email()));
    // an empty range argument set is an error
    assert_eq!(scan_in(&d, &["--range"]).0, 2);
    // tree sees the dirty file
    let (rc, out, _) = scan_in(&d, &["--tree"]);
    assert_eq!((rc, out.as_str()), (1, "f.txt:2:email\n"));
    // annotated tag with a dirty message
    git(&d, &["tag", "-a", "v1", "-m", &format!("tag {}", email())]);
    let (rc, out, _) = scan_in(&d, &["--tag", "v1"]);
    assert_eq!(rc, 1);
    assert!(out.contains(".tag:1:email"));
    assert_eq!(scan_in(&d, &["--tag", "HEAD"]).0, 2);
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn binary_and_oversize_refused() {
    let d = tmp("bin");
    git(&d, &["init", "-q"]);
    std::fs::write(d.join("b.bin"), [0u8, 1, 2, 3, 0, 255]).unwrap();
    git(&d, &["add", "b.bin"]);
    let (rc, out, _) = scan_in(&d, &["--staged"]);
    assert_eq!((rc, out.as_str()), (2, ""));
    git(&d, &["rm", "-q", "--cached", "b.bin"]);
    std::fs::write(d.join("big.txt"), "a".repeat(1024 * 1024 + 10)).unwrap();
    git(&d, &["add", "big.txt"]);
    let (rc, out, _) = scan_in(&d, &["--staged"]);
    assert_eq!((rc, out.as_str()), (2, ""));
    let _ = std::fs::remove_dir_all(d);
}

#[test]
fn invalid_utf8_refused() {
    let d = tmp("utf8");
    git(&d, &["init", "-q"]);
    std::fs::write(d.join("x.txt"), [b'a', 0xff, 0xfe, b'\n']).unwrap();
    git(&d, &["add", "x.txt"]);
    let (rc, out, _) = scan_in(&d, &["--staged"]);
    assert_eq!((rc, out.as_str()), (2, ""));
    let _ = std::fs::remove_dir_all(d);
}

// ---- web-flow committer allowance ----

fn commit_as(d: &PathBuf, author: (&str, &str), committer: (&str, &str)) {
    let o = Command::new("git")
        .current_dir(d)
        .env("GIT_AUTHOR_NAME", author.0)
        .env("GIT_AUTHOR_EMAIL", author.1)
        .env("GIT_COMMITTER_NAME", committer.0)
        .env("GIT_COMMITTER_EMAIL", committer.1)
        .args(["-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "synthetic change"])
        .output()
        .unwrap();
    assert!(o.status.success(), "commit failed");
}

fn range_head(name: &str, author: (&str, &str), committer: (&str, &str)) -> (i32, String) {
    let d = tmp(name);
    git(&d, &["init", "-q"]);
    commit_as(&d, author, committer);
    let (rc, out, _) = scan_in(&d, &["--range", "HEAD"]);
    let _ = std::fs::remove_dir_all(d);
    (rc, out)
}

fn plain_noreply() -> String {
    j(&["1+t", "@", "users.", "noreply.example"])
}

fn webflow_addr() -> String {
    j(&["no", "reply", "@", "github", ".com"])
}

#[test]
fn webflow_committer_allowed_only_in_committer_field() {
    let pn = plain_noreply();
    let ok_author = ("Tester", pn.as_str());
    let wf = webflow_addr();
    // (a) the exact web-flow pair as committer passes
    let (rc, out) = range_head("wf-a", ok_author, ("GitHub", &wf));
    assert_eq!((rc, out.as_str()), (0, ""));
    // (b) the same pair in the author field still fails
    let (rc, out) = range_head("wf-b", ("GitHub", wf.as_str()), ("Tester", pn.as_str()));
    assert_eq!(rc, 1);
    assert!(out.lines().any(|l| l.ends_with(".author:1:email")));
    // (c) a different no-reply-style committer still fails
    let other = j(&["no", "reply", "@", "example", ".invalid"]);
    let (rc, out) = range_head("wf-c", ok_author, ("GitHub", &other));
    assert_eq!(rc, 1);
    assert!(out.lines().any(|l| l.ends_with(".committer:1:email")));
    // right address under a different name still fails
    let (rc, _) = range_head("wf-c2", ok_author, ("Someone", &wf));
    assert_eq!(rc, 1);
    // (d) a personal-style address as committer still fails
    let (rc, out) = range_head("wf-d", ok_author, ("GitHub", &email()));
    assert_eq!(rc, 1);
    assert!(out.lines().any(|l| l.ends_with(".committer:1:email")));
}
