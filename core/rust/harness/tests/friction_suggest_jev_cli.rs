//! Ticket friction-suggest-jev: the Jev arm of `harness friction suggest`. The real binary runs as a child on a
//! scratch inbox with a fake send program (one fixture script, many cases); each row, key, needle and answer is
//! synthetic, and each privacy needle is assembled from fragments at run time.
#![cfg(unix)]
mod friction_common;
use friction_common::*;
use std::fs;
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/friction_suggest_jev/fake-send.sh");

// ---------------------------------------------------------------- the scratch and the fake

struct J {
    t: Tmp,
    dir: PathBuf,
    key: PathBuf,
    ledger: PathBuf,
    tmproot: PathBuf,
}

impl J {
    fn new(case: &str) -> J {
        let t = Tmp::new();
        let dir = t.0.join("fake");
        fs::create_dir_all(&dir).unwrap();
        let send = dir.join("fake-send.sh");
        fs::copy(FIXTURE, &send).unwrap();
        fs::set_permissions(&send, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("case"), case).unwrap();
        let key = t.0.join("key");
        fs::write(&key, "synthetic-key-bytes\n").unwrap();
        let ledger = t.0.join("ledger");
        let tmproot = t.0.join("tmproot");
        fs::create_dir_all(&tmproot).unwrap();
        J { t, dir, key, ledger, tmproot }
    }

    fn send(&self) -> PathBuf {
        self.dir.join("fake-send.sh")
    }

    fn answer(&self, body: &str) {
        fs::write(self.dir.join("answer.json"), body).unwrap();
    }

    fn stderr_text(&self, body: &str) {
        fs::write(self.dir.join("stderr.txt"), body).unwrap();
    }

    fn ran(&self) -> bool {
        self.dir.join("notes").join("ran").exists()
    }

    fn note(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join("notes").join(name)).unwrap_or_default()
    }

    fn request(&self) -> String {
        self.note("request-copy")
    }

    fn args(&self) -> Vec<String> {
        self.note("args").lines().map(|l| l.to_string()).collect()
    }

    fn env_text(&self) -> String {
        self.note("env")
    }

    fn env_names(&self) -> Vec<String> {
        self.note("env-names").lines().map(|l| l.to_string()).collect()
    }

    fn tmproot_empty(&self) -> bool {
        fs::read_dir(&self.tmproot).map(|d| d.count() == 0).unwrap_or(false)
    }
}

fn jev_cmd(j: &J) -> Command {
    let cfg = j.t.config();
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    c
}

fn plain_cmd(t: &Tmp) -> Command {
    let cfg = t.config();
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST_SECRET", DENY).args(["friction", "suggest", "--config", p(&cfg)]).stdin(Stdio::null());
    c
}

fn plain_run(t: &Tmp, rows: &[String], q: &[&str], extra: &[&str]) -> Output {
    t.write_inbox(rows);
    let mut c = plain_cmd(t);
    let mut owned: Vec<String> = q.iter().map(|s| s.to_string()).collect();
    owned.extend(extra.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    c.args(&refs).output().unwrap()
}

fn jev_run(j: &J, rows: &[String], q: &[&str], extra: &[&str]) -> Output {
    j.t.write_inbox(rows);
    let mut c = jev_cmd(j);
    let mut owned: Vec<String> = q.iter().map(|s| s.to_string()).collect();
    owned.extend(extra.iter().map(|s| s.to_string()));
    let refs: Vec<&str> = owned.iter().map(|s| s.as_str()).collect();
    c.args(&refs).output().unwrap()
}

/// The BM25 lines a run with no --jev flag prints for the same input. The `--jev` flags and `--record` go to the
/// Jev run only.
fn bm25_of(t: &Tmp, rows: &[String], q: &[&str], extra: &[&str]) -> String {
    let mut plain_extra: Vec<&str> = Vec::new();
    let mut skip = false;
    for a in extra {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--jev") || *a == "--record" {
            skip = true;
            continue;
        }
        plain_extra.push(a);
    }
    let plain = plain_run(t, rows, q, &plain_extra);
    assert_eq!(code(&plain), 0, "{}", err(&plain));
    assert_eq!(err(&plain), "");
    out(&plain)
}

/// The three asserts of each Jev run: the exit code as a number, an empty stderr, and the whole stdout, which is
/// the given BM25 lines and then the one Jev line.
fn check_out(o: &Output, bm25: &str, jev_line: &str) {
    assert_eq!(code(o), 0, "{}", err(o));
    assert_eq!(err(o), "");
    assert_eq!(out(o), format!("{bm25}{jev_line}"));
}

/// `check_out` with the BM25 lines of the plain run on the same input.
fn assert_jev(t: &Tmp, rows: &[String], q: &[&str], extra: &[&str], o: &Output, jev_line: &str) {
    check_out(o, &bm25_of(t, rows, q, extra), jev_line);
}

/// The whole-stdout assertion: the run with the Jev arm prints exactly what today's verb printed, plus the one
/// Jev line, with an empty stderr and exit 0.
fn assert_whole(j: &J, rows: &[String], q: &[&str], extra: &[&str], jev_line: &str) {
    let o = jev_run(j, rows, q, extra);
    assert_jev(&j.t, rows, q, extra, &o, jev_line);
}

/// A run that must stop on a flag check: exit 2, stdout empty, the given stderr line, nothing run.
fn refuse(args: &[&str], line: &str) {
    let mut owned = vec!["friction", "suggest", "--text", "cache"];
    owned.extend_from_slice(args);
    let o = base().args(&owned).stdin(Stdio::null()).output().unwrap();
    assert_eq!(code(&o), 2, "{args:?}: {}", err(&o));
    assert_eq!(out(&o), "", "{args:?}");
    assert_eq!(err(&o), format!("{line}\n"), "{args:?}");
}

const NEEDS_SEND: &str = "harness: error: a --jev flag needs --jev-send";
const NEEDS_PARTNERS: &str = "harness: error: --jev-send needs --jev-key-file and --jev-ledger-dir";
const BAD_DEADLINE: &str = "harness: error: --jev-deadline-ms must be a whole number from 1 to 600000";

/// The deadline check with all three partner flags in place.
fn refuse_deadline(v: &str) {
    refuse(&["--jev-send", "/send", "--jev-key-file", "/k", "--jev-ledger-dir", "/l", "--jev-deadline-ms", v], BAD_DEADLINE);
}

/// The group id `p-N` of the 5-candidate fixture whose criterion holds the letter `letter` in `req`.
fn group_of5(req: &str, letter: &str) -> String {
    let marker = criteria_pairs(req).into_iter().find(|(k, _)| k == letter).map(|(_, v)| v).unwrap();
    let n: usize = marker.split("mkp").nth(1).unwrap().split(&['\\', ' '][..]).next().unwrap().parse().unwrap();
    format!("p-{n}")
}

/// The (letter, raw value) pairs of the criteria object, in file order.
fn criteria_pairs(req: &str) -> Vec<(String, String)> {
    let start = req.find("\"criteria\":{").expect("a criteria object") + 12;
    let b = req.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    loop {
        assert_eq!(b[i], b'"', "a key");
        let ks = i + 1;
        let ke = ks + req[ks..].find('"').expect("key end");
        let key = req[ks..ke].to_string();
        i = ke + 1;
        assert_eq!(b[i] as char, ':');
        i += 1;
        assert_eq!(b[i], b'"', "a value");
        let vs = i + 1;
        let mut k = vs;
        while k < b.len() {
            if b[k] == b'\\' {
                k += 2;
                continue;
            }
            if b[k] == b'"' {
                break;
            }
            k += 1;
        }
        out.push((key, req[vs..k].to_string()));
        i = k + 1;
        if b[i] == b',' {
            i += 1;
            continue;
        }
        assert_eq!(b[i] as char, '}');
        return out;
    }
}

/// The letter whose criterion value holds `marker` (each candidate text carries one unique marker).
fn letter_holding(req: &str, marker: &str) -> String {
    for n in 0..20u8 {
        let letter = (b'A' + n) as char;
        if let Some((_, v)) = criteria_pairs(req).into_iter().find(|(k, _)| *k == letter.to_string()) {
            if v.contains(marker) {
                return letter.to_string();
            }
        }
    }
    panic!("marker {marker} is in no criterion");
}

/// One discovery run (case no-file), then the letter of the group whose best row holds `marker`.
fn discover_letter(j: &J, rows: &[String], q: &[&str], extra: &[&str], marker: &str) -> String {
    let _ = jev_run(j, rows, q, extra);
    assert!(j.ran(), "the discovery run reached the program");
    letter_holding(&j.request(), marker)
}

/// The group ids of the plain run's suggest lines, best first (--top is 99; the fixtures have fewer groups than that cut).
fn plain_order(t: &Tmp, rows: &[String], q: &[&str]) -> Vec<String> {
    let o = plain_run(t, rows, q, &["--top", "99"]);
    assert_eq!(code(&o), 0, "{}", err(&o));
    out(&o).lines().map(|l| l.split("group=").nth(1).unwrap().split(' ').next().unwrap().to_string()).collect()
}

fn alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn dead_within(pid: u32, secs: u64) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    !alive(pid)
}

// ---------------------------------------------------------------- the fixtures

fn rows3() -> Vec<String> {
    vec![
        row("ga-1", "2026-11-01", "", "cache warm mkga", &[]),
        row("gb-1", "2026-11-02", "", "cache other mkgb", &[]),
        row("gc-1", "2026-11-03", "", "zeta third mkgc", &[]),
    ]
}

