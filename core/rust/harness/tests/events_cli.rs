//! Process-level tests for `harness events append | list | verify` (PRE-6b, v2 run identity in PRE-B). Every check runs the
//! real binary as a child process. `recover` is not built: the plan holds it until the torn-tail recovery question is
//! signed, so the torn-tail arm here is the "stays refused" branch. Since v2 every append carries a run identity: the first
//! append of a log self-issues (`run_issued` line first), later appends inherit the id through `HARNESS_RUN_ID`.
#![cfg(unix)]
use std::fs;
use std::io::{Seek as _, SeekFrom, Write as _};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use harness::events::LOG_READ_CAP;

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let p = std::env::temp_dir().join(format!("hev-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(p.canonicalize().unwrap())
    }

    fn log(&self) -> PathBuf {
        self.0.join("events.jsonl")
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(args: &[&str], cwd: Option<&Path>) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null());
    // The run-identity contract is part of every assertion here, so an ambient value must never leak into a child.
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL"] {
        c.env_remove(k);
    }
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    c.output().unwrap()
}

/// Run with `HARNESS_RUN_ID` set to `id` (the inheritance contract).
fn run_as(args: &[&str], id: &str) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null()).env("HARNESS_RUN_ID", id);
    for k in ["HARNESS_ROLE", "HARNESS_MODEL"] {
        c.env_remove(k);
    }
    c.output().unwrap()
}

/// Run with `HARNESS_ROLE`/`HARNESS_MODEL` in the environment (no flags): the env half of the identity contract.
fn run_env_identity(args: &[&str], role: &str, model: &str) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null()).env("HARNESS_ROLE", role).env("HARNESS_MODEL", model);
    c.env_remove("HARNESS_RUN_ID");
    c.output().unwrap()
}

fn ok(o: &Output) -> bool {
    o.status.code() == Some(0)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn append(log: &Path, kind: &str, ts: &str) -> Output {
    run(
        &[
            "events",
            "append",
            "--log",
            log.to_str().unwrap(),
            "--kind",
            kind,
            "--ticket",
            "t-1",
            "--repo",
            "my-repo",
            "--reason",
            "cap.hit",
            "--role",
            "executor",
            "--model",
            "m-1",
            "--ts-ms",
            ts,
        ],
        None,
    )
}

/// An append that inherits `id` through `HARNESS_RUN_ID`.
fn append_as(log: &Path, kind: &str, ts: &str, id: &str) -> Output {
    let a = [
        "events",
        "append",
        "--log",
        log.to_str().unwrap(),
        "--kind",
        kind,
        "--ticket",
        "t-1",
        "--repo",
        "my-repo",
        "--reason",
        "cap.hit",
        "--role",
        "executor",
        "--model",
        "m-1",
        "--ts-ms",
        ts,
    ];
    run_as(&a, id)
}

fn verify(log: &Path) -> Output {
    run(&["events", "verify", "--log", log.to_str().unwrap()], None)
}

fn list(log: &Path) -> Output {
    run(&["events", "list", "--log", log.to_str().unwrap()], None)
}

/// The `run_id` of line `n` (0-based) of the log, without assuming the id's shape beyond `"run_id":"..."`.
fn run_id_of(text: &str, n: usize) -> String {
    let l = text.lines().nth(n).unwrap_or_else(|| panic!("no line {n} in {text:?}"));
    l.split("\"run_id\":\"").nth(1).unwrap().split('"').next().unwrap().to_string()
}

fn line(kind: &str, ts: u64, run_id: &str) -> String {
    format!("{{\"v\":2,\"kind\":\"{kind}\",\"ts_ms\":{ts},\"run_id\":\"{run_id}\",\"ticket\":\"t-1\",\"repo\":\"my-repo\",\"reason\":\"cap.hit\",\"role\":\"executor\",\"model\":\"m-1\"}}\n")
}

fn issue_line(ts: u64, run_id: &str) -> String {
    format!("{{\"v\":2,\"kind\":\"run_issued\",\"ts_ms\":{ts},\"run_id\":\"{run_id}\",\"ticket\":\"t-1\",\"repo\":\"my-repo\",\"reason\":\"self-issue\",\"role\":\"executor\",\"model\":\"m-1\"}}\n")
}

/// A fresh log holding the three cap kinds under one run: the first append self-issues (two lines), the next two inherit
/// the id. Returns the exact expected bytes.
fn three(t: &Tmp) -> String {
    assert!(ok(&append(&t.log(), "escalation", "1")), "the self-issuing first append");
    let id = run_id_of(&fs::read_to_string(t.log()).unwrap(), 0);
    assert!(ok(&append_as(&t.log(), "replan_required", "2", &id)));
    assert!(ok(&append_as(&t.log(), "budget_exceeded", "3", &id)));
    format!("{}{}{}{}", issue_line(1, &id), line("escalation", 1, &id), line("replan_required", 2, &id), line("budget_exceeded", 3, &id))
}

#[test]
fn ac1_round_trip_prints_the_canonical_bytes_in_append_order() {
    let t = Tmp::new();
    let want = three(&t);
    assert_eq!(fs::read_to_string(t.log()).unwrap(), want);
    assert!(ok(&verify(&t.log())));
    let l = list(&t.log());
    assert!(ok(&l));
    assert_eq!(out(&l), want);
}

#[test]
fn ac1_planted_a_fifth_kind_is_refused_and_the_log_is_unchanged() {
    let t = Tmp::new();
    three(&t);
    let before = fs::read(t.log()).unwrap();
    let o = append(&t.log(), "surprise", "9");
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("EVENT NOT RECORDED"));
    assert_eq!(fs::read(t.log()).unwrap(), before);
}

