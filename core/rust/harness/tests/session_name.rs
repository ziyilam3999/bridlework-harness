//! Process-level tests for `harness session check-name` (S-1.6.2, P1B-16). Every check runs the real
//! binary as a child process, with scratch config files under the system temp folder. The config
//! source is set only on the child (`.env`, `.env_remove`); this file never reads one. Every refusal
//! arm has a planted twin: the same run with one part changed, refused with that part named on
//! stderr. The lane count comes from the config only: the same name is checked at count 4 and 9.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

struct Tmp(PathBuf);

impl Tmp {
    fn new() -> Tmp {
        let root = std::env::temp_dir();
        let p = root.join(format!("hsn-{}-{}", std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        let p = p.canonicalize().unwrap();
        // scratch lives under the system temp folder, never beside the checkout
        assert!(p.starts_with(root.canonicalize().unwrap()), "scratch must be under the temp folder");
        Tmp(p)
    }

    /// A scratch config: the example with `lanes.count` set to `count`.
    fn config(&self, count: u64) -> PathBuf {
        let body = fs::read_to_string(EXAMPLE)
            .unwrap()
            .replace("count: 4", &format!("count: {count}"));
        let f = self.0.join(format!("config-{count}.yaml"));
        fs::write(&f, body).unwrap();
        f
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Run the verb with no config variable on the child (`args` carries `--config` when a file is used).
fn run(args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null()).env_remove("HARNESS_CONFIG");
    c.output().unwrap()
}

/// Run the verb with `HARNESS_CONFIG` set to `env` on the child (a `--config` flag in `args`, when
/// present, must win over it).
fn run_env(env: &Path, args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).stdin(Stdio::null()).env("HARNESS_CONFIG", env);
    c.output().unwrap()
}

fn check_name<'a>(name: &'a str, cfg: &'a Path, extra: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec!["session", "check-name", name, "--config"];
    v.push(cfg.to_str().unwrap());
    v.extend_from_slice(extra);
    v
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}

// ---- AC 4 / AC 5: the split-row refusals, the long survivors and the live launcher names ----

#[test]
fn split_row_names_are_refused_at_count_4() {
    let t = Tmp::new();
    let c4 = t.config(4);
    for name in ["builder2-x", "orchestration-l2-x"] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 1, "{name}: exit");
        assert!(out(&o).is_empty(), "{name}: refusal stdout must be empty");
    }
}

#[test]
fn split_row_passes_and_live_launcher_names_pass_at_count_4() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let names = [
        // AC 4 long-form survivors
        "execution-l1-p1a13",
        "planner-l2-pre4",
        "planner-ahead-pre5",
        "governance-p1a-signoff",
        "Orchestrator",
        // AC 5 live launcher names
        "exec-L3-t101-M-son-med",
        "exec-L1-t113-M-son-med",
        "exec-L4-t120-M-glf-med",
        "e-rvw-L4-t120-M-opu-hig",
        "plan-L2-t115-M-opu-med",
        "p-rvw-L3-t115-M-opu-hig",
        // the long-form survivor of the live list
        "governance-l4-pusher3",
    ];
    for name in names {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 0, "{name}: exit; stderr: {}", err(&o));
        assert!(out(&o).starts_with("ok "), "{name}: stdout: {}", out(&o));
    }
}

// ---- AC 6: the lane bound comes from the config only ----

#[test]
fn lane_bound_comes_from_the_config_only() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let c9 = t.config(9);
    for name in ["execution-l9-x", "exec-L9-t115-M-son-med"] {
        assert_eq!(code(&run(&check_name(name, &c4, &[]))), 1, "{name} at count 4");
        assert_eq!(code(&run(&check_name(name, &c9, &[]))), 0, "{name} at count 9");
    }
    // lane 0 is refused under both counts
    for cfg in [&c4, &c9] {
        let o = run(&check_name("exec-L0-t115-M-son-med", cfg, &[]));
        assert_eq!(code(&o), 1);
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("lane"), "{}", err(&o));
    }
}

// ---- AC 7: one-part twins, each refusal naming its part ----