const Q3: [&str; 2] = ["--text", "cache warm zeta"];

fn rows5() -> Vec<String> {
    (1..=5).map(|i| row(&format!("p-{i}"), "2026-11-01", "", &format!("share mkp{i}"), &[])).collect()
}

const Q5: [&str; 2] = ["--text", "share"];

fn rows6() -> Vec<String> {
    vec![
        row("s-1", "2026-11-01", "", "cone ctwo cthree cfour cfive csix mk1", &[]),
        row("s-2", "2026-11-02", "", "cone ctwo cthree cfour cfive mk2", &[]),
        row("s-3", "2026-11-03", "", "cone ctwo cthree cfour mk3", &[]),
        row("s-4", "2026-11-04", "", "cone ctwo mk4", &[]),
        row("s-5", "2026-11-05", "", "filler mk5a", &[]),
        row("s-5b", "2026-11-06", "s-5", "cone mk5b", &[]),
        row("s-5c", "2026-11-07", "s-5", "cone ctwo cthree mk5c", &[]),
        row("s-6", "2026-11-08", "", "ctwo cthree cfour cfive csix mk6", &[]),
    ]
}

const Q6: [&str; 2] = ["--text", "cone ctwo cthree cfour cfive csix"];

fn rows22() -> Vec<String> {
    (1..=22).map(|i| row(&format!("g-{i:02}"), "2026-11-01", "", &format!("share mk{i:02}"), &[])).collect()
}

const Q22: [&str; 2] = ["--text", "share"];

/// The letter-order fixture: the inbox order, the BM25 order, the group id order and the letter order are four
/// different orders.
fn rows_l() -> Vec<String> {
    vec![
        row("m-3", "2026-11-01", "", "paddle mk3", &[]),
        row("m-6", "2026-11-02", "", "rower mk6", &[]),
        row("m-1", "2026-11-03", "", "paddle alpha mk1", &[]),
        row("m-4", "2026-11-04", "", "rower psi mk4", &[]),
        row("m-5", "2026-11-05", "", "paddle alpha beta mk5", &[]),
        row("m-2", "2026-11-06", "", "paddle alpha beta gamma mk2", &[]),
    ]
}

const QL: [&str; 2] = ["--text", "paddle rower"];

fn answer_json(probs: &[(&str, &str)], usage: Option<&str>) -> String {
    let mut s = String::from("{\"answers\":{\"group\":{\"type\":\"choice\",\"choice\":\"A\",\"probabilities\":{");
    for (i, (k, v)) in probs.iter().enumerate() {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("\"{k}\":{v}"));
    }
    s.push_str("}}}");
    if let Some(u) = usage {
        s.push_str(&format!(",\"usage\":{{{u}}}"));
    }
    s.push('}');
    s
}

// ---------------------------------------------------------------- flags

#[test]
fn a_key_file_flag_without_jev_send_is_exit_2() {
    refuse(&["--jev-key-file", "/k"], NEEDS_SEND);
}

#[test]
fn a_ledger_dir_flag_without_jev_send_is_exit_2() {
    refuse(&["--jev-ledger-dir", "/l"], NEEDS_SEND);
}

#[test]
fn a_deadline_flag_without_jev_send_is_exit_2() {
    refuse(&["--jev-deadline-ms", "10"], NEEDS_SEND);
}

#[test]
fn jev_send_without_the_ledger_dir_is_exit_2() {
    refuse(&["--jev-send", "/s", "--jev-key-file", "/k"], NEEDS_PARTNERS);
}

#[test]
fn jev_send_without_the_key_file_is_exit_2() {
    refuse(&["--jev-send", "/s", "--jev-ledger-dir", "/l"], NEEDS_PARTNERS);
}

#[test]
fn a_relative_jev_send_is_exit_2() {
    refuse(&["--jev-send", "prog", "--jev-key-file", "/k", "--jev-ledger-dir", "/l"], "harness: error: --jev-send must be an absolute path");
}

#[test]
fn a_relative_jev_key_file_is_exit_2() {
    refuse(&["--jev-send", "/send", "--jev-key-file", "k", "--jev-ledger-dir", "/l"], "harness: error: --jev-key-file must be an absolute path");
}

#[test]
fn a_relative_jev_ledger_dir_is_exit_2() {
    refuse(&["--jev-send", "/send", "--jev-key-file", "/k", "--jev-ledger-dir", "l"], "harness: error: --jev-ledger-dir must be an absolute path");
}

#[test]
fn a_deadline_of_0_is_exit_2() {
    refuse_deadline("0");
}

#[test]
fn a_deadline_of_600001_is_exit_2() {
    refuse_deadline("600001");
}

#[test]
fn a_deadline_with_a_leading_zero_is_exit_2() {
    refuse_deadline("012");
}

#[test]
fn a_deadline_with_a_trailing_letter_is_exit_2() {
    refuse_deadline("12x");
}

#[test]
fn an_empty_deadline_is_exit_2() {
    refuse_deadline("");
}

#[test]
fn a_deadline_of_twenty_digits_is_exit_2() {
    refuse_deadline("12345678901234567890");
}

#[test]
fn checks_1_and_4_print_line_1() {
    refuse(&["--jev-key-file", "/k", "--jev-deadline-ms", "0x"], NEEDS_SEND);
}

#[test]
fn checks_2_and_3_print_line_2() {
    refuse(&["--jev-send", "prog", "--jev-key-file", "rel"], NEEDS_PARTNERS);
}

#[test]
fn checks_3_and_4_print_line_3() {
    refuse(
        &["--jev-send", "/s", "--jev-key-file", "/k", "--jev-ledger-dir", "rel", "--jev-deadline-ms", "0"],
        "harness: error: --jev-ledger-dir must be an absolute path",
    );
}

#[test]
fn a_relative_ledger_dir_given_before_a_relative_send_names_jev_send() {
    // The flag order of the checks, not the order on the command line.
    refuse(&["--jev-key-file", "/k", "--jev-ledger-dir", "rel", "--jev-send", "prog"], "harness: error: --jev-send must be an absolute path");
}

#[test]
fn a_jev_flag_given_twice_is_exit_2() {
    refuse(&["--jev-send", "/a", "--jev-send", "/b", "--jev-key-file", "/k", "--jev-ledger-dir", "/l"], "harness: error: --jev-send was given twice");
}

#[test]
fn deadline_600000_is_accepted() {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    let extra = ["--jev-deadline-ms", "600000"];
    let o = jev_run(&j, &rows3(), &Q3, &extra);
    assert_jev(&j.t, &rows3(), &Q3, &extra, &o, "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
}

#[test]
fn the_usage_line_and_the_unknown_option_line_are_the_new_ones() {
    let o = run(&["friction"]);
    assert_eq!(code(&o), 2);
    assert!(err(&o).contains("[--jev-send <program> --jev-key-file <file> --jev-ledger-dir <dir> [--jev-deadline-ms <n>]]"), "{}", err(&o));
    let t = Tmp::new();
    t.write_inbox(&rows3());
    let cfg = t.config();
    let o = run(&["friction", "suggest", "--text", "cache", "--nope", "x", "--config", p(&cfg)]);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "harness: error: an unknown option (the friction usage line lists the suggest flags)\n");
}

// ---------------------------------------------------------------- order and shape with the arm on

#[test]
fn a_pick_prints_one_jev_line_after_the_three_bm25_lines() {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.7"), ("B", "0.1"), ("C", "0.1")], None));
    assert_whole(&j, &rows3(), &Q3, &[], "suggest: jev pick=ga-1 row=ga-1 p=0.70\n");
    assert_eq!(out(&jev_run(&j, &rows3(), &Q3, &[])).lines().count(), 4);
}

#[test]
fn the_fake_sees_only_the_bm25_lines_in_the_engine_stdout_file() {
    let j = J::new("answer");
    let stdout_path = j.t.0.join("engine-stdout.txt");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    fs::write(j.dir.join("engine-stdout"), p(&stdout_path)).unwrap();
    let bm25 = bm25_of(&j.t, &rows3(), &Q3, &[]);
    let mut c = jev_cmd(&j);
    let extra = ["--text", "cache warm zeta"];
    let f = fs::File::create(&stdout_path).unwrap();
    c.stdout(Stdio::from(f)).args(extra);
    let o = c.output().unwrap();
    assert_eq!(code(&o), 0, "{}", err(&o));
    assert_eq!(err(&o), "");
    let copy = fs::read_to_string(j.dir.join("notes").join("engine-stdout-copy")).unwrap();
    assert_eq!(copy, bm25, "the copy holds exactly the BM25 lines");
    let whole = fs::read_to_string(&stdout_path).unwrap();
    assert_eq!(whole, format!("{bm25}suggest: jev pick=ga-1 row=ga-1 p=0.90\n"));
}

#[test]
fn an_empty_inbox_prints_none_then_no_candidates() {
    let j = J::new("answer");
    assert_whole(&j, &[], &["--text", "anything"], &[], "jev: skipped reason=no-candidates\n");
    assert!(!j.ran());
    assert!(j.tmproot_empty());
    assert!(!j.ledger.exists());
}

