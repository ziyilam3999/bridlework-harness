//! `harness lane release`: the old output pinned, then the release record the engine writes when the caller
//! asks for it with `--reason`, `--at` and `--records-dir`. Real binary, synthetic fixtures only.
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
    let d = std::env::temp_dir().join(format!("lane-rel-{}-{}-{}", tag, std::process::id(), SEQ.fetch_add(1, Ordering::SeqCst)));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

fn ticket(dir: &Path, id: &str) -> PathBuf {
    let body = format!(
        "{{\"id\":\"{id}\",\"repo_id\":\"synthetic-repo\",\"title\":\"t\",\"group\":\"g\",\"size\":\"S\",\"objective\":\"o\",\"acceptance_criteria\":[\"a\"],\"dependencies\":[],\"risk\":\"normal\",\"owner_role\":\"execution_verification\",\"evidence_links\":[]}}"
    );
    let p = dir.join(format!("{id}.json"));
    fs::write(&p, body).unwrap();
    p
}

fn scope(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("scope.txt");
    fs::write(&p, body).unwrap();
    p
}

fn run(args: &[&str]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn claim(t: &Path, lanes: &Path, sf: &Path) -> Output {
    run(&["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--scope-file", sf.to_str().unwrap()])
}

fn list(lanes: &Path) -> String {
    let o = run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    String::from_utf8_lossy(&o.stdout).to_string()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).to_string()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).to_string()
}

/// Is there a file whose name ends in `.release` anywhere under `d`?
fn any_release_file(d: &Path) -> bool {
    let Ok(rd) = fs::read_dir(d) else { return false };
    for e in rd {
        let e = e.unwrap();
        if e.file_name().to_string_lossy().ends_with(".release") {
            return true;
        }
        let p = e.path();
        if fs::symlink_metadata(&p).map(|m| m.is_dir()).unwrap_or(false) && any_release_file(&p) {
            return true;
        }
    }
    false
}

/// A temp folder with ticket `id` claimed into lane 1.
fn held_id(tag: &str, id: &str) -> (PathBuf, PathBuf) {
    let d = tmp(tag);
    let lanes = d.join("lanes");
    let sf = scope(&d, "src/a.rs\n");
    let t = ticket(&d, id);
    assert_eq!(claim(&t, &lanes, &sf).status.code(), Some(0));
    (d, lanes)
}

fn held(tag: &str) -> (PathBuf, PathBuf) {
    held_id(tag, "t-one")
}

/// `lane release t-one ... --reason r --at <at> --records-dir <records>`.
fn release(id: &str, lanes: &Path, reason: &str, at: &str, records: &Path) -> Output {
    run(&["lane", "release", id, "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--reason", reason, "--at", at, "--records-dir", records.to_str().unwrap()])
}

/// `lane release t-one ...` plus the given extra options.
fn rel(lanes: &Path, extra: &[&str]) -> Output {
    let mut v: Vec<&str> = vec!["lane", "release", "t-one", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE];
    v.extend_from_slice(extra);
    run(&v)
}

fn still_held(lanes: &Path) {
    assert!(list(lanes).contains("ticket=t-one"));
}