#[test]
fn twins_are_refused_naming_their_part() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let twins = [
        ("exec-L1-t115-X-son-med", "size"),
        ("exec-L1-t115-M-ops-med", "model"),
        ("exec-L1-t115-M-son-high", "effort"),
        ("exec-L1-T115-M-son-med", "task"),
        ("exec-L1-t11-5-M-son-med", "task"),
        ("exec-L1-p1a15-M-son-med", "task"),
        ("exec-l1-t115-M-son-med", "lane"),
        ("rvw-L2-t115-M-opu-hig", "role"),
        ("exec-L1-t115-M-son-hi", "effort"),
        ("push-n3-S-son-med", "lane"),
        ("plan-ahead-p1a10-M-son-med", "lane"),
        ("random-session", "role"),
        ("builder2-p1a20", "role"),
        ("orchestrator", "role"),
        ("executor-L4-t106-M-ops-med", "role"),
        ("exec-L1-t115-M-son-med ", "effort"),
    ];
    for (name, part) in twins {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 1, "{name}: exit");
        assert!(out(&o).is_empty(), "{name}: refusal stdout must be empty");
        assert!(err(&o).contains(part), "{name}: refusal must name `{part}`: {}", err(&o));
        assert!(err(&o).contains(name.trim()), "{name}: refusal must name the name: {}", err(&o));
    }
    // the empty name is refused too, with its reason named
    let o = run(&check_name("", &c4, &[]));
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("empty"), "{}", err(&o));
}

// ---- AC 8: the legacy map and its twins ----

#[test]
fn legacy_names_print_their_mapped_ticket() {
    let t = Tmp::new();
    let c4 = t.config(4);
    for (name, line) in [
        ("builder1-p1a13", "ok kind=legacy ticket=p1a13"),
        ("lane3-pre6a-builder", "ok kind=legacy ticket=pre6a"),
        ("lane2-pre4-planner", "ok kind=legacy ticket=pre4"),
        ("orchestration-l2-p1a06", "ok kind=legacy ticket=p1a06"),
        ("D16", "ok kind=legacy ticket=-"),
    ] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 0, "{name}");
        assert_eq!(out(&o).trim(), line, "{name}");
    }
    for name in ["builder1-p1a14", "orchestration-l2-p1a07", "d16", "lane3-pre6a-planner"] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 1, "{name}");
        assert!(out(&o).is_empty());
    }
}

// ---- AC 9: the read-back line ----

#[test]
fn read_back_lines() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let o = run(&check_name("exec-L1-t113-M-son-med", &c4, &[]));
    assert_eq!(out(&o), "ok kind=launcher role=exec lane=1 ticket=113\n");
    let o = run(&check_name("e-rvw-L4-t120-M-opu-hig", &c4, &[]));
    assert!(out(&o).contains("role=e-rvw lane=4 ticket=120"), "{}", out(&o));
    let o = run(&check_name("execution-l1-p1a13", &c4, &[]));
    assert_eq!(out(&o).trim(), "ok kind=long lane=1 ticket=p1a13");
    let o = run(&check_name("builder1-p1a13", &c4, &[]));
    assert!(out(&o).contains("kind=legacy ticket=p1a13"), "{}", out(&o));
    for name in ["governance-p1a-signoff", "Orchestrator"] {
        let o = run(&check_name(name, &c4, &[]));
        assert!(out(&o).contains("lane=- ticket=-"), "{name}: {}", out(&o));
    }
}

// ---- AC 10: --new accepts the launcher form only ----
// #177 contract change: under --new the launcher form must carry the round token (`-r<n>`),
// so the accepted name below is the round form, not the roundless one.

#[test]
fn new_flag_accepts_the_launcher_form_only() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let o = run(&check_name("exec-L1-t113-r1-M-son-med", &c4, &["--new"]));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    for name in ["execution-l1-p1a13", "builder1-p1a13", "Orchestrator"] {
        let o = run(&check_name(name, &c4, &["--new"]));
        assert_eq!(code(&o), 1, "{name} under --new");
        assert!(out(&o).is_empty());
        assert!(err(&o).contains("--new"), "{name}: {}", err(&o));
    }
}

// ---- AC 1 / #177: the round token reads back ----