#[test]
fn ac2_torn_tail_verify_names_it_and_list_prints_exactly_three() {
    let t = Tmp::new();
    let want = three(&t);
    let cut = want.len() - 10;
    fs::write(t.log(), &want.as_bytes()[..cut]).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(1));
    assert!(err(&v).contains("torn tail") && err(&v).contains("line 4"), "{}", err(&v));
    assert_eq!(out(&v), "");
    let l = list(&t.log());
    assert_eq!(l.status.code(), Some(1));
    assert_eq!(out(&l), want.lines().take(3).map(|s| format!("{s}\n")).collect::<String>());
}

#[test]
fn ac2_planted_a_clean_boundary_is_not_torn_and_a_valid_json_fragment_still_is() {
    let t = Tmp::new();
    let want = three(&t);
    let three_lines: String = want.lines().take(3).map(|s| format!("{s}\n")).collect();
    fs::write(t.log(), &three_lines).unwrap();
    assert!(ok(&verify(&t.log())), "a cut exactly at a line boundary is clean");
    fs::write(t.log(), format!("{three_lines}{}", want.lines().nth(3).unwrap())).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(1), "a complete-looking last line with no newline is torn");
    assert!(err(&v).contains("torn tail"));
    assert_eq!(out(&list(&t.log())).lines().count(), 3);
}

#[test]
fn ac3_append_onto_a_torn_tail_is_refused_every_time_and_the_bytes_are_unchanged() {
    let t = Tmp::new();
    let want = three(&t);
    let torn = &want.as_bytes()[..want.len() - 10];
    fs::write(t.log(), torn).unwrap();
    let id = run_id_of(&want, 0);
    for ts in ["10", "11"] {
        let o = append_as(&t.log(), "escalation", ts, &id);
        assert_eq!(o.status.code(), Some(2));
        assert!(err(&o).contains("torn tail") && err(&o).contains("EVENT NOT RECORDED"), "{}", err(&o));
        assert_eq!(fs::read(t.log()).unwrap(), torn, "no self-heal: the file is never touched");
    }
    // `recover` does not exist yet.
    assert_eq!(run(&["events", "recover", "--log", t.log().to_str().unwrap()], None).status.code(), Some(2));
    // Twin: the same file with its newline restored accepts the append.
    fs::write(t.log(), &want).unwrap();
    assert!(ok(&append_as(&t.log(), "escalation", "10", &id)));
    assert_eq!(out(&list(&t.log())).lines().count(), 5);
}

#[test]
fn ac4_unknown_or_missing_v_is_refused_by_verify_and_list_naming_line_and_v() {
    let t = Tmp::new();
    let good = line("escalation", 1, "run-x");
    for (bad, want) in [
        (good.replace("\"v\":2", "\"v\":3"), "unsupported v 3"),
        (good.replace("\"v\":2", "\"v\":1"), "unsupported v 1"),
        (good.replace("\"v\":2", "\"v\":0"), "unsupported v 0"),
        (good.replace("\"v\":2,", ""), "v missing"),
    ] {
        fs::write(t.log(), format!("{good}{bad}{good}")).unwrap();
        for o in [verify(&t.log()), list(&t.log())] {
            assert_eq!(o.status.code(), Some(1));
            assert!(err(&o).contains("line 2") && err(&o).contains(want), "{}", err(&o));
        }
        assert_eq!(out(&list(&t.log())), good, "only the line before the bad one is printed");
    }
    fs::write(t.log(), format!("{good}{good}")).unwrap();
    assert!(ok(&verify(&t.log())), "twin: v=2 passes");
}

#[test]
fn ac4_a_v1_line_is_refused_naming_v_and_never_upgraded() {
    let t = Tmp::new();
    let v1 = "{\"v\":1,\"kind\":\"escalation\",\"ts_ms\":1,\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\"}\n";
    fs::write(t.log(), v1).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(1));
    assert!(err(&v).contains("line 1") && err(&v).contains("v"), "{}", err(&v));
}