/// A bad `--reason` value: exit 2 with the named message, the lane held, no records dir, no record file.
fn reason_refused(tag: &str, reason: &str, msg: &str) {
    let (d, lanes) = held(tag);
    let rec = d.join("logs");
    let o = release("t-one", &lanes, reason, AT, &rec);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert_eq!(err(&o), format!("lane: error: {msg}\n"));
    still_held(&lanes);
    assert!(!rec.exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn release_without_reason_is_unchanged() {
    let d = tmp("plain");
    let lanes = d.join("lanes");
    let sf = scope(&d, "src/a.rs\n");
    let t = ticket(&d, "t-one");
    assert_eq!(claim(&t, &lanes, &sf).status.code(), Some(0));
    let o = run(&["lane", "release", "t-one", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one released\n");
    assert_eq!(err(&o), "");
    assert!(!any_release_file(&d));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn not_live_without_reason_is_unchanged() {
    let d = tmp("plain-dead");
    let lanes = d.join("lanes");
    let o = run(&["lane", "release", "t-one", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "lane: refused reason=not-live\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn the_usage_text_holds_the_release_record_options() {
    let o = run(&["lane"]);
    assert_eq!(o.status.code(), Some(2));
    let e = err(&o);
    assert!(e.contains("lane release <ticket-id> --lanes-dir <absolute-dir> --config <file> [--reason <text> --at <YYYY-MM-DDTHH:MM:SSZ> --records-dir <absolute-dir>]"), "{e}");
    assert!(e.contains("lane claim <ticket-file>"), "{e}");
    assert!(e.contains("lane admission-check <file>"), "{e}");
}

#[test]
fn the_record_options_go_together() {
    let (d, lanes) = held("trio");
    let rec = d.join("logs");
    let r = rec.to_str().unwrap();
    for extra in [
        vec!["--reason", "r"],
        vec!["--at", AT],
        vec!["--records-dir", r],
        vec!["--reason", "r", "--at", AT],
        vec!["--reason", "r", "--records-dir", r],
        vec!["--at", AT, "--records-dir", r],
    ] {
        let o = rel(&lanes, &extra);
        assert_eq!(o.status.code(), Some(2), "{:?}", err(&o));
        assert_eq!(err(&o), "lane: error: --reason, --at and --records-dir go together\n");
    }
    still_held(&lanes);
    assert!(!rec.exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn at_must_be_a_utc_timestamp() {
    let (d, lanes) = held("at");
    let rec = d.join("logs");
    for at in ["2026-11-16 10:00:00", "2026-02-30T10:00:00Z", "2026-11-16T24:00:00Z", ""] {
        let o = release("t-one", &lanes, "r", at, &rec);
        assert_eq!(o.status.code(), Some(2), "{at}");
        assert_eq!(err(&o), "lane: error: --at must be a UTC time written YYYY-MM-DDTHH:MM:SSZ\n", "{at}");
    }
    still_held(&lanes);
    assert!(!rec.exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_must_be_absolute() {
    let (d, lanes) = held("rd-rel");
    let o = rel(&lanes, &["--reason", "r", "--at", AT, "--records-dir", "logs"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --records-dir must be an absolute path\n");
    still_held(&lanes);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn at_given_twice() {
    let (d, lanes) = held("at-two");
    let rec = d.join("logs");
    let o = rel(&lanes, &["--reason", "r", "--at", AT, "--at", AT, "--records-dir", rec.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --at was given twice\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn at_missing_value() {
    let (d, lanes) = held("at-miss");
    let rec = d.join("logs");
    let o = rel(&lanes, &["--reason", "r", "--records-dir", rec.to_str().unwrap(), "--at"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --at is missing its value\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_given_twice() {
    let (d, lanes) = held("rd-two");
    let rec = d.join("logs");
    let r = rec.to_str().unwrap();
    let o = rel(&lanes, &["--reason", "r", "--at", AT, "--records-dir", r, "--records-dir", r]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --records-dir was given twice\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_missing_value() {
    let (d, lanes) = held("rd-miss");
    let o = rel(&lanes, &["--reason", "r", "--at", AT, "--records-dir"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --records-dir is missing its value\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn reason_given_twice() {
    let (d, lanes) = held("r-two");
    let rec = d.join("logs");
    let o = rel(&lanes, &["--reason", "a", "--reason", "b", "--at", AT, "--records-dir", rec.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --reason was given twice\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn reason_missing_value() {
    let (d, lanes) = held("r-miss");
    let rec = d.join("logs");
    let o = rel(&lanes, &["--at", AT, "--records-dir", rec.to_str().unwrap(), "--reason"]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --reason is missing its value\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn reasons_unknown_option() {
    let (d, lanes) = held("r-plural");
    let o = rel(&lanes, &["--reasons", "x", "--at", AT, "--records-dir", d.join("logs").to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: unknown option --reasons\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn reason_takes_the_next_flag_as_text_and_the_date_becomes_a_second_positional() {
    let (d, lanes) = held("r-flag");
    let rec = d.join("logs");
    let o = rel(&lanes, &["--reason", "--at", AT, "--records-dir", rec.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: more than one positional argument given\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn claim_refuses_the_record_options() {
    let d = tmp("verb-claim");
    let lanes = d.join("lanes");
    let sf = scope(&d, "a\n");
    let t = ticket(&d, "t-one");
    for opt in ["--reason", "--at", "--records-dir"] {
        let o = run(&["lane", "claim", t.to_str().unwrap(), "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--scope-file", sf.to_str().unwrap(), opt, "x"]);
        assert_eq!(o.status.code(), Some(2), "{opt}");
        assert_eq!(err(&o), format!("lane: error: unknown option {opt}\n"));
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn list_refuses_the_record_options() {
    let d = tmp("verb-list");
    let lanes = d.join("lanes");
    for opt in ["--reason", "--at", "--records-dir"] {
        let o = run(&["lane", "list", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, opt, "x"]);
        assert_eq!(o.status.code(), Some(2), "{opt}");
        assert_eq!(err(&o), format!("lane: error: unknown option {opt}\n"));
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn admission_check_refuses_the_record_options() {
    let d = tmp("verb-adm");
    let adm = d.join("adm");
    for opt in ["--reason", "--at", "--records-dir"] {
        let o = run(&["lane", "admission-check", adm.to_str().unwrap(), "--config", EXAMPLE, opt, "x"]);
        assert_eq!(o.status.code(), Some(2), "{opt}");
        assert_eq!(err(&o), format!("lane: error: unknown option {opt}\n"));
    }
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn reason_empty() {
    reason_refused("r-empty", "", "--reason must not be empty or only white space");
}

#[test]
fn reason_blank_spaces() {
    reason_refused("r-blank", "   ", "--reason must not be empty or only white space");
}

#[test]
fn reason_tab_only() {
    reason_refused("r-tab", "\t", "--reason must not be empty or only white space");
}

#[test]
fn reason_nbsp_only() {
    reason_refused("r-nbsp", "\u{a0}", "--reason must not be empty or only white space");
}

#[test]
fn reason_two_lines() {
    reason_refused("r-two-lines", "a\nb", "--reason must be one line");
}

#[test]
fn reason_carriage_return() {
    reason_refused("r-cr", "a\rb", "--reason must hold no control character");
}

#[test]
fn reason_escape() {
    reason_refused("r-esc", "a\u{1b}b", "--reason must hold no control character");
}

#[test]
fn reason_next_line_u0085() {
    reason_refused("r-nel", "a\u{85}b", "--reason must hold no control character");
}

#[test]
fn reason_line_separator_u2028() {
    reason_refused("r-u2028", "a\u{2028}b", "--reason must hold no control character");
}

#[test]
fn reason_1025_bytes() {
    let (d, lanes) = held("r-cap");
    let rec = d.join("logs");
    let o = release("t-one", &lanes, &"x".repeat(1025), AT, &rec);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert_eq!(err(&o), "lane: error: --reason is over 1024 bytes\n");
    still_held(&lanes);
    assert!(!rec.exists());
    let ok = release("t-one", &lanes, &"x".repeat(1024), AT, &rec);
    assert_eq!(ok.status.code(), Some(0), "{}", err(&ok));
    assert_eq!(fs::read_to_string(rec.join("t-one.release")).unwrap(), format!("{AT} lane=1 reason={}\n", "x".repeat(1024)));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_is_lanes_dir() {
    let (d, lanes) = held_id("rd-same", "lane-1");
    let o = release("lane-1", &lanes, "r", AT, &lanes);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --records-dir must not be the lanes dir or inside it\n");
    assert!(list(&lanes).contains("ticket=lane-1"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_inside_lanes_dir() {
    let (d, lanes) = held("rd-in");
    let rec = lanes.join("sub");
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --records-dir must not be the lanes dir or inside it\n");
    still_held(&lanes);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_with_a_dot_dot_part() {
    let (d, lanes) = held("rd-dotdot");
    let rec = d.join("logs").join("..").join("logs2");
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --records-dir must not be the lanes dir or inside it\n");
    still_held(&lanes);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_inside_lanes_dir_through_a_linked_parent() {
    use std::os::unix::fs::symlink;
    let (d, lanes) = held("rd-link");
    symlink(&d, d.join("alias")).unwrap();
    let rec = d.join("alias").join("lanes").join("lane-2");
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: --records-dir must not be the lanes dir or inside it\n");
    still_held(&lanes);
    assert!(!lanes.join("lane-2").exists());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn not_live_with_reason_writes_nothing() {
    let d = tmp("dead-rec");
    let lanes = d.join("lanes");
    fs::create_dir_all(&lanes).unwrap();
    let rec = d.join("logs");
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(out(&o), "");
    assert_eq!(err(&o), "lane: refused reason=not-live\n");
    assert!(!rec.exists());
    assert!(!any_release_file(&d));
    let _ = fs::remove_dir_all(&d);
}

/// A refused release with a planted records dir: exit 2, the named message, the lane held.
fn record_refused(tag: &str, plant: impl FnOnce(&Path), msg: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (d, lanes) = held(tag);
    let rec = d.join("logs");
    plant(&rec);
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(2), "{}", err(&o));
    assert_eq!(err(&o), format!("lane: error: {msg}\n"));
    still_held(&lanes);
    (d, lanes, rec)
}

#[test]
fn record_is_symlink_to_file() {
    use std::os::unix::fs::symlink;
    let (d, lanes) = held("rec-link");
    let rec = d.join("logs");
    fs::create_dir_all(&rec).unwrap();
    let target = d.join("real.release");
    fs::write(&target, "old\n").unwrap();
    symlink(&target, rec.join("t-one.release")).unwrap();
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: the release record is not a regular file\n");
    still_held(&lanes);
    // the planted link and its target keep their bytes
    assert!(fs::symlink_metadata(rec.join("t-one.release")).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(&target).unwrap(), "old\n");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn record_is_dangling_symlink() {
    use std::os::unix::fs::symlink;
    let (d, _lanes, _rec) = record_refused(
        "rec-dangling",
        |rec| {
            fs::create_dir_all(rec).unwrap();
            symlink("/nonexistent-lane-release-target", rec.join("t-one.release")).unwrap();
        },
        "the release record is not a regular file",
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn record_is_folder() {
    let (d, _lanes, rec) = record_refused("rec-dir", |rec| fs::create_dir_all(rec.join("t-one.release")).unwrap(), "the release record is not a regular file");
    assert!(fs::symlink_metadata(rec.join("t-one.release")).unwrap().is_dir());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn record_is_fifo() {
    use std::os::unix::fs::FileTypeExt;
    let (d, _lanes, rec) = record_refused(
        "rec-fifo",
        |rec| {
            fs::create_dir_all(rec).unwrap();
            Command::new("mkfifo").arg(rec.join("t-one.release")).status().unwrap();
        },
        "the release record is not a regular file",
    );
    assert!(fs::symlink_metadata(rec.join("t-one.release")).unwrap().file_type().is_fifo());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_is_symlink() {
    use std::os::unix::fs::symlink;
    let d = tmp("recdir-link");
    let lanes = d.join("lanes");
    let sf = scope(&d, "src/a.rs\n");
    let t = ticket(&d, "t-one");
    assert_eq!(claim(&t, &lanes, &sf).status.code(), Some(0));
    let real = d.join("real-logs");
    fs::create_dir_all(&real).unwrap();
    let rec = d.join("logs");
    symlink(&real, &rec).unwrap();
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: the records dir is not a folder or is a symbolic link\n");
    still_held(&lanes);
    assert!(fs::read_dir(&real).unwrap().next().is_none());
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_is_regular_file() {
    let (d, lanes) = held("recdir-file");
    let rec = d.join("logs");
    fs::write(&rec, "").unwrap();
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(2));
    assert_eq!(err(&o), "lane: error: the records dir is not a folder or is a symbolic link\n");
    still_held(&lanes);
    assert_eq!(fs::read(&rec).unwrap(), b"");
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn record_over_1_mib() {
    let big = vec![b'x'; 1024 * 1024 + 2];
    let (d, _lanes, rec) = record_refused("rec-mib", |rec| {
        fs::create_dir_all(rec).unwrap();
        fs::write(rec.join("t-one.release"), &big).unwrap();
    }, "the release record is over 1 MiB");
    assert_eq!(fs::read(rec.join("t-one.release")).unwrap(), big);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn record_not_utf8() {
    let bytes = b"\xff\xfe\n".to_vec();
    let (d, _lanes, rec) = record_refused("rec-utf8", |rec| {
        fs::create_dir_all(rec).unwrap();
        fs::write(rec.join("t-one.release"), &bytes).unwrap();
    }, "the release record is not utf-8");
    assert_eq!(fs::read(rec.join("t-one.release")).unwrap(), bytes);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn record_torn_tail() {
    let bytes = b"2026-01-01T00:00:00Z lane=1 reason=old".to_vec();
    let (d, _lanes, rec) = record_refused("rec-torn", |rec| {
        fs::create_dir_all(rec).unwrap();
        fs::write(rec.join("t-one.release"), &bytes).unwrap();
    }, "the release record does not end with a newline");
    assert_eq!(fs::read(rec.join("t-one.release")).unwrap(), bytes);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn records_dir_missing_is_created() {
    let (d, lanes) = held("rec-new");
    let rec = d.join("deep").join("nested").join("logs");
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one released\n");
    assert_eq!(err(&o), "");
    assert_eq!(fs::read_to_string(rec.join("t-one.release")).unwrap(), format!("{AT} lane=1 reason=r\n"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn existing_empty_record_is_accepted() {
    let (d, lanes) = held("rec-empty");
    let rec = d.join("logs");
    fs::create_dir_all(&rec).unwrap();
    fs::write(rec.join("t-one.release"), "").unwrap();
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(fs::read_to_string(rec.join("t-one.release")).unwrap(), format!("{AT} lane=1 reason=r\n"));
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn existing_crlf_record_is_accepted() {
    let (d, lanes) = held("rec-crlf");
    let rec = d.join("logs");
    fs::create_dir_all(&rec).unwrap();
    let old = b"2026-01-01T00:00:00Z lane=1 reason=old\r\n";
    fs::write(rec.join("t-one.release"), old).unwrap();
    let o = release("t-one", &lanes, "r", AT, &rec);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let mut want = old.to_vec();
    want.extend_from_slice(format!("{AT} lane=1 reason=r\n").as_bytes());
    assert_eq!(fs::read(rec.join("t-one.release")).unwrap(), want);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn two_releases_append_two_lines() {
    let (d, lanes) = held("rec-two");
    let rec = d.join("logs");
    assert_eq!(release("t-one", &lanes, "first one", "2026-11-16T10:00:00Z", &rec).status.code(), Some(0));
    let sf = scope(&d, "src/a.rs\n");
    let t = ticket(&d, "t-one");
    assert_eq!(claim(&t, &lanes, &sf).status.code(), Some(0));
    assert_eq!(release("t-one", &lanes, "second one", "2026-11-17T11:30:00Z", &rec).status.code(), Some(0));
    assert_eq!(
        fs::read_to_string(rec.join("t-one.release")).unwrap(),
        "2026-11-16T10:00:00Z lane=1 reason=first one\n2026-11-17T11:30:00Z lane=1 reason=second one\n"
    );
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn worked_case_record_bytes() {
    let (d, lanes) = held("worked");
    let rec = d.join("logs");
    let want: &[u8] = b"2026-11-16T10:00:00Z lane=1 reason=will not finish this week\n";
    let o = release("t-one", &lanes, "will not finish this week", "2026-11-16T10:00:00Z", &rec);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    assert_eq!(out(&o), "ticket=t-one released\n");
    assert_eq!(err(&o), "");
    assert_eq!(fs::read(rec.join("t-one.release")).unwrap(), want);
    let again = release("t-one", &lanes, "will not finish this week", "2026-11-16T10:00:00Z", &rec);
    assert_eq!(again.status.code(), Some(1));
    assert_eq!(out(&again), "");
    assert_eq!(err(&again), "lane: refused reason=not-live\n");
    assert_eq!(fs::read(rec.join("t-one.release")).unwrap(), want);
    let _ = fs::remove_dir_all(&d);
}

#[test]
fn racing_releases_with_reason_write_one_line() {
    let (d, lanes) = held("race-rel");
    let rec = d.join("logs");
    let mk = || {
        Command::new(BIN)
            .args(["lane", "release", "t-one", "--lanes-dir", lanes.to_str().unwrap(), "--config", EXAMPLE, "--reason", "r", "--at", AT, "--records-dir", rec.to_str().unwrap()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a = mk();
    let b = mk();
    let oa = a.wait_with_output().unwrap();
    let ob = b.wait_with_output().unwrap();
    let res = [(oa.status.code(), err(&oa)), (ob.status.code(), err(&ob))];
    assert!(res.iter().filter(|r| r.0 == Some(0)).count() == 1, "{res:?}");
    assert!(res.iter().filter(|r| r.0 == Some(1) && r.1 == "lane: refused reason=not-live\n").count() == 1, "{res:?}");
    let text = fs::read_to_string(rec.join("t-one.release")).unwrap();
    assert_eq!(text, format!("{AT} lane=1 reason=r\n"));
    let _ = fs::remove_dir_all(&d);
}