#[test]
fn a_damaged_inbox_refuses_as_today_with_the_arm_on() {
    let j = J::new("answer");
    let good = row("ga-1", "2026-11-01", "", "cache", &[]);
    let body = format!("{good}\nnot json at all\n");
    fs::write(j.t.inbox(), &body).unwrap();
    let cfg = j.t.config();
    let o = run(&["friction", "suggest", "--text", "cache", "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)]);
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
    assert!(!j.ran());
}

#[test]
fn a_privacy_hit_in_the_query_refuses_as_today_with_nothing_made() {
    let j = J::new("answer");
    let needle = ["/Us", "ers/", "zztester", "/proj"].concat();
    let cfg = j.t.config();
    j.t.write_inbox(&rows3());
    let o = run(&["friction", "suggest", "--text", &format!("cache {needle} here"), "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)]);
    assert_eq!(code(&o), 1);
    assert_eq!(out(&o), "");
    assert!(!j.ran());
    assert!(j.tmproot_empty());
    assert!(!j.ledger.exists());
}

// ---------------------------------------------------------------- candidates, letters, request

#[test]
fn the_request_holds_six_criteria_and_the_pick_names_the_fifth_ranked_group_best_row() {
    let j = J::new("answer");
    let order = plain_order(&j.t, &rows6(), &Q6);
    assert_eq!(order.len(), 6);
    let fifth = order[4].clone();
    assert_eq!(fifth, "s-5", "the fixture ranks the three-row group fifth");
    let letter = discover_letter(&j, &rows6(), &Q6, &[], "mk5c");
    let keys = criteria_pairs(&j.request()).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
    let probs: Vec<(String, String)> = keys.iter().map(|k| (k.clone(), if *k == letter { "0.8".to_string() } else { "0.1".to_string() })).collect();
    let owned: Vec<(&str, &str)> = probs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    j.answer(&answer_json(&owned, None));
    // The asserted run: 6 criteria, only the best row of the fifth group, only 3 BM25 lines.
    let o = jev_run(&j, &rows6(), &Q6, &[]);
    assert_jev(&j.t, &rows6(), &Q6, &[], &o, "suggest: jev pick=s-5 row=s-5c p=0.80\n");
    let req = j.request();
    assert_eq!(criteria_pairs(&req).len(), 6);
    let values: String = criteria_pairs(&req).into_iter().map(|(_, v)| v).collect();
    assert!(values.contains("mk5c"), "the best row's text is sent");
    assert!(!values.contains("mk5a") && !values.contains("mk5b"), "the other two rows are not");
    assert_eq!(out(&o).lines().count(), 4, "3 BM25 lines plus the Jev line");
}

#[test]
fn twenty_two_groups_give_the_letters_a_to_t_and_drop_the_two_last_ranked() {
    let j = J::new("no-file");
    let order = plain_order(&j.t, &rows22(), &Q22);
    let _ = jev_run(&j, &rows22(), &Q22, &[]);
    let req = j.request();
    let pairs = criteria_pairs(&req);
    let keys: Vec<String> = pairs.iter().map(|(k, _)| k.clone()).collect();
    let want: Vec<String> = (0..20).map(|i| ((b'A' + i as u8) as char).to_string()).collect();
    assert_eq!(keys, want);
    let values: String = pairs.into_iter().map(|(_, v)| v).collect();
    let drop1 = format!("mk{:02}", order[20].split('-').nth(1).unwrap().parse::<usize>().unwrap());
    let drop2 = format!("mk{:02}", order[21].split('-').nth(1).unwrap().parse::<usize>().unwrap());
    assert!(!values.contains(&drop1) && !values.contains(&drop2), "the two last-ranked groups are not sent");
}

#[test]
fn the_letter_map_is_hash_order_and_survives_a_reversed_inbox() {
    // The letter of each group is the rank of sha256("<query sha256 hex>:<group id>"), smallest first; one of
    // those hashes, recomputed for this fixture (the query is `paddle rower`, no --why):
    //   printf %s "$(printf 'paddle rower\n' | shasum -a 256 | cut -d' ' -f1):m-5" | shasum -a 256
    // is the smallest of the six, so m-5 carries the letter A.
    let j = J::new("no-file");
    let _ = jev_run(&j, &rows_l(), &QL, &[]);
    let req = j.request();
    let want: Vec<(String, String)> = vec![
        ("m-1".to_string(), "B".to_string()),
        ("m-2".to_string(), "C".to_string()),
        ("m-3".to_string(), "D".to_string()),
        ("m-4".to_string(), "E".to_string()),
        ("m-5".to_string(), "A".to_string()),
        ("m-6".to_string(), "F".to_string()),
    ];
    for (group, letter) in &want {
        assert_eq!(&letter_holding(&req, &format!("mk{}", group.split('-').nth(1).unwrap())), letter, "group {group}");
    }
    // The four orders differ.
    let order = plain_order(&j.t, &rows_l(), &QL);
    let ids: Vec<String> = vec!["m-1".into(), "m-2".into(), "m-3".into(), "m-4".into(), "m-5".into(), "m-6".into()];
    let inbox: Vec<String> = rows_l().iter().map(|r| r.split('"').nth(3).unwrap().to_string()).collect();
    assert_ne!(order, inbox);
    assert_ne!(order, ids);
    let mut by_letter = want.clone();
    by_letter.sort_by(|a, b| a.1.cmp(&b.1));
    let letter_order: Vec<String> = by_letter.into_iter().map(|(g, _)| g).collect();
    assert_eq!(letter_order, vec!["m-5", "m-1", "m-2", "m-3", "m-4", "m-6"]);
    assert_ne!(order, letter_order, "the BM25 order differs from the letter order");
    assert_ne!(inbox, letter_order);
    assert_ne!(ids, letter_order);
    // The same fixture with the inbox rows in reverse order gives the same letter for each group.
    let mut rev = rows_l();
    rev.reverse();
    let _ = jev_run(&j, &rev, &QL, &[]);
    let req2 = j.request();
    for (group, letter) in &want {
        assert_eq!(&letter_holding(&req2, &format!("mk{}", group.split('-').nth(1).unwrap())), letter, "group {group}");
    }
}

#[test]
fn the_same_input_twice_gives_a_byte_identical_request() {
    let j = J::new("no-file");
    let _ = jev_run(&j, &rows_l(), &QL, &[]);
    let first = j.request();
    let _ = jev_run(&j, &rows_l(), &QL, &[]);
    assert_eq!(j.request(), first);
}

#[test]
fn planted_ids_of_all_three_shapes_and_cases_are_not_in_the_request() {
    let j = J::new("no-file");
    let rows = vec![
        row("k-1", "2026-11-01", "", "row one F-20991231-01 issue-42 #7 mkone", &[]),
        row("k-2", "2026-11-02", "", "row two f-20991231-02 ISSUE-43 #8 mktwo", &[]),
        row("k-3", "2026-11-03", "", "row three Issue-44 #9 mkthree", &[]),
    ];
    let q = ["--text", "row words f-20991231-03 ISSUE-45 #1", "--why", "cause #2 Issue-46"];
    let _ = jev_run(&j, &rows, &q, &[]);
    let req = j.request();
    for shape in ["20991231", "issue-42", "ISSUE-43", "Issue-44", "ISSUE-45", "Issue-46"] {
        assert!(!req.contains(shape), "the id shape {shape} left the machine");
    }
    assert!(!req.contains("#7") && !req.contains("#8") && !req.contains("#9"), "hash ids left the machine");
    for id in ["k-1", "k-2", "k-3"] {
        assert!(!req.contains(id), "the row id {id} left the machine");
    }
}

#[test]
fn before_limits_the_candidates_as_it_limits_the_ranking() {
    let j = J::new("no-file");
    let _ = jev_run(&j, &rows3(), &Q3, &["--before", "gc-1"]);
    let req = j.request();
    assert!(req.contains("mkga") && req.contains("mkgb"), "the two earlier groups are candidates");
    assert!(!req.contains("mkgc"), "the later group is left out of the request");
}

#[test]
fn the_request_bytes_are_exactly_the_fixed_shape() {
    let j = J::new("no-file");
    let qa = "quote row: \\\"paired\\\" back\\\\slash li\\nne tab\\there MKQA upper AB lower ab";
    let rows = vec![
        row("q-1", "2026-11-01", "", qa, &[]),
        row("q-2", "2026-11-02", "", "plain second row MKQB with Zebra and zebra", &[]),
        row("q-3", "2026-11-03", "", "third row MKQC also has Q and q", &[]),
    ];
    let q = ["--text", "find MKQA MKQB MKQC needle", "--why", "the why W and w"];
    let _ = jev_run(&j, &rows, &q, &[]);
    // The literal below is assembled from parts so the JSON escapes stay byte-exact.
    let nl = "\\n";
    let tab = "\\t";
    let quote = "\\\"";
    let bs = "\\\\";
    let mut w = String::new();
    w.push_str("{\"model\":\"typesafe/jev-1.13\",\"state\":{\"query\":\"find MKQA MKQB MKQC needle");
    w.push_str(nl);
    w.push_str("Why: the why W and w\"},\"questions\":{\"group\":{\"type\":\"choice\",\"instructions\":\"The query above is a new friction report: a problem someone hit while working, with why it matters. Each option is an earlier friction report that stands for one group of reports about the same underlying problem. Which option describes the same underlying problem that the new report repeats? Choose the single best option.\",\"criteria\":{\"A\":\"third row MKQC also has Q and q");
    w.push_str(nl);
    w.push_str("Why: \",\"B\":\"quote row: ");
    w.push_str(quote);
    w.push_str("paired");
    w.push_str(quote);
    w.push_str(" back");
    w.push_str(bs);
    w.push_str("slash li");
    w.push_str(nl);
    w.push_str("ne tab");
    w.push_str(tab);
    w.push_str("here MKQA upper AB lower ab");
    w.push_str(nl);
    w.push_str("Why: \",\"C\":\"plain second row MKQB with Zebra and zebra");
    w.push_str(nl);
    w.push_str("Why: \"}}}}");
    assert_eq!(j.request(), w);
}

// ---------------------------------------------------------------- checks before the run

fn assert_skip_before_run(j: &J, rows: &[String], q: &[&str], extra: &[&str], token: &str, ledger_made: bool) {
    assert_whole(j, rows, q, extra, &format!("jev: skipped reason={token}\n"));
    assert!(!j.ran(), "the program was not run");
    assert!(j.tmproot_empty(), "no temp folder is left");
    assert_eq!(j.ledger.exists(), ledger_made, "the ledger folder");
}

#[test]
fn code_in_the_text_skips() {
    let j = J::new("answer");
    let fence: String = ["`", "`", "`"].concat();
    let q = ["--text", &format!("cache {fence}code{fence} here")];
    assert_skip_before_run(&j, &rows3(), &q, &[], "code-skipped", false);
}

#[test]
fn code_in_the_why_skips() {
    let j = J::new("answer");
    let q = ["--text", "cache", "--why", "fine\n\ttabbed cause line"];
    assert_skip_before_run(&j, &rows3(), &q, &[], "code-skipped", false);
}

#[test]
fn a_candidate_with_an_indented_line_is_left_out_of_the_request() {
    let j = J::new("answer");
    let indented = "indented row\\n    four blanks MKCD";
    let mut rows = rows5();
    rows[3] = row("p-4", "2026-11-04", "", indented, &[]);
    let _ = jev_run(&j, &rows, &Q5, &[]);
    let req = j.request();
    assert!(!req.contains("MKCD"), "the code-carrying candidate is not sent");
    for m in ["mkp1", "mkp2", "mkp3", "mkp5"] {
        assert!(req.contains(m), "{m} is sent");
    }
    assert_eq!(criteria_pairs(&req).len(), 4);
}

#[test]
fn one_group_left_gives_no_candidates() {
    let j = J::new("answer");
    assert_skip_before_run(&j, &[row("solo-1", "2026-11-01", "", "cache solo", &[])], &["--text", "cache"], &[], "no-candidates", false);
}

#[test]
fn no_candidates_also_prints_after_three_bm25_lines() {
    let j = J::new("answer");
    // Two of the three groups are left out by the code rule, so one candidate is left.
    let rows = vec![
        row("n-1", "2026-11-01", "", "cache one\\n    indented mknone1", &[]),
        row("n-2", "2026-11-02", "", "cache two\\n    indented mknone2", &[]),
        row("n-3", "2026-11-03", "", "cache three mkn3", &[]),
    ];
    assert_skip_before_run(&j, &rows, &["--text", "cache"], &[], "no-candidates", false);
}

#[test]
fn the_byte_cap_is_199999() {
    let j = J::new("no-file");
    // Pad --text and --why, each under the 131072-byte argv bound, so the request lands at an exact size.
    let why = format!("cause {}", "y".repeat(98000));
    let measure = |pad: usize, line: &str| -> String {
        let text = format!("cache padstart{}", "x".repeat(pad));
        let q = ["--text", &text, "--why", &why];
        let o = jev_run(&j, &rows3(), &q, &[]);
        assert_jev(&j.t, &rows3(), &q, &[], &o, line);
        j.request()
    };
    let size = measure(100000, "jev: skipped reason=bad-answer\n").len();
    assert!(size < 199999, "the padding starts under the cap ({size})");
    // The fake answers nothing, so a request at the cap reaches the program and ends as bad-answer.
    let req = measure(100000 + 199999 - size, "jev: skipped reason=bad-answer\n");
    assert_eq!(req.len(), 199999, "the request is exactly at the cap");
    let _ = measure(100000 + 199999 - size + 1, "jev: skipped reason=payload\n");
    assert!(j.tmproot_empty());
}

#[test]
fn the_request_scan_refuses_only_when_the_whole_body_hits() {
    // A list whose second entry is the phrase the request assembles across fields: each field alone passes.
    let phrase = "why: plover";
    let j = J::new("answer");
    let rows = vec![
        row("w-1", "2026-11-01", "", "cache thing mkw1", &[("why", "plover nests here")]),
        row("w-2", "2026-11-02", "", "other thing mkw2", &[]),
        row("w-3", "2026-11-03", "", "third thing mkw3", &[]),
    ];
    let list = j.t.file("list.txt", format!("{DENY}\n{phrase}\n"));
    let q = ["--text", "cache plover"];
    // The refused run: the request holds "Why: plover nests here" on one line.
    let cfg = j.t.config();
    j.t.write_inbox(&rows);
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST", p(&list))
        .env_remove("HARNESS_PRIVACY_LIST_SECRET")
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--text", q[1], "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_jev(&j.t, &rows, &q, &[], &o, "jev: skipped reason=scan-refused\n");
    assert!(!j.ran());
    assert!(j.tmproot_empty());
    assert!(!j.ledger.exists());
    // The same fixture with the list that lacks the entry runs the program and prints a pick.
    let plain_list = j.t.file("plain-list.txt", format!("{DENY}\n"));
    let letter = discover_letter(&j, &rows, &q, &[], "mkw1");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    j.t.write_inbox(&rows);
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST", p(&plain_list))
        .env_remove("HARNESS_PRIVACY_LIST_SECRET")
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--text", q[1], "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_jev(&j.t, &rows, &q, &[], &o, "suggest: jev pick=w-1 row=w-1 p=0.90\n");
}

#[test]
fn the_body_scan_runs_with_the_test_mode_off() {
    let j = J::new("answer");
    let needle = ["synthetic", "-test", "-marker"].concat();
    let rows = vec![
        row("t-1", "2026-11-01", "", &format!("cache {needle} mkt1"), &[]),
        row("t-2", "2026-11-02", "", "other mkt2", &[]),
        row("t-3", "2026-11-03", "", "third mkt3", &[]),
    ];
    let cfg = j.t.config();
    j.t.write_inbox(&rows);
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .env("TMPDIR", p(&j.tmproot))
        .args(["--testmode", "synthetic-test", "friction", "suggest", "--text", "cache", "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    // The BM25 lines of the same run with the Jev flags left off.
    let mut pc = base();
    pc.env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .args(["--testmode", "synthetic-test", "friction", "suggest", "--text", "cache", "--config", p(&cfg)])
        .stdin(Stdio::null());
    let plain = pc.output().unwrap();
    assert_eq!(code(&plain), 0, "{}", err(&plain));
    assert!(out(&plain).starts_with("suggest: rank=1 group=t-1"), "{}", out(&plain));
    check_out(&o, &out(&plain), "jev: skipped reason=scan-refused\n");
    assert!(!j.ran());
    assert!(j.tmproot_empty());
}

#[test]
fn a_missing_key_file_skips_no_key() {
    let j = J::new("answer");
    fs::remove_file(&j.key).unwrap();
    assert_skip_before_run(&j, &rows3(), &Q3, &[], "no-key", false);
}

#[test]
fn a_key_file_with_mode_000_still_runs() {
    let j = J::new("answer");
    fs::set_permissions(&j.key, fs::Permissions::from_mode(0o000)).unwrap();
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    assert_whole(&j, &rows3(), &Q3, &[], "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
}

#[test]
fn a_key_link_runs_and_a_dangling_key_link_skips() {
    let j = J::new("answer");
    let link = j.t.0.join("key-link");
    symlink(&j.key, &link).unwrap();
    let cfg = j.t.config();
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    j.t.write_inbox(&rows3());
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--text", "cache warm zeta", "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&link), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
    // A link to nothing names no regular file, and nothing runs.
    let j2 = J::new("answer");
    let dangling = j2.t.0.join("key-nothing");
    symlink(j2.t.0.join("not-there"), &dangling).unwrap();
    let cfg2 = j2.t.config();
    j2.t.write_inbox(&rows3());
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .env("TMPDIR", p(&j2.tmproot))
        .args(["friction", "suggest", "--text", "cache warm zeta", "--config", p(&cfg2), "--jev-send", p(&j2.send()), "--jev-key-file", p(&dangling), "--jev-ledger-dir", p(&j2.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_jev(&j2.t, &rows3(), &Q3, &[], &o, "jev: skipped reason=no-key\n");
    assert!(!j2.ran());
    assert!(j2.tmproot_empty());
    assert!(!j2.ledger.exists());
}

#[test]
fn a_ledger_path_that_is_a_file_skips_ledger() {
    let j = J::new("answer");
    fs::write(&j.ledger, "a file").unwrap();
    assert_skip_before_run(&j, &rows3(), &Q3, &[], "ledger", true);
}

#[test]
fn a_ledger_path_whose_parent_is_missing_skips_ledger_and_makes_nothing() {
    let j = J::new("answer");
    let deep = j.t.0.join("no-parent").join("led");
    let cfg = j.t.config();
    j.t.write_inbox(&rows3());
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST_SECRET", DENY)
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--text", "cache warm zeta", "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&deep)])
        .stdin(Stdio::null());
    let bm25 = bm25_of(&j.t, &rows3(), &Q3, &[]);
    let o = c.output().unwrap();
    check_out(&o, &bm25, "jev: skipped reason=ledger\n");
    assert!(!j.ran());
    assert!(j.tmproot_empty());
    assert!(!deep.exists() && !deep.parent().unwrap().exists(), "nothing was made");
}

#[test]
fn a_missing_ledger_folder_is_made_with_mode_0700() {
    let j = J::new("answer");
    assert!(!j.ledger.exists());
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    assert_whole(&j, &rows3(), &Q3, &[], "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
    let mode = fs::metadata(&j.ledger).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);
    // The engine did not make the ledger file itself.
    assert!(fs::read_dir(&j.ledger).unwrap().count() == 0);
}

#[test]
fn the_checks_before_the_run_run_in_order() {
    let fence: String = ["`", "`", "`"].concat();
    // code in --text beats a missing key file.
    let j = J::new("answer");
    fs::remove_file(&j.key).unwrap();
    assert_skip_before_run(&j, &rows3(), &["--text", &format!("cache {fence}")], &[], "code-skipped", false);
    // a request over the cap that also holds the scan hit prints payload.
    let j = J::new("answer");
    let phrase_list = j.t.file("phrase-list.txt", format!("{DENY}\nwhy: plover\n"));
    let rows = vec![
        row("o-1", "2026-11-01", "", "cache thing mko1", &[("why", "plover nests here")]),
        row("o-2", "2026-11-02", "", "other mko2", &[]),
        row("o-3", "2026-11-03", "", "third mko3", &[]),
    ];
    let text = format!("cache {}", "x".repeat(200500));
    let cfg = j.t.config();
    j.t.write_inbox(&rows);
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST", p(&phrase_list))
        .env_remove("HARNESS_PRIVACY_LIST_SECRET")
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--text", &text, "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_jev(&j.t, &rows, &["--text", &text], &[], &o, "jev: skipped reason=payload\n");
    assert!(!j.ran());
    // the scan hit beats a missing key file.
    let j = J::new("answer");
    fs::remove_file(&j.key).unwrap();
    let cfg = j.t.config();
    j.t.write_inbox(&rows);
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST", p(&phrase_list))
        .env_remove("HARNESS_PRIVACY_LIST_SECRET")
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--text", "cache plover", "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_jev(&j.t, &rows, &["--text", "cache plover"], &[], &o, "jev: skipped reason=scan-refused\n");
    assert!(!j.ran());
    // code in --text beats an inbox of one group.
    let j = J::new("answer");
    assert_skip_before_run(&j, &[row("solo-1", "2026-11-01", "", "cache solo", &[])], &["--text", &format!("cache {fence}")], &[], "code-skipped", false);
    // an inbox of one group beats a request that would be over the cap.
    let j = J::new("answer");
    let text = format!("cache {}", "x".repeat(200500));
    assert_skip_before_run(&j, &[row("solo-1", "2026-11-01", "", "cache solo", &[])], &["--text", &text], &[], "no-candidates", false);
    // a missing key file beats a ledger path that is a file.
    let j = J::new("answer");
    fs::remove_file(&j.key).unwrap();
    fs::write(&j.ledger, "a file").unwrap();
    assert_skip_before_run(&j, &rows3(), &Q3, &[], "no-key", true);
    // a ledger path that is a file beats a temp root that is a regular file.
    let j = J::new("answer");
    fs::write(&j.ledger, "a file").unwrap();
    fs::remove_dir_all(&j.tmproot).unwrap();
    fs::write(&j.tmproot, "a file").unwrap();
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=ledger\n");
    assert!(!j.ran());
}

#[test]
fn a_temp_root_that_is_a_regular_file_gives_spawn_failed() {
    let j = J::new("answer");
    fs::remove_dir_all(&j.tmproot).unwrap();
    fs::write(&j.tmproot, "a file").unwrap();
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=spawn-failed\n");
    assert!(!j.ran());
}

// ---------------------------------------------------------------- the run

#[test]
fn the_program_gets_exactly_the_eight_arguments() {
    // The fake writes no answer file, so the run ends as bad-answer after the program has been started.
    let j = J::new("no-file");
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=bad-answer\n");
    assert!(j.tmproot_empty(), "the temp folder is removed after the run");
    let a = j.args();
    assert_eq!(a.len(), 8, "{a:?}");
    assert_eq!(a[0], "--payload");
    assert!(a[1].ends_with("/request.json"));
    assert_eq!(a[2], "--surface");
    assert_eq!(a[3], "prose");
    assert_eq!(a[4], "--spend-ledger");
    let ledger_arg = &a[5];
    let base = ledger_arg.rsplit('/').next().unwrap();
    let rest = ledger_arg.strip_suffix(base).unwrap();
    assert_eq!(rest, format!("{}/", j.ledger.to_str().unwrap()));
    assert!(base.starts_with("spend-friction-suggest-") && base.ends_with(".jsonl"), "{base}");
    let date = base.trim_start_matches("spend-friction-suggest-").trim_end_matches(".jsonl");
    let d = date.as_bytes();
    assert!(d.len() == 10 && d[4] == b'-' && d[7] == b'-' && d[0..4].iter().all(|c| c.is_ascii_digit()) && d[5..7].iter().all(|c| c.is_ascii_digit()) && d[8..10].iter().all(|c| c.is_ascii_digit()), "{date}");
    assert_eq!(a[6], "--out");
    assert!(a[7].ends_with("/answer.json"));
    // The current folder is the temp folder, and TMPDIR is the same folder.
    let cwd = j.note("cwd").trim().to_string();
    assert_eq!(Path::new(&cwd), Path::new(a[1].as_str()).parent().unwrap());
    assert!(j.env_text().contains(&format!("TMPDIR={cwd}")));
}

#[test]
fn the_temp_folder_and_request_file_have_private_modes() {
    let j = J::new("no-file");
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=bad-answer\n");
    assert!(j.tmproot_empty());
    assert!(j.note("dir-mode").starts_with("drwx------"), "{}", j.note("dir-mode"));
    assert!(j.note("request-mode").starts_with("-rw-------"), "{}", j.note("request-mode"));
}

#[test]
fn the_child_environment_is_cleared_but_for_the_named_values() {
    let j = J::new("no-file");
    let list = j.t.file("list.txt", format!("{DENY}\n"));
    let cfg = j.t.config();
    j.t.write_inbox(&rows3());
    let mut c = base();
    c.env("HARNESS_PRIVACY_LIST", p(&list))
        .env("HARNESS_CONFIG", "/not/a/real/config")
        .env("ZZ_PLANTED", "zz")
        .env("TMPDIR", p(&j.tmproot))
        .args(["friction", "suggest", "--text", "cache warm zeta", "--config", p(&cfg), "--jev-send", p(&j.send()), "--jev-key-file", p(&j.key), "--jev-ledger-dir", p(&j.ledger)])
        .stdin(Stdio::null());
    let o = c.output().unwrap();
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "jev: skipped reason=bad-answer\n");
    assert!(j.tmproot_empty());
    let allowed =["PATH", "HOME", "TMPDIR", "LANG", "LC_ALL", "OPENROUTER_KEY_FILE", "PWD", "OLDPWD", "SHLVL", "_"];
    for name in j.env_names() {
        assert!(allowed.contains(&name.as_str()), "the name {name} reached the program");
    }
    assert_eq!(j.note("key-file"), format!("{}\n", j.key.to_str().unwrap()));
}

#[test]
fn path_home_and_lang_pass_through_and_an_unset_lc_all_is_not_made() {
    let j = J::new("no-file");
    j.t.write_inbox(&rows3());
    let mut c = jev_cmd(&j);
    c.env("PATH", "/synthetic/path:/usr/bin:/bin").env("HOME", "/synthetic/home").env("LANG", "xx_XX").env_remove("LC_ALL");
    let o = c.args(Q3).output().unwrap();
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "jev: skipped reason=bad-answer\n");
    assert!(j.tmproot_empty());
    let env = j.env_text();
    assert!(env.contains("PATH=/synthetic/path:/usr/bin:/bin\n"), "{env}");
    assert!(env.contains("HOME=/synthetic/home\n"), "{env}");
    assert!(env.contains("LANG=xx_XX\n"), "{env}");
    assert!(!env.lines().any(|l| l.starts_with("LC_ALL=")), "{env}");
}

#[test]
fn a_hang_leaves_nothing_in_the_temp_root_after_the_deadline() {
    let j = J::new("hang");
    let started = Instant::now();
    // The deadline leaves the fake time to write its file first; the fake lists its TMPDIR to a note, so the
    // test sees that the file it checks was there before the kill.
    let extra = ["--jev-deadline-ms", "5000"];
    let o = jev_run(&j, &rows3(), &Q3, &extra);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_jev(&j.t, &rows3(), &Q3, &extra, &o, "jev: skipped reason=timeout\n");
    assert!(j.note("tmpdir-listing").contains("left-by-fake"), "the fake wrote its file before it hung");
    assert!(j.tmproot_empty(), "the file the fake wrote into its TMPDIR is gone with the folder");
}

#[test]
fn a_big_stderr_with_exit_10_and_http_402_gives_no_credit_fast() {
    let j = J::new("exit10");
    let mut big = String::new();
    // 41 bytes a line: 25600 lines are over 1 MiB before the last line.
    for _ in 0..25600 {
        big.push_str("filler line of stderr text zz padding zz\n");
    }
    assert!(big.len() >= 1048576);
    big.push_str("send.sh: HTTP 402\n");
    j.stderr_text(&big);
    let started = Instant::now();
    let o = jev_run(&j, &rows3(), &Q3, &[]);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "jev: skipped reason=no-credit\n");
    assert!(j.tmproot_empty());
}

#[test]
fn a_program_with_mode_644_gives_spawn_failed() {
    let j = J::new("answer");
    fs::set_permissions(j.send(), fs::Permissions::from_mode(0o644)).unwrap();
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=spawn-failed\n");
    assert!(!j.ran());
    assert!(j.tmproot_empty());
}

#[test]
fn a_program_path_that_is_not_there_gives_spawn_failed() {
    let j = J::new("answer");
    fs::remove_file(j.send()).unwrap();
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=spawn-failed\n");
    assert!(j.tmproot_empty());
}

// ---------------------------------------------------------------- outcomes by exit code

fn assert_outcome(case: &str, stderr_text: &str, token: &str) {
    let j = J::new(case);
    j.stderr_text(stderr_text);
    assert_whole(&j, &rows3(), &Q3, &[], &format!("jev: skipped reason={token}\n"));
    assert!(j.tmproot_empty());
}

#[test]
fn exit_5_is_no_key() {
    assert_outcome("exit5", "send.sh: key file missing\n", "no-key");
}

#[test]
fn exit_7_is_scan_refused() {
    assert_outcome("exit7", "send.sh: payload refused by policy\n", "scan-refused");
}

#[test]
fn exit_8_with_the_cap_text_is_cap() {
    assert_outcome("exit8", "send.sh: SPEND-CAP reached (calls=200 total_usd=0.05)\n", "cap");
}

#[test]
fn exit_8_with_the_ledger_text_is_ledger() {
    assert_outcome("exit8", "send.sh: SPEND-CAP ledger unwritable (fail closed)\n", "ledger");
}

#[test]
fn exit_9_is_transport() {
    assert_outcome("exit9", "send.sh: connection reset by peer\n", "transport");
}

#[test]
fn exit_10_with_http_402_is_no_credit() {
    assert_outcome("exit10", "send.sh: HTTP 402\n", "no-credit");
}

#[test]
fn exit_10_with_http_500_is_http_500() {
    assert_outcome("exit10", "send.sh: HTTP 500\n", "http-500");
}

#[test]
fn exit_10_uses_the_last_http_line() {
    assert_outcome("exit10", "x: HTTP 500\nsend.sh: HTTP 402\n", "no-credit");
}

#[test]
fn exit_10_with_no_http_line_is_http_unknown() {
    assert_outcome("exit10", "send.sh: something odd happened\n", "http-unknown");
}

#[test]
fn exit_10_with_a_four_digit_code_is_http_unknown() {
    assert_outcome("exit10", "HTTP 4021\n", "http-unknown");
}

#[test]
fn exit_11_is_ledger() {
    assert_outcome("exit11", "send.sh: ledger append failed\n", "ledger");
}

#[test]
fn exit_12_is_ledger() {
    assert_outcome("exit12", "send.sh: ledger fsync failed\n", "ledger");
}

#[test]
fn exit_6_is_send_6() {
    assert_outcome("exit6", "send.sh: unexpected state\n", "send-6");
}

#[test]
fn exit_3_is_send_3() {
    assert_outcome("exit3", "send.sh: bad arguments\n", "send-3");
}

#[test]
fn a_signal_end_is_send_signal() {
    let j = J::new("signal");
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=send-signal\n");
    assert!(j.tmproot_empty());
}

#[test]
fn a_marker_in_the_program_output_reaches_neither_verb_stream() {
    let marker = "ZZMARKERZZ";
    let j = J::new("exit10");
    j.stderr_text(&format!("{marker}\nsend.sh: HTTP 500\n"));
    let o = jev_run(&j, &rows3(), &Q3, &[]);
    assert!(!out(&o).contains(marker) && !err(&o).contains(marker));
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "jev: skipped reason=http-500\n");
}

// ---------------------------------------------------------------- the answer

/// Run the 5-candidate fixture with the given probabilities (per letter, in A..E order) and return the output.
fn run5(j: &J, probs: &[(&str, &str)], usage: Option<&str>) -> Output {
    let letter_p1 = discover_letter(j, &rows5(), &Q5, &[], "mkp1");
    let _ = letter_p1;
    j.answer(&answer_json(probs, usage));
    jev_run(j, &rows5(), &Q5, &[])
}

/// One discovery run, then the group of the 5-candidate fixture that carries the letter `letter`.
fn discover_group5(j: &J, letter: &str) -> String {
    let _ = jev_run(j, &rows5(), &Q5, &[]);
    group_of5(&j.request(), letter)
}

#[test]
fn a_pick_in_the_middle_prints_its_group_and_row() {
    let j = J::new("answer");
    // C highest, A and E lower: the pick names the group whose letter is C.
    let group_c = discover_group5(&j, "C");
    let o = run5(&j, &[("A", "0.10"), ("B", "0.20"), ("C", "0.90"), ("D", "0.30"), ("E", "0.10")], None);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, &format!("suggest: jev pick={group_c} row={group_c} p=0.90\n"));
}

#[test]
fn the_highest_value_shared_by_two_letters_goes_to_the_earlier_letter() {
    let j = J::new("answer");
    // D is written first in the file, B shares the highest value: B wins.
    let group_b = discover_group5(&j, "B");
    let o = run5(&j, &[("D", "0.80"), ("B", "0.80"), ("A", "0.10"), ("C", "0.10"), ("E", "0.10")], None);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, &format!("suggest: jev pick={group_b} row={group_b} p=0.80\n"));
}

