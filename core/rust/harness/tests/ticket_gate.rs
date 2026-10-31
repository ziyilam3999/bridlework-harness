//! Process-level smoke for `harness ticket gate` (P1A-07). Every check runs the real binary as a child process against scratch repos
//! under the system temp folder, from the folder of a DIFFERENT repo. Variables are set only on the child; this file never reads one.
//! Every refusal has a planted failing twin: the same run with one thing changed that a wrong build gets wrong.
//!
//! Fixture repo `r`: `gates/t.json` (both ends, `ci/t.sh`: green only when `src/value.txt` holds `ok`), `gates/p.json` (head only, `ci/p.sh`:
//! green only when `docs/x.txt` is non-empty), `gates/path-map.json` (`src` -> t, `docs` -> p). Every script appends one line `<name> <pwd>`
//! to the file named by `GATE_MARKER`, a file OUTSIDE `r`, so "nothing ran" is "the marker file is absent".
#![cfg(unix)]
use harness::gate_result::{GateResult, Status};
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("hgate-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        // A read-only result folder must be writable again before the tree can go.
        if let Ok(rd) = fs::read_dir(&self.0) {
            for e in rd.flatten() {
                let _ = fs::set_permissions(e.path(), fs::Permissions::from_mode(0o755));
            }
        }
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
    assert!(out.status.success(), "scratch git call failed: {:?}", args);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn gate_json(id: &str, program: &str, both_ends: bool) -> String {
    format!("{{\"id\":\"{id}\",\"program\":\"{program}\",\"args\":[],\"timeout_ms\":20000,\"max_output_bytes\":4096,\"both_ends\":{both_ends}}}")
}

fn script(name: &str, test: &str) -> String {
    format!("#!/bin/sh\necho \"{name} $(pwd)\" >> \"$GATE_MARKER\"\n{test}\n")
}

/// How the scratch repo is built. `files` are written last, so a row can override a default script or add a gate.
struct Cfg {
    map: String,
    t_both: bool,
    p_both: bool,
    value0: String,
    files: Vec<(String, String, bool)>,
    links: Vec<(String, PathBuf)>,
}

impl Cfg {
    fn new() -> Cfg {
        Cfg { map: r#"{"src":["t"],"docs":["p"]}"#.to_string(), t_both: true, p_both: false, value0: "bad".to_string(), files: vec![], links: vec![] }
    }

    fn map(mut self, m: &str) -> Cfg {
        self.map = m.to_string();
        self
    }

    fn file(mut self, path: &str, body: &str, exec: bool) -> Cfg {
        self.files.push((path.to_string(), body.to_string(), exec));
        self
    }

    /// A gate file plus its program, head-only unless `both`.
    fn gate(self, id: &str, test: &str, both: bool) -> Cfg {
        let (g, p) = (format!("gates/{id}.json"), format!("ci/{id}.sh"));
        let body = gate_json(id, &p, both);
        self.file(&g, &body, false).file(&p, &script(id, test), true)
    }
}

struct Fx {
    t: Tmp,
    repo: PathBuf,
    other: PathBuf,
    marker: PathBuf,
    res: PathBuf,
    caps: PathBuf,
    c0: String,
}

fn write_file(path: &Path, body: &str, exec: bool) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(if exec { 0o755 } else { 0o644 })).unwrap();
}

impl Fx {
    fn new() -> Fx {
        Fx::with(Cfg::new())
    }