#[test]
fn round_token_reads_back() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let o = run(&check_name("e-rvw-L2-t180-r2-M-opu-hig", &c4, &[]));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "ok kind=launcher role=e-rvw lane=2 ticket=180 round=2\n");
    // the roundless line is byte-identical to today (the AC 2 guard)
    let o = run(&check_name("e-rvw-L2-t180-M-opu-hig", &c4, &[]));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o), "ok kind=launcher role=e-rvw lane=2 ticket=180\n");
    // the round digit boundaries: one digit and the six-digit cap
    for (name, line) in [
        ("exec-L1-t113-r1-M-son-med", "ok kind=launcher role=exec lane=1 ticket=113 round=1"),
        ("exec-L1-t113-r999999-M-son-med", "ok kind=launcher role=exec lane=1 ticket=113 round=999999"),
    ] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 0, "{name}: stderr: {}", err(&o));
        assert_eq!(out(&o).trim(), line, "{name}");
    }
}

// ---- AC 3 / #177: --new requires the round token ----

#[test]
fn new_flag_requires_the_round_token() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let o = run(&check_name("exec-L1-t113-r1-M-son-med", &c4, &["--new"]));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let o = run(&check_name("exec-L1-t113-M-son-med", &c4, &["--new"]));
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    assert!(err(&o).contains("round"), "{}", err(&o));
    // a malformed round token is refused under --new too, still naming `round`
    let o = run(&check_name("exec-L1-t113-r0-M-son-med", &c4, &["--new"]));
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("round"), "{}", err(&o));
}

// ---- AC 4 / #177: the round twins, each refusal naming its part ----

#[test]
fn round_twins_are_refused() {
    let t = Tmp::new();
    let c4 = t.config(4);
    // the positive half: AC 1's round name passes in the default mode
    let o = run(&check_name("e-rvw-L2-t180-r2-M-opu-hig", &c4, &[]));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    // one malformed round spelling per twin, each refused naming `round`
    for name in [
        "exec-L1-t113-r0-M-son-med",       // round zero
        "exec-L1-t113-r01-M-son-med",      // leading zero
        "exec-L1-t113-r-M-son-med",        // no digits
        "exec-L1-t113-R2-M-son-med",       // uppercase R
        "exec-L1-t113-r1234567-M-son-med", // seven digits
        "exec-L1-t113-r1-r2-M-son-med",    // two round tokens
    ] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 1, "{name}: exit");
        assert!(out(&o).is_empty(), "{name}: refusal stdout must be empty");
        assert!(err(&o).contains("round"), "{name}: refusal must name `round`: {}", err(&o));
        assert!(err(&o).contains(name), "{name}: refusal must name the name: {}", err(&o));
    }
    // the round token before the task part: refused naming `task` or `round`
    let o = run(&check_name("exec-L1-r2-t113-M-son-med", &c4, &[]));
    assert_eq!(code(&o), 1);
    let e = err(&o);
    assert!(e.contains("task") || e.contains("round"), "round before task must name a part: {e}");
    // the round token after the size part: refused naming a part
    let o = run(&check_name("exec-L1-t113-M-r2-son-med", &c4, &[]));
    assert_eq!(code(&o), 1);
    let e = err(&o);
    let parts = ["lane", "task", "round", "size", "model", "effort", "role"];
    assert!(parts.iter().any(|p| e.contains(p)), "round after size must name a part: {e}");
}

// ---- AC 11: no config source is an error, never a default count ----

#[test]
fn no_config_source_is_an_error() {
    let o = run(&["session", "check-name", "exec-L1-t113-M-son-med"]);
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
    let o = run(&["session", "check-name", "exec-L1-t113-M-son-med", "--config", ""]);
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
}

// ---- the config source: the environment half and the flag precedence ----

#[test]
fn config_from_the_environment_and_flag_precedence() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let c9 = t.config(9);
    // the environment alone bounds the lane
    let o = run_env(&c4, &["session", "check-name", "exec-L4-t113-M-son-med"]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    let o = run_env(&c4, &["session", "check-name", "exec-L5-t113-M-son-med"]);
    assert_eq!(code(&o), 1);
    // the flag wins over the environment: the env file (count 4) would refuse lane 9
    let o = run_env(&c4, &["session", "check-name", "exec-L9-t113-M-son-med", "--config", c9.to_str().unwrap()]);
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    // an unreadable or bad config file is a command error
    let o = run(&["session", "check-name", "exec-L1-t113-M-son-med", "--config", "/nonexistent-harness-config.yaml"]);
    assert_eq!(code(&o), 2);
    assert!(out(&o).is_empty());
}

// ---- usage errors ----