#[test]
fn ac5_each_corruption_type_in_the_middle_is_refused_and_pins_list() {
    let t = Tmp::new();
    let g = line("replan_required", 1, "run-x");
    let gt = g.trim_end();
    let bads = [
        "{not json}".to_string(),
        gt.replace('}', ",\"extra\":\"x\"}"),
        gt.replace(",\"reason\":\"cap.hit\"", ""),
        gt.replace("\"run_id\":\"run-x\",", ""),
        gt.replace("\"ticket\":\"t-1\",", ""),
        gt.replace("\"repo\":\"my-repo\",", ""),
        gt.replace("\"kind\":\"replan_required\",", ""),
        gt.replace("\"ts_ms\":1,", ""),
        gt.replace("\"repo\":\"my-repo\",", "\"repo\":\"a\",\"repo\":\"b\","),
        gt.replace('}', ",\"model\":\"m-9\"}"),
        gt.replace(",\"role\":\"executor\"", ""),
        gt.replace(",\"model\":\"m-1\"", ""),
        gt.replace("\"role\":\"executor\"", "\"role\":\"\""),
        gt.replace("replan_required", "nope"),
        gt.replace("\"ts_ms\":1", "\"ts_ms\":\"1\""),
        gt.replace("\"role\":\"executor\"", "\"role\":\"Executor\""),
    ];
    for bad in bads {
        fs::write(t.log(), format!("{g}{g}{bad}\n{g}")).unwrap();
        let v = verify(&t.log());
        assert_eq!(v.status.code(), Some(1), "{bad}");
        assert!(err(&v).contains("line 3"), "{}", err(&v));
        let l = list(&t.log());
        assert_eq!(l.status.code(), Some(1));
        assert_eq!(out(&l), format!("{g}{g}"), "intact events before the bad line, nothing after");
    }
    // A missing identity key in the LAST line of the log is refused just the same, not only mid-log.
    fs::write(t.log(), format!("{g}{g}{}\n", gt.replace("\"ticket\":\"t-1\",", ""))).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(1), "a missing ticket in the last line is corrupt");
    assert!(err(&v).contains("line 3") && err(&v).contains("ticket missing"), "{}", err(&v));
    // Twin: three complete v2 lines, all four identity fields present, are read and listed whole.
    fs::write(t.log(), format!("{g}{g}{g}")).unwrap();
    assert!(ok(&verify(&t.log())), "complete lines verify clean");
    assert_eq!(out(&list(&t.log())).lines().count(), 3, "the complete log lists in full");
    fs::write(t.log(), format!("garbage\n{g}")).unwrap();
    let l = list(&t.log());
    assert_eq!((l.status.code(), out(&l).as_str()), (Some(1), ""), "a corrupt first line prints nothing");
}

