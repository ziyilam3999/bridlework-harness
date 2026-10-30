//! Tree preparation smoke (Unix). Every scenario runs in a CHILD run of this test binary (`child_run`, ignored by default) so that TMPDIR and GIT_* are
//! really set in the child's environment; the parameters travel in a file inside the scratch repo's git folder. Each scenario has its own scratch repo and root.
#![cfg(unix)]
use harness::runner::Ended;
use harness::tree_prep::{prepare_and_run, Spec};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const GOOD_CHECK: &str = "#!/bin/sh\ntest \"$(cat value.txt)\" = good\n";

// ---------- child side ----------

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('|', "\\p")
}

fn end_text(e: &Ended) -> String {
    match e {
        Ended::Exit(c) => format!("exit:{}", c),
        Ended::Signal(s) => format!("signal:{}", s),
        Ended::TimedOut => "timeout".to_string(),
        Ended::NotStarted => "notstarted".to_string(),
        Ended::Unknown => "unknown".to_string(),
    }
}

#[test]
#[ignore]
fn child_run() {
    let text = std::fs::read_to_string(".git/tp-params").expect("params file");
    let get = |k: &str| -> String {
        text.lines().find_map(|l| l.strip_prefix(&format!("{}=", k)).map(|v| v.to_string())).unwrap_or_default()
    };
    let budget: u64 = get("budget_ms").parse().unwrap_or(10_000);
    let spec = Spec { id: "ticket-test", program: &get("program"), args: &[], budget: Duration::from_millis(budget), max_output: 4000 };
    match prepare_and_run(Path::new(&get("repo")), &get("base"), &get("head"), &get("recorded"), &spec) {
        Err(e) => println!("TP|ERR|{}", esc(&e)),
        Ok(r) => {
            println!("TP|OK|");
            for (n, e) in [("base", &r.base), ("head", &r.head)] {
                println!("TP|{}_sha|{}", n, e.sha);
                println!("TP|{}_end|{}", n, end_text(&e.ended));
                println!("TP|{}_out|{}", n, esc(&e.output));
            }
        }
    }
}

// ---------- parent side ----------