#[test]
fn usage_errors_exit_2_with_empty_stdout() {
    for args in [
        vec!["session"],
        vec!["session", "check-name"],
        vec!["session", "rename", "x"],
        vec!["session", "check-name", "a", "b"],
        vec!["session", "check-name", "exec-L1-t113-M-son-med", "--frob"],
        vec!["session", "check-name", "exec-L1-t113-M-son-med", "--config"],
    ] {
        let o = run(&args);
        assert_eq!(code(&o), 2, "{args:?}");
        assert!(out(&o).is_empty(), "{args:?}: {}", out(&o));
    }
}

// ---- AC 15 first half: the word-arm canaries ----

#[test]
fn word_arm_canaries_pass_at_count_4() {
    let t = Tmp::new();
    let c4 = t.config(4);
    for name in [
        "rsch-L2-tab-S-fab-xhi",
        "ship-L1-t9-S-loc-low",
        "know-L3-tz-M-glm-med",
        "push-L2-t117-S-son-low",
    ] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 0, "{name}: stderr: {}", err(&o));
        assert!(out(&o).starts_with("ok kind=launcher"), "{name}: {}", out(&o));
    }
}

// ---- the refusal line: one line, naming the name and the reason ----

#[test]
fn the_refusal_line_names_the_name_and_the_reason() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let o = run(&check_name("exec-L9-t113-M-son-med", &c4, &[]));
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    let text = err(&o);
    let lines: Vec<&str> = text.trim_end().split('\n').collect();
    assert_eq!(lines.len(), 1, "one stderr line: {}", err(&o));
    assert!(lines[0].contains("exec-L9-t113-M-son-med"), "{}", lines[0]);
    assert!(lines[0].contains("lane"), "{}", lines[0]);
}

// ---- extra probes of shapes the acceptance lists never use (own variants) ----

#[test]
fn own_probe_variants() {
    let t = Tmp::new();
    let c4 = t.config(4);
    // a lane written with a leading zero or as zero: refused, naming lane
    for name in ["exec-L01-t113-M-son-med", "execution-l0-x"] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 1, "{name}");
        assert!(err(&o).contains("lane"), "{name}: {}", err(&o));
    }
    // a lane above the configured count: refused at 4, accepted at 9
    let o = run(&check_name("rsch-L6-tq1-S-hai-max", &c4, &[]));
    assert_eq!(code(&o), 1);
    let c9 = t.config(9);
    let o = run(&check_name("rsch-L6-tq1-S-hai-max", &c9, &[]));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    // an uppercase role word never matches a word
    let o = run(&check_name("Exec-L1-t113-M-son-med", &c4, &[]));
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("role"), "{}", err(&o));
    // a trailing newline is not part of any word
    let o = run(&check_name("exec-L1-t113-M-son-med\n", &c4, &[]));
    assert_eq!(code(&o), 1);
    assert!(out(&o).is_empty());
    // a two-hyphen task id splits into extra parts: refused, naming task
    let o = run(&check_name("exec-L1-t11--5-M-son-med", &c4, &[]));
    assert_eq!(code(&o), 1);
    assert!(err(&o).contains("task"), "{}", err(&o));
    // a size letter L (the least-used size) reads back
    let o = run(&check_name("gov-L3-t441-L-fab-xhi", &c4, &[]));
    assert_eq!(code(&o), 0, "stderr: {}", err(&o));
    assert_eq!(out(&o).trim(), "ok kind=launcher role=gov lane=3 ticket=441");
}

// ---- F1 (round 1): a multi-byte character at a word-length boundary refuses, never panics ----

#[test]
fn multi_byte_names_are_refused_without_panicking() {
    let t = Tmp::new();
    let c4 = t.config(4);
    for name in [
        "exec-L\u{661}-t1-M-son-med",  // Arabic-Indic digit inside the lane digits
        "exec-L\u{ff11}-t1-M-son-med", // full-width digit inside the lane digits
        "exec-L1-t\u{f1}-M-son-med",   // multi-byte letter at the `learning`/`knowledge` length
        "go\u{e9}-L1-t1-M-son-med",    // multi-byte letter at the `gov` word length
        "pla\u{e9}-L1-t1-M-son-med",   // multi-byte letter at the `plan` word length
        "\u{e9}xec-L1-t1-M-son-med",   // multi-byte first letter: no word can match
        "exec-L1-t1-M-son-m\u{e9}",    // multi-byte last letter: no effort word matches
    ] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 1, "{name}: exit (a panic would be 101); stderr: {}", err(&o));
        assert!(out(&o).is_empty(), "{name}: refusal stdout must be empty");
        let text = err(&o);
        let lines: Vec<&str> = text.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1, "{name}: exactly one stderr line, no panic text: {text}");
        assert!(lines[0].contains(name), "{name}: the line names the name: {text}");
    }
}