#[test]
fn the_choice_field_is_not_read_for_the_pick() {
    let j = J::new("answer");
    let group_b = discover_group5(&j, "B");
    j.answer(&answer_json(&[("A", "0.10"), ("B", "0.90"), ("C", "0.10"), ("D", "0.10"), ("E", "0.10")], None).replace("\"choice\":\"A\"", "\"choice\":\"E\""));
    let o = jev_run(&j, &rows5(), &Q5, &[]);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, &format!("suggest: jev pick={group_b} row={group_b} p=0.90\n"));
}

#[test]
fn whole_number_values_one_and_zero_are_numbers() {
    let j = J::new("answer");
    let group_a = discover_group5(&j, "A");
    let o = run5(&j, &[("A", "1"), ("B", "0"), ("C", "0"), ("D", "0"), ("E", "0")], None);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, &format!("suggest: jev pick={group_a} row={group_a} p=1.00\n"));
}

#[test]
fn an_exponent_form_prints_two_decimals() {
    let j = J::new("answer");
    let group_e = discover_group5(&j, "E");
    let o = run5(&j, &[("A", "0.1"), ("B", "0.1"), ("C", "0.1"), ("D", "0.1"), ("E", "2.5e-1")], None);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, &format!("suggest: jev pick={group_e} row={group_e} p=0.25\n"));
}

