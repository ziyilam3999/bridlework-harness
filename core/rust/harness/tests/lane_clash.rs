//! The `clash` refusal of `harness lane claim`: a live ticket of the same repo with an overlapping scope entry. Real binary, synthetic
//! fixtures only, the same style as `tests/lane_admission.rs`.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
static SEQ: AtomicUsize = AtomicUsize::new(0);

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lane-clash-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A ticket of repo `repo` (the clash is only ever between tickets of one repo).
fn ticket(dir: &Path, id: &str, repo: &str) -> PathBuf {
    let body = format!(
        "{{\"id\":\"{id}\",\"repo_id\":\"{repo}\",\"title\":\"t\",\"group\":\"g\",\"size\":\"S\",\"objective\":\"o\",\"acceptance_criteria\":[\"a\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}"
    );
    let p = dir.join(format!("{id}-{repo}.json"));
    fs::write(&p, body).unwrap();
    p
}

fn scope(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join(format!("scope-{}.txt", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&p, body).unwrap();
    p
}

/// A full valid config whose `lanes.count` is `count` (placeholders only, like the example config).
fn config(dir: &Path, count: u32) -> PathBuf {
    let text = format!(
        "repos:\nbudgets:\n  s_wall_seconds: 1\n  s_tokens: 1\n  m_wall_seconds: 1\n  m_tokens: 1\n  l_wall_seconds: 1\n  l_tokens: 1\nshadow:\n  p1a_cap: 1\n  p4a_cap: 1\n  d2a_cap: 1\n  p7_cap: 1\n  p1a_token_budget: 1\n  p4a_token_budget: 1\n  d2a_token_budget: 1\n  p7_token_budget: 1\nlanes:\n  count: {count}\nfriction:\n  inbox: /placeholder/friction-inbox.jsonl\nfactory:\n  design_tier: T3\n"
    );
    let p = dir.join(format!("config-{count}-{}.yaml", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&p, text).unwrap();
    p
}

/// A valid admission record admitting `lanes` lanes.
fn admission(dir: &Path, lanes: u32) -> PathBuf {
    let text = format!("lane-admission v1\nlanes={lanes}\nbaseline=main@0f1e2d3\nstep2=p1a 5 p4a 10 d2a 20\nreviewer=rev-one\nverdict=approve\noperator=go-2026-11-14b\nl3=pass\nsigned=2026-11-14\n");
    let p = dir.join(format!("admission-{lanes}-{}.txt", SEQ.fetch_add(1, Ordering::SeqCst)));
    fs::write(&p, text).unwrap();
    p
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn claim(t: &Path, lanes: &Path, cfg: &Path, sf: &Path, adm: Option<&Path>) -> Output {
    let mut args = vec!["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", sf.to_str().unwrap()];
    if let Some(a) = adm {
        args.push("--admission");
        args.push(a.to_str().unwrap());
    }
    run(&args)
}

fn list(lanes: &Path, cfg: &Path, adm: Option<&Path>) -> String {
    let mut args = vec!["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap()];
    if let Some(a) = adm {
        args.push("--admission");
        args.push(a.to_str().unwrap());
    }
    let o = run(&args);
    assert_eq!(o.status.code(), Some(0), "{}", String::from_utf8_lossy(&o.stderr));
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn release(id: &str, lanes: &Path, cfg: &Path) -> Output {
    run(&["lane", "release", id, "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap()])
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// Names and sizes of everything in the dir (links not followed).
fn snapshot(lanes: &Path) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = fs::read_dir(lanes)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (e.file_name().to_string_lossy().to_string(), fs::symlink_metadata(e.path()).unwrap().len())
        })
        .collect();
    v.sort();
    v
}

/// Exit 1 with exactly one stderr line `lane: refused reason=<token>` and nothing after the token.
fn refused_once(o: &Output, token: &str) -> bool {
    o.status.code() == Some(1) && err(o).trim() == format!("lane: refused reason={token}")
}

/// Exit 1 with exactly one stderr line that names the live ticket and the overlapping pair as stored, the live entry first.
fn clash_once(o: &Output, live_ticket: &str, live_entry: &str, new_entry: &str) -> bool {
    let e = err(o);
    o.status.code() == Some(1) && e.trim() == format!("lane: refused reason=clash ticket={live_ticket} live={live_entry} new={new_entry}")
}

/// Claim `live_entry` for `t-live` of repo `r`, then `new_entry` for `t-new` of the same repo must be the exact clash line, and the
/// lanes dir and the list output must be unchanged by the refusal.
fn assert_clash(d: &Path, cfg: &Path, adm: &Path, live_entry: &str, new_entry: &str) {
    let lanes = d.join(format!("lanes-{}", SEQ.fetch_add(1, Ordering::SeqCst)));
    let o = claim(&ticket(d, "t-live", "r"), &lanes, cfg, &scope(d, &format!("{live_entry}\n")), Some(adm));
    assert_eq!(o.status.code(), Some(0), "{live_entry} vs {new_entry}: {}", err(&o));
    let before = snapshot(&lanes);
    let listed = list(&lanes, cfg, Some(adm));
    let o = claim(&ticket(d, "t-new", "r"), &lanes, cfg, &scope(d, &format!("{new_entry}\n")), Some(adm));
    assert!(clash_once(&o, "t-live", live_entry, new_entry), "{live_entry} vs {new_entry}: {}", err(&o));
    assert_eq!(snapshot(&lanes), before, "{live_entry} vs {new_entry}");
    assert_eq!(list(&lanes, cfg, Some(adm)), listed, "{live_entry} vs {new_entry}");
}

/// Claim `a` then `b` of one repo (an admission for 2 lanes): both take a lane, in both orders.
fn assert_no_clash(d: &Path, cfg: &Path, adm: &Path, a: &str, b: &str) {
    for (first, second) in [(a, b), (b, a)] {
        let lanes = d.join(format!("lanes-{}", SEQ.fetch_add(1, Ordering::SeqCst)));
        let o = claim(&ticket(d, "t-one", "r"), &lanes, cfg, &scope(d, &format!("{first}\n")), Some(adm));
        assert_eq!(o.status.code(), Some(0), "{first} then {second}: {}", err(&o));
        assert!(out(&o).contains("lane=1"), "{}", out(&o));
        let o = claim(&ticket(d, "t-two", "r"), &lanes, cfg, &scope(d, &format!("{second}\n")), Some(adm));
        assert_eq!(o.status.code(), Some(0), "{first} then {second}: {}", err(&o));
        assert!(out(&o).contains("lane=2"), "{}", out(&o));
        assert!(list(&lanes, cfg, Some(adm)).contains("live=2 cap=2"), "{first} then {second}");
    }
}

#[test]
fn a_folder_entry_clashes_with_an_entry_inside_it_in_both_orders() {
    let d = tmp("folder");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    assert_clash(&d, &cfg, &adm, "docs/", "docs/x/y.md");
    assert_clash(&d, &cfg, &adm, "docs/x/y.md", "docs/");
    assert_clash(&d, &cfg, &adm, "docs/", "docs/sub/");
    assert_clash(&d, &cfg, &adm, "docs/sub/", "docs/");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_file_entry_clashes_with_a_folder_of_the_same_name_and_what_lies_inside_it() {
    // the safe answer: one name cannot be both a file and a folder in one tree
    let d = tmp("filefolder");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    assert_clash(&d, &cfg, &adm, "docs", "docs/");
    assert_clash(&d, &cfg, &adm, "docs/", "docs");
    assert_clash(&d, &cfg, &adm, "docs", "docs/x.md");
    assert_clash(&d, &cfg, &adm, "docs/x.md", "docs");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_normal_form_clashes_dot_parts_and_case_fold() {
    let d = tmp("normal");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    assert_clash(&d, &cfg, &adm, "docs/a.md", "docs/./a.md");
    assert_clash(&d, &cfg, &adm, "./docs/a.md", "docs/a.md");
    assert_clash(&d, &cfg, &adm, "Docs/A.md", "docs/a.md");
    assert_clash(&d, &cfg, &adm, "docs/", "./Docs/X.md");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_whole_repo_entry_clashes_with_every_entry_of_the_repo() {
    let d = tmp("whole");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    // an entry that is nothing but `.` parts names the whole repo
    assert_clash(&d, &cfg, &adm, ".", "src/a.rs");
    assert_clash(&d, &cfg, &adm, "src/a.rs", "./");
    assert_clash(&d, &cfg, &adm, "./", ".");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn near_misses_are_not_a_clash() {
    let d = tmp("near");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    // whole path parts: `a.md` and `a.md.bak` differ, `docs` and `docs2` differ
    assert_no_clash(&d, &cfg, &adm, "docs/a.md", "docs/a.md.bak");
    assert_no_clash(&d, &cfg, &adm, "docs/", "docs2/x.md");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn disjoint_scopes_of_one_repo_both_take_a_lane() {
    let d = tmp("disjoint");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    assert_no_clash(&d, &cfg, &adm, "src/a.rs", "docs/b.md");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_same_entries_in_different_repos_both_take_a_lane() {
    let d = tmp("repos");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let sf = scope(&d, "src/a.rs\n");
    let lanes = d.join("lanes");
    let o = claim(&ticket(&d, "t-one", "repo-a"), &lanes, &cfg, &sf, Some(&adm));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("lane=1"), "{}", out(&o));
    let o = claim(&ticket(&d, "t-two", "repo-b"), &lanes, &cfg, &sf, Some(&adm));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("lane=2"), "{}", out(&o));
    let text = list(&lanes, &cfg, Some(&adm));
    assert!(text.contains("ticket=t-one") && text.contains("ticket=t-two") && text.contains("live=2 cap=2"), "{text}");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn after_the_release_the_refused_claim_succeeds() {
    let d = tmp("release");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let lanes = d.join("lanes");
    assert_eq!(claim(&ticket(&d, "t-live", "r"), &lanes, &cfg, &scope(&d, "docs/\n"), Some(&adm)).status.code(), Some(0));
    let o = claim(&ticket(&d, "t-new", "r"), &lanes, &cfg, &scope(&d, "docs/x.md\n"), Some(&adm));
    assert!(clash_once(&o, "t-live", "docs/", "docs/x.md"), "{}", err(&o));
    assert_eq!(release("t-live", &lanes, &cfg).status.code(), Some(0));
    let o = claim(&ticket(&d, "t-new", "r"), &lanes, &cfg, &scope(&d, "docs/x.md\n"), Some(&adm));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("ticket=t-new lane=1"), "{}", out(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_line_names_the_first_pair_in_the_stated_order() {
    let d = tmp("order");
    let cfg = config(&d, 4);
    let adm = admission(&d, 4);
    let lanes = d.join("lanes");
    // lane order first: t-a (lane 1) is scanned before t-b (lane 2), and within t-a the stored entry order picks `docs/` over `other/`
    assert_eq!(claim(&ticket(&d, "t-a", "r"), &lanes, &cfg, &scope(&d, "other/\ndocs/\n"), Some(&adm)).status.code(), Some(0));
    assert_eq!(claim(&ticket(&d, "t-b", "r"), &lanes, &cfg, &scope(&d, "src/b.rs\n"), Some(&adm)).status.code(), Some(0));
    // the new ticket's entries in stored order: `mm.md` is passed over, the clash is reported for `docs/a.md`
    let before = snapshot(&lanes);
    let listed = list(&lanes, &cfg, Some(&adm));
    let o = claim(&ticket(&d, "t-c", "r"), &lanes, &cfg, &scope(&d, "mm.md\ndocs/a.md\n"), Some(&adm));
    assert!(clash_once(&o, "t-a", "docs/", "docs/a.md"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    assert_eq!(list(&lanes, &cfg, Some(&adm)), listed);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_clash_with_a_record_in_lane_2_names_that_ticket() {
    let d = tmp("lane2");
    let cfg = config(&d, 4);
    let adm = admission(&d, 4);
    let lanes = d.join("lanes");
    // lane 1 holds a disjoint scope of the same repo, lane 2 holds `b/`: the clash is found in the lane-2 record, not only in lane 1
    assert_eq!(claim(&ticket(&d, "t-a", "r"), &lanes, &cfg, &scope(&d, "a/\n"), Some(&adm)).status.code(), Some(0));
    assert_eq!(claim(&ticket(&d, "t-b", "r"), &lanes, &cfg, &scope(&d, "b/\n"), Some(&adm)).status.code(), Some(0));
    let before = snapshot(&lanes);
    let listed = list(&lanes, &cfg, Some(&adm));
    let o = claim(&ticket(&d, "t-c", "r"), &lanes, &cfg, &scope(&d, "b/x\n"), Some(&adm));
    assert!(clash_once(&o, "t-b", "b/", "b/x"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    assert_eq!(list(&lanes, &cfg, Some(&adm)), listed);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_live_entries_are_scanned_before_the_new_entries() {
    let d = tmp("entryorder");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let lanes = d.join("lanes");
    assert_eq!(claim(&ticket(&d, "t-live", "r"), &lanes, &cfg, &scope(&d, "a/\nb/\n"), Some(&adm)).status.code(), Some(0));
    // scanning the new ticket's entries first would report `live=b/ new=b/x`; the stated order scans the live record's entries first and
    // reports `live=a/ new=a/x`
    let o = claim(&ticket(&d, "t-new", "r"), &lanes, &cfg, &scope(&d, "b/x\na/x\n"), Some(&adm));
    assert!(clash_once(&o, "t-live", "a/", "a/x"), "{}", err(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_live_record_whose_repo_id_is_not_a_plain_name_is_a_bad_record() {
    let d = tmp("badrepo");
    let cfg = config(&d, 4);
    let adm = admission(&d, 4);
    for bad in ["repo_id=R", "repo_id=r ", "repo_id="] {
        let lanes = d.join(format!("lanes-{}", SEQ.fetch_add(1, Ordering::SeqCst)));
        fs::create_dir_all(&lanes).unwrap();
        // a hand-written live record: read as another repo, a claim of repo `r` for `docs/a.md` would take lane 2 past the live `docs/`
        fs::write(lanes.join("lane-1"), format!("lane-record v1\nticket=t-live\n{bad}\nscope=docs/\n")).unwrap();
        let before = snapshot(&lanes);
        let o = claim(&ticket(&d, "t-new", "r"), &lanes, &cfg, &scope(&d, "docs/a.md\n"), Some(&adm));
        assert_eq!(o.status.code(), Some(2), "{bad}: {}", err(&o));
        assert!(err(&o).starts_with("lane: error: ") && err(&o).trim().lines().count() == 1, "{bad}: {}", err(&o));
        let o = run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--admission", adm.to_str().unwrap()]);
        assert_eq!(o.status.code(), Some(2), "{bad}: {}", err(&o));
        let o = release("t-live", &lanes, &cfg);
        assert_eq!(o.status.code(), Some(2), "{bad}: {}", err(&o));
        assert_eq!(snapshot(&lanes), before, "{bad}");
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_first_claim_with_no_live_ticket_is_never_a_clash() {
    let d = tmp("first");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let lanes = d.join("lanes");
    // the whole-repo entry itself, on an empty lanes dir
    let o = claim(&ticket(&d, "t-one", "r"), &lanes, &cfg, &scope(&d, ".\n"), Some(&adm));
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("ticket=t-one lane=1"), "{}", out(&o));
    // and with no admission at all
    let lanes2 = d.join("lanes2");
    let o = claim(&ticket(&d, "t-two", "r"), &lanes2, &cfg, &scope(&d, "docs/./a.md\n"), None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert!(out(&o).contains("ticket=t-two lane=1"), "{}", out(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn already_live_comes_before_clash() {
    let d = tmp("already");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let lanes = d.join("lanes");
    assert_eq!(claim(&ticket(&d, "t-one", "r"), &lanes, &cfg, &scope(&d, "docs/\n"), Some(&adm)).status.code(), Some(0));
    // the ticket's own live record overlaps the new scope; already-live is checked first, as before
    let o = claim(&ticket(&d, "t-one", "r"), &lanes, &cfg, &scope(&d, "docs/x.md\n"), Some(&adm));
    assert!(refused_once(&o, "already-live"), "{}", err(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_clash_with_a_valid_admission_and_a_free_lane_is_refused() {
    let d = tmp("freelane");
    let cfg = config(&d, 4);
    let adm = admission(&d, 4);
    let lanes = d.join("lanes");
    assert_eq!(claim(&ticket(&d, "t-live", "r"), &lanes, &cfg, &scope(&d, "docs/\n"), Some(&adm)).status.code(), Some(0));
    let before = snapshot(&lanes);
    let o = claim(&ticket(&d, "t-new", "r"), &lanes, &cfg, &scope(&d, "docs/x.md\n"), Some(&adm));
    assert!(clash_once(&o, "t-live", "docs/", "docs/x.md"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_cap_comes_before_the_clash() {
    let d = tmp("capfirst");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let lanes = d.join("lanes");
    // a full cap (two live under an admission of 2): the clashing third claim is refused cap-full, not clash
    assert_eq!(claim(&ticket(&d, "t-a", "r"), &lanes, &cfg, &scope(&d, "src/a.rs\n"), Some(&adm)).status.code(), Some(0));
    assert_eq!(claim(&ticket(&d, "t-b", "r"), &lanes, &cfg, &scope(&d, "docs/b.md\n"), Some(&adm)).status.code(), Some(0));
    let before = snapshot(&lanes);
    let o = claim(&ticket(&d, "t-c", "r"), &lanes, &cfg, &scope(&d, "src/a.rs\n"), Some(&adm));
    assert!(refused_once(&o, "cap-full"), "{}", err(&o));
    assert_eq!(snapshot(&lanes), before);
    // no valid admission (cap 1): one clashing live ticket refuses with no-admission, not clash
    let lanes2 = d.join("lanes2");
    assert_eq!(claim(&ticket(&d, "t-live", "r"), &lanes2, &cfg, &scope(&d, "docs/\n"), None).status.code(), Some(0));
    let o = claim(&ticket(&d, "t-new", "r"), &lanes2, &cfg, &scope(&d, "docs/x.md\n"), None);
    assert!(refused_once(&o, "no-admission"), "{}", err(&o));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn a_bad_admission_refuses_a_clashing_claim_before_the_lanes_dir_is_touched() {
    let d = tmp("badadm");
    let cfg = config(&d, 4);
    let adm = admission(&d, 2);
    let lanes = d.join("lanes");
    assert_eq!(claim(&ticket(&d, "t-live", "r"), &lanes, &cfg, &scope(&d, "docs/\n"), Some(&adm)).status.code(), Some(0));
    let before = snapshot(&lanes);
    let bad = d.join("bad.txt");
    fs::write(&bad, "lane-admission v1\nlanes=1\nbaseline=main@0f1e2d3\nstep2=p1a 5 p4a 10 d2a 20\nreviewer=rev-one\nverdict=approve\noperator=go-2026-11-14b\nl3=pass\nsigned=2026-11-14\n").unwrap();
    let o = claim(&ticket(&d, "t-new", "r"), &lanes, &cfg, &scope(&d, "docs/x.md\n"), Some(&bad));
    let e = err(&o);
    assert_eq!(o.status.code(), Some(1), "{e}");
    assert!(e.trim().starts_with("lane: refused reason=bad-admission"), "{e}");
    assert_eq!(e.trim().lines().count(), 1);
    assert_eq!(snapshot(&lanes), before);
    let _ = fs::remove_dir_all(&d);
}

/// Start every claim at once, then wait for all: (exit code, stderr) per claim.
fn race(lanes: &Path, cfg: &Path, sf: &Path, adm: Option<&Path>, tickets: &[PathBuf]) -> Vec<(Option<i32>, String)> {
    let kids: Vec<_> = tickets
        .iter()
        .map(|t| {
            let mut args = vec!["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", sf.to_str().unwrap()];
            if let Some(a) = adm {
                args.push("--admission");
                args.push(a.to_str().unwrap());
            }
            Command::new(BIN)
                .args(&args)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    kids.into_iter()
        .map(|k| {
            let o = k.wait_with_output().unwrap();
            (o.status.code(), String::from_utf8_lossy(&o.stderr).to_string())
        })
        .collect()
}

#[test]
fn racing_claims_of_one_scope_leave_exactly_one_live() {
    let d = tmp("race");
    let cfg = config(&d, 4);
    let adm = admission(&d, 4);
    let sf = scope(&d, "src/a.rs\n");
    let tickets: Vec<PathBuf> = (0..32).map(|i| ticket(&d, &format!("t-{i}"), "r")).collect();
    for round in 0..2 {
        let lanes = d.join(format!("lanes-{round}"));
        let res = race(&lanes, &cfg, &sf, Some(&adm), &tickets);
        // the cap of 4 has lanes free, but the same scope of one repo leaves exactly one live ticket
        assert_eq!(res.iter().filter(|r| r.0 == Some(0)).count(), 1, "{res:?}");
        assert!(
            res.iter().filter(|r| r.0 != Some(0)).all(|r| r.0 == Some(1) && r.1.trim().starts_with("lane: refused reason=clash ")),
            "{res:?}"
        );
        let names: Vec<String> = fs::read_dir(&lanes).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).filter(|n| n.starts_with("lane-")).collect();
        assert_eq!(names.len(), 1, "{names:?}");
        let text = list(&lanes, &cfg, Some(&adm));
        assert!(text.contains("live=1 cap=4"), "{text}");
    }
    let _ = fs::remove_dir_all(&d);
}