#[test]
fn ac6_eight_child_processes_fifty_appends_each_leave_a_clean_401_line_log() {
    let t = Tmp::new();
    // One shared, already-issued run id: every child append inherits it and adds exactly one line.
    assert!(ok(&append(&t.log(), "escalation", "0")));
    let first = fs::read_to_string(t.log()).unwrap();
    let id = run_id_of(&first, 0);
    let handles: Vec<_> = (0..8)
        .map(|w| {
            let log = t.log();
            let id = id.clone();
            std::thread::spawn(move || {
                for i in 0..50 {
                    let a = [
                        "events",
                        "append",
                        "--log",
                        log.to_str().unwrap(),
                        "--kind",
                        "escalation",
                        "--ticket",
                        &format!("w{w}"),
                        "--repo",
                        "r",
                        "--reason",
                        "x",
                        "--role",
                        "executor",
                        "--model",
                        "m-1",
                        "--ts-ms",
                        &i.to_string(),
                    ];
                    let o = run_as(&a, &id);
                    assert!(ok(&o), "append refused: {}", err(&o));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    assert!(ok(&verify(&t.log())));
    let o = list(&t.log());
    assert!(ok(&o));
    let text = out(&o);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 402, "the issuing run (run_issued + escalation) plus 400 inherited events");
    for w in 0..8 {
        let mine = lines.iter().filter(|l| l.contains(&format!("\"ticket\":\"w{w}\""))).count();
        assert_eq!(mine, 50, "writer {w}");
    }
}

#[test]
fn ac6_concurrent_self_issuing_appends_each_add_their_own_two_lines() {
    let t = Tmp::new();
    let handles: Vec<_> = (0..8)
        .map(|w| {
            let log = t.log();
            std::thread::spawn(move || {
                for i in 0..5 {
                    let a = [
                        "events",
                        "append",
                        "--log",
                        log.to_str().unwrap(),
                        "--kind",
                        "escalation",
                        "--ticket",
                        &format!("w{w}"),
                        "--repo",
                        "r",
                        "--reason",
                        "x",
                        "--role",
                        "executor",
                        "--model",
                        "m-1",
                        "--ts-ms",
                        &i.to_string(),
                    ];
                    let o = run(&a, None);
                    assert!(ok(&o), "append refused: {}", err(&o));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
    let o = list(&t.log());
    assert!(ok(&o), "{}", err(&o));
    let text = out(&o);
    assert_eq!(text.lines().count(), 80, "40 appends, each with its own run_issued line first");
    let ids: Vec<String> = text.lines().filter(|l| l.contains("\"run_issued\"")).map(|l| run_id_of(l, 0)).collect();
    assert_eq!(ids.len(), 40);
    let mut uniq = ids.clone();
    uniq.sort();
    uniq.dedup();
    assert_eq!(uniq.len(), 40, "every self-issued id is distinct even under concurrency");
    for w in 0..8 {
        let mine = text.lines().filter(|l| l.contains(&format!("\"ticket\":\"w{w}\""))).count();
        assert_eq!(mine, 10, "writer {w}: 5 events plus 5 issue lines");
    }
}

#[test]
fn ac6_planted_a_two_write_shell_writer_is_caught_by_the_same_check() {
    let t = Tmp::new();
    let script = format!(
        "i=0; while [ $i -lt 50 ]; do {{ printf '%s' '{{\"v\":2,\"kind\":\"escalation\",\"ts_ms\":1,'; sleep 0.001; printf '%s\\n' '\"run_id\":\"run-x\",\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\",\"role\":\"e\",\"model\":\"m\"}}'; }} >> '{}'; i=$((i+1)); done",
        t.log().display()
    );
    let kids: Vec<_> = (0..8).map(|_| Command::new("sh").arg("-c").arg(&script).spawn().unwrap()).collect();
    for mut k in kids {
        assert!(k.wait().unwrap().success());
    }
    let v = verify(&t.log());
    let n = out(&list(&t.log())).lines().count();
    assert!(v.status.code() != Some(0) || n != 400, "the check must see the interleaving of a two-write writer");
}

#[test]
fn ac7_an_over_cap_event_is_refused_and_a_returned_append_is_visible_to_a_fresh_process() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "1")));
    let id = run_id_of(&fs::read_to_string(t.log()).unwrap(), 0);
    let before = fs::read(t.log()).unwrap();
    let big = "a".repeat(4096);
    let o = run(
        &[
            "events",
            "append",
            "--log",
            t.log().to_str().unwrap(),
            "--kind",
            "escalation",
            "--ticket",
            "t",
            "--repo",
            "r",
            "--reason",
            &big,
            "--role",
            "executor",
            "--model",
            "m-1",
        ],
        None,
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("cap") && err(&o).contains("EVENT NOT RECORDED"));
    assert_eq!(fs::read(t.log()).unwrap(), before);
    // The writing process is gone (exit 0 seen); a different process reads the events back.
    assert_eq!(out(&list(&t.log())), format!("{}{}", issue_line(1, &id), line("escalation", 1, &id)));
}

#[test]
fn ac8_two_processes_then_a_third_give_the_events_in_order() {
    let t = Tmp::new();
    let want = three(&t);
    assert_eq!(out(&list(&t.log())), want);
}

#[test]
fn ac8_planted_a_bad_first_line_is_refused_by_verify_and_list_and_a_good_one_passes() {
    let t = Tmp::new();
    fs::write(t.log(), format!("garbage\n{}", line("escalation", 1, "run-x"))).unwrap();
    assert_eq!(verify(&t.log()).status.code(), Some(1));
    let l = list(&t.log());
    assert_eq!((l.status.code(), out(&l).as_str()), (Some(1), ""));
    fs::write(t.log(), line("escalation", 1, "run-x")).unwrap();
    assert!(ok(&verify(&t.log())));
}

#[test]
fn ac9_bad_tokens_are_refused_and_create_no_file() {
    let t = Tmp::new();
    let log = t.log();
    for bad in ["", "Upper", "has space", "nl\n", "q\"uote"] {
        for field in ["--ticket", "--repo", "--reason", "--role", "--model"] {
            let mut a = vec![
                "events",
                "append",
                "--log",
                log.to_str().unwrap(),
                "--kind",
                "escalation",
                "--ticket",
                "t",
                "--repo",
                "r",
                "--reason",
                "x",
                "--role",
                "executor",
                "--model",
                "m-1",
            ];
            let i = a.iter().position(|x| *x == field).unwrap();
            a[i + 1] = bad;
            let o = run(&a, None);
            assert_eq!(o.status.code(), Some(2), "{field} {bad:?}");
            assert!(!t.log().exists());
        }
    }
    assert!(ok(&append(&t.log(), "escalation", "1")), "twin: valid tokens pass");
}

#[test]
fn ac9_a_directory_a_link_out_of_the_root_and_an_unwritable_log_are_refused() {
    let t = Tmp::new();
    let d = t.0.join("adir");
    fs::create_dir(&d).unwrap();
    assert_eq!(append(&d, "escalation", "1").status.code(), Some(2));
    let outside = Tmp::new();
    assert!(ok(&append(&outside.log(), "escalation", "1")), "the outside file exists and holds one run");
    let outside_before = fs::read(outside.log()).unwrap();
    symlink(outside.log(), t.log()).unwrap();
    let with_root = |log: &Path, root: &Path| {
        run(
            &[
                "events",
                "append",
                "--log",
                log.to_str().unwrap(),
                "--root",
                root.to_str().unwrap(),
                "--kind",
                "escalation",
                "--ticket",
                "t",
                "--repo",
                "r",
                "--reason",
                "x",
                "--role",
                "executor",
                "--model",
                "m-1",
                "--ts-ms",
                "2",
            ],
            None,
        )
    };
    let o = with_root(&t.log(), &t.0);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("EVENT NOT RECORDED"));
    assert_eq!(fs::read(outside.log()).unwrap(), outside_before, "nothing was written through the link");
    // With no --root the root is the log's parent folder, so the same link is refused (a root forced to `/` would accept it).
    let o = append(&t.log(), "escalation", "3");
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert_eq!(fs::read(outside.log()).unwrap(), outside_before, "no --root: still nothing written through the link");
    // Twin: the same link with a --root that contains the target is accepted.
    assert!(ok(&with_root(&t.log(), &outside.0)));
    assert_eq!(out(&list(&outside.log())).lines().count(), 4, "the earlier run (2 lines) plus the inherited one (2 lines)");
    // --root is honoured on its own: a real log is refused when it lies outside the given root.
    let elsewhere = Tmp::new();
    let real = elsewhere.log();
    let before = fs::read(&real).unwrap_or_default();
    let o = with_root(&real, &t.0);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert_eq!(fs::read(&real).unwrap_or_default(), before);
    assert!(ok(&with_root(&real, &elsewhere.0)), "twin: the root that contains it accepts");
    let u = Tmp::new();
    assert!(ok(&append(&u.log(), "escalation", "1")));
    fs::set_permissions(u.log(), fs::Permissions::from_mode(0o400)).unwrap();
    let still_writable = fs::OpenOptions::new().append(true).open(u.log()).is_ok();
    let o = append(&u.log(), "escalation", "2");
    fs::set_permissions(u.log(), fs::Permissions::from_mode(0o600)).unwrap();
    if !still_writable {
        assert_eq!(o.status.code(), Some(2));
        assert_eq!(out(&list(&u.log())).lines().count(), 2);
    }
}

#[test]
fn ac10_the_default_log_is_git_ignored_and_works_from_the_working_folder() {
    let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let g = Command::new("git").args(["check-ignore", "-q", ".harness/events.jsonl"]).current_dir(&top).stdin(Stdio::null()).status().unwrap();
    assert!(g.success(), "the default log path must be git-ignored");
    let t = Tmp::new();
    let a = [
        "events",
        "append",
        "--kind",
        "escalation",
        "--ticket",
        "t",
        "--repo",
        "r",
        "--reason",
        "x",
        "--role",
        "executor",
        "--model",
        "m-1",
        "--ts-ms",
        "5",
    ];
    assert!(ok(&run(&a, Some(&t.0))));
    assert!(t.0.join(".harness/events.jsonl").is_file());
    let l = run(&["events", "list"], Some(&t.0));
    let text = out(&l);
    let id = run_id_of(&text, 0);
    let issued = format!("{{\"v\":2,\"kind\":\"run_issued\",\"ts_ms\":5,\"run_id\":\"{id}\",\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"self-issue\",\"role\":\"executor\",\"model\":\"m-1\"}}\n");
    let escalated = format!("{{\"v\":2,\"kind\":\"escalation\",\"ts_ms\":5,\"run_id\":\"{id}\",\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\",\"role\":\"executor\",\"model\":\"m-1\"}}\n");
    let want = format!("{issued}{escalated}");
    assert_eq!(text, want);
    assert!(ok(&run(&["events", "verify"], Some(&t.0))));
}

#[test]
fn ac13_ts_ms_is_written_exactly_and_a_bad_one_is_refused_with_the_file_unchanged() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "123456789012312")));
    assert!(out(&list(&t.log())).contains("\"ts_ms\":123456789012312,"));
    let id = run_id_of(&fs::read_to_string(t.log()).unwrap(), 0);
    let before = fs::read(t.log()).unwrap();
    for bad in ["-1", "abc", "1.5", "", "+5", "99999999999999999999999"] {
        let o = append_as(&t.log(), "escalation", bad, &id);
        assert_eq!(o.status.code(), Some(2), "{bad:?}");
        assert_eq!(fs::read(t.log()).unwrap(), before);
    }
    // Without the flag the wall clock is written: a positive integer.
    let t2 = Tmp::new();
    let t2log = t2.log();
    let a = [
        "events",
        "append",
        "--log",
        t2log.to_str().unwrap(),
        "--kind",
        "escalation",
        "--ticket",
        "t",
        "--repo",
        "r",
        "--reason",
        "x",
        "--role",
        "executor",
        "--model",
        "m-1",
    ];
    let o = run(&a, None);
    assert!(ok(&o));
    let s = out(&list(&t2.log()));
    for l in s.lines() {
        let ts: u64 = l.split("\"ts_ms\":").nth(1).unwrap().split(',').next().unwrap().parse().unwrap();
        assert!(ts > 0, "both the issue line and the event carry the clock");
    }
}