/// The five asked letters of the 5-candidate fixture, as a probabilities map with one letter changed.
fn five(j: &J, changed: &[(&str, &str)]) -> Vec<(String, String)> {
    let _ = jev_run(j, &rows5(), &Q5, &[]);
    let keys = criteria_pairs(&j.request()).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
    keys.into_iter()
        .map(|k| {
            if let Some((_, v)) = changed.iter().find(|(c, _)| *c == k) {
                (k, v.to_string())
            } else {
                (k, "0.1".to_string())
            }
        })
        .collect()
}

fn assert_bad_answer(j: &J, probs: Vec<(String, String)>) {
    let owned: Vec<(&str, &str)> = probs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let o = run5(j, &owned, None);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, "jev: skipped reason=bad-answer\n");
}

/// A run on the 5-candidate fixture with the given raw answer file body: the whole stdout is the BM25 lines and
/// then the given Jev line.
fn assert_raw_answer(j: &J, body: &str, jev_line: &str) {
    let _ = discover_letter(j, &rows5(), &Q5, &[], "mkp1");
    j.answer(body);
    let o = jev_run(j, &rows5(), &Q5, &[]);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, jev_line);
}

#[test]
fn a_missing_letter_is_bad_answer() {
    let j = J::new("answer");
    let _ = discover_letter(&j, &rows5(), &Q5, &[], "mkp1");
    let mut probs = five(&j, &[]);
    probs.remove(0);
    assert_bad_answer(&j, probs);
}

