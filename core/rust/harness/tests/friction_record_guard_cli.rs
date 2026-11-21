//! The `--record` guard of `friction patterns`, `rank` and `suggest`: one function, three verbs. The real binary
//! runs as a child on a scratch inbox; each test runs over the three verbs. The values are synthetic.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

const VERBS: [&str; 3] = ["patterns", "rank", "suggest"];
const ABS: &str = "--record must be an absolute path";
const UNRESOLVED: &str = "--record cannot be resolved";
const INBOX: &str = "--record must not name the friction inbox";
const CONFIG: &str = "--record must not name a config file";
const SHAPE: &str = "--record names a file that is not a JSON-lines record";

/// A scratch folder with a loadable config and a one-row inbox.
fn scratch() -> Tmp {
    let t = Tmp::new();
    t.write_inbox(&[row("a-1", "2026-11-01", "", "lesson", &[])]);
    t.config();
    t
}

/// Run one verb with `--record`, a 20 s timeout (a hang is a failure). `cfg` None leaves out `--config`.
fn go(verb: &str, record: &str, cfg: Option<&Path>, envs: &[(&str, &str)], cwd: &Path, secret: bool) -> Output {
    let mut c = base();
    c.args(["friction", verb]);
    if verb == "suggest" {
        c.args(["--text", "cache"]);
    }
    c.args(["--record", record]);
    if let Some(f) = cfg {
        c.args(["--config", p(f)]);
    }
    if secret {
        c.env("HARNESS_PRIVACY_LIST_SECRET", DENY);
    }
    for (k, v) in envs {
        c.env(k, v);
    }
    let mut child = c.current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let start = Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > Duration::from_secs(20) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{verb} hung on --record");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn refused(o: &Output, msg: &str) {
    assert_eq!(code(o), 2, "{}", err(o));
    assert!(out(o).is_empty(), "{}", out(o));
    assert_eq!(err(o), format!("harness: error: {msg}\n"));
}

fn lines_of(f: &Path) -> Vec<String> {
    fs::read_to_string(f).unwrap().lines().map(|l| l.to_string()).collect()
}

#[test]
fn a_relative_record_is_refused_and_nothing_is_created() {
    let t = scratch();
    let cfg = t.config();
    fs::create_dir_all(t.0.join("sub")).unwrap();
    for verb in VERBS {
        for rec in ["rec.jsonl", "./rec.jsonl", "sub/../rec.jsonl"] {
            let o = go(verb, rec, Some(&cfg), &[], &t.0, true);
            refused(&o, ABS);
            assert!(!t.0.join("rec.jsonl").exists(), "{verb} {rec}");
        }
    }
}

#[test]
fn a_record_naming_the_config_is_refused_in_each_form() {
    let t = scratch();
    let cfg = t.config();
    fs::create_dir_all(t.0.join("sub")).unwrap();
    symlink(&cfg, t.0.join("link.yaml")).unwrap();
    let before = fs::read(&cfg).unwrap();
    let forms = [p(&cfg).to_string(), format!("{}/link.yaml", p(&t.0)), format!("{}/sub/../config.yaml", p(&t.0))];
    for verb in VERBS {
        for rec in &forms {
            refused(&go(verb, rec, Some(&cfg), &[], &t.0, true), CONFIG);
            assert_eq!(fs::read(&cfg).unwrap(), before, "{verb} {rec}");
        }
    }
}

#[test]
fn a_record_naming_the_config_from_the_environment_is_refused() {
    let t = scratch();
    let cfg = t.config();
    let before = fs::read(&cfg).unwrap();
    for verb in VERBS {
        let o = go(verb, p(&cfg), None, &[("HARNESS_CONFIG", p(&cfg))], &t.0, true);
        refused(&o, CONFIG);
        assert_eq!(fs::read(&cfg).unwrap(), before);
    }
}

#[test]
fn both_the_config_flag_and_the_config_variable_are_protected() {
    let t = scratch();
    let cfg = t.config();
    let other = t.file("other.yaml", fs::read(&cfg).unwrap());
    let before = fs::read(&other).unwrap();
    for verb in VERBS {
        let o = go(verb, p(&other), Some(&cfg), &[("HARNESS_CONFIG", p(&other))], &t.0, true);
        refused(&o, CONFIG);
        let o = go(verb, p(&cfg), Some(&cfg), &[("HARNESS_CONFIG", p(&other))], &t.0, true);
        refused(&o, CONFIG);
        assert_eq!(fs::read(&other).unwrap(), before);
    }
}