#[test]
fn append_option_errors_say_the_event_was_not_recorded_and_write_nothing() {
    let t = Tmp::new();
    let lp = t.log();
    let lp = lp.to_str().unwrap();
    let base = [
        "events",
        "append",
        "--log",
        lp,
        "--kind",
        "escalation",
        "--ticket",
        "t",
        "--repo",
        "r",
        "--reason",
        "x",
        "--role",
        "executor",
        "--model",
        "m-1",
    ];
    let with = |extra: &[&str]| -> Vec<String> { base.iter().chain(extra.iter()).map(|s| s.to_string()).collect() };
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("unknown flag", with(&["--bogus", "1"])),
        ("flag twice", with(&["--kind", "escalation"])),
        ("flag with no value", with(&["--ts-ms"])),
    ];
    for (name, a) in cases {
        let refs: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        let o = run(&refs, None);
        assert_eq!(o.status.code(), Some(2), "{name}");
        assert!(err(&o).contains("EVENT NOT RECORDED"), "{name}: {}", err(&o));
        assert_eq!(out(&o), "", "{name}");
        assert!(!t.log().exists(), "{name}: nothing written");
    }
    assert!(ok(&run(&base, None)), "twin: the clean command passes");
}

#[test]
fn a_refused_default_log_append_leaves_no_empty_folder_behind() {
    let t = Tmp::new();
    let other = Tmp::new();
    let a = [
        "events",
        "append",
        "--root",
        other.0.to_str().unwrap(),
        "--kind",
        "escalation",
        "--ticket",
        "t",
        "--repo",
        "r",
        "--reason",
        "x",
        "--role",
        "executor",
        "--model",
        "m-1",
    ];
    let o = run(&a, Some(&t.0));
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("EVENT NOT RECORDED"));
    assert!(!t.0.join(".harness").exists(), "the refusal came before any folder was made");
    // Twin: a root that contains the working folder is accepted and creates the folder.
    let a = [
        "events",
        "append",
        "--root",
        t.0.to_str().unwrap(),
        "--kind",
        "escalation",
        "--ticket",
        "t",
        "--repo",
        "r",
        "--reason",
        "x",
        "--role",
        "executor",
        "--model",
        "m-1",
    ];
    assert!(ok(&run(&a, Some(&t.0))));
    assert!(t.0.join(".harness/events.jsonl").is_file());
}