#[test]
fn an_extra_letter_is_bad_answer() {
    let j = J::new("answer");
    let mut probs = five(&j, &[]);
    probs.push(("F".to_string(), "0.5".to_string()));
    assert_bad_answer(&j, probs);
}

#[test]
fn the_wrong_key_set_is_bad_answer() {
    let j = J::new("answer");
    let mut probs = five(&j, &[("E", "0.5")]);
    assert_eq!(probs.len(), 5);
    probs[4] = ("Z".to_string(), "0.5".to_string());
    assert_bad_answer(&j, probs);
}

#[test]
fn lower_case_keys_are_bad_answer() {
    let j = J::new("answer");
    let probs: Vec<(String, String)> = five(&j, &[]).into_iter().map(|(k, _)| (k.to_lowercase(), "0.5".to_string())).collect();
    assert_bad_answer(&j, probs);
}

#[test]
fn a_key_with_a_blank_after_the_letter_is_bad_answer() {
    let j = J::new("answer");
    let mut probs = five(&j, &[]);
    probs[0] = ("A ".to_string(), "0.5".to_string());
    assert_bad_answer(&j, probs);
}

#[test]
fn a_parenthesised_key_is_bad_answer() {
    let j = J::new("answer");
    let mut probs = five(&j, &[]);
    probs[0] = ("(A)".to_string(), "0.5".to_string());
    assert_bad_answer(&j, probs);
}