    fn with(cfg: Cfg) -> Fx {
        let t = Tmp::new();
        let (repo, other, marker_dir, res, caps) = (t.0.join("r"), t.0.join("o"), t.0.join("marker"), t.0.join("res"), t.0.join("caps"));
        for d in [&repo, &other, &marker_dir, &res, &caps] {
            fs::create_dir_all(d).unwrap();
        }
        let mut files: Vec<(String, String, bool)> = vec![
            ("gates/t.json".to_string(), gate_json("t", "ci/t.sh", cfg.t_both), false),
            ("gates/p.json".to_string(), gate_json("p", "ci/p.sh", cfg.p_both), false),
            ("gates/path-map.json".to_string(), cfg.map.clone(), false),
            ("ci/t.sh".to_string(), script("t", "[ \"$(cat src/value.txt)\" = ok ]"), true),
            ("ci/p.sh".to_string(), script("p", "[ -s docs/x.txt ]"), true),
            ("src/value.txt".to_string(), cfg.value0.clone(), false),
            ("docs/x.txt".to_string(), "doc0".to_string(), false),
        ];
        files.extend(cfg.files.iter().cloned());
        git(&repo, &["init", "-q"]);
        for (p, b, x) in &files {
            write_file(&repo.join(p), b, *x);
        }
        for (p, target) in &cfg.links {
            fs::create_dir_all(repo.join(p).parent().unwrap()).unwrap();
            let _ = fs::remove_file(repo.join(p));
            symlink(target, repo.join(p)).unwrap();
        }
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "c0"]);
        git(&other, &["init", "-q"]);
        write_file(&other.join("o.txt"), "other repo", false);
        git(&other, &["add", "-A"]);
        git(&other, &["commit", "-q", "-m", "other"]);
        let c0 = git(&repo, &["rev-parse", "HEAD"]);
        Fx { repo: repo.canonicalize().unwrap(), other: other.canonicalize().unwrap(), marker: marker_dir.join("m.log"), res: res.canonicalize().unwrap(), caps: caps.canonicalize().unwrap(), c0, t }
    }

    fn commit(&self, files: &[(&str, &str)]) -> String {
        for (p, b) in files {
            write_file(&self.repo.join(p), b, false);
        }
        git(&self.repo, &["add", "-A"]);
        git(&self.repo, &["commit", "-q", "-m", "next"]);
        self.head()
    }

    fn head(&self) -> String {
        git(&self.repo, &["rev-parse", "HEAD"])
    }

    fn markers(&self) -> Vec<String> {
        fs::read_to_string(&self.marker).map(|s| s.lines().map(|l| l.to_string()).collect()).unwrap_or_default()
    }

    fn ran(&self, name: &str) -> usize {
        self.markers().iter().filter(|l| l.starts_with(&format!("{name} "))).count()
    }

    fn result_file(&self, id: &str) -> PathBuf {
        self.res.join(format!("{id}.json"))
    }

    fn result(&self, id: &str) -> GateResult {
        GateResult::from_json(&fs::read_to_string(self.result_file(id)).expect("a result file was written")).expect("the file parses")
    }
}