#[test]
fn option_misuse_is_a_usage_error_with_nothing_on_stdout() {
    let t = Tmp::new();
    let l = t.log();
    let lp = l.to_str().unwrap();
    for a in [
        vec!["events"],
        vec!["events", "nope"],
        vec!["events", "list", "--log"],
        vec!["events", "list", "--log", lp, "--log", lp],
        vec!["events", "list", "--bogus", "x"],
        vec!["events", "append", "--log", lp, "--kind", "escalation"],
    ] {
        let o = run(&a, None);
        assert_eq!(o.status.code(), Some(2), "{a:?}");
        assert_eq!(out(&o), "");
    }
    let o = list(&t.log());
    assert_eq!((o.status.code(), out(&o).as_str()), (Some(2), ""), "a missing log is an error, not an empty log");
}

// ---------------------------------------------------------------- run identity (PRE-B)

#[test]
fn a_missing_required_flag_is_refused_and_nothing_is_written() {
    let t = Tmp::new();
    let lp = t.log().to_str().unwrap().to_string();
    let full = |role: bool, model: bool, ticket: bool| -> Vec<String> {
        let mut a: Vec<String> = vec!["events".into(), "append".into(), "--log".into(), lp.clone(), "--kind".into(), "escalation".into(), "--repo".into(), "r".into(), "--reason".into(), "cap.hit".into()];
        if ticket {
            a.extend(["--ticket".into(), "t-1".into()]);
        }
        if role {
            a.extend(["--role".into(), "executor".into()]);
        }
        if model {
            a.extend(["--model".into(), "m-1".into()]);
        }
        a
    };
    let cases = [
        ("neither role nor model", full(false, false, true)),
        ("role only", full(true, false, true)),
        ("model only", full(false, true, true)),
        ("no ticket", full(true, true, false)),
    ];
    for (name, a) in cases {
        let refs: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        let o = run(&refs, None);
        assert_eq!(o.status.code(), Some(2), "{name}");
        assert!(err(&o).contains("EVENT NOT RECORDED"), "{name}: {}", err(&o));
        assert!(!t.log().exists(), "{name}: no file, no line");
    }
    // An empty --ticket value is as bad as a missing one: refused, nothing written.
    let mut empty = full(true, true, true);
    let i = empty.iter().position(|s| s == "t-1").unwrap();
    empty[i] = String::new();
    let refs: Vec<&str> = empty.iter().map(|s| s.as_str()).collect();
    let o = run(&refs, None);
    assert_eq!(o.status.code(), Some(2), "empty ticket");
    assert!(err(&o).contains("EVENT NOT RECORDED"), "empty ticket: {}", err(&o));
    assert!(!t.log().exists(), "empty ticket: no file, no line");
    // Every other required flag refuses on its own too: kind, repo, reason.
    for (name, flag, val) in [("kind", "--kind", "escalation"), ("repo", "--repo", "r"), ("reason", "--reason", "cap.hit")] {
        let a: Vec<String> = full(true, true, true).into_iter().filter(|s| s != flag && s != val).collect();
        let refs: Vec<&str> = a.iter().map(|s| s.as_str()).collect();
        let o = run(&refs, None);
        assert_eq!(o.status.code(), Some(2), "no {name}");
        assert!(err(&o).contains("EVENT NOT RECORDED"), "no {name}: {}", err(&o));
        assert!(!t.log().exists(), "no {name}: no file, no line");
    }
    // Twin: ticket, role and model all given — the append is accepted and records exactly one event.
    let both = full(true, true, true);
    let refs: Vec<&str> = both.iter().map(|s| s.as_str()).collect();
    assert!(ok(&run(&refs, None)), "twin: the clean command passes");
    let text = fs::read_to_string(t.log()).unwrap();
    assert_eq!(text.lines().count(), 2, "run_issued plus exactly one escalation line");
}

#[test]
fn role_and_model_may_come_from_the_environment() {
    let t = Tmp::new();
    let logp = t.log();
    let a = ["events", "append", "--log", logp.to_str().unwrap(), "--kind", "escalation", "--ticket", "t-1", "--repo", "r", "--reason", "cap.hit", "--ts-ms", "7"];
    let o = run_env_identity(&a, "executor", "m-1");
    assert!(ok(&o), "{}", err(&o));
    let text = fs::read_to_string(t.log()).unwrap();
    assert!(text.lines().all(|l| l.contains("\"role\":\"executor\"") && l.contains("\"model\":\"m-1\"")));
    // A bad env role is refused exactly like a bad flag.
    let o = run_env_identity(&a, "Executor", "m-1");
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("EVENT NOT RECORDED"));
}

