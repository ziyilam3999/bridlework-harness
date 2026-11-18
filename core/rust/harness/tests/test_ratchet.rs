//! Tests of the test-list ratchet gate (`gates/test-ratchet.sh`). Each runs the real program, and for the engine arms the real
//! binary, on a scratch repo holding a tiny no-dependency crate at `core/rust`. This file never reads an environment variable; the
//! three gate names are removed on every child before any is set, because the suite itself may run under a gate run that sets them.
#![cfg(unix)]
use harness::gate_result::{GateResult, Status};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const GATE_NAMES: [&str; 3] = ["HARNESS_GATE_BASE", "HARNESS_GATE_HEAD", "HARNESS_GATE_REMOVED_TESTS"];
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("hratchet-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
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

fn real_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

/// A scratch git call: no hooks, no inherited repo-redirecting names, no stdin, fixed identity.
fn git(dir: &Path, args: &[&str]) -> String {
    let mut c = Command::new("git");
    let contact = format!("user.email=t{}scratch", '@');
    c.arg("-C").arg(dir).args(["-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false", "-c", "user.name=t", "-c", contact.as_str()]).args(args);
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    let out = c.stdin(Stdio::null()).output().unwrap();
    assert!(out.status.success(), "scratch git call failed: {:?}", args);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn put(path: &Path, body: &str, exec: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
}

const MANIFEST: &str = "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";
const LIB: &str = "pub fn f() {}\n";
const T_RS: &str = "#[test]\nfn a_lost_test() {}\n#[test]\nfn kept() {}\n";
const ID_LOST: &str = "tests/t.rs::a_lost_test";

struct Repo {
    t: Tmp,
    dir: PathBuf,
    base: String,
}

impl Repo {
    /// `files` are paths under `core/rust`; the real gate program and gate file are copied in; the commit is the base.
    fn new(files: &[(&str, &str)]) -> Repo {
        let t = Tmp::new();
        let dir = t.0.join("r");
        fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q"]);
        for (k, v) in [("maintenance.auto", "false"), ("gc.auto", "0"), ("gc.autoDetach", "false")] {
            git(&dir, &["config", k, v]);
        }
        let root = real_root();
        put(&dir.join("gates/test-ratchet.sh"), &fs::read_to_string(root.join("gates/test-ratchet.sh")).unwrap(), true);
        put(&dir.join("gates/test-ratchet.json"), &fs::read_to_string(root.join("gates/test-ratchet.json")).unwrap(), false);
        put(&dir.join("gates/path-map.json"), "{\"core/rust\":[\"test-ratchet\"]}", false);
        put(&dir.join(".gitignore"), "target/\n", false);
        put(&dir.join("core/rust/Cargo.toml"), MANIFEST, false);
        for (p, b) in files {
            put(&dir.join("core/rust").join(p), b, false);
        }
        let gen = Command::new("cargo")
            .args(["generate-lockfile", "--offline", "--manifest-path"])
            .arg(dir.join("core/rust/Cargo.toml"))
            .env_remove("CARGO_TARGET_DIR")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(gen.status.success(), "lock file: {}", String::from_utf8_lossy(&gen.stderr));
        git(&dir, &["add", "-A"]);
        git(&dir, &["commit", "-q", "-m", "base"]);
        let base = git(&dir, &["rev-parse", "HEAD"]);
        Repo { t, dir, base }
    }

    /// Write (`Some`) or delete (`None`) files under `core/rust`, commit, return the new head.
    fn change(&self, files: &[(&str, Option<&str>)]) -> String {
        for (p, b) in files {
            match b {
                Some(b) => put(&self.dir.join("core/rust").join(p), b, false),
                None => fs::remove_file(self.dir.join("core/rust").join(p)).unwrap(),
            }
        }
        git(&self.dir, &["add", "-A"]);
        git(&self.dir, &["commit", "-q", "-m", "head"]);
        git(&self.dir, &["rev-parse", "HEAD"])
    }

    fn status(&self) -> String {
        git(&self.dir, &["status", "--porcelain"])
    }

    /// The program alone: the three names removed first, then `env` set; no engine.
    fn program(&self, env: &[(&str, &str)], tmpdir: Option<&Path>) -> Output {
        let mut c = Command::new(self.dir.join("gates/test-ratchet.sh"));
        c.current_dir(&self.dir).stdin(Stdio::null());
        for k in GATE_NAMES.iter().chain(["CARGO_TARGET_DIR", "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"].iter()) {
            c.env_remove(k);
        }
        if let Some(d) = tmpdir {
            c.env("TMPDIR", d);
        }
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    /// The program with a full, valid env for `head`.
    fn run(&self, head: &str, removed: &str) -> Output {
        self.program(&[("HARNESS_GATE_BASE", &self.base), ("HARNESS_GATE_HEAD", head), ("HARNESS_GATE_REMOVED_TESTS", removed)], None)
    }

    /// `harness ticket gate` on this repo at `base`, the ticket naming `removed` (each `<id> -- <reason>`).
    fn gate(&self, removed: &[&str]) -> (Output, GateResult) {
        let mut pairs: Vec<(&str, String)> = vec![
            ("id", "\"T-1\"".to_string()),
            ("repo_id", "\"r\"".to_string()),
            ("title", "\"A title\"".to_string()),
            ("group", "\"grp\"".to_string()),
            ("size", "\"M\"".to_string()),
            ("objective", "\"An objective\"".to_string()),
            ("acceptance_criteria", "[\"one\"]".to_string()),
            ("dependencies", "[]".to_string()),
            ("risk", "\"normal\"".to_string()),
            ("owner_role", "\"execution_verification\"".to_string()),
            ("evidence_links", "[]".to_string()),
        ];
        if !removed.is_empty() {
            let els: Vec<String> = removed.iter().map(|e| format!("\"{e}\"")).collect();
            pairs.push(("removed_tests", format!("[{}]", els.join(","))));
        }
        let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
        let tf = self.t.0.join("ticket.json");
        fs::write(&tf, format!("{{{}}}", body.join(","))).unwrap();
        let (res, caps, cwd) = (self.t.0.join("res"), self.t.0.join("caps"), self.t.0.join("cwd"));
        for d in [&res, &caps, &cwd] {
            fs::create_dir_all(d).unwrap();
        }
        let mut c = Command::new(BIN);
        c.arg("ticket").arg("gate").arg(&tf).arg("--repo-map").arg(format!("r={}", self.dir.display()));
        c.args(["--base", &self.base, "--result-dir"]).arg(&res).arg("--caps-dir").arg(&caps);
        c.current_dir(&cwd).stdin(Stdio::null());
        for k in GATE_NAMES.iter().chain(["CARGO_TARGET_DIR", "GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"].iter()) {
            c.env_remove(k);
        }
        let o = c.output().unwrap();
        let r = fs::read_to_string(res.join("T-1.json")).unwrap_or_else(|_| panic!("no result file; stderr: {}", String::from_utf8_lossy(&o.stderr)));
        (o, GateResult::from_json(&r).unwrap())
    }
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn failed(r: &GateResult, needle: &str) {
    assert_eq!(r.status, Status::Fail, "message: {}", r.message);
    assert!(r.message.contains(needle), "message: {}", r.message);
}

fn passed(r: &GateResult) {
    assert_eq!(r.status, Status::Pass, "message: {}", r.message);
}

const BASE_FILES: [(&str, &str); 2] = [("src/lib.rs", LIB), ("tests/t.rs", T_RS)];

#[test]
fn a_lost_test_fails_the_gate() {
    let r = Repo::new(&BASE_FILES);
    r.change(&[("tests/t.rs", Some("#[test]\nfn kept() {}\n"))]);
    let (_, res) = r.gate(&[]);
    failed(&res, ID_LOST);
    assert!(!res.message.contains("tests/t.rs::kept"), "message: {}", res.message);
}

#[test]
fn an_added_test_passes() {
    let r = Repo::new(&BASE_FILES);
    r.change(&[("tests/t.rs", Some("#[test]\nfn a_lost_test() {}\n#[test]\nfn kept() {}\n#[test]\nfn added() {}\n"))]);
    passed(&r.gate(&[]).1);
}

#[test]
fn a_named_removal_passes() {
    let r = Repo::new(&BASE_FILES);
    r.change(&[("tests/t.rs", Some("#[test]\nfn kept() {}\n"))]);
    passed(&r.gate(&["tests/t.rs::a_lost_test -- merged into kept"]).1);
}

#[test]
fn a_named_test_still_present_fails() {
    let r = Repo::new(&BASE_FILES);
    r.change(&[("tests/t.rs", Some("#[test]\nfn a_lost_test() {}\n#[test]\nfn kept() {}\n#[test]\nfn added() {}\n"))]);
    let (_, res) = r.gate(&["tests/t.rs::kept -- not really gone"]);
    failed(&res, "named-not-removed");
    assert!(res.message.contains("tests/t.rs::kept"), "message: {}", res.message);
}

#[test]
fn a_renamed_test_is_a_loss_unless_named() {
    let r = Repo::new(&BASE_FILES);
    r.change(&[("tests/t.rs", Some("#[test]\nfn a_new_name() {}\n#[test]\nfn kept() {}\n"))]);
    failed(&r.gate(&[]).1, ID_LOST);
    passed(&r.gate(&["tests/t.rs::a_lost_test -- renamed"]).1);
}

#[test]
fn a_deleted_test_file_loses_its_tests() {
    let r = Repo::new(&BASE_FILES);
    r.change(&[("tests/t.rs", None)]);
    let (_, res) = r.gate(&[]);
    failed(&res, ID_LOST);
    assert!(res.message.contains("tests/t.rs::kept"), "message: {}", res.message);
}

#[test]
fn one_name_in_two_targets_is_two_ids() {
    let r = Repo::new(&[("src/lib.rs", LIB), ("tests/a.rs", "#[test]\nfn same() {}\n"), ("tests/b.rs", "#[test]\nfn same() {}\n")]);
    r.change(&[("tests/b.rs", Some("#[test]\nfn other() {}\n"))]);
    let (_, res) = r.gate(&[]);
    failed(&res, "tests/b.rs::same");
    assert!(!res.message.contains("tests/a.rs::same"), "message: {}", res.message);
}

#[test]
fn a_lost_unit_test_is_named_by_its_source_path() {
    let unit = "pub mod caps {\n    #[cfg(test)]\n    mod tests {\n        #[test]\n        fn x() {}\n    }\n}\n";
    let r = Repo::new(&[("src/lib.rs", unit), ("tests/t.rs", T_RS)]);
    r.change(&[("src/lib.rs", Some("pub mod caps {}\n"))]);
    failed(&r.gate(&[]).1, "src/lib.rs::caps::tests::x");
    passed(&r.gate(&["src/lib.rs::caps::tests::x -- dropped"]).1);
}

#[test]
fn a_base_that_does_not_build_fails() {
    let r = Repo::new(&[("src/lib.rs", "pub fn (\n"), ("tests/t.rs", T_RS)]);
    r.change(&[("src/lib.rs", Some(LIB))]);
    failed(&r.gate(&[]).1, "cannot be built at the base");
}

#[test]
fn a_head_that_does_not_build_fails() {
    let r = Repo::new(&BASE_FILES);
    r.change(&[("src/lib.rs", Some("pub fn (\n"))]);
    failed(&r.gate(&[]).1, "cannot be built at the head");
}

#[test]
fn a_base_with_no_listed_test_fails() {
    let r = Repo::new(&[("src/lib.rs", LIB)]);
    r.change(&[("tests/t.rs", Some(T_RS))]);
    failed(&r.gate(&[]).1, "holds no test id");
}

#[test]
fn no_base_env_fails() {
    let r = Repo::new(&BASE_FILES);
    let o = r.program(&[], None);
    assert!(!o.status.success());
    assert!(stderr(&o).contains("HARNESS_GATE_BASE"), "stderr: {}", stderr(&o));
}

#[test]
fn a_missing_gate_env_value_fails_the_gate() {
    let r = Repo::new(&BASE_FILES);
    let b = r.base.clone();
    let up = b.to_uppercase();
    // (env set, name that the one stderr line must hold)
    let arms: Vec<(Vec<(&str, &str)>, &str)> = vec![
        (vec![("HARNESS_GATE_HEAD", &b), ("HARNESS_GATE_REMOVED_TESTS", "")], "HARNESS_GATE_BASE"),
        (vec![("HARNESS_GATE_BASE", &b), ("HARNESS_GATE_REMOVED_TESTS", "")], "HARNESS_GATE_HEAD"),
        (vec![("HARNESS_GATE_BASE", &b), ("HARNESS_GATE_HEAD", &b)], "HARNESS_GATE_REMOVED_TESTS"),
        (vec![("HARNESS_GATE_BASE", &up), ("HARNESS_GATE_HEAD", &b), ("HARNESS_GATE_REMOVED_TESTS", "")], "HARNESS_GATE_BASE"),
        (vec![("HARNESS_GATE_BASE", &b), ("HARNESS_GATE_HEAD", ""), ("HARNESS_GATE_REMOVED_TESTS", "")], "HARNESS_GATE_HEAD"),
    ];
    for (env, name) in arms {
        let o = r.program(&env, None);
        assert!(!o.status.success(), "env {env:?} must fail");
        let e = stderr(&o);
        assert_eq!(e.lines().count(), 1, "stderr: {e}");
        assert!(e.contains(name), "stderr: {e}");
    }
    // twin: an empty removed list is allowed.
    assert!(r.run(&b, "").status.success());
}

#[test]
fn a_bad_base_env_fails() {
    let r = Repo::new(&BASE_FILES);
    for bad in [r.base[..39].to_string(), r.base.to_uppercase(), "0".repeat(40)] {
        let o = r.program(&[("HARNESS_GATE_BASE", &bad), ("HARNESS_GATE_HEAD", &r.base), ("HARNESS_GATE_REMOVED_TESTS", "")], None);
        assert!(!o.status.success(), "base {bad} must fail");
    }
    assert!(r.run(&r.base, "").status.success());
}

#[test]
fn a_head_env_that_is_not_the_folder_head_fails() {
    let r = Repo::new(&BASE_FILES);
    let head = r.change(&[("tests/t.rs", Some(T_RS.replace("kept", "kept2").as_str()))]);
    let o = r.run(&r.base.clone(), "");
    assert!(!o.status.success());
    assert!(stderr(&o).contains("HARNESS_GATE_HEAD"), "stderr: {}", stderr(&o));
    assert!(r.run(&head, "tests/t.rs::kept").status.success());
}

#[test]
fn the_program_leaves_the_tree_clean() {
    let r = Repo::new(&BASE_FILES);
    assert!(r.run(&r.base.clone(), "").status.success());
    assert_eq!(r.status(), "");
    let head = r.change(&[("tests/t.rs", Some("#[test]\nfn kept() {}\n"))]);
    assert!(!r.run(&head, "").status.success());
    assert_eq!(r.status(), "");
}

#[test]
fn the_program_leaves_no_temp_folder() {
    let r = Repo::new(&BASE_FILES);
    let tmp = r.t.0.join("tmpdir");
    fs::create_dir_all(&tmp).unwrap();
    let env = |head: &str, base: &str| {
        r.program(&[("HARNESS_GATE_BASE", base), ("HARNESS_GATE_HEAD", head), ("HARNESS_GATE_REMOVED_TESTS", "")], Some(&tmp))
    };
    let base = r.base.clone();
    assert!(env(&base, &base).status.success());
    assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0, "a pass left a folder");
    let head = r.change(&[("tests/t.rs", Some("#[test]\nfn kept() {}\n"))]);
    assert!(!env(&head, &base).status.success());
    assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0, "a fail left a folder");
}

#[test]
fn the_real_program_is_executable() {
    let mode = fs::metadata(real_root().join("gates/test-ratchet.sh")).unwrap().permissions().mode();
    assert!(mode & 0o111 != 0, "the gate program must be executable");
}

#[test]
fn the_ratchet_gate_file_is_head_only() {
    let gates = harness::gates::Gates::load(&real_root()).unwrap();
    let gate = gates.get("test-ratchet").unwrap();
    assert!(!gate.both_ends, "test-ratchet must stay head only: both-ends judging passes caller env values through");
    assert_eq!(gate.program, "gates/test-ratchet.sh");
}
