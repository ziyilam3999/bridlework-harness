//! `harness plugin`: the real binary against the committed tree and scratch copies of it.
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../..");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("plugin-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::create_dir_all(&d).unwrap();
    d
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for e in fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        if e.path().is_dir() {
            copy_dir(&e.path(), &to.join(e.file_name()));
        } else {
            fs::copy(e.path(), to.join(e.file_name())).unwrap();
        }
    }
}

fn read(rel: &str) -> String {
    fs::read_to_string(Path::new(ROOT).join(rel)).unwrap()
}

#[test]
fn seat_outputs_are_byte_identical_to_the_committed_files() {
    // a neighbour pin: these hold on main too
    let hooks = read("plugins/claude/hooks/hooks.json");
    let plugin = read("plugins/claude/.claude-plugin/plugin.json");
    for args in [
        vec!["plugin", "hooks-json", "--repo", ROOT],
        vec!["plugin", "hooks-json", "--repo", ROOT, "--profile", "seat"],
    ] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, stderr(&o));
        assert_eq!(stdout(&o), hooks, "{:?}", args);
    }
    for args in [vec!["plugin", "plugin-json"], vec!["plugin", "plugin-json", "--profile", "seat"]] {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(0), "{:?}: {}", args, stderr(&o));
        assert_eq!(stdout(&o), plugin, "{:?}", args);
    }
}

#[test]
fn orchestrator_outputs_equal_the_committed_files() {
    let hooks = read("plugins/claude/orchestrator/hooks/hooks.json");
    let plugin = read("plugins/claude/orchestrator/.claude-plugin/plugin.json");
    let o = run(&["plugin", "hooks-json", "--repo", ROOT, "--profile", "orchestrator"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), hooks);
    let o = run(&["plugin", "plugin-json", "--profile", "orchestrator"]);
    assert_eq!(o.status.code(), Some(0), "{}", stderr(&o));
    assert_eq!(stdout(&o), plugin);
}

#[test]
fn plugin_check_exits_0_on_the_committed_tree() {
    let o = run(&["plugin", "check", "--repo", ROOT]);
    assert_eq!(o.status.code(), Some(0), "{}{}", stdout(&o), stderr(&o));
    assert_eq!(stdout(&o), "");
}

#[test]
fn plugin_check_names_a_stale_orchestrator_file() {
    let d = tmp("stale");
    copy_dir(&Path::new(ROOT).join("controls"), &d.join("controls"));
    copy_dir(&Path::new(ROOT).join("plugins/claude"), &d.join("plugins/claude"));
    let p = d.join("plugins/claude/orchestrator/hooks/hooks.json");
    let text = fs::read_to_string(&p).unwrap();
    fs::write(&p, text.replace("Orchestrator session", "other session")).unwrap();
    let o = run(&["plugin", "check", "--repo", d.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(1), "{}{}", stdout(&o), stderr(&o));
    assert!(stdout(&o).is_empty(), "{}", stdout(&o));
    assert!(
        stderr(&o).starts_with("harness: plugin: plugins/claude/orchestrator/hooks/hooks.json: "),
        "{}",
        stderr(&o)
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_bad_profile_flag_is_a_usage_error() {
    let root = ROOT;
    let mut cases: Vec<Vec<&str>> = vec![
        vec!["plugin", "hooks-json", "--repo", root, "--profile", "Orchestrator"],
        vec!["plugin", "hooks-json", "--repo", root, "--profile", ""],
        vec!["plugin", "hooks-json", "--repo", root, "--profile"],
        vec!["plugin", "hooks-json", "--repo", root, "--profile", "seat", "--profile", "seat"],
        vec!["plugin", "plugin-json", "--profile", "Orchestrator"],
        vec!["plugin", "plugin-json", "--profile", ""],
        vec!["plugin", "plugin-json", "--profile"],
        vec!["plugin", "plugin-json", "--profile", "seat", "--profile", "seat"],
    ];
    // neighbour pins: these exit 2 today too
    cases.push(vec!["plugin", "check", "--repo", root, "--profile", "orchestrator"]);
    cases.push(vec!["plugin", "check", "--repo", root, "--bogus"]);
    for args in cases {
        let o = run(&args);
        assert_eq!(o.status.code(), Some(2), "{:?}", args);
        assert!(stdout(&o).is_empty(), "{:?}: {}", args, stdout(&o));
        let err = stderr(&o);
        assert_eq!(err.lines().count(), 1, "{:?}: {}", args, err);
        assert!(err.starts_with("harness: plugin: usage: harness plugin"), "{:?}: {}", args, err);
        assert!(err.contains("--profile"), "{:?}: {}", args, err);
    }
}
