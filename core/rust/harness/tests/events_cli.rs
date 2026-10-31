//! Process-level tests for `harness events append | list | verify` (PRE-6b). Every check runs the real binary as a child process.
//! `recover` is not built: the plan holds it until the torn-tail recovery question is signed, so the torn-tail arm here is the
//! "stays refused" branch.
#![cfg(unix)]
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
    if let Some(d) = cwd {
        c.current_dir(d);
    }
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
    run(&["events", "append", "--log", log.to_str().unwrap(), "--kind", kind, "--ticket", "t-1", "--repo", "my-repo", "--reason", "cap.hit", "--ts-ms", ts], None)
}

fn verify(log: &Path) -> Output {
    run(&["events", "verify", "--log", log.to_str().unwrap()], None)
}

fn list(log: &Path) -> Output {
    run(&["events", "list", "--log", log.to_str().unwrap()], None)
}

fn line(kind: &str, ts: u64) -> String {
    format!("{{\"v\":1,\"kind\":\"{kind}\",\"ts_ms\":{ts},\"ticket\":\"t-1\",\"repo\":\"my-repo\",\"reason\":\"cap.hit\"}}\n")
}

const KINDS: [&str; 3] = ["escalation", "replan_required", "budget_exceeded"];

fn three(t: &Tmp) -> String {
    for (i, k) in KINDS.iter().enumerate() {
        assert!(ok(&append(&t.log(), k, &(i + 1).to_string())));
    }
    KINDS.iter().enumerate().map(|(i, k)| line(k, i as u64 + 1)).collect()
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
fn ac1_planted_a_fourth_kind_is_refused_and_the_log_is_unchanged() {
    let t = Tmp::new();
    three(&t);
    let before = fs::read(t.log()).unwrap();
    let o = append(&t.log(), "surprise", "9");
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("EVENT NOT RECORDED"));
    assert_eq!(fs::read(t.log()).unwrap(), before);
}

#[test]
fn ac2_torn_tail_verify_names_it_and_list_prints_exactly_two() {
    let t = Tmp::new();
    let want = three(&t);
    let cut = want.len() - 10;
    fs::write(t.log(), &want.as_bytes()[..cut]).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(1));
    assert!(err(&v).contains("torn tail") && err(&v).contains("line 3"), "{}", err(&v));
    assert_eq!(out(&v), "");
    let l = list(&t.log());
    assert_eq!(l.status.code(), Some(1));
    assert_eq!(out(&l), want.lines().take(2).map(|s| format!("{s}\n")).collect::<String>());
}

#[test]
fn ac2_planted_a_clean_boundary_is_not_torn_and_a_valid_json_fragment_still_is() {
    let t = Tmp::new();
    let want = three(&t);
    let two: String = want.lines().take(2).map(|s| format!("{s}\n")).collect();
    fs::write(t.log(), &two).unwrap();
    assert!(ok(&verify(&t.log())), "a cut exactly at a line boundary is clean");
    fs::write(t.log(), format!("{two}{}", want.lines().nth(2).unwrap())).unwrap();
    let v = verify(&t.log());
    assert_eq!(v.status.code(), Some(1), "a complete-looking last line with no newline is torn");
    assert!(err(&v).contains("torn tail"));
    assert_eq!(out(&list(&t.log())).lines().count(), 2);
}

#[test]
fn ac3_append_onto_a_torn_tail_is_refused_every_time_and_the_bytes_are_unchanged() {
    let t = Tmp::new();
    let want = three(&t);
    let torn = &want.as_bytes()[..want.len() - 10];
    fs::write(t.log(), torn).unwrap();
    for ts in ["10", "11"] {
        let o = append(&t.log(), "escalation", ts);
        assert_eq!(o.status.code(), Some(2));
        assert!(err(&o).contains("torn tail") && err(&o).contains("EVENT NOT RECORDED"), "{}", err(&o));
        assert_eq!(fs::read(t.log()).unwrap(), torn, "no self-heal: the file is never touched");
    }
    // `recover` does not exist yet.
    assert_eq!(run(&["events", "recover", "--log", t.log().to_str().unwrap()], None).status.code(), Some(2));
    // Twin: the same file with its newline restored accepts the append.
    fs::write(t.log(), &want).unwrap();
    assert!(ok(&append(&t.log(), "escalation", "10")));
    assert_eq!(out(&list(&t.log())).lines().count(), 4);
}