fn ticket_json(repo_id: &str, id: &str, skip_repo_id: bool) -> String {
    let mut pairs: Vec<(&str, String)> = vec![
        ("id", format!("\"{id}\"")),
        ("repo_id", format!("\"{repo_id}\"")),
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
    if skip_repo_id {
        pairs.retain(|(k, _)| *k != "repo_id");
    }
    let body: Vec<String> = pairs.iter().map(|(k, v)| format!("\"{k}\":{v}")).collect();
    format!("{{{}}}", body.join(","))
}

fn ticket_file(fx: &Fx, body: &str) -> PathBuf {
    let d = fx.t.0.join("tickets");
    fs::create_dir_all(&d).unwrap();
    let f = d.join(format!("t{}.json", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&f, body).unwrap();
    f
}

/// Run `harness ticket gate` with fully explicit arguments, from `cwd`. `env` is set on the child only.
fn run_args(cwd: &Path, args: &[String], env: &[(&str, &Path)]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("ticket").arg("gate").args(args).current_dir(cwd).stdin(Stdio::null());
    for k in ["GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_COMMON_DIR"] {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn args_for(tf: &Path, repo: &Path, base: &str, res: &str, caps: &Path) -> Vec<String> {
    vec![tf.to_str().unwrap().into(), "--repo-map".into(), format!("r={}", repo.to_str().unwrap()), "--base".into(), base.into(), "--result-dir".into(), res.into(), "--caps-dir".into(), caps.to_str().unwrap().into()]
}

fn gate_as(fx: &Fx, id: &str, base: &str, res: &Path, cwd: &Path, extra_env: &[(&str, &Path)]) -> Output {
    let tf = ticket_file(fx, &ticket_json("r", id, false));
    let mut env: Vec<(&str, &Path)> = vec![("GATE_MARKER", fx.marker.as_path())];
    env.extend_from_slice(extra_env);
    run_args(cwd, &args_for(&tf, &fx.repo, base, res.to_str().unwrap(), &fx.caps), &env)
}

/// The default run: ticket `T-1`, from the unrelated repo `o`, into the fixture's result folder.
fn gate(fx: &Fx, base: &str) -> Output {
    gate_as(fx, "T-1", base, &fx.res, &fx.other, &[])
}

/// A canonical `fix_rounds=0 in_flight=false` record next to a planted result file (a result without its record is refused as a deleted record).
fn plant_record(fx: &Fx, id: &str) {
    fs::write(fx.caps.join(format!("{id}.rounds")), format!("{{\"v\":1,\"ticket\":\"{id}\",\"repo_id\":\"r\",\"fix_rounds\":0,\"in_flight\":false}}\n")).unwrap();
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

fn leaks(o: &Output, needle: &str) -> bool {
    out(o).contains(needle) || err(o).contains(needle)
}

/// An error: exit 2, stdout empty, never `done=allowed`.
fn errored(o: &Output) {
    assert_eq!(code(o), 2, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty: {}", out(o));
}

fn no_result_file(fx: &Fx) {
    let names: Vec<String> = fs::read_dir(&fx.res).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert!(names.is_empty(), "nothing may be written into the result folder: {names:?}");
}

fn listing(d: &Path) -> Vec<String> {
    fn walk(d: &Path, base: &Path, out: &mut Vec<String>) {
        for e in fs::read_dir(d).unwrap() {
            let p = e.unwrap().path();
            out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            if p.is_dir() && !p.is_symlink() {
                walk(&p, base, out);
            }
        }
    }
    let mut v = Vec::new();
    walk(d, d, &mut v);
    v.sort();
    v
}

// The leak helper really does catch a message that echoes a folder (twin of every "not in the output" assertion below).
#[test]
fn the_leak_helper_catches_an_echoed_folder() {
    let fake = Command::new("sh").args(["-c", "echo /synthetic-folder-0xA1 >&2"]).output().unwrap();
    assert!(leaks(&fake, "/synthetic-folder-0xA1"));
    assert!(!leaks(&fake, "/another-folder"));
}

// AC 1, 2, 21: a FAIL at C1 refuses Done; the fix commit C2 flips the SAME ticket to PASS naming C2, replacing the earlier file.
#[test]
fn a_failing_head_refuses_done_and_the_fix_commit_flips_it_to_pass() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("src/value.txt", "bad2")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("ticket=T-1 head={c1} result=fail done=refused\n"));
    assert!(!leaks(&o, "done=allowed"));
    let r = fx.result("T-1");
    assert_eq!((r.status, r.failure_class.as_deref(), r.head_sha.as_str()), (Status::Fail, Some("check-failed"), c1.as_str()));
    let fail_text = fs::read_to_string(fx.result_file("T-1")).unwrap();

    let c2 = fx.commit(&[("src/value.txt", "ok")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("ticket=T-1 head={c2} result=pass done=allowed\n"));
    let r = fx.result("T-1");
    assert_eq!((r.status.clone(), r.head_sha.as_str()), (Status::Pass, c2.as_str()), "the file names C2, not C1 and not C0");
    let text = fs::read_to_string(fx.result_file("T-1")).unwrap();
    assert_ne!(text, fail_text, "the earlier FAIL file was replaced");
    // AC 21: the file is exactly the canonical form, and the stdout head equals the file's head.
    assert_eq!(r.to_json().unwrap(), text, "the file is the canonical form");
    assert!(out(&o).contains(&format!("head={}", r.head_sha)));
}

// AC 3: a stale PASS never survives a newer FAIL at the same path.
#[test]
fn a_later_fail_replaces_an_earlier_pass() {
    let fx = Fx::new();
    let c2 = fx.commit(&[("src/value.txt", "ok")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: C2 passes");
    assert_eq!(fx.result("T-1").head_sha, c2);
    let c3 = fx.commit(&[("src/value.txt", "bad3")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    let r = fx.result("T-1");
    assert_eq!((r.status, r.head_sha.as_str()), (Status::Fail, c3.as_str()));
}

// AC 4: an unmapped path is FAIL and NO check ran. Twin: the same diff with the path mapped.
#[test]
fn an_unmapped_path_fails_before_any_check_runs() {
    let fx = Fx::new();
    fx.commit(&[("other/x.txt", "new")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    let r = fx.result("T-1");
    assert_eq!((r.failure_class.as_deref(), r.file.as_deref()), (Some("unmapped-path"), Some("other/x.txt")));
    assert!(err(&o).contains("class=unmapped-path"), "{}", err(&o));
    assert!(fx.markers().is_empty(), "nothing ran: {:?}", fx.markers());
    let fx = Fx::with(Cfg::new().map(r#"{"src":["t"],"docs":["p"],"other":["p"]}"#));
    fx.commit(&[("other/x.txt", "new")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "twin: mapped, p runs and passes; stderr: {}", err(&o));
    assert_eq!(fx.ran("p"), 1);
}

// AC 5: a diff that touches no path is FAIL no-checks. Twin: a one-file mapped diff.
#[test]
fn a_diff_that_touches_no_path_is_fail_no_checks() {
    let fx = Fx::new();
    git(&fx.repo, &["commit", "-q", "--allow-empty", "-m", "empty"]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(fx.result("T-1").failure_class.as_deref(), Some("no-checks"));
    assert!(fx.markers().is_empty());
    let fx = Fx::new();
    fx.commit(&[("docs/x.txt", "doc1")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: a mapped one-file diff passes");
}

// AC 6: a changed path mapped to an undeclared gate id is an error and nothing ran. An undeclared id on an untouched path is not looked at.
#[test]
fn a_changed_path_mapped_to_an_undeclared_gate_is_an_error() {
    let map = r#"{"src":["t"],"docs":["p"],"other":["ghost"]}"#;
    let fx = Fx::with(Cfg::new().map(map));
    fx.commit(&[("other/x.txt", "new")]);
    errored(&gate(&fx, &fx.c0));
    assert!(fx.markers().is_empty(), "nothing ran");
    no_result_file(&fx);
    let fx = Fx::with(Cfg::new().map(map));
    fx.commit(&[("docs/x.txt", "doc1")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: the ghost path is untouched, so the run is normal");
}

// AC 7: both_ends true is judged at both ends, both_ends false at head only.
#[test]
fn the_both_ends_flag_picks_the_judge() {
    // A both-ends gate whose check already passes at base catches nothing.
    let fx = Fx::with(Cfg { value0: "ok".to_string(), ..Cfg::new() });
    fx.commit(&[("src/value.txt", "ok\n\n")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(fx.result("T-1").failure_class.as_deref(), Some("passes-at-base"));
    // Twin: the same gate head-only is just required-green at head.
    let fx = Fx::with(Cfg { value0: "ok".to_string(), t_both: false, ..Cfg::new() });
    fx.commit(&[("src/value.txt", "ok\n\n")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0);
    assert_eq!(fx.ran("t"), 1, "head only: one run");
    // A head-only gate on a docs-only diff runs once, at head, in the repo folder, and its pass is PASS.
    let fx = Fx::new();
    fx.commit(&[("docs/x.txt", "doc1")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!((fx.ran("p"), fx.ran("t")), (1, 0));
    assert_eq!(fx.markers()[0], format!("p {}", fx.repo.display()), "head-only runs in the repo folder");
    // Twin: the same gate flagged both-ends passes at base too, so it catches nothing.
    let fx = Fx::with(Cfg { p_both: true, ..Cfg::new() });
    fx.commit(&[("docs/x.txt", "doc1")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 1);
    assert_eq!(fx.result("T-1").failure_class.as_deref(), Some("passes-at-base"));
}

// AC 8: fail fast in sorted id order, and the FAILING gate is the one named.
#[test]
fn the_run_stops_at_the_first_failure_in_sorted_order_and_names_that_gate() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2"), ("docs/x.txt", "doc1")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1, "p passes, t fails: {}", err(&o));
    assert!(err(&o).contains("gate=t"), "{}", err(&o));
    assert!(fx.result("T-1").message.starts_with("t: "), "{}", fx.result("T-1").message);
    assert_eq!((fx.ran("p"), fx.ran("t")), (1, 2), "p once at head; t at both ends");
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "bad2"), ("docs/x.txt", "")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("gate=p"), "{}", err(&o));
    assert!(fx.result("T-1").message.starts_with("p: "));
    assert_eq!((fx.ran("p"), fx.ran("t")), (1, 0), "p failed first, so t never ran");
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok"), ("docs/x.txt", "doc1")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "twin: both passing is PASS; stderr: {}", err(&o));
    assert!(fx.result("T-1").message.contains("p, t"));
}

// AC 9: a dirty folder (modified, then untracked) is an error and nothing ran. Twin: the clean folder gets a normal verdict.
#[test]
fn a_dirty_repo_folder_is_an_error_and_nothing_runs() {
    for untracked in [false, true] {
        let fx = Fx::new();
        fx.commit(&[("src/value.txt", "ok")]);
        if untracked {
            write_file(&fx.repo.join("stray.txt"), "u", false);
        } else {
            write_file(&fx.repo.join("docs/x.txt"), "edited", false);
        }
        errored(&gate(&fx, &fx.c0));
        assert!(fx.markers().is_empty(), "nothing ran");
        no_result_file(&fx);
    }
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: clean");
}

// AC 10: a head-only check that moves HEAD makes the run an error and no PASS is written. Twin: a check that leaves HEAD alone.
#[test]
fn a_check_that_moves_head_is_an_error() {
    let mover = format!("git -c core.hooksPath=/dev/null -c user.name=t -c user.email=t{}scratch commit -q --allow-empty -m moved", '@');
    let map = r#"{"src":["t"],"docs":["m"]}"#;
    let fx = Fx::with(Cfg::new().map(map).gate("m", &mover, false));
    fx.commit(&[("docs/x.txt", "doc1")]);
    let moved_from = fx.head();
    errored(&gate(&fx, &fx.c0));
    assert_eq!(fx.ran("m"), 1, "the check ran");
    assert_ne!(fx.head(), moved_from, "and it really moved HEAD");
    no_result_file(&fx);
    let fx = Fx::with(Cfg::new().map(map).gate("m", "true", false));
    fx.commit(&[("docs/x.txt", "doc1")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: HEAD stays put");
}

// Step 7 (clean tree at the end): a both-ends check run in scratch trees that dirties the REAL folder is an error. Twin: it does not.
#[test]
fn a_check_that_dirties_the_repo_folder_is_an_error() {
    let dirtier = "[ -z \"$GATE_REPO\" ] || echo x > \"$GATE_REPO/stray.txt\"\n[ \"$(cat src/value.txt)\" = ok ]";
    let fx = Fx::with(Cfg::new().gate("t", dirtier, true));
    fx.commit(&[("src/value.txt", "ok")]);
    let repo = fx.repo.clone();
    errored(&gate_as(&fx, "T-1", &fx.c0, &fx.res, &fx.other, &[("GATE_REPO", repo.as_path())]));
    assert!(fx.repo.join("stray.txt").exists(), "the check really dirtied the folder");
    no_result_file(&fx);
    let fx = Fx::with(Cfg::new().gate("t", dirtier, true));
    fx.commit(&[("src/value.txt", "ok")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: no stray file");
}

// AC 11: a base that is not a full lowercase commit sha is refused. Twin: the valid full sha.
#[test]
fn a_base_that_is_not_a_full_lowercase_commit_sha_is_refused() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("src/value.txt", "ok")]);
    let upper = fx.c0.to_uppercase();
    let absent = "b".repeat(40);
    for bad in ["master", "main", "HEAD", &fx.c0[..7], upper.as_str(), absent.as_str()] {
        let o = gate(&fx, bad);
        errored(&o);
        assert!(fx.markers().is_empty(), "{bad}: nothing ran");
        no_result_file(&fx);
    }
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "twin: the full sha; stderr: {}", err(&o));
    assert!(out(&o).contains(&c1));
}

// AC 12: the result directory is refused unless it is an absolute, real, outside-the-repo directory. Twin: such a directory.
#[test]
fn a_result_directory_that_is_not_a_real_outside_directory_is_refused() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::create_dir_all(fx.other.join("res-rel")).unwrap();
    fs::create_dir_all(fx.repo.join("out")).unwrap();
    fs::create_dir_all(fx.repo.join("out2")).unwrap();
    let link = fx.t.0.join("link-res");
    symlink(&fx.res, &link).unwrap();
    let into_repo = fx.t.0.join("lnk");
    symlink(&fx.repo, &into_repo).unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("relative", "res-rel".to_string()),
        ("dot-dot", format!("{}/../res", fx.res.display())),
        ("symlink to a real directory", link.display().to_string()),
        ("inside the repo", fx.repo.join("out").display().to_string()),
        ("parent component is a link into the repo", into_repo.join("out2").display().to_string()),
        ("missing", fx.t.0.join("nope").display().to_string()),
    ];
    for (label, dir) in &cases {
        let tf = ticket_file(&fx, &ticket_json("r", "T-1", false));
        let o = run_args(&fx.other, &args_for(&tf, &fx.repo, &fx.c0, dir, &fx.caps), &[("GATE_MARKER", fx.marker.as_path())]);
        assert_eq!(code(&o), 2, "{label}: stderr {}", err(&o));
        assert!(out(&o).is_empty(), "{label}: stdout");
        assert!(!leaks(&o, dir), "{label}: the result folder must not be echoed: {}", err(&o));
        assert!(fx.markers().is_empty(), "{label}: nothing ran");
        no_result_file(&fx);
        assert!(listing(&fx.repo.join("out")).is_empty() && listing(&fx.repo.join("out2")).is_empty(), "{label}: nothing written into the repo");
    }
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "twin: an absolute real directory outside the repo; stderr: {}", err(&o));
}

// Step 2: the mapped folder is resolved ONCE, so a symlinked folder entry cannot hide that the result folder lies inside the repo.
#[test]
fn a_symlinked_repo_folder_is_resolved_once() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::create_dir_all(fx.repo.join("out")).unwrap();
    let linked = fx.t.0.join("linkr");
    symlink(&fx.repo, &linked).unwrap();
    let tf = ticket_file(&fx, &ticket_json("r", "T-1", false));
    let inside = fx.repo.join("out").display().to_string();
    let o = run_args(&fx.other, &args_for(&tf, &linked, &fx.c0, &inside, &fx.caps), &[("GATE_MARKER", fx.marker.as_path())]);
    errored(&o);
    assert!(listing(&fx.repo.join("out")).is_empty());
    let o = run_args(&fx.other, &args_for(&tf, &linked, &fx.c0, fx.res.to_str().unwrap(), &fx.caps), &[("GATE_MARKER", fx.marker.as_path())]);
    assert_eq!(code(&o), 0, "twin: the same linked entry with an outside result folder; stderr: {}", err(&o));
    assert!(!leaks(&o, linked.to_str().unwrap()), "the mapped folder is not echoed");
}

// AC 13: a ticket id usable only as a path is refused and creates nothing. Twin: a good id with dots.
#[test]
fn a_ticket_id_usable_only_as_a_path_is_refused() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    let before = listing(&fx.t.0);
    for bad in ["../escape", "a/b", ".hidden", &"a".repeat(65)] {
        let o = gate_as(&fx, bad, &fx.c0, &fx.res, &fx.other, &[]);
        errored(&o);
        assert!(!leaks(&o, fx.res.to_str().unwrap()), "{bad}: the result folder is not echoed");
        let after: Vec<String> = listing(&fx.t.0).into_iter().filter(|p| !p.starts_with("tickets")).collect();
        let before_f: Vec<String> = before.iter().filter(|p| !p.starts_with("tickets")).cloned().collect();
        assert_eq!(after, before_f, "{bad}: nothing created anywhere under the temp root");
    }
    let o = gate_as(&fx, "EX-1.v2", &fx.c0, &fx.res, &fx.other, &[]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert!(fx.result_file("EX-1.v2").is_file(), "twin: EX-1.v2.json is written");
}

// AC 14: an existing result entry that is a link is refused and its target is never written. Twin: a regular file is replaced.
#[test]
fn a_result_target_that_is_a_symlink_is_refused() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    let outside = fx.t.0.join("outside.txt");
    fs::write(&outside, "keep me").unwrap();
    symlink(&outside, fx.result_file("T-1")).unwrap();
    let o = gate(&fx, &fx.c0);
    errored(&o);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "keep me", "the link target is untouched");
    assert!(!leaks(&o, fx.res.to_str().unwrap()));
    fs::remove_file(fx.result_file("T-1")).unwrap();
    fs::write(fx.result_file("T-1"), "old").unwrap();
    plant_record(&fx, "T-1");
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: a regular file is replaced");
    assert_eq!(fx.result("T-1").status, Status::Pass);
}

// Step 8: the temp file is created with create-new, so a planted entry at its name (here a link out) is never written through.
#[test]
fn a_planted_temp_file_name_is_never_written_through() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    let outside = fx.t.0.join("outside.txt");
    fs::write(&outside, "keep me").unwrap();
    symlink(&outside, fx.res.join(".T-1.json.tmp")).unwrap();
    let o = gate(&fx, &fx.c0);
    errored(&o);
    assert_eq!(fs::read_to_string(&outside).unwrap(), "keep me");
    assert!(!fx.result_file("T-1").exists(), "no result was put in place");
    fs::remove_file(fx.res.join(".T-1.json.tmp")).unwrap();
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: without the planted name it works");
}

// AC 15: an unwritable result directory is an error even though the checks passed. Twin: a writable directory.
#[test]
fn an_unwritable_result_directory_is_an_error_never_done() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::set_permissions(&fx.res, fs::Permissions::from_mode(0o555)).unwrap();
    if fs::write(fx.res.join("probe"), "x").is_ok() {
        println!("SKIP: this process can write into a mode-0555 directory, so the read-only arm cannot be exercised here");
        fs::set_permissions(&fx.res, fs::Permissions::from_mode(0o755)).unwrap();
        return;
    }
    let o = gate(&fx, &fx.c0);
    fs::set_permissions(&fx.res, fs::Permissions::from_mode(0o755)).unwrap();
    errored(&o);
    assert!(!leaks(&o, "done=allowed") && !leaks(&o, fx.res.to_str().unwrap()));
    assert_eq!(fx.ran("t"), 2, "the checks did run (base and head); only the write failed");
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: writable");
}

// AC 16: a check's own output never reaches stdout. Twin: a quiet check gives the same line.
#[test]
fn a_check_cannot_forge_the_stdout_line() {
    let loud = "echo done=allowed; echo ticket=T-1 head=fake result=pass done=allowed; false";
    let fx = Fx::with(Cfg::new().gate("t", loud, true));
    let c1 = fx.commit(&[("src/value.txt", "bad2")]);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 1, "stderr: {}", err(&o));
    assert_eq!(out(&o), format!("ticket=T-1 head={c1} result=fail done=refused\n"));
    assert!(fx.result("T-1").message.contains("done=allowed"), "the output is in the result file, where it belongs");
    let fx = Fx::new();
    let c1 = fx.commit(&[("src/value.txt", "bad2")]);
    assert_eq!(out(&gate(&fx, &fx.c0)), format!("ticket=T-1 head={c1} result=fail done=refused\n"), "twin: a quiet check");
}

// AC 17: the repo comes from the map, never from the folder the process runs in. Twin: the same run from inside r.
#[test]
fn the_repo_comes_from_the_map_not_the_current_folder() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("src/value.txt", "ok")]);
    let (o_head, before) = (git(&fx.other, &["rev-parse", "HEAD"]), listing(&fx.other));
    assert_ne!(o_head, c1);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(fx.result("T-1").head_sha, c1, "the result names r's head");
    assert!(!leaks(&o, &o_head) && !leaks(&o, fx.other.to_str().unwrap()), "o is not mentioned");
    assert_eq!(listing(&fx.other), before, "o is byte-identical");
    let o = gate_as(&fx, "T-1", &fx.c0, &fx.res, &fx.repo, &[]);
    assert_eq!(code(&o), 0, "twin: from inside r; stderr: {}", err(&o));
    assert_eq!(fx.result("T-1").head_sha, c1);
}

// AC 18: intake refusals exit 1 with stdout empty and no folder named. Twin: a valid ticket.
#[test]
fn intake_refusals_exit_1_and_name_no_folder() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    let run_with = |body: &str, repo_dir: &Path| {
        let tf = ticket_file(&fx, body);
        run_args(&fx.other, &args_for(&tf, repo_dir, &fx.c0, fx.res.to_str().unwrap(), &fx.caps), &[("GATE_MARKER", fx.marker.as_path())])
    };
    let o = run_with(&ticket_json("r", "T-1", true), &fx.repo);
    assert_eq!(code(&o), 1, "no repo id: {}", err(&o));
    assert!(out(&o).is_empty());
    let o = run_with(&ticket_json("q", "T-1", false), &fx.repo);
    assert_eq!(code(&o), 1, "repo id missing from the map: {}", err(&o));
    assert!(out(&o).is_empty() && err(&o).contains("`q`"));
    let plain = fx.t.0.join("plain");
    fs::create_dir_all(&plain).unwrap();
    for (label, dir) in [("plain folder", plain.clone()), ("subfolder of the repo", fx.repo.join("src")), ("missing", fx.t.0.join("gone"))] {
        let o = run_with(&ticket_json("r", "T-1", false), &dir);
        assert_eq!(code(&o), 1, "{label}: {}", err(&o));
        assert!(out(&o).is_empty() && !leaks(&o, dir.to_str().unwrap()), "{label}: no folder named: {}", err(&o));
    }
    assert!(fx.markers().is_empty(), "nothing ran");
    let o = run_with(&ticket_json("r", "T-1", false), &fx.repo);
    assert_eq!(code(&o), 0, "twin: a valid ticket; stderr: {}", err(&o));
}

// AC 19: every head-only check is loaded before the first run, so a program or gate file that leaves the repo refuses with nothing run.
#[test]
fn a_gate_program_or_file_outside_the_repo_refuses_before_anything_runs() {
    let map = r#"{"src":["t"],"docs":["p","z"]}"#;
    let diff = [("src/value.txt", "ok"), ("docs/x.txt", "doc1")];
    // z.sh is a link to a script outside the repo.
    let fx0 = Fx::new();
    let outside = fx0.t.0.join("outside");
    fs::create_dir_all(&outside).unwrap();
    write_file(&outside.join("z.sh"), &script("zout", "true"), true);
    let cfg = Cfg { links: vec![("ci/z.sh".to_string(), outside.join("z.sh"))], ..Cfg::new().map(map) }.file("gates/z.json", &gate_json("z", "ci/z.sh", false), false);
    let fx = Fx::with(cfg);
    fx.commit(&diff);
    errored(&gate(&fx, &fx.c0));
    assert!(fx.markers().is_empty(), "p, t and the outside script never ran: {:?}", fx.markers());
    no_result_file(&fx);
    // A gate FILE that is a link to a valid gate outside the repo.
    let out_gate = fx0.t.0.join("outside/z.json");
    fs::write(&out_gate, gate_json("z", "ci/z.sh", false)).unwrap();
    let cfg = Cfg { links: vec![("gates/z.json".to_string(), out_gate)], ..Cfg::new().map(map) }.file("ci/z.sh", &script("z", "true"), true);
    let fx = Fx::with(cfg);
    fx.commit(&diff);
    errored(&gate(&fx, &fx.c0));
    assert!(fx.markers().is_empty());
    // Twin: an in-repo program for z runs p, t and z.
    let fx = Fx::with(Cfg::new().map(map).gate("z", "true", false));
    fx.commit(&diff);
    let o = gate(&fx, &fx.c0);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!((fx.ran("p"), fx.ran("t") > 0, fx.ran("z")), (1, true, 1));
}

// AC 20: usage errors exit 2, stdout empty, nothing created. Twin: the complete command line.
#[test]
fn usage_errors_exit_2_and_create_nothing() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    let tf = ticket_file(&fx, &ticket_json("r", "T-1", false));
    let (f, rm, res) = (tf.to_str().unwrap().to_string(), format!("r={}", fx.repo.display()), fx.res.to_str().unwrap().to_string());
    let b = fx.c0.clone();
    let cp = fx.caps.to_str().unwrap().to_string();
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    // Every vector carries a valid --caps-dir, so each case fails for its OWN fault (the first stderr line names it), not for a missing option.
    let cases: Vec<(&str, &str, Vec<String>)> = vec![
        ("no arguments", "no ticket file given", s(&["--caps-dir", &cp])),
        ("no ticket file", "no ticket file given", s(&["--repo-map", &rm, "--base", &b, "--result-dir", &res, "--caps-dir", &cp])),
        ("missing --base", "--base is required", s(&[&f, "--repo-map", &rm, "--result-dir", &res, "--caps-dir", &cp])),
        ("missing --result-dir", "--result-dir is required", s(&[&f, "--repo-map", &rm, "--base", &b, "--caps-dir", &cp])),
        ("repeated --base", "given twice", s(&[&f, "--repo-map", &rm, "--base", &b, "--base", &b, "--result-dir", &res, "--caps-dir", &cp])),
        ("repeated --result-dir", "given twice", s(&[&f, "--repo-map", &rm, "--base", &b, "--result-dir", &res, "--result-dir", &res, "--caps-dir", &cp])),
        ("unknown option", "unknown option", s(&[&f, "--nope", "--repo-map", &rm, "--base", &b, "--result-dir", &res, "--caps-dir", &cp])),
        ("two ticket files", "more than one ticket file", s(&[&f, &f, "--repo-map", &rm, "--base", &b, "--result-dir", &res, "--caps-dir", &cp])),
        ("option without a value", "missing its value", s(&[&f, "--repo-map", &rm, "--caps-dir", &cp, "--base"])),
    ];
    for (label, own_fault, args) in &cases {
        let o = run_args(&fx.other, args, &[("GATE_MARKER", fx.marker.as_path())]);
        assert_eq!(code(&o), 2, "{label}: {}", err(&o));
        assert!(out(&o).is_empty(), "{label}: stdout");
        assert!(err(&o).lines().next().unwrap_or("").contains(own_fault), "{label}: must fail for its own fault `{own_fault}`: {}", err(&o));
        no_result_file(&fx);
    }
    let o = run_args(&fx.other, &s(&[&f, "--repo-map", &rm, "--base", &b, "--result-dir", &res, "--caps-dir", &cp]), &[("GATE_MARKER", fx.marker.as_path())]);
    assert_eq!(code(&o), 0, "twin: complete; stderr: {}", err(&o));
}

// AC 22: a head-only check that edits a tracked file is an error right after THAT check; a later gate never runs. Twin: a check that edits nothing.
#[test]
fn a_head_only_check_that_edits_a_tracked_file_stops_the_run() {
    let map = r#"{"src":["t"],"docs":["e","p"]}"#;
    let fx = Fx::with(Cfg::new().map(map).gate("e", "echo more >> docs/x.txt", false));
    fx.commit(&[("docs/x.txt", "doc1")]);
    errored(&gate(&fx, &fx.c0));
    assert_eq!((fx.ran("e"), fx.ran("p")), (1, 0), "e ran, the later gate p did not");
    no_result_file(&fx);
    let fx = Fx::with(Cfg::new().map(map).gate("e", "true", false));
    fx.commit(&[("docs/x.txt", "doc1")]);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin");
    assert_eq!((fx.ran("e"), fx.ran("p")), (1, 1));
}

// AC 23: a result name that differs only by letter case is a collision (by name, on every filesystem). Twin: the exact name is replaced.
#[test]
fn a_case_variant_of_the_result_name_is_refused() {
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::write(fx.res.join("ex-1.json"), "keep me").unwrap();
    let o = gate_as(&fx, "EX-1", &fx.c0, &fx.res, &fx.other, &[]);
    errored(&o);
    assert_eq!(fs::read_to_string(fx.res.join("ex-1.json")).unwrap(), "keep me");
    let names: Vec<String> = fs::read_dir(&fx.res).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(names, vec!["ex-1.json".to_string()], "nothing was written");
    let fx = Fx::new();
    fx.commit(&[("src/value.txt", "ok")]);
    fs::write(fx.res.join("EX-1.json"), "old").unwrap();
    plant_record(&fx, "EX-1");
    let o = gate_as(&fx, "EX-1", &fx.c0, &fx.res, &fx.other, &[]);
    assert_eq!(code(&o), 0, "twin: the exact name is replaced; stderr: {}", err(&o));
    assert_eq!(fx.result("EX-1").status, Status::Pass);
}

// AC 24: a base equal to the head the repo reports is refused and nothing runs. Twin: the same command with base C0.
#[test]
fn a_base_equal_to_the_head_is_refused() {
    let fx = Fx::new();
    let c1 = fx.commit(&[("src/value.txt", "ok")]);
    errored(&gate(&fx, &c1));
    assert!(fx.markers().is_empty());
    no_result_file(&fx);
    assert_eq!(code(&gate(&fx, &fx.c0)), 0, "twin: base C0");
}