#[test]
fn a_hard_link_or_a_case_variant_of_the_config_is_refused() {
    let t = scratch();
    let cfg = t.config();
    let hard = t.0.join("hard.yaml");
    fs::hard_link(&cfg, &hard).unwrap();
    let upper = t.0.join("CONFIG.YAML");
    let insensitive = upper.exists();
    let before = fs::read(&cfg).unwrap();
    for verb in VERBS {
        refused(&go(verb, p(&hard), Some(&cfg), &[], &t.0, true), CONFIG);
        if insensitive {
            refused(&go(verb, p(&upper), Some(&cfg), &[], &t.0, true), CONFIG);
        }
        assert_eq!(fs::read(&cfg).unwrap(), before);
    }
}

#[test]
fn a_record_naming_a_privacy_list_file_is_refused() {
    let t = scratch();
    let cfg = t.config();
    let list = t.file("list.txt", format!("{DENY}\n"));
    let old = t.file("old-list.txt", format!("{DENY}\n"));
    for verb in VERBS {
        // The list file is not read with the secret set, and its path is still protected.
        let o = go(verb, p(&list), Some(&cfg), &[("HARNESS_PRIVACY_LIST", p(&list))], &t.0, true);
        refused(&o, CONFIG);
        let o = go(verb, p(&list), Some(&cfg), &[("HARNESS_PRIVACY_LIST", p(&list))], &t.0, false);
        refused(&o, CONFIG);
        let o = go(verb, p(&old), Some(&cfg), &[("HARNESS_PRIVACY_OLD_LIST", p(&old))], &t.0, true);
        refused(&o, CONFIG);
        assert_eq!(fs::read_to_string(&list).unwrap(), format!("{DENY}\n"));
        assert_eq!(fs::read_to_string(&old).unwrap(), format!("{DENY}\n"));
    }
}

#[test]
fn a_privacy_list_path_that_is_relative_or_missing_is_protected() {
    let t = scratch();
    let cfg = t.config();
    fs::create_dir_all(t.0.join("sub")).unwrap();
    let rel = t.file("rel-list.txt", format!("{DENY}\n"));
    for verb in VERBS {
        // A relative list value is taken against the working folder.
        let o = go(verb, p(&rel), Some(&cfg), &[("HARNESS_PRIVACY_LIST", "rel-list.txt")], &t.0, true);
        refused(&o, CONFIG);
        // A relative and an absolute list path that do not exist yet: refused, and not made.
        let o = go(verb, &format!("{}/missing-rel.txt", p(&t.0)), Some(&cfg), &[("HARNESS_PRIVACY_LIST", "missing-rel.txt")], &t.0, true);
        refused(&o, CONFIG);
        assert!(!t.0.join("missing-rel.txt").exists());
        let abs = format!("{}/missing-abs.txt", p(&t.0));
        let via = format!("{}/sub/../missing-abs.txt", p(&t.0));
        let o = go(verb, &via, Some(&cfg), &[("HARNESS_PRIVACY_LIST", &abs)], &t.0, true);
        refused(&o, CONFIG);
        assert!(!t.0.join("missing-abs.txt").exists());
    }
}

#[test]
fn a_protected_path_that_cannot_be_resolved_is_refused() {
    let t = scratch();
    let cfg = t.config();
    let target = t.0.join("list-target.txt");
    let link = t.0.join("list-link.txt");
    symlink(&target, &link).unwrap();
    for verb in VERBS {
        // The record names the missing target of a dangling list link: refused, not made.
        let o = go(verb, p(&target), Some(&cfg), &[("HARNESS_PRIVACY_LIST", p(&link))], &t.0, true);
        refused(&o, UNRESOLVED);
        let o = go(verb, p(&target), Some(&cfg), &[("HARNESS_PRIVACY_OLD_LIST", p(&link))], &t.0, true);
        refused(&o, UNRESOLVED);
        // The same for a dangling config variable (the config flag is read, so it is a valid file).
        let o = go(verb, p(&target), Some(&cfg), &[("HARNESS_CONFIG", p(&link))], &t.0, true);
        refused(&o, UNRESOLVED);
        assert!(!target.exists(), "{verb}");
    }
}