#[test]
fn a_complete_event_is_accepted_and_self_issues() {
    let t = Tmp::new();
    let o = append(&t.log(), "escalation", "9");
    assert!(ok(&o), "{}", err(&o));
    let text = fs::read_to_string(t.log()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "exactly two events");
    assert!(lines[0].contains("\"kind\":\"run_issued\""), "the first line issues the run: {}", lines[0]);
    assert!(lines[1].contains("\"kind\":\"escalation\""));
    let (id0, id1) = (run_id_of(lines[0], 0), run_id_of(lines[1], 0));
    assert_eq!(id0, id1, "both lines carry the same run_id");
    for l in lines {
        assert!(l.contains("\"ticket\":\"t-1\"") && l.contains("\"role\":\"executor\"") && l.contains("\"model\":\"m-1\""), "{l}");
    }
}

#[test]
fn ids_are_unique_tokens_even_back_to_back() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "1")));
    assert!(ok(&append(&t.log(), "escalation", "1")));
    let text = fs::read_to_string(t.log()).unwrap();
    let ids: Vec<String> = text.lines().filter(|l| l.contains("\"run_issued\"")).map(|l| run_id_of(l, 0)).collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1], "two appends in the same millisecond differ");
    for id in &ids {
        assert!(!id.is_empty() && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')), "{id} matches the token rule");
    }
}

#[test]
fn an_inherited_issued_id_adds_exactly_one_line() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "1")));
    let id = run_id_of(&fs::read_to_string(t.log()).unwrap(), 0);
    let before = fs::read_to_string(t.log()).unwrap();
    assert!(ok(&append_as(&t.log(), "escalation", "2", &id)));
    let after = fs::read_to_string(t.log()).unwrap();
    assert_eq!(after.lines().count(), before.lines().count() + 1, "exactly one new line, no new run_issued");
    let new_line = after.lines().last().unwrap();
    assert!(new_line.contains("\"kind\":\"escalation\"") && new_line.contains(&format!("\"run_id\":\"{id}\"")), "{new_line}");
}

#[test]
fn an_unissued_or_malformed_inherited_id_is_refused_and_writes_nothing() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "1")));
    let before = fs::read(t.log()).unwrap();
    for bad in ["never-issued", "Bad_ID"] {
        let o = append_as(&t.log(), "escalation", "2", bad);
        assert_eq!(o.status.code(), Some(2), "{bad}");
        assert!(err(&o).contains("EVENT NOT RECORDED"), "{bad}: {}", err(&o));
        assert!(err(&o).contains("HARNESS_RUN_ID"), "{bad}: the error names the source: {}", err(&o));
        assert_eq!(fs::read(t.log()).unwrap(), before, "{bad}: the log is unchanged");
    }
}

#[test]
fn read_only_commands_write_nothing_and_create_no_file() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "1")));
    let before = fs::read(t.log()).unwrap();
    let files_before = fs::read_dir(&t.0).unwrap().count();
    assert!(ok(&list(&t.log())));
    assert!(ok(&verify(&t.log())));
    assert_eq!(fs::read(t.log()).unwrap(), before, "byte size unchanged");
    assert_eq!(fs::read_dir(&t.0).unwrap().count(), files_before, "no new file appeared");
}

// ---------------------------------------------------------------- #211: the read cap (a giant, linked or non-regular log is refused BEFORE it is read)

/// A sparse file of exactly `len` bytes: `head` at the start, `last` as the final byte, zeros in between.
/// Sparse on APFS/ext4, so a 64 MiB fixture costs a few blocks.
fn sparse(path: &Path, len: u64, head: &[u8], last: u8) {
    let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).open(path).unwrap();
    f.write_all(head).unwrap();
    f.seek(SeekFrom::Start(len - 1)).unwrap();
    f.write_all(&[last]).unwrap();
    f.set_len(len).unwrap();
}

#[test]
fn ac3_211_an_over_cap_log_is_refused_before_reading_no_log_bytes_leak() {
    let t = Tmp::new();
    let big = t.0.join("big.jsonl");
    sparse(&big, LOG_READ_CAP + 1, b"MARKER-211-SECRET\n", 0);
    let o = verify(&big);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(out(&o), "");
    let e = err(&o);
    assert_eq!(e.lines().count(), 1, "exactly one stderr line: {e}");
    assert!(e.contains("read cap"), "{e}");
    assert!(e.contains(&LOG_READ_CAP.to_string()), "the cap value itself is named: {e}");
    assert!(!e.contains("MARKER-211-SECRET"), "no log byte is ever echoed: {e}");
}

#[test]
fn ac4_211_a_log_exactly_at_the_cap_is_read_not_refused() {
    let t = Tmp::new();
    let big = t.0.join("cap.jsonl");
    sparse(&big, LOG_READ_CAP, b"", 0);
    let o = verify(&big);
    assert_eq!(o.status.code(), Some(1), "all-zero bytes with no newline: a torn tail, not a cap refusal: {}", err(&o));
    assert!(err(&o).contains("torn tail"), "{}", err(&o));
    assert!(!err(&o).contains("read cap"), "{}", err(&o));
}

