//! Process-level tests for `harness ticket review-fail` and `harness ticket fix-round-check`. Each test runs the real binary against scratch
//! folders under the system temp folder. The identity variables are removed on the child, so a value in the caller shell cannot change a result.
#![cfg(unix)]
use harness::caps::Caps;
use harness::events;
use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Fx {
    root: PathBuf,
    repo: PathBuf,
    caps: PathBuf,
    tf: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        let p = std::env::temp_dir().join(format!("hrrc-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        let root = p.canonicalize().unwrap();
        let (repo, caps) = (root.join("r"), root.join("caps"));
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&caps).unwrap();
        let tf = root.join("ticket.json");
        fs::write(&tf, ticket_json("r", "T-1")).unwrap();
        Fx { repo: repo.canonicalize().unwrap(), caps: caps.canonicalize().unwrap(), tf, root }
    }

    fn rec_file(&self) -> PathBuf {
        self.caps.join("T-1.reviews")
    }

    fn log(&self) -> PathBuf {
        self.caps.join("events.jsonl")
    }

    fn args(&self) -> Vec<String> {
        vec![
            self.tf.to_str().unwrap().into(),
            "--repo-map".into(),
            format!("r={}", self.repo.to_str().unwrap()),
            "--caps-dir".into(),
            self.caps.to_str().unwrap().into(),
        ]
    }

    fn fail(&self, head: &str) -> Output {
        let mut a = self.args();
        a.extend(["--head".to_string(), head.to_string()]);
        run("review-fail", &a)
    }

    fn check(&self) -> Output {
        self.check_as("e-rvw", "m-1")
    }

    fn check_as(&self, role: &str, model: &str) -> Output {
        let mut a = self.args();
        a.extend(["--role".to_string(), role.to_string(), "--model".to_string(), model.to_string()]);
        run("fix-round-check", &a)
    }

    fn fails(&self, n: usize) {
        for i in 0..n {
            let o = self.fail(&sha(i));
            assert_eq!(code(&o), 0, "{}", err(&o));
        }
    }

    fn escalations(&self) -> Vec<events::Record> {
        match events::read_log(&self.log()) {
            Ok(r) => r.events.into_iter().filter(|e| e.kind.as_str() == "escalation").collect(),
            Err(_) => Vec::new(),
        }
    }

    fn files(&self) -> Vec<String> {
        let mut v: Vec<String> = fs::read_dir(&self.caps).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn ticket_json(repo_id: &str, id: &str) -> String {
    format!(
        "{{\"id\":\"{id}\",\"repo_id\":\"{repo_id}\",\"title\":\"A title\",\"group\":\"grp\",\"size\":\"M\",\"objective\":\"An objective\",\"acceptance_criteria\":[\"one\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}"
    )
}

fn sha(i: usize) -> String {
    format!("{:040x}", i + 1)
}

fn record(ticket: &str, heads: &[String]) -> String {
    let h: Vec<String> = heads.iter().map(|x| format!("\"{x}\"")).collect();
    format!("{{\"v\":1,\"ticket\":\"{ticket}\",\"repo_id\":\"r\",\"fail_heads\":[{}]}}\n", h.join(","))
}

fn run(verb: &str, args: &[String]) -> Output {
    let mut c = Command::new(BIN);
    c.arg("ticket").arg(verb).args(args).current_dir(std::env::temp_dir()).stdin(Stdio::null());
    for k in ["HARNESS_ROLE", "HARNESS_MODEL", "HARNESS_RUN_ID"] {
        c.env_remove(k);
    }
    c.output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or_else(|| panic!("killed by a signal: {}", err(o)))
}

/// An error: exit 2, stdout empty.
fn errored(o: &Output) {
    assert_eq!(code(o), 2, "stderr: {}", err(o));
    assert!(out(o).is_empty(), "stdout must be empty: {}", out(o));
}

#[test]
fn three_fails_refuse_the_next_fix_round() {
    let fx = Fx::new();
    fx.fails(3);
    let o = fx.check();
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert_eq!(out(&o), "ticket=T-1 result=refused reason=review-round-cap fails=3 next=operator-waiting\n");
    let ev = fx.escalations();
    assert_eq!(ev.len(), 1, "one escalation event");
    let e = &ev[0];
    assert_eq!((e.ticket.as_str(), e.repo.as_str(), e.reason.as_str()), ("t-1", "r", "review-round-cap"));
    assert_eq!((e.role.as_str(), e.model.as_str()), ("e-rvw", "m-1"));
}

#[test]
fn two_fails_allow_a_fix_round() {
    let fx = Fx::new();
    fx.fails(2);
    let before = (fx.files(), fs::read(fx.rec_file()).unwrap());
    let o = fx.check();
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "ticket=T-1 result=allowed fails=2\n");
    assert!(!fx.log().exists(), "no event");
    assert_eq!((fx.files(), fs::read(fx.rec_file()).unwrap()), before, "nothing written");
}

#[test]
fn note_rounds_do_not_count() {
    let fx = Fx::new();
    fx.fails(2);
    for _ in 0..5 {
        let o = fx.check();
        assert_eq!(code(&o), 0, "{}", err(&o));
        assert_eq!(out(&o), "ticket=T-1 result=allowed fails=2\n");
    }
    assert!(fx.escalations().is_empty());
}

#[test]
fn the_same_head_twice_counts_once() {
    let fx = Fx::new();
    let first = fx.fail(&sha(0));
    assert_eq!(out(&first), format!("review-fail ticket=T-1 head={} fails=1 counted=yes\n", sha(0)));
    let bytes = fs::read(fx.rec_file()).unwrap();
    let again = fx.fail(&sha(0));
    assert_eq!(code(&again), 0, "{}", err(&again));
    assert_eq!(out(&again), format!("review-fail ticket=T-1 head={} fails=1 counted=no\n", sha(0)));
    assert_eq!(fs::read(fx.rec_file()).unwrap(), bytes, "nothing changed");
}

#[test]
fn a_refused_check_appends_one_event_per_call() {
    let fx = Fx::new();
    fx.fails(3);
    for n in 1..=3 {
        assert_eq!(code(&fx.check()), 1);
        assert_eq!(fx.escalations().len(), n);
    }
}

#[test]
fn a_refusal_records_the_flag_identity_without_role_env() {
    let fx = Fx::new();
    fx.fails(4);
    let o = fx.check_as("gov", "model-x.1");
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(out(&o).contains("fails=4"), "{}", out(&o));
    let ev = fx.escalations();
    assert_eq!(ev.len(), 1);
    assert_eq!((ev[0].role.as_str(), ev[0].model.as_str()), ("gov", "model-x.1"));
}

#[test]
fn a_missing_record_is_zero_fails() {
    let fx = Fx::new();
    let o = fx.check();
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(out(&o), "ticket=T-1 result=allowed fails=0\n");
    assert!(fx.files().iter().all(|n| n != "T-1.reviews"), "no record was created");
}

#[test]
fn a_copied_record_is_refused() {
    let fx = Fx::new();
    fs::write(fx.caps.join("other.tmp"), record("T-2", &[sha(0), sha(1), sha(2)])).unwrap();
    fs::rename(fx.caps.join("other.tmp"), fx.rec_file()).unwrap();
    errored(&fx.check());
    errored(&fx.fail(&sha(9)));
    assert!(fx.escalations().is_empty());
}

#[test]
fn a_reordered_record_is_refused() {
    let fx = Fx::new();
    fs::write(fx.rec_file(), format!("{{\"v\":1,\"repo_id\":\"r\",\"ticket\":\"T-1\",\"fail_heads\":[\"{}\"]}}\n", sha(0))).unwrap();
    errored(&fx.check());
    fs::write(fx.rec_file(), record("T-1", &[sha(0)]).replace(",\"", ", \"")).unwrap();
    errored(&fx.check());
    fs::write(fx.rec_file(), record("T-1", &[sha(0)]).replace('\n', "")).unwrap();
    errored(&fx.check());
}

#[test]
fn a_record_with_a_duplicate_head_is_refused() {
    let fx = Fx::new();
    fs::write(fx.rec_file(), record("T-1", &[sha(0), sha(0)])).unwrap();
    errored(&fx.check());
    fs::write(fx.rec_file(), record("T-1", &[sha(10).to_uppercase()])).unwrap();
    errored(&fx.check());
    fs::write(fx.rec_file(), record("T-1", &["abc123".to_string()])).unwrap();
    errored(&fx.check());
}

#[test]
fn an_out_of_range_record_is_refused() {
    let fx = Fx::new();
    let heads: Vec<String> = (0..17).map(sha).collect();
    fs::write(fx.rec_file(), record("T-1", &heads)).unwrap();
    errored(&fx.check());
    errored(&fx.fail(&sha(99)));
}

#[test]
fn a_linked_record_is_refused() {
    let fx = Fx::new();
    let real = fx.root.join("real.reviews");
    fs::write(&real, record("T-1", &[sha(0)])).unwrap();
    symlink(&real, fx.rec_file()).unwrap();
    errored(&fx.check());
    errored(&fx.fail(&sha(1)));
    assert_eq!(fs::read_to_string(&real).unwrap(), record("T-1", &[sha(0)]), "the link target is untouched");
}

#[test]
fn a_case_twin_record_name_is_refused() {
    let fx = Fx::new();
    fs::write(fx.caps.join("T-1.Reviews"), record("T-1", &[sha(0)])).unwrap();
    errored(&fx.check());
    errored(&fx.fail(&sha(1)));
}

#[test]
fn a_busy_lock_refuses_review_fail() {
    let fx = Fx::new();
    let caps = Caps::open(&fx.caps, &fx.repo).unwrap();
    let held = caps.lock("T-1").unwrap();
    let o = fx.fail(&sha(0));
    errored(&o);
    assert!(err(&o).contains("a run for this ticket is in progress"), "{}", err(&o));
    assert!(!fx.rec_file().exists());
    drop(held);
}

#[test]
fn a_seventeenth_head_is_refused_and_nothing_is_written() {
    let fx = Fx::new();
    fx.fails(16);
    let before = fs::read(fx.rec_file()).unwrap();
    errored(&fx.fail(&sha(16)));
    assert_eq!(fs::read(fx.rec_file()).unwrap(), before);
    let again = fx.fail(&sha(3));
    assert_eq!(out(&again), format!("review-fail ticket=T-1 head={} fails=16 counted=no\n", sha(3)));
}

#[test]
fn an_unwritable_event_log_exits_2_escalation_not_recorded() {
    let fx = Fx::new();
    fx.fails(3);
    fs::create_dir(fx.log()).unwrap();
    let o = fx.check();
    errored(&o);
    assert!(err(&o).contains("ESCALATION NOT RECORDED"), "{}", err(&o));
    assert!(!out(&o).contains("refused"));
}

#[test]
fn a_ticket_whose_repo_is_not_in_the_map_is_refused() {
    let fx = Fx::new();
    fs::write(&fx.tf, ticket_json("zz", "T-1")).unwrap();
    assert_eq!(code(&fx.check()), 1);
    assert_eq!(code(&fx.fail(&sha(0))), 1);
}

#[test]
fn usage_faults_exit_2() {
    let fx = Fx::new();
    let with = |extra: &[&str]| -> Vec<String> {
        let mut a = fx.args();
        a.extend(extra.iter().map(|s| s.to_string()));
        a
    };
    let h = sha(10);
    let inside = fx.repo.join("caps");
    fs::create_dir_all(&inside).unwrap();
    let cases: Vec<(&str, Vec<String>)> = vec![
        ("review-fail", fx.args()),
        ("review-fail", with(&["--head", &h, "--head", &h])),
        ("review-fail", with(&["--head", &h, "--bogus", "x"])),
        ("review-fail", with(&["--head", "abc123"])),
        ("review-fail", with(&["--head", &h.to_uppercase()])),
        ("review-fail", vec![fx.tf.to_str().unwrap().into(), "--repo-map".into(), format!("r={}", fx.repo.display()), "--caps-dir".into(), "caps".into(), "--head".into(), h.clone()]),
        ("review-fail", vec![fx.tf.to_str().unwrap().into(), "--repo-map".into(), format!("r={}", fx.repo.display()), "--caps-dir".into(), inside.to_str().unwrap().into(), "--head".into(), h.clone()]),
        ("fix-round-check", with(&["--role", "gov"])),
        ("fix-round-check", with(&["--role", "Gov", "--model", "m-1"])),
        ("fix-round-check", with(&["--role", "gov", "--model", ""])),
        ("fix-round-check", with(&["--role", "gov", "--model", "m", "--role", "gov"])),
    ];
    for (verb, args) in cases {
        let o = run(verb, &args);
        errored(&o);
        assert!(err(&o).contains("harness: error:"), "{verb}: {}", err(&o));
        assert!(!err(&o).contains(Path::new(&fx.caps).to_str().unwrap()), "no folder is printed");
    }
    assert!(fx.files().iter().all(|n| n != "T-1.reviews"), "nothing was written");
}