#[test]
fn a_record_naming_the_inbox_keeps_its_own_refusal() {
    let t = scratch();
    let cfg = t.config();
    symlink(t.inbox(), t.0.join("inbox-link.jsonl")).unwrap();
    let before = fs::read(t.inbox()).unwrap();
    for verb in VERBS {
        refused(&go(verb, p(&t.inbox()), Some(&cfg), &[], &t.0, true), INBOX);
        refused(&go(verb, &format!("{}/inbox-link.jsonl", p(&t.0)), Some(&cfg), &[], &t.0, true), INBOX);
        assert_eq!(fs::read(t.inbox()).unwrap(), before);
    }
}

#[test]
fn a_file_that_is_not_a_record_is_refused_and_a_record_file_is_appended() {
    for verb in VERBS {
        let t = scratch();
        let cfg = t.config();
        let kv = t.file("wrapper.conf", "record=x\n");
        refused(&go(verb, p(&kv), Some(&cfg), &[], &t.0, true), SHAPE);
        assert_eq!(fs::read_to_string(&kv).unwrap(), "record=x\n");
        let one = t.file("one-byte.txt", "x");
        refused(&go(verb, p(&one), Some(&cfg), &[], &t.0, true), SHAPE);
        assert_eq!(fs::read_to_string(&one).unwrap(), "x");
        let dir = t.0.join("adir");
        fs::create_dir_all(&dir).unwrap();
        refused(&go(verb, p(&dir), Some(&cfg), &[], &t.0, true), SHAPE);
        // An empty file gets one line, then a second.
        let empty = t.file("empty.jsonl", "");
        let o = go(verb, p(&empty), Some(&cfg), &[], &t.0, true);
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert_eq!(lines_of(&empty).len(), 1);
        let o = go(verb, p(&empty), Some(&cfg), &[], &t.0, true);
        assert_eq!(code(&o), 0, "{}", err(&o));
        let l = lines_of(&empty);
        assert!(l.len() == 2 && l.iter().all(|x| x.starts_with('{')), "{l:?}");
        // A new file is made and starts with {.
        let fresh = t.0.join("fresh.jsonl");
        let o = go(verb, p(&fresh), Some(&cfg), &[], &t.0, true);
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert!(fs::read_to_string(&fresh).unwrap().starts_with('{'));
    }
}

#[test]
fn a_fifo_or_a_device_is_refused_before_any_read() {
    let t = scratch();
    let cfg = t.config();
    let fifo = t.0.join("pipe.fifo");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status().unwrap();
    assert!(made.success());
    for verb in VERBS {
        refused(&go(verb, p(&fifo), Some(&cfg), &[], &t.0, true), SHAPE);
        refused(&go(verb, "/dev/null", Some(&cfg), &[], &t.0, true), SHAPE);
    }
}

#[test]
fn a_record_that_cannot_be_resolved_is_refused_and_nothing_is_created() {
    let t = scratch();
    let cfg = t.config();
    symlink(t.0.join("target.jsonl"), t.0.join("dangle.jsonl")).unwrap();
    for verb in VERBS {
        let o = go(verb, &format!("{}/nodir/rec.jsonl", p(&t.0)), Some(&cfg), &[], &t.0, true);
        refused(&o, UNRESOLVED);
        assert!(!t.0.join("nodir").exists());
        let o = go(verb, &format!("{}/dangle.jsonl", p(&t.0)), Some(&cfg), &[], &t.0, true);
        refused(&o, UNRESOLVED);
        assert!(!t.0.join("target.jsonl").exists());
        let o = go(verb, &format!("{}/no-such/..", p(&t.0)), Some(&cfg), &[], &t.0, true);
        refused(&o, UNRESOLVED);
    }
}

#[test]
fn the_guard_runs_before_the_privacy_list_is_loaded() {
    let t = scratch();
    let cfg = t.config();
    for verb in VERBS {
        let o = go(verb, "rec.jsonl", Some(&cfg), &[], &t.0, false);
        refused(&o, ABS);
    }
}

#[test]
fn a_new_record_through_a_linked_folder_is_made() {
    for verb in VERBS {
        let t = scratch();
        let cfg = t.config();
        let real: PathBuf = t.0.join("real");
        fs::create_dir_all(&real).unwrap();
        symlink(&real, t.0.join("linkdir")).unwrap();
        let o = go(verb, &format!("{}/linkdir/new.jsonl", p(&t.0)), Some(&cfg), &[], &t.0, true);
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert!(fs::read_to_string(real.join("new.jsonl")).unwrap().starts_with('{'));
    }
}