#[test]
fn ac5_211_a_fifo_is_refused_without_opening_it_the_child_never_hangs() {
    let t = Tmp::new();
    let fifo = t.0.join("log.fifo");
    let mk = Command::new("mkfifo").arg(&fifo).output().unwrap();
    assert!(mk.status.success(), "mkfifo must exist on unix: {}", String::from_utf8_lossy(&mk.stderr));
    let mut c = Command::new(BIN);
    c.args(["events", "verify", "--log", fifo.to_str().unwrap()]).stdin(Stdio::null())
        .stdout(Stdio::piped()).stderr(Stdio::piped());
    for k in ["HARNESS_RUN_ID", "HARNESS_ROLE", "HARNESS_MODEL"] {
        c.env_remove(k);
    }
    let mut child = c.spawn().unwrap();
    // A hang guard, not a timing assert: nothing here asserts how fast the child is, only that it comes
    // back at all. Without the pre-open check the open of a FIFO blocks until a writer appears.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut killed = false;
    let status = loop {
        match child.try_wait().unwrap() {
            Some(s) => break s,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                killed = true;
                break child.wait().unwrap();
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    };
    assert!(!killed, "harness events verify blocked on a FIFO: the log must be refused before it is opened");
    let o = child.wait_with_output().unwrap();
    assert_eq!(status.code(), o.status.code());
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&o.stdout), "");
    assert!(String::from_utf8_lossy(&o.stderr).contains("not a regular file"), "{}", String::from_utf8_lossy(&o.stderr));
}

#[test]
fn ac6_211_a_symlinked_log_is_refused_on_read_but_the_target_reads() {
    let t = Tmp::new();
    let real = t.0.join("events.jsonl");
    fs::write(&real, line("escalation", 1, "run-x")).unwrap();
    let link = t.0.join("link.jsonl");
    symlink(&real, &link).unwrap();
    let o = list(&link);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("symbolic link"), "{}", err(&o));
    // Twin: the very same file given directly reads clean and lists its one event.
    let d = list(&real);
    assert_eq!(d.status.code(), Some(0));
    assert_eq!(out(&d).lines().count(), 1);
}

#[test]
fn ac7_211_a_directory_is_refused_as_a_log() {
    let t = Tmp::new();
    let o = verify(&t.0);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(out(&o), "");
    assert!(err(&o).contains("not a regular file"), "{}", err(&o));
}

#[test]
fn ac8_211_an_inherited_append_onto_an_over_cap_log_is_refused_and_writes_nothing() {
    let t = Tmp::new();
    let big = t.0.join("big.jsonl");
    // The final byte is a newline so the append path passes its torn-tail check and reaches the inherited read.
    sparse(&big, LOG_READ_CAP + 1, b"", b'\n');
    let o = append_as(&big, "escalation", "1", "run-1");
    assert_eq!(o.status.code(), Some(2));
    let e = err(&o);
    assert!(e.contains("read cap"), "{e}");
    assert!(e.contains("EVENT NOT RECORDED"), "{e}");
    assert_eq!(fs::metadata(&big).unwrap().len(), LOG_READ_CAP + 1, "the over-cap log is left byte-for-byte unchanged");
}

// ---------------------------------------------------------------- #213: the writer-side cap (an append that would take the log past the read cap is refused)

/// The exact two-line stderr of a writer-side cap refusal, built from the constant (never a hand-typed number).
fn over_cap_stderr() -> String {
    format!(
        "harness: error: log refused: the append would take the log over the {}-byte read cap; nothing was appended\nharness: EVENT NOT RECORDED\n",
        LOG_READ_CAP
    )
}

/// The shared shape of every #213 CLI refusal: exit 2, empty stdout, exactly the two refusal lines on stderr,
/// no log byte echoed, and the log left byte-for-byte as the fixture made it (full content compare).
fn assert_over_cap_refusal(o: &Output, big: &Path, before: &[u8]) {
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(out(o), "");
    assert_eq!(err(o), over_cap_stderr(), "exactly the two refusal lines, nothing else");
    assert!(!err(o).contains("MARKER-213-SECRET"), "no log byte is ever echoed: {}", err(o));
    assert_eq!(fs::read(big).unwrap(), before, "the log is byte-for-byte unchanged");
}

#[test]
fn ac2_213_an_append_onto_an_at_cap_log_is_refused_and_writes_nothing() {
    let t = Tmp::new();
    let big = t.0.join("at.jsonl");
    // A log of exactly LOG_READ_CAP bytes, ending in a newline so the append reaches the size check: any append,
    // even one byte, would take it past the cap the reader enforces.
    sparse(&big, LOG_READ_CAP, b"MARKER-213-SECRET\n", b'\n');
    let before = fs::read(&big).unwrap();
    let o = append(&big, "escalation", "1");
    assert_over_cap_refusal(&o, &big, &before);
}

#[test]
fn ac3_213_an_append_one_byte_under_the_cap_is_refused_the_payload_completes_the_crossing() {
    let t = Tmp::new();
    let big = t.0.join("under.jsonl");
    // One byte under the cap is NOT room: the payload itself would carry the log past the cap, so it is refused.
    sparse(&big, LOG_READ_CAP - 1, b"MARKER-213-SECRET\n", b'\n');
    let before = fs::read(&big).unwrap();
    let o = append(&big, "escalation", "1");
    assert_over_cap_refusal(&o, &big, &before);
}

#[test]
fn ac4_213_a_self_issued_append_onto_an_over_cap_log_is_refused_and_writes_nothing() {
    let t = Tmp::new();
    let big = t.0.join("over.jsonl");
    // Already over the cap and no HARNESS_RUN_ID: a self-issue never reads the log, so only the writer-side
    // check stands between this append and an unreadable log.
    sparse(&big, LOG_READ_CAP + 1, b"MARKER-213-SECRET\n", b'\n');
    let before = fs::read(&big).unwrap();
    let o = append(&big, "escalation", "1");
    assert_over_cap_refusal(&o, &big, &before);
}
