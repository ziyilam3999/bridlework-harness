//! Pre-commit hook tests. Each test makes a throwaway git repo, turns the hook on, and runs a real `git commit`.
//! Every planted string is assembled at run time or printed by the engine's own `fixtures` command.
//! The deny list is a synthetic list injected into the child process only; the real list is never read.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const HOOK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../.githooks/pre-commit");
const MSG_HOOK: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../.githooks/commit-msg");
const README: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../README.md");

fn j(parts: &[&str]) -> String {
    parts.concat()
}

fn engine_out(args: &[&str]) -> String {
    let o = Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap();
    assert_eq!(o.status.code(), Some(0), "fixtures command failed");
    String::from_utf8(o.stdout).unwrap().trim_end().to_string()
}

fn fx_line(name: &str) -> String {
    engine_out(&["fixtures", "line", name])
}

fn fx_marker(name: &str) -> String {
    engine_out(&["fixtures", "marker", name])
}

fn synth_list() -> String {
    let mut s = engine_out(&["fixtures", "list"]);
    s.push('\n');
    s
}

const CLEAN: &str = "just an ordinary line of text";

struct Scratch {
    dir: PathBuf,
    list: Option<String>,
    extra_env: Vec<(String, String)>,
}

fn scratch(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("pc-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    let s = Scratch { dir, list: Some(synth_list()), extra_env: Vec::new() };
    s.git(&["init", "-q"]);
    s
}

impl Scratch {
    fn cmd(&self, prog: &str) -> Command {
        let mut c = Command::new(prog);
        c.current_dir(&self.dir)
            .env_remove("HARNESS_PRIVACY_LIST")
            .env_remove("HARNESS_PRIVACY_OLD_LIST")
            .env_remove("HARNESS_PRIVACY_LIST_SECRET")
            .env_remove("HARNESS_PRIVACY_TESTMODE")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", j(&["t", "@", "example", ".invalid"]))
            .env("GIT_COMMITTER_EMAIL", j(&["t", "@", "example", ".invalid"]))
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .stdin(Stdio::null());
        c
    }

    fn git(&self, args: &[&str]) -> (i32, String) {
        let o = self.cmd("git").args(args).output().unwrap();
        let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
        t.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap(), t)
    }

    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.dir.join(rel);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(p, bytes).unwrap();
    }

    fn stage(&self, rel: &str, content: &str) {
        self.write(rel, format!("{}\n", content).as_bytes());
        assert_eq!(self.git(&["add", rel]).0, 0);
    }

    fn install_engine(&self) {
        let d = self.dir.join("core/rust/target/release");
        fs::create_dir_all(&d).unwrap();
        fs::copy(BIN, d.join("harness")).unwrap();
    }

    fn install_stub_engine(&self, code: i32) {
        let d = self.dir.join("core/rust/target/release");
        fs::create_dir_all(&d).unwrap();
        let p = d.join("harness");
        fs::write(&p, format!("#!/bin/sh\nexit {}\n", code)).unwrap();
        chmod(&p, 0o755);
    }

    /// Turn the hook on the way the README says: copy the hook folder in and point the hooks path at it.
    fn enable_hook(&self) {
        let d = self.dir.join(".githooks");
        fs::create_dir_all(&d).unwrap();
        fs::copy(HOOK, d.join("pre-commit")).unwrap();
        chmod(&d.join("pre-commit"), 0o755);
        assert_eq!(self.git(&["config", "core.hooksPath", ".githooks"]).0, 0);
    }

    /// Turn only the message hook on, so a message test can be judged by that hook alone.
    fn enable_msg_hook(&self) {
        let d = self.dir.join(".githooks");
        fs::create_dir_all(&d).unwrap();
        fs::copy(MSG_HOOK, d.join("commit-msg")).unwrap();
        chmod(&d.join("commit-msg"), 0o755);
        assert_eq!(self.git(&["config", "core.hooksPath", ".githooks"]).0, 0);
    }

    /// Seed a first commit with no hook on (so a planted string can already sit in history).
    fn seed(&self, rel: &str, content: &str) {
        self.stage(rel, content);
        let (rc, t) = self.git(&["commit", "-q", "-m", "seed"]);
        assert_eq!(rc, 0, "seed commit failed: {}", t);
    }

    /// State that a refused commit must leave untouched: HEAD, every ref, every reachable object.
    fn state(&self) -> String {
        let head = self.git(&["rev-parse", "-q", "--verify", "HEAD"]).1;
        let refs = self.git(&["for-each-ref"]).1;
        let objs = self.git(&["rev-list", "--all", "--objects"]).1;
        format!("{}|{}|{}", head, refs, objs)
    }

    fn commit_with(&self, msg: &str) -> (i32, String) {
        self.run_commit(&["-m", msg])
    }

    fn run_commit(&self, args: &[&str]) -> (i32, String) {
        let mut c = self.cmd("git");
        c.arg("commit").arg("-q").args(args);
        if let Some(l) = &self.list {
            c.env("HARNESS_PRIVACY_LIST_SECRET", l);
        }
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
        t.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap(), t)
    }

    fn commit(&self) -> (i32, String) {
        self.commit_with("change")
    }

    /// The commit must be refused: non-zero exit, state identical, and the output never carries a matched value.
    fn assert_refused(&self, why: &str) -> String {
        let before = self.state();
        let (rc, t) = self.commit();
        assert_ne!(rc, 0, "{}: commit was not refused", why);
        assert_eq!(before, self.state(), "{}: state changed by a refused commit", why);
        t
    }

    /// The commit must go through: exit 0 and HEAD advanced by exactly one commit.
    fn assert_commits(&self, why: &str) {
        let n = |s: &Scratch| s.git(&["rev-list", "--all", "--count"]).1.trim().to_string();
        let before: i32 = n(self).parse().unwrap();
        let (rc, t) = self.commit();
        assert_eq!(rc, 0, "{}: clean commit refused: {}", why, t);
        let after: i32 = n(self).parse().unwrap();
        assert_eq!(after, before + 1, "{}: HEAD did not advance by one", why);
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn chmod(p: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
}

/// A ready scratch repo: engine in place, hook on, one clean seed commit unless `root` is asked for.
fn ready(name: &str, root: bool) -> Scratch {
    let s = scratch(name);
    s.install_engine();
    if !root {
        s.seed("seed.txt", CLEAN);
    }
    s.enable_hook();
    s
}

/// Plant one line in a fresh file, expect refusal naming `class` with no matched bytes; then the clean twin commits.
fn pair(name: &str, planted: &str, marker: &str, class: &str, clean: &str, root: bool) {
    let s = ready(name, root);
    s.stage("f.txt", planted);
    let t = s.assert_refused(name);
    assert!(t.contains(&format!("class={}", class)), "{}: class {} not named in: {}", name, class, t);
    if !marker.is_empty() {
        assert!(!t.contains(marker), "{}: output carried the matched bytes", name);
    }
    s.stage("f.txt", clean);
    s.assert_commits(&format!("{} clean twin", name));
}

// ---- AC 1: home path ----

#[test]
fn ac1_home_path_raw() {
    let raw = j(&["see /Us", "ers/", "tester", "/proj"]);
    pair("home-raw", &raw, "tester", "home-path", CLEAN, false);
}

#[test]
fn ac1_home_path_dash_encoded_from_fixtures() {
    pair("home-dash", &fx_line("home-dash"), &fx_marker("home-dash"), "home-path", CLEAN, false);
}

// ---- AC 2: email ----

#[test]
fn ac2_email_and_noreply_twin() {
    let addr = j(&["bob", "@", "example", ".invalid"]);
    let noreply = j(&["12345+bob", "@", "users.", "noreply.github.com"]);
    pair("email", &format!("contact {}", addr), &addr, "email", &format!("contact {}", noreply), false);
}

// ---- AC 3: brand token through the list ----

#[test]
fn ac3_deny_token_regulated_and_case() {
    let deny = fx_marker("deny-profile");
    let reg = fx_marker("regulated");
    pair("deny", &format!("owner: {}", deny), &deny, "deny-token", CLEAN, false);
    pair("reg", &format!("ref {}", reg), &reg, "regulated-id", CLEAN, false);
    pair("deny-up", &format!("owner: {}", deny.to_uppercase()), &deny.to_uppercase(), "deny-token", CLEAN, false);
    pair("reg-up", &format!("ref {}", reg.to_uppercase()), &reg.to_uppercase(), "regulated-id", CLEAN, false);
}

// ---- AC 4: credential ----

#[test]
fn ac4_credential_and_variable_reference_twin() {
    let cred = fx_line("credential");
    pair("cred", &cred, &fx_marker("credential"), "credential", "API_KEY=$MY_VAR", false);
}

// ---- AC 5: only added lines count ----

#[test]
fn ac5a_removal_only_commit_goes_through() {
    let s = scratch("removal");
    s.install_engine();
    s.seed("f.txt", &fx_line("home-raw"));
    s.enable_hook();
    s.stage("f.txt", CLEAN);
    s.assert_commits("removal of a planted line");
}

#[test]
fn ac5b_unstaged_planted_file_does_not_stop_a_clean_commit() {
    let s = ready("unstaged", false);
    s.write("f.txt", format!("{}\n", fx_line("home-raw")).as_bytes());
    s.write("other.txt", format!("{}\n", fx_line("email")).as_bytes());
    s.stage("g.txt", CLEAN);
    s.assert_commits("unstaged planted files");
}

#[test]
fn ac5c_judged_on_staged_content_only() {
    let s = ready("staged-only", false);
    // staged clean, then the working tree is edited to a planted line: the commit judges the staged side
    s.stage("f.txt", CLEAN);
    s.write("f.txt", format!("{}\n", fx_line("home-raw")).as_bytes());
    s.assert_commits("clean staged, planted working tree");
    // staged planted, then the working tree is edited clean: still refused, the index holds the plant
    s.stage("h.txt", &fx_line("home-raw"));
    s.write("h.txt", format!("{}\n", CLEAN).as_bytes());
    let t = s.assert_refused("planted staged, clean working tree");
    assert!(t.contains("class=home-path"));
    // staged again after the edit: now clean, goes through
    assert_eq!(s.git(&["add", "h.txt"]).0, 0);
    s.assert_commits("re-staged clean");
}

// ---- AC 6: root commit ----

#[test]
fn ac6_root_commit() {
    let s = ready("root", true);
    assert_ne!(s.git(&["rev-parse", "-q", "--verify", "HEAD"]).0, 0, "scratch repo must start unborn");
    s.stage("f.txt", &fx_line("home-raw"));
    let t = s.assert_refused("root commit planted");
    assert!(t.contains("class=home-path"));
    s.stage("f.txt", CLEAN);
    let (rc, t) = s.commit();
    assert_eq!(rc, 0, "root clean twin refused: {}", t);
    assert_eq!(s.git(&["rev-list", "--all", "--count"]).1.trim(), "1");
}

// ---- AC 7: the scan cannot decide ----

fn undecided_base(name: &str) -> Scratch {
    let s = ready(name, false);
    s.stage("f.txt", CLEAN);
    s
}

#[test]
fn ac7a_engine_missing() {
    let s = scratch("no-engine");
    s.seed("seed.txt", CLEAN);
    s.enable_hook();
    s.stage("f.txt", CLEAN);
    let t = s.assert_refused("engine missing");
    assert!(t.contains("engine missing"));
}

#[test]
fn ac7b_no_list_configured() {
    let mut s = undecided_base("no-list");
    s.list = None;
    s.assert_refused("list not configured");
}

#[test]
fn ac7c_empty_or_blank_list() {
    let mut s = undecided_base("blank-list");
    for blank in ["", "   \n\t\n", "# only a comment\n"] {
        s.list = Some(blank.to_string());
        s.assert_refused("blank list text");
    }
    s.list = None;
    let empty = s.dir.join("empty.list");
    fs::write(&empty, "").unwrap();
    s.extra_env = vec![("HARNESS_PRIVACY_LIST".to_string(), empty.to_string_lossy().into_owned())];
    s.assert_refused("empty list file");
}

#[test]
fn ac7d_unreadable_list_file() {
    let mut s = undecided_base("bad-list");
    s.list = None;
    let missing = s.dir.join("absent.list");
    s.extra_env = vec![("HARNESS_PRIVACY_LIST".to_string(), missing.to_string_lossy().into_owned())];
    s.assert_refused("list path missing");
    let locked = s.dir.join("locked.list");
    fs::write(&locked, synth_list()).unwrap();
    chmod(&locked, 0o000);
    s.extra_env = vec![("HARNESS_PRIVACY_LIST".to_string(), locked.to_string_lossy().into_owned())];
    s.assert_refused("list file unreadable");
    chmod(&locked, 0o600);
}

#[test]
fn ac7e_binary_file_staged() {
    let s = undecided_base("binary");
    s.write("b.bin", &[0u8, 1, 2, 0, 255, 0, 7]);
    assert_eq!(s.git(&["add", "b.bin"]).0, 0);
    s.assert_refused("binary file");
}

#[test]
fn ac7f_file_over_size_cap() {
    let s = undecided_base("big");
    let body = "ordinary words here\n".repeat(70_000);
    s.write("big.txt", body.as_bytes());
    assert_eq!(s.git(&["add", "big.txt"]).0, 0);
    s.assert_refused("file over the size cap");
}

#[test]
fn ac7g_not_valid_utf8() {
    let s = undecided_base("utf8");
    let mut b = b"plain text ".to_vec();
    b.extend_from_slice(&[0xff, 0xfe, 0xfd]);
    b.extend_from_slice(b" more text\n");
    s.write("bad.txt", &b);
    assert_eq!(s.git(&["add", "bad.txt"]).0, 0);
    s.assert_refused("invalid utf-8");
}

#[test]
fn ac7h_engine_stub_with_no_verdict() {
    for code in [2, 3, 127] {
        let s = scratch(&format!("stub{}", code));
        s.install_stub_engine(code);
        s.seed("seed.txt", CLEAN);
        s.enable_hook();
        s.stage("f.txt", CLEAN);
        s.assert_refused(&format!("stub engine exit {}", code));
    }
}

// ---- AC 8: no bypass ----

fn switch_env() -> Vec<(String, String)> {
    let mut v = vec![("HARNESS_PRIVACY_TESTMODE".to_string(), "synthetic-test".to_string())];
    for n in [
        "PRIVACY_GATE_OFF", "HARNESS_OFF", "HARNESS_PRIVACY_OFF", "PRIVACY_OVERRIDE", "HARNESS_OVERRIDE",
        "HARNESS_PRIVACY_OVERRIDE", "PRIVACY_OK", "HARNESS_PRIVACY_OK", "PRIVACY_DISABLE", "HARNESS_DISABLE",
        "HARNESS_PRIVACY_DISABLE", "SKIP", "SKIP_HOOKS", "SKIP_PRIVACY", "HARNESS_SKIP", "HUSKY", "NO_VERIFY",
        "GIT_SKIP_HOOKS",
    ] {
        v.push((n.to_string(), "1".to_string()));
        v.push((format!("{}_X", n), "0".to_string()));
    }
    v
}

fn planted_cases() -> Vec<(&'static str, String)> {
    vec![
        ("home-path", fx_line("home-raw")),
        ("email", fx_line("email")),
        ("deny-token", fx_line("deny-profile")),
        ("regulated-id", fx_line("regulated")),
        ("credential", fx_line("credential")),
    ]
}

#[test]
fn ac8a_no_environment_switch() {
    for (class, line) in planted_cases() {
        let mut s = ready(&format!("sw-{}", class), false);
        s.extra_env = switch_env();
        s.stage("f.txt", &line);
        let t = s.assert_refused(class);
        assert!(t.contains(&format!("class={}", class)), "{}: not named", class);
    }
    // the test-mode value would admit the synthetic marker class if the hook honoured it; it must not
    let mut s = ready("sw-synthetic", false);
    s.extra_env = switch_env();
    s.stage("f.txt", &engine_out(&["fixtures", "extra", "synthetic"]));
    s.assert_refused("synthetic marker with test-mode variable set");
}

#[test]
fn ac8b_inline_skip_markers_change_nothing() {
    let shapes = [
        "privacy-ok", "# nosec", "harness-skip", "allow-privacy", "NOQA: privacy", "[skip hook]", "no-verify",
    ];
    for (class, line) in planted_cases() {
        let s = ready(&format!("inl-{}", class), false);
        let mut body = String::new();
        for sh in &shapes {
            body.push_str(&format!("{}\n", sh));
        }
        body.push_str(&line);
        s.stage("f.txt", &body);
        let before = s.state();
        let (rc, t) = s.commit_with("privacy-ok skip-privacy [skip hook] no-verify");
        assert_ne!(rc, 0, "{}: skip-shaped markers let a planted line through", class);
        assert_eq!(before, s.state());
        assert!(t.contains(&format!("class={}", class)));
    }
}

#[test]
fn ac8c_hook_source_has_no_variable_or_testmode_door() {
    let src = fs::read_to_string(HOOK).unwrap();
    assert!(!src.contains(&j(&["HARNESS", "_"])), "hook mentions an engine variable");
    assert!(!src.contains(&j(&["--test", "mode"])), "hook passes the test-mode argument");
    assert!(!src.contains(&j(&["test", "mode"])), "hook mentions test mode");
    assert!(!src.contains("--no-verify"));
}

// ---- AC 9: only the staged scan, touches nothing ----

#[test]
fn ac9_staged_mode_only_and_nothing_rewritten() {
    let src = fs::read_to_string(HOOK).unwrap();
    for other in ["--range", "--tree", "--tag", "--text", "git add", "update-index", "checkout", "reset", "git rm", "write-tree"] {
        assert!(!src.contains(other), "hook uses {}", other);
    }
    assert!(src.contains("privacy scan --staged"));
    let s = ready("untouched", false);
    s.write("w.txt", b"working tree only\n");
    s.stage("f.txt", &fx_line("home-raw"));
    let index = s.git(&["ls-files", "-s"]).1;
    let tree = |s: &Scratch| -> String {
        let mut v: Vec<String> = Vec::new();
        fn walk(d: &Path, root: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(d).unwrap() {
                let p = e.unwrap().path();
                if p.file_name().unwrap() == ".git" {
                    continue;
                }
                if p.is_dir() {
                    walk(&p, root, out);
                } else {
                    let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
                    out.push(format!("{}={:?}", rel, fs::read(&p).unwrap()));
                }
            }
        }
        walk(&s.dir, &s.dir, &mut v);
        v.sort();
        v.join("\n")
    };
    let before = tree(&s);
    s.assert_refused("untouched");
    assert_eq!(index, s.git(&["ls-files", "-s"]).1, "index changed by a refused commit");
    assert_eq!(before, tree(&s), "working tree changed by a refused commit");
}

// ---- AC 10: README ----

#[test]
fn ac10_readme_has_map_line_and_turn_on_line() {
    let r = fs::read_to_string(README).unwrap();
    assert!(r.contains("`.githooks/pre-commit`"), "map line for the hook is missing");
    assert_eq!(r.matches("git config core.hooksPath .githooks").count(), 1, "turn-on line must appear exactly once");
}

// ================= Q1-2: the commit-msg hook =================
// These tests turn on the message hook alone, so every refusal below comes from it.

fn msg_ready(name: &str, root: bool) -> Scratch {
    let s = scratch(name);
    s.install_engine();
    if !root {
        s.seed("seed.txt", CLEAN);
    }
    s.enable_msg_hook();
    s
}

impl Scratch {
    /// Commit the staged change with `msg`, passed by `-m` or by a message file (`-F`).
    fn commit_message(&self, msg: &str, by_file: bool) -> (i32, String) {
        if !by_file {
            return self.run_commit(&["-m", msg]);
        }
        let p = self.dir.with_extension("msg");
        fs::write(&p, format!("{}\n", msg)).unwrap();
        let r = self.run_commit(&["-F", p.to_str().unwrap()]);
        let _ = fs::remove_file(&p);
        r
    }

    /// Refused: non-zero exit, state identical, the report names the class and the message surface, no matched bytes.
    fn assert_msg_refused(&self, msg: &str, by_file: bool, class: &str, marker: &str) -> String {
        let before = self.state();
        let (rc, t) = self.commit_message(msg, by_file);
        assert_ne!(rc, 0, "{}: message was not refused (file={})", class, by_file);
        assert_eq!(before, self.state(), "{}: state changed by a refused commit", class);
        assert!(t.contains(&format!("class={}", class)), "{}: class not named in: {}", class, t);
        assert!(t.contains("commit_message"), "{}: report does not name the message surface: {}", class, t);
        if !marker.is_empty() {
            assert!(!t.contains(marker), "{}: output carried the matched bytes", class);
        }
        t
    }

    fn assert_msg_commits(&self, msg: &str, by_file: bool, why: &str) {
        let n = |s: &Scratch| s.git(&["rev-list", "--all", "--count"]).1.trim().parse::<i32>().unwrap();
        let before = n(self);
        let (rc, t) = self.commit_message(msg, by_file);
        assert_eq!(rc, 0, "{}: clean message refused: {}", why, t);
        assert_eq!(n(self), before + 1, "{}: HEAD did not advance by one", why);
    }

    /// Run the message hook directly on a message file, from inside the scratch repo.
    fn run_msg_hook(&self, arg: Option<&Path>) -> (i32, String) {
        let mut c = self.cmd("sh");
        c.arg(self.dir.join(".githooks/commit-msg"));
        if let Some(a) = arg {
            c.arg(a);
        }
        if let Some(l) = &self.list {
            c.env("HARNESS_PRIVACY_LIST_SECRET", l);
        }
        for (k, v) in &self.extra_env {
            c.env(k, v);
        }
        let o = c.output().unwrap();
        let mut t = String::from_utf8_lossy(&o.stdout).into_owned();
        t.push_str(&String::from_utf8_lossy(&o.stderr));
        (o.status.code().unwrap(), t)
    }
}

// ---- AC 12: message carries a string ----

fn msg_cases() -> Vec<(&'static str, &'static str)> {
    vec![
        ("home-raw", "home-path"),
        ("home-dash", "home-path"),
        ("email", "email"),
        ("deny-profile", "deny-token"),
        ("deny-employer", "deny-token"),
        ("regulated", "regulated-id"),
        ("credential", "credential"),
    ]
}

#[test]
fn ac12_message_carries_a_string_by_flag_and_by_file() {
    for (fixture, class) in msg_cases() {
        for by_file in [false, true] {
            let s = msg_ready(&format!("m12-{}-{}", fixture, by_file), false);
            s.stage("f.txt", CLEAN);
            let planted = format!("change\n\n{}", fx_line(fixture));
            s.assert_msg_refused(&planted, by_file, class, &fx_marker(fixture));
            // the same staged change with a clean message goes through
            s.assert_msg_commits("change", by_file, &format!("{} clean twin", fixture));
        }
    }
}

#[test]
fn ac12_upper_case_list_tokens_in_message_are_refused() {
    for fixture in ["deny-profile", "regulated"] {
        let s = msg_ready(&format!("m12up-{}", fixture), false);
        s.stage("f.txt", CLEAN);
        let m = fx_marker(fixture);
        let class = if fixture == "regulated" { "regulated-id" } else { "deny-token" };
        s.assert_msg_refused(&format!("note {}", m.to_uppercase()), false, class, &m.to_uppercase());
    }
}

#[test]
fn ac12_message_judged_alone_staged_content_is_not_its_business() {
    // a planted line already in the staged change does not stop the message hook (the file hook owns that)
    let s = msg_ready("m12-staged-plant", false);
    s.stage("f.txt", &fx_line("home-raw"));
    s.assert_msg_commits("change", false, "message hook ignores staged content");
    // a removal-only commit with a clean message commits
    let s = scratch("m12-removal");
    s.install_engine();
    s.seed("f.txt", &fx_line("home-raw"));
    s.enable_msg_hook();
    s.stage("f.txt", CLEAN);
    s.assert_msg_commits("remove the planted line", false, "removal-only commit");
}

#[test]
fn ac12_root_commit_message() {
    let s = msg_ready("m12-root", true);
    assert_ne!(s.git(&["rev-parse", "-q", "--verify", "HEAD"]).0, 0, "scratch repo must start unborn");
    s.stage("f.txt", CLEAN);
    s.assert_msg_refused(&fx_line("home-raw"), false, "home-path", &fx_marker("home-raw"));
    s.assert_msg_commits("first", false, "root clean twin");
    assert_eq!(s.git(&["rev-list", "--all", "--count"]).1.trim(), "1");
}

// ---- AC 13: trailers ----

#[test]
fn ac13_session_trailer_and_coauthor_trailers() {
    let s = msg_ready("m13", false);
    s.stage("f.txt", CLEAN);
    let link = fx_marker("session-link");
    let session = format!("change\n\n{}{}", j(&["Claude-", "Session: "]), link);
    s.assert_msg_refused(&session, false, "session-link", &link);
    let personal = format!("change\n\n{}", engine_out(&["fixtures", "extra", "coauthor-trailer"]));
    s.assert_msg_refused(&personal, false, "coauthor-personal", &engine_out(&["fixtures", "extra", "addr"]));
    let noreply = j(&["Co-authored", "-by: Fixture <", "12345+fixture", "@", "users.", "noreply.github.com", ">"]);
    s.assert_msg_commits(&format!("change\n\n{}", noreply), false, "noreply co-author trailer");
}

// ---- AC 14: the scan cannot decide ----

fn undecided_msg(name: &str) -> Scratch {
    let s = msg_ready(name, false);
    s.stage("f.txt", CLEAN);
    s
}

fn assert_msg_undecided(s: &Scratch, why: &str) {
    let before = s.state();
    let (rc, _) = s.commit_message("change", false);
    assert_ne!(rc, 0, "{}: commit was not refused", why);
    assert_eq!(before, s.state(), "{}: state changed by a refused commit", why);
}

#[test]
fn ac14_engine_missing() {
    let s = scratch("m14-no-engine");
    s.seed("seed.txt", CLEAN);
    s.enable_msg_hook();
    s.stage("f.txt", CLEAN);
    let before = s.state();
    let (rc, t) = s.commit_message("change", false);
    assert_ne!(rc, 0);
    assert_eq!(before, s.state());
    assert!(t.contains("engine missing"), "{}", t);
}

#[test]
fn ac14_list_not_configured_or_blank() {
    let mut s = undecided_msg("m14-no-list");
    s.list = None;
    assert_msg_undecided(&s, "list not configured");
    for blank in ["", "   \n\t\n", "# only a comment\n"] {
        s.list = Some(blank.to_string());
        assert_msg_undecided(&s, "blank list text");
    }
    s.list = None;
    let empty = s.dir.join("empty.list");
    fs::write(&empty, "").unwrap();
    s.extra_env = vec![("HARNESS_PRIVACY_LIST".to_string(), empty.to_string_lossy().into_owned())];
    assert_msg_undecided(&s, "empty list file");
}

#[test]
fn ac14_unreadable_list_file() {
    let mut s = undecided_msg("m14-bad-list");
    s.list = None;
    let missing = s.dir.join("absent.list");
    s.extra_env = vec![("HARNESS_PRIVACY_LIST".to_string(), missing.to_string_lossy().into_owned())];
    assert_msg_undecided(&s, "list path missing");
    let locked = s.dir.join("locked.list");
    fs::write(&locked, synth_list()).unwrap();
    chmod(&locked, 0o000);
    s.extra_env = vec![("HARNESS_PRIVACY_LIST".to_string(), locked.to_string_lossy().into_owned())];
    assert_msg_undecided(&s, "list file unreadable");
    chmod(&locked, 0o600);
}

#[test]
fn ac14_engine_stub_with_no_verdict() {
    for code in [2, 3, 127] {
        let s = scratch(&format!("m14-stub{}", code));
        s.install_stub_engine(code);
        s.seed("seed.txt", CLEAN);
        s.enable_msg_hook();
        s.stage("f.txt", CLEAN);
        assert_msg_undecided(&s, &format!("stub engine exit {}", code));
    }
}

#[test]
fn ac14_message_file_missing_unreadable_or_not_utf8() {
    let s = undecided_msg("m14-msgfile");
    let (rc, _) = s.run_msg_hook(None);
    assert_ne!(rc, 0, "no argument must refuse");
    let (rc, _) = s.run_msg_hook(Some(&s.dir.join("absent.msg")));
    assert_ne!(rc, 0, "missing message file must refuse");
    let (rc, _) = s.run_msg_hook(Some(&s.dir));
    assert_ne!(rc, 0, "a folder in place of the message file must refuse");
    let locked = s.dir.join("locked.msg");
    fs::write(&locked, b"clean words\n").unwrap();
    chmod(&locked, 0o000);
    let (rc, _) = s.run_msg_hook(Some(&locked));
    assert_ne!(rc, 0, "unreadable message file must refuse");
    chmod(&locked, 0o600);
    let bad = s.dir.join("bad.msg");
    let mut b = b"plain words ".to_vec();
    b.extend_from_slice(&[0xff, 0xfe, 0xfd]);
    b.extend_from_slice(b" more words\n");
    fs::write(&bad, &b).unwrap();
    let (rc, _) = s.run_msg_hook(Some(&bad));
    assert_ne!(rc, 0, "message file that is not valid UTF-8 must refuse");
    // the same hook on a clean readable message file is the control: it passes
    let good = s.dir.join("good.msg");
    fs::write(&good, b"clean words\n").unwrap();
    let (rc, t) = s.run_msg_hook(Some(&good));
    assert_eq!(rc, 0, "control: clean message refused: {}", t);
}

// ---- AC 15: no bypass ----

#[test]
fn ac15a_no_environment_switch_for_messages() {
    for (fixture, class) in msg_cases() {
        let mut s = msg_ready(&format!("m15a-{}", fixture), false);
        s.extra_env = switch_env();
        s.stage("f.txt", CLEAN);
        s.assert_msg_refused(&fx_line(fixture), false, class, &fx_marker(fixture));
    }
    // the test-mode value would admit the synthetic marker class if the hook honoured it; it must not
    let mut s = msg_ready("m15a-synthetic", false);
    s.extra_env = switch_env();
    s.stage("f.txt", CLEAN);
    let before = s.state();
    let (rc, _) = s.commit_message(&engine_out(&["fixtures", "extra", "synthetic"]), false);
    assert_ne!(rc, 0, "synthetic marker admitted by the test-mode variable");
    assert_eq!(before, s.state());
}

#[test]
fn ac15b_inline_skip_markers_in_message_change_nothing() {
    let shapes = [
        "privacy-ok", "# nosec", "harness-skip", "allow-privacy", "NOQA: privacy", "[skip hook]", "no-verify",
    ];
    for (fixture, class) in msg_cases() {
        let s = msg_ready(&format!("m15b-{}", fixture), false);
        s.stage("f.txt", CLEAN);
        let mut body = shapes.join("\n");
        body.push('\n');
        body.push_str(&fx_line(fixture));
        s.assert_msg_refused(&body, false, class, &fx_marker(fixture));
    }
}

#[test]
fn ac15c_message_hook_source_has_no_variable_or_testmode_door() {
    let src = fs::read_to_string(MSG_HOOK).unwrap();
    assert!(!src.contains(&j(&["HARNESS", "_"])), "hook mentions an engine variable");
    assert!(!src.contains(&j(&["--test", "mode"])), "hook passes the test-mode argument");
    assert!(!src.contains(&j(&["test", "mode"])), "hook mentions test mode");
    assert!(!src.contains("--no-verify"));
}

#[test]
fn ac15d_message_hook_uses_text_mode_only_and_rewrites_nothing() {
    let src = fs::read_to_string(MSG_HOOK).unwrap();
    for other in ["--staged", "--range", "--tree", "--tag", "git add", "update-index", "checkout", "reset", "git rm", "write-tree"] {
        assert!(!src.contains(other), "hook uses {}", other);
    }
    assert!(src.contains("privacy scan --text"));
    let s = undecided_msg("m15d");
    let planted = s.dir.join("planted.msg");
    let body = format!("change\n\n{}\n", fx_line("home-raw"));
    fs::write(&planted, &body).unwrap();
    let index = s.git(&["ls-files", "-s"]).1;
    let (rc, t) = s.run_msg_hook(Some(&planted));
    assert_eq!(rc, 1, "{}", t);
    assert_eq!(fs::read_to_string(&planted).unwrap(), body, "hook rewrote the message file");
    assert_eq!(index, s.git(&["ls-files", "-s"]).1, "index changed by the hook");
}

// ---- AC 16: comment lines are not exempt (OQ4 answered: the raw message file is judged) ----

#[test]
fn ac16_comment_looking_lines_are_judged_raw() {
    let s = undecided_msg("m16");
    let planted = s.dir.join("c.msg");
    fs::write(&planted, format!("change\n\n# {}\n", fx_line("home-raw"))).unwrap();
    let (rc, t) = s.run_msg_hook(Some(&planted));
    assert_eq!(rc, 1, "comment line planted: {}", t);
    assert!(t.contains("class=home-path"), "{}", t);
    // through git, the comment line is still judged raw (git strips it only after this hook has run)
    let before = s.state();
    let (rc, t) = s.commit_message(&format!("change\n\n# {}", fx_line("home-raw")), true);
    assert_ne!(rc, 0, "comment-looking planted line let through: {}", t);
    assert_eq!(before, s.state());
    // clean twin: an ordinary comment line commits
    s.assert_msg_commits("change\n\n# an ordinary note", true, "ordinary comment line");
}