#[test]
fn a_value_over_one_is_bad_answer() {
    let j = J::new("answer");
    assert_bad_answer(&j, five(&j, &[("A", "1.5")]));
}

#[test]
fn a_negative_value_is_bad_answer() {
    let j = J::new("answer");
    assert_bad_answer(&j, five(&j, &[("A", "-0.1")]));
}

#[test]
fn a_text_value_is_bad_answer() {
    let j = J::new("answer");
    assert_bad_answer(&j, five(&j, &[("A", "\"0.5\"")]));
}

#[test]
fn an_empty_probabilities_object_is_bad_answer() {
    let j = J::new("answer");
    assert_raw_answer(&j, "{\"answers\":{\"group\":{\"type\":\"choice\",\"probabilities\":{}}}}", "jev: skipped reason=bad-answer\n");
}

#[test]
fn an_empty_answer_file_is_bad_answer() {
    let j = J::new("answer");
    assert_raw_answer(&j, "", "jev: skipped reason=bad-answer\n");
}

#[test]
fn no_answer_file_is_bad_answer() {
    let j = J::new("no-file");
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=bad-answer\n");
}

#[test]
fn the_text_of_a_letter_alone_is_bad_answer() {
    let j = J::new("answer");
    assert_raw_answer(&j, "A", "jev: skipped reason=bad-answer\n");
}

#[test]
fn all_values_equal_is_flat() {
    let j = J::new("answer");
    let o = run5(&j, &[("A", "0.2"), ("B", "0.2"), ("C", "0.2"), ("D", "0.2"), ("E", "0.2")], None);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, "jev: skipped reason=flat\n");
}

#[test]
fn all_values_zero_is_flat() {
    let j = J::new("answer");
    let o = run5(&j, &[("A", "0"), ("B", "0"), ("C", "0"), ("D", "0"), ("E", "0")], None);
    assert_jev(&j.t, &rows5(), &Q5, &[], &o, "jev: skipped reason=flat\n");
}

#[test]
fn an_infinite_value_is_bad_answer() {
    let j = J::new("answer");
    assert_bad_answer(&j, five(&j, &[("A", "1e999")]));
}

#[test]
fn a_bare_nan_word_is_bad_answer() {
    let j = J::new("answer");
    assert_bad_answer(&j, five(&j, &[("A", "NaN")]));
}

#[test]
fn a_true_value_is_bad_answer() {
    let j = J::new("answer");
    assert_bad_answer(&j, five(&j, &[("A", "true")]));
}

#[test]
fn a_null_value_is_bad_answer() {
    let j = J::new("answer");
    assert_bad_answer(&j, five(&j, &[("A", "null")]));
}

#[test]
fn a_key_given_twice_is_bad_answer() {
    let j = J::new("answer");
    let _ = discover_letter(&j, &rows5(), &Q5, &[], "mkp1");
    let keys: Vec<String> = criteria_pairs(&j.request()).into_iter().map(|(k, _)| k).collect();
    let mut body = String::from("{\"answers\":{\"group\":{\"type\":\"choice\",\"probabilities\":{");
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            body.push(',');
        }
        body.push_str(&format!("\"{k}\":0.1"));
    }
    body.push_str(&format!(",\"{}\":0.9", keys[0]));
    body.push_str("}}}}");
    assert_raw_answer(&j, &body, "jev: skipped reason=bad-answer\n");
}

#[test]
fn an_answer_path_that_is_a_link_is_bad_answer() {
    let j = J::new("answer-link");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    let real = j.dir.join("real-answer.json");
    fs::write(&real, answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None)).unwrap();
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=bad-answer\n");
}

#[test]
fn an_answer_of_exactly_one_mib_is_read_and_one_byte_over_is_not() {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    let mut base = answer_json(&[(letter.as_str(), "0.7"), ("B", "0.1"), ("C", "0.1")], None);
    let pad = 1048576 - base.len();
    base.push_str(&" ".repeat(pad));
    assert_eq!(base.len(), 1048576);
    j.answer(&base);
    assert_whole(&j, &rows3(), &Q3, &[], "suggest: jev pick=ga-1 row=ga-1 p=0.70\n");
    base.push(' ');
    j.answer(&base);
    assert_whole(&j, &rows3(), &Q3, &[], "jev: skipped reason=bad-answer\n");
}

#[test]
fn a_changed_answer_file_changes_the_pick() {
    let j = J::new("answer");
    let _ = jev_run(&j, &rows5(), &Q5, &[]);
    let req = j.request();
    let group_of = |letter: &str| group_of5(&req, letter);
    let keys = criteria_pairs(&req).into_iter().map(|(k, _)| k).collect::<Vec<_>>();
    let hi = keys[0].clone();
    let lo = keys[4].clone();
    j.answer(&answer_json(&[(hi.as_str(), "0.9"), (lo.as_str(), "0.1"), (keys[1].as_str(), "0.1"), (keys[2].as_str(), "0.1"), (keys[3].as_str(), "0.1")], None));
    let first = jev_run(&j, &rows5(), &Q5, &[]);
    j.answer(&answer_json(&[(hi.as_str(), "0.1"), (lo.as_str(), "0.9"), (keys[1].as_str(), "0.1"), (keys[2].as_str(), "0.1"), (keys[3].as_str(), "0.1")], None));
    let second = jev_run(&j, &rows5(), &Q5, &[]);
    let g_hi = group_of(&hi);
    let g_lo = group_of(&lo);
    assert_ne!(g_hi, g_lo);
    assert_jev(&j.t, &rows5(), &Q5, &[], &first, &format!("suggest: jev pick={g_hi} row={g_hi} p=0.90\n"));
    assert_jev(&j.t, &rows5(), &Q5, &[], &second, &format!("suggest: jev pick={g_lo} row={g_lo} p=0.90\n"));
    assert_ne!(out(&first), out(&second));
}

// ---------------------------------------------------------------- the deadline

#[test]
fn the_deadline_kills_the_whole_group() {
    let j = J::new("hang-bg");
    let started = Instant::now();
    // 3000, not the 300 of the ticket: the first start of a fresh script copy took over 300 ms here, before the fake wrote its note.
    let extra = ["--jev-deadline-ms", "3000"];
    let o = jev_run(&j, &rows3(), &Q3, &extra);
    assert!(started.elapsed() < Duration::from_secs(10), "the verb ended within 10 s");
    assert_jev(&j.t, &rows3(), &Q3, &extra, &o, "jev: skipped reason=timeout\n");
    let pid: u32 = j.note("bg-pid").trim().parse().expect("the file with the id of the background sleep");
    assert!(dead_within(pid, 2), "the background sleep is not alive within 2 s");
    assert!(j.tmproot_empty());
}

#[test]
fn a_deadline_that_does_not_fire_leaves_a_pick() {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    assert_whole(&j, &rows3(), &Q3, &["--jev-deadline-ms", "2000"], "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
}

#[test]
fn a_grandchild_holding_stderr_does_not_hold_the_pick() {
    let j = J::new("bg-answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    let started = Instant::now();
    let o = jev_run(&j, &rows3(), &Q3, &[]);
    assert!(started.elapsed() < Duration::from_secs(10));
    let pid: u32 = j.note("bg-pid").trim().parse().unwrap();
    // The test ends the background sleep itself before it asserts.
    let _ = Command::new("/bin/kill").args(["-KILL", &pid.to_string()]).status();
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
}

#[test]
fn with_no_deadline_flag_a_two_second_program_still_picks() {
    let j = J::new("delay-answer");
    fs::write(j.dir.join("sleep-secs"), "2").unwrap();
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    let started = Instant::now();
    let o = jev_run(&j, &rows3(), &Q3, &[]);
    assert!(started.elapsed() >= Duration::from_secs(2));
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
}

// ---------------------------------------------------------------- the record

#[test]
fn a_pick_appends_the_second_record_line() {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.7"), ("B", "0.1"), ("C", "0.1")], Some("\"cost\":0.000145194")));
    let rec = j.t.0.join("suggest.jsonl");
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_jev(&j.t, &rows3(), &Q3, &["--record", p(&rec)], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.70\n");
    let body = fs::read_to_string(&rec).unwrap();
    let lines: Vec<&str> = body.lines().collect();
    assert_eq!(lines.len(), 2, "{body}");
    let ts = lines[0].split("\"ts\":\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let sha = lines[0].split("\"query_sha256\":\"").nth(1).unwrap().split('"').next().unwrap().to_string();
    let want = format!(
        "{{\"ts\":\"{ts}\",\"query_sha256\":\"{sha}\",\"via\":\"jev\",\"status\":\"pick\",\"reason\":\"\",\"letter\":\"{letter}\",\"group\":\"ga-1\",\"p\":0.70,\"cost_usd\":0.000145,\"latency_ms\":",
    );
    assert!(lines[1].starts_with(&want), "{}\nvs\n{want}", lines[1]);
    assert!(lines[1].ends_with("}"));
    // The first line is the line a run with no --jev flag writes for the same input (ts cut out).
    let rec2 = j.t.0.join("plain.jsonl");
    let _ = plain_run(&j.t, &rows3(), &Q3, &["--record", p(&rec2)]);
    let plain_line = fs::read_to_string(&rec2).unwrap().lines().next().unwrap().to_string();
    let cut = |l: &str| l.split("\"query_sha256\"").nth(1).unwrap().to_string();
    assert_eq!(cut(lines[0]), cut(&plain_line));
    // A second run gives four lines in total.
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_jev(&j.t, &rows3(), &Q3, &["--record", p(&rec)], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.70\n");
    assert_eq!(fs::read_to_string(&rec).unwrap().lines().count(), 4);
}

/// A pick whose answer holds the given raw usage.cost value: the second record line must book the estimate.
fn assert_bad_usage_cost_books_the_estimate(cost: &str) {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], Some(&format!("\"cost\":{cost}"))));
    let rec = j.t.0.join("suggest.jsonl");
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_jev(&j.t, &rows3(), &Q3, &["--record", p(&rec)], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
    let body = fs::read_to_string(&rec).unwrap();
    assert!(body.contains("\"cost_usd\":0.001400"), "{cost}: {body}");
    assert!(body.contains(&format!("\"letter\":\"{letter}\"")), "{cost}: {body}");
}