#[test]
fn ac4_unknown_or_missing_v_is_refused_by_verify_and_list_naming_line_and_v() {
    let t = Tmp::new();
    let good = line("escalation", 1);
    for (bad, want) in [
        (good.replace("\"v\":1", "\"v\":2"), "unsupported v 2"),
        (good.replace("\"v\":1", "\"v\":0"), "unsupported v 0"),
        (good.replace("\"v\":1,", ""), "v missing"),
    ] {
        fs::write(t.log(), format!("{good}{bad}{good}")).unwrap();
        for o in [verify(&t.log()), list(&t.log())] {
            assert_eq!(o.status.code(), Some(1));
            assert!(err(&o).contains("line 2") && err(&o).contains(want), "{}", err(&o));
        }
        assert_eq!(out(&list(&t.log())), good, "only the line before the bad one is printed");
    }
    fs::write(t.log(), format!("{good}{good}")).unwrap();
    assert!(ok(&verify(&t.log())), "twin: v=1 passes");
}

#[test]
fn ac5_each_corruption_type_in_the_middle_is_refused_and_pins_list() {
    let t = Tmp::new();
    let g = line("replan_required", 1);
    let gt = g.trim_end();
    let bads = [
        "{not json}".to_string(),
        gt.replace('}', ",\"extra\":\"x\"}"),
        gt.replace(",\"reason\":\"cap.hit\"", ""),
        gt.replace("\"repo\":\"my-repo\",", "\"repo\":\"a\",\"repo\":\"b\","),
        gt.replace("replan_required", "nope"),
        gt.replace("\"ts_ms\":1", "\"ts_ms\":\"1\""),
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
    fs::write(t.log(), format!("garbage\n{g}")).unwrap();
    let l = list(&t.log());
    assert_eq!((l.status.code(), out(&l).as_str()), (Some(1), ""), "a corrupt first line prints nothing");
}

#[test]
fn ac6_eight_child_processes_fifty_appends_each_leave_a_clean_400_line_log() {
    let t = Tmp::new();
    let handles: Vec<_> = (0..8)
        .map(|w| {
            let log = t.log();
            std::thread::spawn(move || {
                for i in 0..50 {
                    let o = run(
                        &["events", "append", "--log", log.to_str().unwrap(), "--kind", "escalation", "--ticket", &format!("w{w}"), "--repo", "r", "--reason", "x", "--ts-ms", &i.to_string()],
                        None,
                    );
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
    assert_eq!(lines.len(), 400);
    for w in 0..8 {
        let mine = lines.iter().filter(|l| l.contains(&format!("\"ticket\":\"w{w}\""))).count();
        assert_eq!(mine, 50, "writer {w}");
    }
}

#[test]
fn ac6_planted_a_two_write_shell_writer_is_caught_by_the_same_check() {
    let t = Tmp::new();
    let script = format!(
        "i=0; while [ $i -lt 50 ]; do {{ printf '%s' '{{\"v\":1,\"kind\":\"escalation\",\"ts_ms\":1,'; sleep 0.001; printf '%s\\n' '\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\"}}'; }} >> '{}'; i=$((i+1)); done",
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
    let before = fs::read(t.log()).unwrap();
    let big = "a".repeat(4096);
    let o = run(&["events", "append", "--log", t.log().to_str().unwrap(), "--kind", "escalation", "--ticket", "t", "--repo", "r", "--reason", &big], None);
    assert_eq!(o.status.code(), Some(2));
    assert!(err(&o).contains("cap") && err(&o).contains("EVENT NOT RECORDED"));
    assert_eq!(fs::read(t.log()).unwrap(), before);
    // The writing process is gone (exit 0 seen); a different process reads the event back.
    assert_eq!(out(&list(&t.log())), line("escalation", 1));
}

#[test]
fn ac8_two_processes_then_a_third_give_three_events_in_order() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "1")));
    assert!(ok(&append(&t.log(), "replan_required", "2")));
    assert!(ok(&append(&t.log(), "budget_exceeded", "3")));
    assert_eq!(out(&list(&t.log())), format!("{}{}{}", line("escalation", 1), line("replan_required", 2), line("budget_exceeded", 3)));
}

#[test]
fn ac8_planted_a_bad_first_line_is_refused_by_verify_and_list_and_a_good_one_passes() {
    let t = Tmp::new();
    fs::write(t.log(), format!("garbage\n{}", line("escalation", 1))).unwrap();
    assert_eq!(verify(&t.log()).status.code(), Some(1));
    let l = list(&t.log());
    assert_eq!((l.status.code(), out(&l).as_str()), (Some(1), ""));
    fs::write(t.log(), line("escalation", 1)).unwrap();
    assert!(ok(&verify(&t.log())));
}

#[test]
fn ac9_bad_tokens_are_refused_and_create_no_file() {
    let t = Tmp::new();
    let log = t.log();
    for bad in ["", "Upper", "has space", "nl\n", "q\"uote"] {
        for field in ["--ticket", "--repo", "--reason"] {
            let mut a = vec!["events", "append", "--log", log.to_str().unwrap(), "--kind", "escalation", "--ticket", "t", "--repo", "r", "--reason", "x"];
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
    symlink(outside.log(), t.log()).unwrap();
    let o = run(&["events", "append", "--log", t.log().to_str().unwrap(), "--root", t.0.to_str().unwrap(), "--kind", "escalation", "--ticket", "t", "--repo", "r", "--reason", "x"], None);
    assert_eq!(o.status.code(), Some(2));
    assert!(!outside.log().exists(), "nothing was written through the link");
    let u = Tmp::new();
    assert!(ok(&append(&u.log(), "escalation", "1")));
    fs::set_permissions(u.log(), fs::Permissions::from_mode(0o400)).unwrap();
    let still_writable = fs::OpenOptions::new().append(true).open(u.log()).is_ok();
    let o = append(&u.log(), "escalation", "2");
    fs::set_permissions(u.log(), fs::Permissions::from_mode(0o600)).unwrap();
    if !still_writable {
        assert_eq!(o.status.code(), Some(2));
        assert_eq!(out(&list(&u.log())).lines().count(), 1);
    }
}

#[test]
fn ac10_the_default_log_is_git_ignored_and_works_from_the_working_folder() {
    let top = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let g = Command::new("git").args(["check-ignore", "-q", ".harness/events.jsonl"]).current_dir(&top).stdin(Stdio::null()).status().unwrap();
    assert!(g.success(), "the default log path must be git-ignored");
    let t = Tmp::new();
    let a = ["events", "append", "--kind", "escalation", "--ticket", "t", "--repo", "r", "--reason", "x", "--ts-ms", "5"];
    assert!(ok(&run(&a, Some(&t.0))));
    assert!(t.0.join(".harness/events.jsonl").is_file());
    let l = run(&["events", "list"], Some(&t.0));
    assert_eq!(out(&l), "{\"v\":1,\"kind\":\"escalation\",\"ts_ms\":5,\"ticket\":\"t\",\"repo\":\"r\",\"reason\":\"x\"}\n");
    assert!(ok(&run(&["events", "verify"], Some(&t.0))));
}

#[test]
fn ac13_ts_ms_is_written_exactly_and_a_bad_one_is_refused_with_the_file_unchanged() {
    let t = Tmp::new();
    assert!(ok(&append(&t.log(), "escalation", "1234567890123")));
    assert!(out(&list(&t.log())).contains("\"ts_ms\":1234567890123,"));
    let before = fs::read(t.log()).unwrap();
    for bad in ["-1", "abc", "1.5", "", "+5", "99999999999999999999999"] {
        let o = append(&t.log(), "escalation", bad);
        assert_eq!(o.status.code(), Some(2), "{bad:?}");
        assert_eq!(fs::read(t.log()).unwrap(), before);
    }
    // Without the flag the wall clock is written: a positive integer.
    let t2 = Tmp::new();
    let o = run(&["events", "append", "--log", t2.log().to_str().unwrap(), "--kind", "escalation", "--ticket", "t", "--repo", "r", "--reason", "x"], None);
    assert!(ok(&o));
    let s = out(&list(&t2.log()));
    let ts: u64 = s.split("\"ts_ms\":").nth(1).unwrap().split(',').next().unwrap().parse().unwrap();
    assert!(ts > 0);
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