fn base_dir(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("htp-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Tester")
        .env("GIT_AUTHOR_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
        .env("GIT_COMMITTER_NAME", "Tester")
        .env("GIT_COMMITTER_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
        .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git failed: {:?} {}", args, String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn put(dir: &Path, rel: &str, body: &str, mode: u32) {
    let f = dir.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(&f, body).unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// A scratch world: `repo` (a git repo), `tmp` (the child's scratch root, starts empty), `work` (decoys, logs, markers).
struct Fx {
    root: PathBuf,
    repo: PathBuf,
    tmp: PathBuf,
    work: PathBuf,
    base: String,
    head: String,
}

/// Two commits. Base holds value.txt = `base_val` (and the check when `check_at_base`); head holds value.txt = `head_val` and the check.
fn fixture(name: &str, base_val: &str, head_val: &str, check_at_base: bool, check: &str) -> Fx {
    fixture_mode(name, base_val, head_val, if check_at_base { Some(0o755) } else { None }, check)
}

/// Like `fixture`, with the mode the check has in the base commit (None: absent at base).
fn fixture_mode(name: &str, base_val: &str, head_val: &str, base_mode: Option<u32>, check: &str) -> Fx {
    let root = base_dir(name);
    let (repo, tmp, work) = (root.join("repo"), root.join("tmp"), root.join("work"));
    for d in [&repo, &tmp, &work] {
        std::fs::create_dir_all(d).unwrap();
    }
    git(&repo, &["init", "-q"]);
    put(&repo, "value.txt", base_val, 0o644);
    if let Some(m) = base_mode {
        put(&repo, "ci/check.sh", check, m);
    }
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    put(&repo, "value.txt", head_val, 0o644);
    put(&repo, "ci/check.sh", check, 0o755);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    Fx { root, repo, tmp, work, base, head }
}

#[derive(Debug, Default)]
struct Out {
    err: Option<String>,
    ok: bool,
    base_sha: String,
    base_end: String,
    base_out: String,
    head_sha: String,
    head_end: String,
    head_out: String,
    raw: String,
}

fn write_params(fx: &Fx, params: &[(&str, &str)]) {
    let mut p: Vec<(String, String)> = vec![
        ("repo".into(), fx.repo.to_string_lossy().into_owned()),
        ("base".into(), fx.base.clone()),
        ("head".into(), fx.head.clone()),
        ("recorded".into(), fx.head.clone()),
        ("program".into(), "ci/check.sh".into()),
    ];
    for (k, v) in params {
        p.retain(|(pk, _)| pk != k);
        p.push((k.to_string(), v.to_string()));
    }
    let body: String = p.iter().map(|(k, v)| format!("{}={}\n", k, v)).collect();
    std::fs::write(fx.repo.join(".git/tp-params"), body).unwrap();
}

fn child_cmd(fx: &Fx, env: &[(&str, &str)]) -> Command {
    let mut c = Command::new(std::env::current_exe().unwrap());
    c.args(["--ignored", "--exact", "child_run", "--nocapture", "--test-threads=1"]).current_dir(&fx.repo).env("TMPDIR", &fx.tmp);
    for (k, v) in env {
        c.env(k, v);
    }
    c
}

fn run_with(fx: &Fx, params: &[(&str, &str)], env: &[(&str, &str)]) -> Out {
    write_params(fx, params);
    parse_out(child_cmd(fx, env).output().unwrap())
}

fn parse_out(o: std::process::Output) -> Out {
    let stdout = String::from_utf8_lossy(&o.stdout).into_owned();
    let mut out = Out { raw: stdout.clone(), ..Default::default() };
    for line in stdout.lines() {
        let Some(at) = line.find("TP|") else { continue };
        let rest = &line[at + 3..];
        let (k, v) = rest.split_once('|').unwrap_or((rest, ""));
        match k {
            "ERR" => out.err = Some(v.to_string()),
            "OK" => out.ok = true,
            "base_sha" => out.base_sha = v.to_string(),
            "base_end" => out.base_end = v.to_string(),
            "base_out" => out.base_out = v.to_string(),
            "head_sha" => out.head_sha = v.to_string(),
            "head_end" => out.head_end = v.to_string(),
            "head_out" => out.head_out = v.to_string(),
            _ => {}
        }
    }
    assert!(out.ok || out.err.is_some(), "child produced no result: {}\n{}", stdout, String::from_utf8_lossy(&o.stderr));
    out
}

fn run(fx: &Fx) -> Out {
    run_with(fx, &[], &[])
}

/// The cleanup check every scenario ends with.
fn assert_clean(fx: &Fx) {
    let list = git(&fx.repo, &["worktree", "list", "--porcelain"]);
    assert_eq!(list.lines().filter(|l| l.starts_with("worktree ")).count(), 1, "stale worktree entries:\n{}", list);
    let admin = fx.repo.join(".git/worktrees");
    if admin.exists() {
        assert_eq!(std::fs::read_dir(&admin).unwrap().count(), 0, "stale admin entries");
    }
    assert_eq!(std::fs::read_dir(&fx.tmp).unwrap().count(), 0, "scratch root not empty");
}

fn refused(o: &Out, needle: &str) {
    let e = o.err.as_deref().unwrap_or_else(|| panic!("expected a refusal, got: {}", o.raw));
    assert!(e.contains(needle), "refusal {:?} lacks {:?}", e, needle);
}

#[test]
fn red_at_base_green_at_head_reports_both_shas_and_exit_codes() {
    let fx = fixture("ac1", "bad", "good", true, GOOD_CHECK);
    let o = run(&fx);
    assert!(o.ok, "{}", o.raw);
    assert_eq!((o.base_sha.as_str(), o.base_end.as_str()), (fx.base.as_str(), "exit:1"));
    assert_eq!((o.head_sha.as_str(), o.head_end.as_str()), (fx.head.as_str(), "exit:0"));
    assert!(fx.base.len() >= 40 && fx.head.len() >= 40);
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn a_check_added_by_the_fix_is_carried_to_base() {
    let fx = fixture("ac2", "bad", "good", false, GOOD_CHECK);
    let o = run(&fx);
    assert!(o.ok, "{}", o.raw);
    assert_eq!(o.base_end, "exit:1");
    assert_eq!(o.head_end, "exit:0");
    assert_eq!(git(&fx.repo, &["status", "--porcelain"]), "");
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn a_symlinked_check_folder_at_base_never_writes_outside_the_tree() {
    let root = base_dir("ac3");
    let (repo, tmp, work) = (root.join("repo"), root.join("tmp"), root.join("work"));
    for d in [&repo, &tmp, &work] {
        std::fs::create_dir_all(d).unwrap();
    }
    let outside = work.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("check.sh"), "victim\n").unwrap();
    git(&repo, &["init", "-q"]);
    symlink(&outside, repo.join("ci")).unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    std::fs::remove_file(repo.join("ci")).unwrap();
    put(&repo, "ci/check.sh", GOOD_CHECK, 0o755);
    put(&repo, "value.txt", "good", 0o644);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let fx = Fx { root, repo, tmp, work, base, head };
    let o = run(&fx);
    assert!(o.err.is_some(), "{}", o.raw);
    assert_eq!(std::fs::read_to_string(outside.join("check.sh")).unwrap(), "victim\n");
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn a_dot_dot_check_path_never_writes_outside_the_trees() {
    let root = base_dir("ac4");
    let (repo, tmp, work) = (root.join("repo"), root.join("tmp"), root.join("work"));
    for d in [&repo, &tmp, &work] {
        std::fs::create_dir_all(d).unwrap();
    }
    let outside = work.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let decoy = outside.join("decoy.txt");
    std::fs::write(&decoy, "victim\n").unwrap();
    let rel = decoy.to_str().unwrap().trim_start_matches('/').to_string();
    git(&repo, &["init", "-q"]);
    put(&repo, "value.txt", "bad", 0o644);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "base"]);
    let base = git(&repo, &["rev-parse", "HEAD"]);
    let deep = (0..30).map(|_| "d").collect::<Vec<_>>().join("/");
    std::fs::create_dir_all(repo.join(&deep)).unwrap();
    std::fs::write(repo.join(&deep).join("keep"), "x").unwrap();
    symlink(&deep, repo.join("a")).unwrap();
    put(&repo, &rel, "#!/bin/sh\nexit 0\n", 0o755);
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "head"]);
    let head = git(&repo, &["rev-parse", "HEAD"]);
    let fx = Fx { root, repo, tmp, work, base, head };
    let program = format!("a/{}{}", "../".repeat(30), rel);
    let o = run_with(&fx, &[("program", &program)], &[]);
    assert!(o.err.is_some(), "{}", o.raw);
    assert_eq!(std::fs::read_to_string(&decoy).unwrap(), "victim\n");
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 1);
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn a_relative_tmpdir_is_refused_and_creates_nothing_in_the_repo() {
    let fx = fixture("ac5", "bad", "good", true, GOOD_CHECK);
    // a relative TMPDIR, child cwd = the repo work tree
    let o = run_with(&fx, &[], &[("TMPDIR", "rel-tmp")]);
    refused(&o, "absolute");
    assert_eq!(git(&fx.repo, &["status", "--porcelain", "--ignored"]), "");
    assert!(!fx.repo.join("rel-tmp").exists());
    // an absolute TMPDIR inside the work tree
    let inside = fx.repo.join("inside");
    std::fs::create_dir_all(&inside).unwrap();
    let o = run_with(&fx, &[], &[("TMPDIR", inside.to_str().unwrap())]);
    refused(&o, "inside the repo");
    assert_eq!(std::fs::read_dir(&inside).unwrap().count(), 0);
    // an absolute TMPDIR inside the git folder
    let ginner = fx.repo.join(".git/inner");
    std::fs::create_dir_all(&ginner).unwrap();
    let o = run_with(&fx, &[], &[("TMPDIR", ginner.to_str().unwrap())]);
    refused(&o, "inside the repo");
    assert_eq!(std::fs::read_dir(&ginner).unwrap().count(), 0);
    let admin = fx.repo.join(".git/worktrees");
    assert!(!admin.exists() || std::fs::read_dir(&admin).unwrap().count() == 0);
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn a_check_path_naming_dot_git_in_any_case_is_refused() {
    let fx = fixture("ac6", "bad", "good", true, GOOD_CHECK);
    for bad in [".git", ".GIT", ".Git", "sub/.gIt/x"] {
        let o = run_with(&fx, &[("program", bad)], &[]);
        refused(&o, "plain names");
        assert_clean(&fx);
    }
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn a_check_that_breaks_its_own_dot_git_leaves_no_stale_entry() {
    for (name, body) in [("deletes", "#!/bin/sh\nrm -f .git\nexit 0\n"), ("junk", "#!/bin/sh\necho junk > .git\nexit 0\n")] {
        let fx = fixture(&format!("ac7-{}", name), "bad", "good", true, body);
        let o = run(&fx);
        assert!(o.ok, "{} {}", name, o.raw);
        assert_eq!((o.base_end.as_str(), o.head_end.as_str()), ("exit:0", "exit:0"));
        assert_clean(&fx);
        let _ = std::fs::remove_dir_all(fx.root);
    }
}

#[test]
fn the_check_sees_no_git_variables_and_cannot_touch_another_index() {
    let check = "#!/bin/sh\nif env | grep -q '^GIT_'; then exit 1; fi\ngit add -A\nexit 0\n";
    let fx = fixture("ac8", "bad", "good", true, check);
    let decoy = fx.work.join("decoy");
    std::fs::create_dir_all(&decoy).unwrap();
    git(&decoy, &["init", "-q"]);
    put(&decoy, "f.txt", "x", 0o644);
    git(&decoy, &["add", "-A"]);
    git(&decoy, &["commit", "-q", "-m", "d"]);
    put(&decoy, "g.txt", "y", 0o644); // untracked: a stray `git add -A` against this index would change it
    let dindex = decoy.join(".git/index");
    let mindex = fx.repo.join(".git/index");
    let (d0, m0) = (std::fs::read(&dindex).unwrap(), std::fs::read(&mindex).unwrap());
    let gd = decoy.join(".git");
    let o = run_with(
        &fx,
        &[],
        &[("GIT_DIR", gd.to_str().unwrap()), ("GIT_WORK_TREE", decoy.to_str().unwrap()), ("GIT_INDEX_FILE", dindex.to_str().unwrap())],
    );
    assert!(o.ok, "{}", o.raw);
    assert_eq!((o.base_end.as_str(), o.head_end.as_str()), ("exit:0", "exit:0"));
    assert_eq!(std::fs::read(&dindex).unwrap(), d0);
    assert_eq!(std::fs::read(&mindex).unwrap(), m0);
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn both_ends_run_at_an_identical_path() {
    let root = base_dir("ac9-log");
    let log = root.join("log.txt");
    let check = format!("#!/bin/sh\n{{ pwd -P; echo \"$PWD\"; cat ./.git; }} >> {}\nexit 0\n", log.display());
    let fx = fixture("ac9", "bad", "good", true, &check);
    let o = run(&fx);
    assert!(o.ok, "{}", o.raw);
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 6, "{}", text);
    assert_eq!(lines[0..3], lines[3..6]);
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_signal_at_base_is_reported_as_a_signal() {
    for (name, body, budget, want) in [
        ("kill", "#!/bin/sh\nkill -KILL $$\n", "10000", "signal:9"),
        ("term", "#!/bin/sh\nkill -TERM $$\n", "10000", "signal:15"),
        ("slow", "#!/bin/sh\nexec sleep 5\n", "300", "timeout"),
        ("e126", "#!/bin/sh\nexit 126\n", "10000", "exit:126"),
    ] {
        let fx = fixture(&format!("ac10-{}", name), "bad", "good", true, body);
        let o = run_with(&fx, &[("budget_ms", budget)], &[]);
        assert!(o.ok, "{} {}", name, o.raw);
        assert_eq!(o.base_end, want, "case {}", name);
        assert_clean(&fx);
        let _ = std::fs::remove_dir_all(fx.root);
    }
}

#[test]
fn bad_inputs_are_refused_before_any_tree_exists() {
    // The check leaves a marker whenever it runs anywhere; a refused request must never leave one (nothing ran, so no tree existed).
    let marker = std::env::temp_dir().join(format!("htp-ac11-{}", std::process::id())).join("work/ran");
    let check = format!("#!/bin/sh\ntouch {}\ntest \"$(cat value.txt)\" = good\n", marker.display());
    let fx = fixture("ac11", "bad", "good", true, &check);
    // extra files at head: a symlink blob, a non-executable file
    symlink("check.sh", fx.repo.join("ci/link.sh")).unwrap();
    put(&fx.repo, "ci/plain.sh", GOOD_CHECK, 0o644);
    git(&fx.repo, &["add", "-A"]);
    git(&fx.repo, &["commit", "-q", "-m", "more"]);
    let head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let fx = Fx { head: head.clone(), ..fx };
    let sub = fx.repo.join("ci");
    let sub_s = sub.to_str().unwrap().to_string();
    let wrong = fx.base.clone();
    let cases: Vec<(&str, Vec<(&str, &str)>)> = vec![
        ("dash ref", vec![("base", "-x")]),
        ("blob ref", vec![("base", "HEAD:value.txt")]),
        ("unknown ref", vec![("base", "no-such-ref")]),
        ("head not the recorded sha", vec![("recorded", &wrong)]),
        ("base equal to head", vec![("base", &head)]),
        ("repo is a subfolder", vec![("repo", &sub_s)]),
        ("symlink check", vec![("program", "ci/link.sh")]),
        ("folder check", vec![("program", "ci")]),
        ("not executable", vec![("program", "ci/plain.sh")]),
        ("missing check", vec![("program", "ci/missing.sh")]),
        ("empty check", vec![("program", "")]),
    ];
    for (name, params) in cases {
        let o = run_with(&fx, &params, &[]);
        assert!(o.err.is_some(), "case {} was not refused: {}", name, o.raw);
        assert!(!marker.exists(), "case {}: the check ran before the refusal", name);
        assert_clean(&fx);
    }
    // a linked work tree of the scratch repo is refused as the repo path
    let linked = fx.work.join("linked");
    git(&fx.repo, &["worktree", "add", "--detach", linked.to_str().unwrap(), "HEAD"]);
    let linked_s = linked.to_str().unwrap().to_string();
    let o = run_with(&fx, &[("repo", &linked_s)], &[]);
    refused(&o, "linked work tree");
    assert!(!marker.exists(), "linked work tree: the check ran before the refusal");
    git(&fx.repo, &["worktree", "remove", "--force", linked.to_str().unwrap()]);
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn git_hooks_never_fire_during_tree_preparation() {
    let fx = fixture("ac16", "bad", "good", true, GOOD_CHECK);
    let marker = fx.work.join("marker");
    put(&fx.repo, ".git/hooks/post-checkout", &format!("#!/bin/sh\ntouch {}\n", marker.display()), 0o755);
    let o = run(&fx);
    assert!(o.ok, "{}", o.raw);
    assert!(!marker.exists(), "a repo hook fired during tree preparation");
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn an_existing_non_executable_check_at_base_still_runs_there() {
    let fx = fixture_mode("fix1", "bad", "good", Some(0o644), GOOD_CHECK);
    let o = run(&fx);
    assert!(o.ok, "{}", o.raw);
    assert_eq!((o.base_end.as_str(), o.head_end.as_str()), ("exit:1", "exit:0"));
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn relative_worktree_pointers_do_not_break_cleanup_between_the_ends() {
    for (name, body) in [("deletes", "#!/bin/sh\nrm -f .git\nexit 0\n"), ("junk", "#!/bin/sh\necho junk > .git\nexit 0\n")] {
        let fx = fixture(&format!("fix2-{}", name), "bad", "good", true, body);
        git(&fx.repo, &["config", "worktree.useRelativePaths", "true"]);
        let o = run(&fx);
        assert!(o.ok, "{} {}", name, o.raw);
        assert_eq!((o.base_end.as_str(), o.head_end.as_str()), ("exit:0", "exit:0"), "case {}", name);
        assert_clean(&fx);
        let _ = std::fs::remove_dir_all(fx.root);
    }
}

#[test]
fn concurrent_runs_on_one_repo_see_the_same_pointer_at_both_ends() {
    let root = base_dir("fix3-log");
    let log = root.join("log.txt");
    // Run A is short and starts first; run B is long (its scratch root name says so) and starts later, so B's base tree is made while A's exists
    // and B's head tree is made after A's is gone: under a constant tree-folder name git would give B two different admin names.
    let check = format!("#!/bin/sh\ncase \"$TMPDIR\" in *slow*) sleep 3.5;; *) sleep 1.5;; esac\necho \"$(pwd -P) $(cat ./.git)\" >> {}\nexit 0\n", log.display());
    let fx = fixture("fix3", "bad", "good", true, &check);
    let (ta, tb) = (fx.root.join("ta"), fx.root.join("slow-tb"));
    std::fs::create_dir_all(&ta).unwrap();
    std::fs::create_dir_all(&tb).unwrap();
    write_params(&fx, &[]);
    let a = child_cmd(&fx, &[("TMPDIR", ta.to_str().unwrap())]).spawn_piped();
    std::thread::sleep(Duration::from_millis(400));
    let b = child_cmd(&fx, &[("TMPDIR", tb.to_str().unwrap())]).spawn_piped();
    let (oa, ob) = (parse_out(a.wait_with_output().unwrap()), parse_out(b.wait_with_output().unwrap()));
    assert!(oa.ok && ob.ok, "{} {}", oa.raw, ob.raw);
    let text = std::fs::read_to_string(&log).unwrap();
    let mut by_dir: std::collections::BTreeMap<&str, Vec<&str>> = std::collections::BTreeMap::new();
    for l in text.lines() {
        by_dir.entry(l.split_once(' ').unwrap().0).or_default().push(l);
    }
    assert_eq!(by_dir.len(), 2, "{}", text);
    for (d, lines) in &by_dir {
        assert_eq!(lines.len(), 2, "{}", d);
        assert_eq!(lines[0], lines[1], "pointer differs between the ends of one run:\n{}", text);
    }
    assert_clean(&fx);
    assert_eq!(std::fs::read_dir(&ta).unwrap().count() + std::fs::read_dir(&tb).unwrap().count(), 0, "scratch roots not empty");
    let _ = std::fs::remove_dir_all(fx.root);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_content_filter_at_head_does_not_change_what_runs() {
    let fx = fixture("fix4", "bad", "good", true, GOOD_CHECK);
    put(&fx.repo, ".gitattributes", "ci/check.sh text eol=crlf\n", 0o644);
    git(&fx.repo, &["add", "-A"]);
    git(&fx.repo, &["commit", "-q", "-m", "attrs"]);
    let head = git(&fx.repo, &["rev-parse", "HEAD"]);
    let fx = Fx { head, ..fx };
    let o = run(&fx);
    assert!(o.ok, "{}", o.raw);
    assert_eq!((o.base_end.as_str(), o.head_end.as_str()), ("exit:1", "exit:0"), "{}", o.raw);
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

#[test]
fn read_only_folders_left_by_a_check_do_not_block_cleanup() {
    let fx = fixture("fix5", "bad", "good", true, "#!/bin/sh\nmkdir -p d/e\ntouch d/e/f\nchmod 500 d/e d\nexit 0\n");
    let o = run(&fx);
    assert!(o.ok, "{}", o.raw);
    assert_eq!((o.base_end.as_str(), o.head_end.as_str()), ("exit:0", "exit:0"));
    assert_clean(&fx);
    let _ = std::fs::remove_dir_all(fx.root);
}

trait SpawnPiped {
    fn spawn_piped(&mut self) -> std::process::Child;
}

impl SpawnPiped for Command {
    fn spawn_piped(&mut self) -> std::process::Child {
        self.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn().unwrap()
    }
}