#[test]
fn a_usage_cost_given_as_a_text_books_the_estimate() {
    assert_bad_usage_cost_books_the_estimate("\"0.5\"");
}

#[test]
fn a_usage_cost_of_1e999_books_the_estimate() {
    assert_bad_usage_cost_books_the_estimate("1e999");
}

#[test]
fn a_usage_cost_of_7_books_the_estimate() {
    assert_bad_usage_cost_books_the_estimate("7");
}

#[test]
fn a_bad_answer_with_a_good_usage_cost_books_that_cost() {
    let j = J::new("answer");
    // The letter C is missing from probabilities: bad-answer; usage.cost is a good number, so it is booked.
    j.answer(&answer_json(&[("A", "0.9"), ("B", "0.1")], Some("\"cost\":0.000145194")));
    let rec = j.t.0.join("suggest.jsonl");
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_jev(&j.t, &rows3(), &Q3, &["--record", p(&rec)], &o, "jev: skipped reason=bad-answer\n");
    let body = fs::read_to_string(&rec).unwrap();
    let second = body.lines().nth(1).unwrap();
    assert!(second.contains("\"status\":\"skipped\",\"reason\":\"bad-answer\""), "{second}");
    assert!(second.contains("\"p\":null,\"cost_usd\":0.000145,"), "{second}");
}

#[test]
fn a_no_credit_skip_books_the_estimate_and_a_null_p() {
    let j = J::new("exit10");
    j.stderr_text("send.sh: HTTP 402\n");
    let rec = j.t.0.join("suggest.jsonl");
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_jev(&j.t, &rows3(), &Q3, &["--record", p(&rec)], &o, "jev: skipped reason=no-credit\n");
    let body = fs::read_to_string(&rec).unwrap();
    let second = body.lines().nth(1).unwrap();
    let ts =body.lines().next().unwrap().split("\"ts\":\"").nth(1).unwrap().split('"').next().unwrap();
    let sha = body.lines().next().unwrap().split("\"query_sha256\":\"").nth(1).unwrap().split('"').next().unwrap();
    assert_eq!(
        second.split(",\"latency_ms\"").next().unwrap(),
        format!("{{\"ts\":\"{ts}\",\"query_sha256\":\"{sha}\",\"via\":\"jev\",\"status\":\"skipped\",\"reason\":\"no-credit\",\"letter\":\"\",\"group\":\"\",\"p\":null,\"cost_usd\":0.001400")
    );
}

#[test]
fn a_skip_before_the_run_books_zero_cost_and_zero_latency() {
    let j = J::new("answer");
    fs::remove_file(&j.key).unwrap();
    let rec = j.t.0.join("suggest.jsonl");
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_jev(&j.t, &rows3(), &Q3, &["--record", p(&rec)], &o, "jev: skipped reason=no-key\n");
    let body = fs::read_to_string(&rec).unwrap();
    let second = body.lines().nth(1).unwrap();
    assert!(second.contains("\"reason\":\"no-key\""), "{second}");
    assert!(second.contains("\"cost_usd\":0.000000"), "{second}");
    assert!(second.ends_with("\"latency_ms\":0}"), "{second}");
}

#[test]
fn a_timeout_books_the_estimate() {
    let j = J::new("hang");
    let rec = j.t.0.join("suggest.jsonl");
    let extra = ["--record", p(&rec), "--jev-deadline-ms", "300"];
    let o = jev_run(&j, &rows3(), &Q3, &extra);
    assert_jev(&j.t, &rows3(), &Q3, &extra, &o, "jev: skipped reason=timeout\n");
    let second = fs::read_to_string(&rec).unwrap().lines().nth(1).unwrap().to_string();
    assert!(second.contains("\"reason\":\"timeout\""), "{second}");
    assert!(second.contains("\"cost_usd\":0.001400"), "{second}");
}

#[test]
fn no_record_line_holds_row_text_or_query_text() {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.7"), ("B", "0.1"), ("C", "0.1")], None));
    let rec = j.t.0.join("suggest.jsonl");
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_jev(&j.t, &rows3(), &Q3, &["--record", p(&rec)], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.70\n");
    let body = fs::read_to_string(&rec).unwrap();
    assert!(!body.contains("cache") && !body.contains("warm") && !body.contains("mkga"), "{body}");
}

#[test]
fn a_failed_second_record_write_is_exit_2_after_the_bm25_lines() {
    let j = J::new("record-dir");
    let rec = j.t.0.join("suggest.jsonl");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    // The plain baseline first, then arm the record-dir case for the Jev run.
    let plain = plain_run(&j.t, &rows3(), &Q3, &["--record", p(&rec)]);
    fs::write(&rec, "").unwrap();
    fs::write(j.dir.join("record-path"), p(&rec)).unwrap();
    let o = jev_run(&j, &rows3(), &Q3, &["--record", p(&rec)]);
    assert_eq!(code(&o), 2);
    assert_eq!(out(&o), out(&plain), "the BM25 lines only");
    assert_eq!(err(&o), "harness: error: the suggest record cannot be written\n");
    assert!(j.tmproot_empty());
}

#[test]
fn with_no_record_flag_no_record_file_is_made() {
    let j = J::new("answer");
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    let o = jev_run(&j, &rows3(), &Q3, &[]);
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
    assert!(!j.t.0.join("suggest.jsonl").exists());
}

// ---------------------------------------------------------------- privacy

#[test]
fn the_engine_sources_hold_no_socket_and_no_url_needle() {
    let needles = [
        ["std", "::", "net"].concat(),
        ["Tcp", "Stream"].concat(),
        ["Udp", "Socket"].concat(),
        ["Tcp", "Listener"].concat(),
        ["htt", "p://"].concat(),
        ["https", "://"].concat(),
    ];
    // A needle built wrong must fail the test: each one hits the sample.
    let sample = "x std::net y TcpStream z UdpSocket w TcpListener v http:// u https://";
    for n in &needles {
        assert!(sample.contains(n.as_str()), "the needle {n} is built wrong");
    }
    for f in ["src/friction_suggest.rs", "src/friction_suggest_jev.rs"] {
        let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(f);
        let text = fs::read_to_string(&p).unwrap_or_default();
        assert!(!text.is_empty(), "{f} is missing or empty");
        for n in &needles {
            assert!(!text.contains(n.as_str()), "{f} holds a socket or url needle");
        }
    }
}

#[test]
fn the_inbox_is_byte_identical_after_pick_skip_and_timeout() {
    let j = J::new("answer");
    j.t.write_inbox(&rows3());
    let before = fs::read(j.t.inbox()).unwrap();
    let letter = discover_letter(&j, &rows3(), &Q3, &[], "mkga");
    j.answer(&answer_json(&[(letter.as_str(), "0.9"), ("B", "0.1"), ("C", "0.1")], None));
    let o = jev_run(&j, &rows3(), &Q3, &[]);
    // The inbox is read right after the run, before the plain run of the assert writes the same rows again.
    assert_eq!(fs::read(j.t.inbox()).unwrap(), before, "after a pick");
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "suggest: jev pick=ga-1 row=ga-1 p=0.90\n");
    fs::remove_file(&j.key).unwrap();
    let o = jev_run(&j, &rows3(), &Q3, &[]);
    assert_eq!(fs::read(j.t.inbox()).unwrap(), before, "after a skip before the run");
    assert_jev(&j.t, &rows3(), &Q3, &[], &o, "jev: skipped reason=no-key\n");
    let j2 = J::new("hang");
    j2.t.write_inbox(&rows3());
    let before2 = fs::read(j2.t.inbox()).unwrap();
    let extra = ["--jev-deadline-ms", "300"];
    let o = jev_run(&j2, &rows3(), &Q3, &extra);
    assert_eq!(fs::read(j2.t.inbox()).unwrap(), before2, "after a timeout");
    assert_jev(&j2.t, &rows3(), &Q3, &extra, &o, "jev: skipped reason=timeout\n");
}
