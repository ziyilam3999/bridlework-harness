//! `harness lane park`: the parked file list that keeps a ticket's scope reserved after its lane is freed, the `parked` refusal of a claim,
//! the parked removals of `release`, the parked lines of `list`, and the path spelling rule of `--lanes-dir` and `--records-dir`. Real binary,
//! synthetic fixtures only, the same style as `tests/lane_clash.rs`.
#![cfg(unix)]
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const BIN: &str = env!("CARGO_BIN_EXE_harness");
const EXAMPLE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../config/harness.example.yaml");
static SEQ: AtomicUsize = AtomicUsize::new(0);

const AT: &str = "2026-11-16T10:00:00Z";

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("lane-park-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A ticket of repo `repo` (the parked overlap, like the clash, is only ever between tickets of one repo).
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

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

/// A temp copy of the example config with the disk floor lowered to 1 GB, so a claim passes on a disk with little free space.
fn floor_config(dir: &Path) -> PathBuf {
    let text = fs::read_to_string(EXAMPLE).unwrap().replace("  min_free_gb: 50", "  min_free_gb: 1");
    let p = dir.join("floor-1.yaml");
    fs::write(&p, text).unwrap();
    p
}

/// A valid admission record admitting `lanes` lanes.
fn admission(dir: &Path, lanes: u32) -> PathBuf {
    let text = format!("lane-admission v1\nlanes={lanes}\nbaseline=main@0f1e2d3\nstep2=p1a 5 p4a 10 d2a 20\nreviewer=rev-one\nverdict=approve\noperator=go-2026-11-14b\nl3=pass\nsigned=2026-11-14\n");
    let p = dir.join(format!("admission-{lanes}.txt"));
    fs::write(&p, text).unwrap();
    p
}

/// A claim with no admission (cap 1) under the low disk floor config.
fn claim(t: &Path, lanes: &Path, sf: &Path) -> Output {
    let cfg = floor_config(t.parent().unwrap());
    run(&["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", sf.to_str().unwrap()])
}

/// A claim with an admission, under the low disk floor config.
fn claim_adm(t: &Path, lanes: &Path, sf: &Path, adm: &Path) -> Output {
    let cfg = floor_config(t.parent().unwrap());
    run(&["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", sf.to_str().unwrap(), "--admission", adm.to_str().unwrap()])
}

fn release(id: &str, lanes: &Path) -> Output {
    run(&["lane", "release", id, "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE])
}

fn release_rec(id: &str, lanes: &Path, reason: &str, at: &str, records: &Path) -> Output {
    run(&["lane", "release", id, "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--reason", reason, "--at", at, "--records-dir", records.to_str().unwrap()])
}

fn park(id: &str, lanes: &Path) -> Output {
    run(&["lane", "park", id, "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE])
}

fn park_rec(id: &str, lanes: &Path, reason: &str, at: &str, records: &Path) -> Output {
    run(&["lane", "park", id, "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--reason", reason, "--at", at, "--records-dir", records.to_str().unwrap()])
}

fn list(lanes: &Path) -> String {
    let o = run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    out(&o)
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// A lane-record body for `ticket` of `repo` with the given scope entries.
fn rec_text(ticket: &str, repo: &str, scope: &[&str]) -> String {
    let mut s = format!("lane-record v1\nticket={ticket}\nrepo_id={repo}\n");
    for e in scope {
        s.push_str(&format!("scope={e}\n"));
    }
    s
}

fn plant(lanes: &Path, name: &str, body: &str) {
    fs::create_dir_all(lanes).unwrap();
    fs::write(lanes.join(name), body).unwrap();
}

/// A temp folder with ticket `t-one` of `synthetic-repo` claimed into lane 1 with scope `src/a.rs`.
fn held(tag: &str) -> (PathBuf, PathBuf) {
    let d = tmp(tag);
    let lanes = d.join("lanes");
    let sf = scope(&d, "src/a.rs\n");
    let t = ticket(&d, "t-one", "synthetic-repo");
    assert_eq!(claim(&t, &lanes, &sf).status.code(), Some(0));
    (d, lanes)
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

/// Does the folder resolve two names that differ only in ASCII case to one file?
fn case_ignoring(d: &Path) -> bool {
    let a = d.join("bw-case-probe-A");
    fs::write(&a, b"p").unwrap();
    let ignores = fs::symlink_metadata(d.join("bw-case-probe-a")).is_ok();
    let _ = fs::remove_file(&a);
    let _ = fs::remove_file(d.join("bw-case-probe-a"));
    ignores
}

#[test]
fn park_moves_the_record_and_frees_the_lane() {
    let (d, lanes) = held("move");
    let o = park("t-one", &lanes);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one parked lane=1\n");
    assert_eq!(err(&o), "");
    assert_eq!(list(&lanes), "parked ticket=t-one repo_id=synthetic-repo scope=1\nlive=0 cap=1\n");
    // the lane is free again: a disjoint claim of another ticket takes lane 1
    let sf2 = scope(&d, "docs/b.md\n");
    let t2 = ticket(&d, "t-two", "synthetic-repo");
    let o = claim(&t2, &lanes, &sf2);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-two lane=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn park_of_a_ticket_that_is_not_live_is_refused() {
    let (d, lanes) = held("notlive");
    // a ticket that was parked is no longer live
    assert_eq!(park("t-one", &lanes).status.code(), Some(0));
    let o = park("t-one", &lanes);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "lane: refused reason=not-live\n");
    // a ticket that was never live, in a dir that exists
    let o = park("t-other", &lanes);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "lane: refused reason=not-live\n");
    // no lanes dir at all: not-live, and the dir is not created
    let d2 = tmp("notlive-missing");
    let lanes2 = d2.join("lanes");
    let o = park("t-one", &lanes2);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "lane: refused reason=not-live\n");
    assert!(!lanes2.exists());
    let _ = fs::remove_dir_all(&d);
    let _ = fs::remove_dir_all(&d2);
}

#[test]
fn park_does_not_replace_an_existing_parked_file() {
    let (d, lanes) = held("noreplace");
    // the ticket's own parked file, holding another scope
    let parked_body = rec_text("t-one", "synthetic-repo", &["other/"]);
    plant(&lanes, "parked-t-one", &parked_body);
    let before = snapshot(&lanes);
    let o = park("t-one", &lanes);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "lane: refused reason=already-parked\n");
    assert_eq!(snapshot(&lanes), before);
    assert_eq!(fs::read_to_string(lanes.join("lane-1")).unwrap(), rec_text("t-one", "synthetic-repo", &["src/a.rs"]));
    assert_eq!(fs::read_to_string(lanes.join("parked-t-one")).unwrap(), parked_body);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn park_of_a_case_twin() {
    let d = tmp("casetwin");
    let lanes = d.join("lanes");
    plant(&lanes, "lane-1", &rec_text("foo", "synthetic-repo", &["x/"]));
    let parked_foo = rec_text("Foo", "synthetic-repo", &["y/"]);
    plant(&lanes, "parked-Foo", &parked_foo);
    if case_ignoring(&d) {
        // the file system resolves parked-Foo as the parked name of foo, so the move would replace it
        let before = snapshot(&lanes);
        let o = park("foo", &lanes);
        assert_eq!(o.status.code(), Some(1));
        assert_eq!(err(&o), "lane: refused reason=already-parked\n");
        assert_eq!(snapshot(&lanes), before);
    } else {
        // two names, two files: foo is parked next to Foo
        let o = park("foo", &lanes);
        assert_eq!(o.status.code(), Some(0), "{}", err(&o));
        assert_eq!(out(&o), "ticket=foo parked lane=1\n");
        assert_eq!(fs::read_to_string(lanes.join("parked-foo")).unwrap(), rec_text("foo", "synthetic-repo", &["x/"]));
        assert_eq!(list(&lanes), "parked ticket=Foo repo_id=synthetic-repo scope=1\nparked ticket=foo repo_id=synthetic-repo scope=1\nlive=0 cap=1\n");
    }
    assert_eq!(fs::read_to_string(lanes.join("parked-Foo")).unwrap(), parked_foo);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn park_with_a_ticket_id_that_is_a_path_exits_2() {
    let d = tmp("badid");
    let lanes = d.join("lanes");
    for id in ["a/b", "../x", ".x"] {
        let o = park(id, &lanes);
        assert_eq!(o.status.code(), Some(2), "{id}");
        let e = err(&o);
        assert!(e.starts_with("lane: error: ") && e.trim().lines().count() == 1, "{id}: {e}");
        // the id check runs before anything is touched: no lanes dir, no file outside it
        assert!(!lanes.exists(), "{id}");
        let names: Vec<String> = fs::read_dir(&d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().to_string()).collect();
        assert!(names.iter().all(|n| n != "x" && n != "a" && !n.starts_with("parked-")), "{id}: {names:?}");
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn park_record_line_is_written() {
    let (d, lanes) = held("recline");
    let rec = d.join("logs");
    let o = park_rec("t-one", &lanes, "session gone", AT, &rec);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one parked lane=1\n");
    assert_eq!(fs::read_to_string(rec.join("t-one.release")).unwrap(), format!("{AT} lane=1 reason=session gone\n"));
    // the lane is free: a disjoint claim takes it, and the record keeps its line
    let sf2 = scope(&d, "docs/b.md\n");
    let t2 = ticket(&d, "t-two", "synthetic-repo");
    assert_eq!(claim(&t2, &lanes, &sf2).status.code(), Some(0));
    assert_eq!(fs::read_to_string(rec.join("t-one.release")).unwrap(), format!("{AT} lane=1 reason=session gone\n"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn park_record_options_one_or_two_of_three_exit_2() {
    let (d, lanes) = held("trio");
    let rec = d.join("logs");
    let r = rec.to_str().unwrap();
    fn extra<'a>(lanes: &'a Path, v: Vec<&'a str>) -> Vec<&'a str> {
        let mut a: Vec<&'a str> = vec!["lane", "park", "t-one", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE];
        a.extend(v);
        a
    }
    for v in [
        vec!["--reason", "r"],
        vec!["--at", AT],
        vec!["--records-dir", r],
        vec!["--reason", "r", "--at", AT],
        vec!["--reason", "r", "--records-dir", r],
        vec!["--at", AT, "--records-dir", r],
    ] {
        let o = run(&extra(&lanes, v));
        assert_eq!(o.status.code(), Some(2), "{:?}", err(&o));
        assert_eq!(err(&o), "lane: error: --reason, --at and --records-dir go together\n");
    }
    assert!(list(&lanes).contains("ticket=t-one lane=1"));
    assert!(!rec.exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn claim_overlapping_a_parked_scope_is_refused() {
    let (d, lanes) = held("overlap");
    assert_eq!(park("t-one", &lanes).status.code(), Some(0));
    let before = snapshot(&lanes);
    let sf2 = scope(&d, "src/a.rs\n");
    let t2 = ticket(&d, "t-two", "synthetic-repo");
    let o = claim(&t2, &lanes, &sf2);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "lane: refused reason=parked ticket=t-one parked=src/a.rs new=src/a.rs\n");
    assert_eq!(snapshot(&lanes), before);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn claim_of_another_repo_is_not_refused_by_a_parked_scope() {
    let (d, lanes) = held("otherrepo");
    assert_eq!(park("t-one", &lanes).status.code(), Some(0));
    let sf2 = scope(&d, "src/a.rs\n");
    let t2 = ticket(&d, "t-two", "another-repo");
    let o = claim(&t2, &lanes, &sf2);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-two lane=1\n");
    assert_eq!(list(&lanes), "ticket=t-two lane=1 repo_id=another-repo scope=1\nparked ticket=t-one repo_id=synthetic-repo scope=1\nlive=1 cap=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn claim_of_the_parked_ticket_itself_succeeds_and_removes_its_parked_file() {
    let (d, lanes) = held("own");
    assert_eq!(park("t-one", &lanes).status.code(), Some(0));
    let sf2 = scope(&d, "src/a.rs\n");
    let t = ticket(&d, "t-one", "synthetic-repo");
    let o = claim(&t, &lanes, &sf2);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one lane=1\n");
    assert!(!lanes.join("parked-t-one").exists());
    assert_eq!(list(&lanes), "ticket=t-one lane=1 repo_id=synthetic-repo scope=1\nlive=1 cap=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn live_clash_wins_over_parked() {
    let d = tmp("clashfirst");
    let lanes = d.join("lanes");
    let adm = admission(&d, 2);
    // an admission of 2 lanes, so the cap does not refuse first
    assert_eq!(claim_adm(&ticket(&d, "t-a", "synthetic-repo"), &lanes, &scope(&d, "docs/\n"), &adm).status.code(), Some(0));
    assert_eq!(claim_adm(&ticket(&d, "t-b", "synthetic-repo"), &lanes, &scope(&d, "src/x.md\n"), &adm).status.code(), Some(0));
    assert_eq!(park("t-b", &lanes).status.code(), Some(0));
    // the claim overlaps the live t-a (docs/) and the parked t-b (src/x.md): the live clash is named
    let o = claim_adm(&ticket(&d, "t-c", "synthetic-repo"), &lanes, &scope(&d, "docs/y.md\nsrc/x.md\n"), &adm);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "lane: refused reason=clash ticket=t-a live=docs/ new=docs/y.md\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn release_of_a_parked_only_ticket_removes_it() {
    let (d, lanes) = held("relparked");
    assert_eq!(park("t-one", &lanes).status.code(), Some(0));
    let rec = d.join("logs");
    let o = release_rec("t-one", &lanes, "drop the ticket", AT, &rec);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one released\n");
    assert_eq!(err(&o), "");
    assert_eq!(fs::read_to_string(rec.join("t-one.release")).unwrap(), format!("{AT} lane=parked reason=drop the ticket\n"));
    assert!(!lanes.join("parked-t-one").exists());
    assert_eq!(list(&lanes), "live=0 cap=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn release_of_a_live_and_parked_ticket_removes_both() {
    let (d, lanes) = held("relboth");
    // a claim of the parked ticket whose parked-file removal failed leaves it both live and parked; here that state is planted by hand
    plant(&lanes, "parked-t-one", &rec_text("t-one", "synthetic-repo", &["other/"]));
    let o = release("t-one", &lanes);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one released\n");
    assert_eq!(fs::read_dir(&lanes).unwrap().count(), 0);
    assert_eq!(list(&lanes), "live=0 cap=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn release_of_a_case_twin_leaves_the_parked_file() {
    let d = tmp("reltwin");
    let lanes = d.join("lanes");
    let parked_foo = rec_text("Foo", "synthetic-repo", &["x/"]);
    plant(&lanes, "parked-Foo", &parked_foo);
    let o = release("foo", &lanes);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(err(&o), "lane: refused reason=not-live\n");
    assert_eq!(fs::read_to_string(lanes.join("parked-Foo")).unwrap(), parked_foo);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_records_do_not_count_toward_the_cap() {
    let (d, lanes) = held("cap");
    assert_eq!(park("t-one", &lanes).status.code(), Some(0));
    // cap 1 with no admission: were the parked record counted as live, this claim would be no-admission
    let sf2 = scope(&d, "docs/b.md\n");
    let t2 = ticket(&d, "t-two", "synthetic-repo");
    let o = claim(&t2, &lanes, &sf2);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-two lane=1\n");
    assert_eq!(list(&lanes), "ticket=t-two lane=1 repo_id=synthetic-repo scope=1\nparked ticket=t-one repo_id=synthetic-repo scope=1\nlive=1 cap=1\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn lanes_dir_link_with_a_trailing_slash_or_dot_is_refused() {
    use std::os::unix::fs::symlink;
    let d = tmp("dirlink");
    let real = d.join("real");
    fs::create_dir_all(&real).unwrap();
    let link = d.join("lanes");
    symlink(&real, &link).unwrap();
    let cfg = floor_config(&d);
    let sf = scope(&d, "a\n");
    let t = ticket(&d, "t-one", "synthetic-repo");
    for suffix in ["/", "/."] {
        let ld = format!("{}{}", link.to_str().unwrap(), suffix);
        let outs = [
            run(&["lane", "park", "t-one", "--lanes-dir", &ld, "--config", EXAMPLE]),
            run(&["lane", "claim", t.to_str().unwrap(), "--lanes-dir", &ld, "--config", cfg.to_str().unwrap(), "--scope-file", sf.to_str().unwrap()]),
            run(&["lane", "release", "t-one", "--lanes-dir", &ld, "--config", EXAMPLE]),
            run(&["lane", "list", "--lanes-dir", &ld, "--config", EXAMPLE]),
        ];
        for o in &outs {
            assert_eq!(o.status.code(), Some(2), "{suffix}: {}", err(o));
            assert_eq!(err(o), "lane: error: --lanes-dir must not end with a slash or hold a . or .. part\n", "{suffix}");
        }
        assert!(fs::read_dir(&real).unwrap().next().is_none(), "{suffix}");
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_link_with_a_trailing_slash_is_refused() {
    use std::os::unix::fs::symlink;
    let (d, lanes) = held("reclink");
    let real = d.join("real-logs");
    fs::create_dir_all(&real).unwrap();
    let link = d.join("logs");
    symlink(&real, &link).unwrap();
    let rd = format!("{}/", link.to_str().unwrap());
    let outs = [
        park_rec("t-one", &lanes, "r", AT, Path::new(&rd)),
        release_rec("t-one", &lanes, "r", AT, Path::new(&rd)),
    ];
    for o in &outs {
        assert_eq!(o.status.code(), Some(2), "{}", err(o));
        assert_eq!(err(o), "lane: error: --records-dir must not end with a slash\n");
    }
    assert!(fs::read_dir(&real).unwrap().next().is_none());
    assert!(list(&lanes).contains("ticket=t-one lane=1"));
    let _ = fs::remove_dir_all(&d);
}

/// Each bad parked file: `list`, `claim`, `release` and `park` exit 2 with one `lane: error:` line naming the cause, and the dir is
/// unchanged. The lanes dir holds `t-x` live in `lane-1` next to the bad parked file.
fn all_verbs_refuse_parked(d: &Path, lanes: &Path, cause: &str) {
    let sf = scope(d, "zz/\n");
    let t = ticket(d, "t-new", "synthetic-repo");
    let before = snapshot(lanes);
    let outs = [
        run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]),
        claim(&t, lanes, &sf),
        release("t-x", lanes),
        park("t-x", lanes),
    ];
    for o in &outs {
        let e = err(o);
        assert_eq!(o.status.code(), Some(2), "{cause}: {e}");
        assert!(e.starts_with("lane: error: ") && e.trim().lines().count() == 1, "{cause}: {e}");
        assert!(e.contains(cause), "{cause}: {e}");
    }
    assert_eq!(snapshot(lanes), before, "{cause}");
}

/// A lanes dir with `t-x` of `synthetic-repo` live in `lane-1`.
fn live_t_x(lanes: &Path) {
    plant(lanes, "lane-1", &rec_text("t-x", "synthetic-repo", &["a"]));
}

#[test]
fn parked_file_that_is_a_symlink() {
    use std::os::unix::fs::symlink;
    let d = tmp("pk-link");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    let target = d.join("real-parked");
    fs::write(&target, rec_text("t-x", "synthetic-repo", &["docs/"])).unwrap();
    symlink(&target, lanes.join("parked-t-x")).unwrap();
    all_verbs_refuse_parked(&d, &lanes, "not a regular file");
    assert!(fs::symlink_metadata(lanes.join("parked-t-x")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), rec_text("t-x", "synthetic-repo", &["docs/"]));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_that_is_a_folder() {
    let d = tmp("pk-dir");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    fs::create_dir_all(lanes.join("parked-t-x")).unwrap();
    all_verbs_refuse_parked(&d, &lanes, "not a regular file");
    assert!(lanes.join("parked-t-x").is_dir());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_that_is_a_fifo() {
    use std::os::unix::fs::FileTypeExt;
    let d = tmp("pk-fifo");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    let mk = Command::new("mkfifo").arg(lanes.join("parked-t-x")).output().unwrap();
    assert!(mk.status.success(), "mkfifo: {}", String::from_utf8_lossy(&mk.stderr));
    all_verbs_refuse_parked(&d, &lanes, "not a regular file");
    assert!(fs::symlink_metadata(lanes.join("parked-t-x")).unwrap().file_type().is_fifo());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_with_a_bad_header() {
    let d = tmp("pk-header");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    plant(&lanes, "parked-t-x", "lane-record v2\nticket=t-x\nrepo_id=synthetic-repo\nscope=docs/\n");
    all_verbs_refuse_parked(&d, &lanes, "unknown header");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_cut_short() {
    let d = tmp("pk-short");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    plant(&lanes, "parked-t-x", "lane-record v1\nticket=t-x\nrepo_id=synthetic-repo\nscope=docs/");
    all_verbs_refuse_parked(&d, &lanes, "cut short");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_empty() {
    let d = tmp("pk-empty");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    plant(&lanes, "parked-t-x", "");
    all_verbs_refuse_parked(&d, &lanes, "empty");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_with_no_scope_line() {
    let d = tmp("pk-noscope");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    plant(&lanes, "parked-t-x", "lane-record v1\nticket=t-x\nrepo_id=synthetic-repo\n");
    all_verbs_refuse_parked(&d, &lanes, "no scope entry");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_over_1_mib() {
    let d = tmp("pk-mib");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    let mut big = String::from("lane-record v1\nticket=t-x\nrepo_id=synthetic-repo\n");
    let entry = format!("scope={}\n", "a".repeat(4000));
    while big.len() <= 1024 * 1024 {
        big.push_str(&entry);
    }
    plant(&lanes, "parked-t-x", &big);
    all_verbs_refuse_parked(&d, &lanes, "over 1 MiB");
    assert_eq!(fs::read_to_string(lanes.join("parked-t-x")).unwrap(), big);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_not_utf8() {
    let d = tmp("pk-utf8");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    let bytes: &[u8] = b"lane-record v1\nticket=t-x\nrepo_id=synthetic-repo\nscope=a\xffb\n";
    fs::create_dir_all(&lanes).unwrap();
    fs::write(lanes.join("parked-t-x"), bytes).unwrap();
    all_verbs_refuse_parked(&d, &lanes, "not utf-8");
    assert_eq!(fs::read(lanes.join("parked-t-x")).unwrap(), bytes);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_name_differs_from_its_ticket() {
    // parked-a holding ticket=b, and parked-A holding ticket=a: two runs, one fault each
    for (name, body_ticket) in [("parked-a", "b"), ("parked-A", "a")] {
        let d = tmp("pk-name");
        let lanes = d.join("lanes");
        live_t_x(&lanes);
        plant(&lanes, name, &rec_text(body_ticket, "synthetic-repo", &["docs/"]));
        all_verbs_refuse_parked(&d, &lanes, "does not match its ticket");
        let _ = fs::remove_dir_all(&d);
    }
}

#[test]
fn parked_name_with_an_empty_rest() {
    let d = tmp("pk-rest");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    plant(&lanes, "parked-", &rec_text("t-x", "synthetic-repo", &["docs/"]));
    all_verbs_refuse_parked(&d, &lanes, "no ticket id");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_with_a_bad_repo_id() {
    let d = tmp("pk-repo");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    plant(&lanes, "parked-t-x", "lane-record v1\nticket=t-x\nrepo_id=R\nscope=docs/\n");
    all_verbs_refuse_parked(&d, &lanes, "not a plain name");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn parked_file_unreadable() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmp("pk-mode");
    let lanes = d.join("lanes");
    live_t_x(&lanes);
    let p = lanes.join("parked-t-x");
    fs::write(&p, rec_text("t-x", "synthetic-repo", &["docs/"])).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::File::open(&p).is_ok() {
        println!("note: this run reads the file anyway (root), so the unreadable parked record case is skipped");
    } else {
        all_verbs_refuse_parked(&d, &lanes, "cannot be read");
    }
    fs::set_permissions(&p, fs::Permissions::from_mode(0o644)).unwrap();
    let _ = fs::remove_dir_all(&d);
}

/// A smoke race: park of A against claim of B (scope inside A's) in two processes; B is refused with clash or parked, and is live in no
/// round. No switch-off is claimed for this test.
#[test]
fn park_and_claim_race() {
    let d = tmp("race");
    let cfg = floor_config(&d);
    let adm = admission(&d, 2);
    let sf_a = scope(&d, "x/\n");
    let sf_b = scope(&d, "x/a\n");
    let ta = ticket(&d, "t-a", "synthetic-repo");
    let tb = ticket(&d, "t-b", "synthetic-repo");
    for round in 0..20 {
        let lanes = d.join(format!("lanes-{round}"));
        assert_eq!(claim_adm(&ta, &lanes, &sf_a, &adm).status.code(), Some(0));
        let spawn = |args: Vec<&str>| {
            Command::new(BIN).args(&args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap()
        };
        let p = spawn(vec!["lane", "park", "t-a", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
        let c = spawn(vec!["lane", "claim", tb.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", cfg.to_str().unwrap(), "--scope-file", sf_b.to_str().unwrap(), "--admission", adm.to_str().unwrap()]);
        let op = p.wait_with_output().unwrap();
        assert_eq!(op.status.code(), Some(0), "round {round}: {}", String::from_utf8_lossy(&op.stderr));
        let oc = c.wait_with_output().unwrap();
        assert_eq!(oc.status.code(), Some(1), "round {round}: {}", String::from_utf8_lossy(&oc.stderr));
        let e = String::from_utf8_lossy(&oc.stderr).to_string();
        assert!(
            e == "lane: refused reason=clash ticket=t-a live=x/ new=x/a\n" || e == "lane: refused reason=parked ticket=t-a parked=x/ new=x/a\n",
            "round {round}: {e}"
        );
        // the lanes dir does not hold B live
        for ent in fs::read_dir(&lanes).unwrap() {
            let ent = ent.unwrap();
            let name = ent.file_name().to_string_lossy().to_string();
            if name.starts_with("lane-") {
                let body = fs::read_to_string(ent.path()).unwrap();
                assert!(!body.contains("ticket=t-b"), "round {round}: {name}: {body}");
            }
        }
        let _ = fs::remove_dir_all(&lanes);
    }
    let _ = fs::remove_dir_all(&d);
}