// ---- F2 (round 1): one process-level twin per refusal condition, each naming its part ----

#[test]
fn per_condition_twins_are_refused() {
    let t = Tmp::new();
    let c4 = t.config(4);
    let twins = [
        // lane digits only: `+1` still parses as 1, only the digit check refuses it
        ("exec-L+1-t1-M-son-med", "lane"),
        // size is exactly S, M or L, upper case included
        ("exec-L1-t1-m-son-med", "size"),
        // effort and model words are exact, case included
        ("exec-L1-t1-M-son-MED", "effort"),
        ("exec-L1-t1-M-SON-med", "model"),
        // the long-form slug is lowercase letters, digits and `-` only
        ("execution-l1-X", "task"),
        // the sign-off phase must start with a digit
        ("governance-pab-signoff", "task"),
        // sign-off is a governance / risk-review shape only
        ("execution-p1-signoff", "lane"),
        ("planner-p1-signoff", "lane"),
        // `ahead-` is a planner shape only
        ("execution-ahead-x", "lane"),
        // a word with nothing after its `-` is not a word match
        ("exec-", "role"),
        // a word run with no `-` terminator at all is not a word match either
        ("executive-x", "role"),
        // an empty task slug is not `t<slug>`
        ("exec-L1-t-M-son-med", "task"),
        // the long form needs a slug after the lane, and it may not be empty
        ("planner-l2", "task"),
        ("planner-l2-", "task"),
        // the `ahead-` slug is a slug of lowercase letters, digits and `-` only
        ("planner-ahead-X9", "task"),
        // the sign-off phase may not be empty, and its charset is lowercase/digit/`-`
        ("governance-p-signoff", "task"),
        ("governance-p1A-signoff", "task"),
        // without the `-signoff` suffix the name is not a sign-off shape
        ("governance-p1a-x", "lane"),
    ];
    for (name, part) in twins {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 1, "{name}: exit");
        assert!(out(&o).is_empty(), "{name}: refusal stdout must be empty");
        assert!(err(&o).contains(part), "{name}: refusal must name `{part}`: {}", err(&o));
        assert!(err(&o).contains(name), "{name}: refusal must name the name: {}", err(&o));
    }
}

// ---- F2 (round 1): one process-level acceptance per word, so a dropped word dies here alone ----

#[test]
fn every_word_has_a_process_level_acceptance() {
    let t = Tmp::new();
    let c4 = t.config(4);
    // one launcher name per launcher role word, one model word and one effort word per run
    for name in [
        "plan-L2-t31-S-son-low",
        "exec-L1-t32-M-opu-med",
        "p-rvw-L3-t33-S-glm-hig",
        "e-rvw-L4-t34-M-fab-med",
        "rsch-L2-tab-S-fab-xhi",
        "ship-L1-t9-S-loc-low",
        "learn-L1-t35-S-hai-max",
        "know-L3-tz-M-glm-med",
        "gov-L3-t36-S-opu-low",
        "push-L2-t117-S-son-low",
    ] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 0, "{name}: stderr: {}", err(&o));
        assert!(out(&o).starts_with("ok kind=launcher"), "{name}: {}", out(&o));
    }
    // one long-form name per long role word, plus each non-lane long shape once
    for name in [
        "planner-l2-pre4",
        "execution-l1-p1a13",
        "risk-review-l3-q1",
        "learning-l1-x1",
        "knowledge-l2-x2",
        "governance-l4-pusher3",
        "planner-ahead-pre5",
        "governance-p1a-signoff",
    ] {
        let o = run(&check_name(name, &c4, &[]));
        assert_eq!(code(&o), 0, "{name}: stderr: {}", err(&o));
        assert!(out(&o).starts_with("ok kind=long"), "{name}: {}", out(&o));
    }
}
