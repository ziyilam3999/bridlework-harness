//! `harness status` (std only, read-only). Joins the three places a ticket's standing lives: the ticket file, the repo's newest commit
//! (both through the merged intake) and the one result note the ticket gate left behind. It prints ONE fixed line and runs nothing: no
//! check, no `git status`, no write of any kind. A note is reported, never judged again; it is marked stale when it is about another commit.
//! Every value is checked once against its one root: the repo folder comes from the map only and is canonicalized once, the result folder
//! is canonicalized once and must lie outside the repo, the note's name comes only from the validated ticket id. Every printed value is
//! a validated token or sha; nothing is escaped, anything outside the allow-list is refused before output. No error or stdout line repeats a
//! mapped, result, caps or ticket-file folder, and a note's `message`, `file` and `line` are never printed.
//!
//! `fix_rounds` is the cap record's `rounds` (0-4: runs that started and did not PASS, one in flight counted), read ONLY through the public
//! read-only reader `Caps::read` of the caps folder (`--caps-dir`, required). Status never creates, locks or writes anything there. No record
//! and no note is 0; a record is its rounds; no record but a note is refused (the gate's own meaning: a deleted record); any record the reader
//! refuses is refused, never read as 0.

use crate::gate_result::{GateResult, Status};
use crate::caps::Caps;
use crate::intake::{intake, RepoMap};
use crate::ticket::Ticket;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub const USAGE: &str = "usage: harness status <ticket-file> --repo-map <repo_id>=<absolute-dir> [--repo-map ...] --result-dir <absolute-dir> --caps-dir <absolute-dir>";
/// One byte over the 1 MiB cap: an oversize file is read no further than this, then refused.
const READ_LIMIT: u64 = 1024 * 1024 + 1;
const MAX_BYTES: usize = 1024 * 1024;

/// A stop: the exit code and the full stderr line. Code 1 is a ticket refusal, code 2 is every other error. Stdout stays empty.
struct Stop(i32, String);

fn err2(m: impl AsRef<str>) -> Stop {
    Stop(2, format!("harness: error: {}", m.as_ref()))
}

fn usage_err(m: &str) -> Stop {
    Stop(2, format!("harness: error: {}\n{}", m, USAGE))
}

struct Args {
    ticket_file: String,
    map: RepoMap,
    result_dir: String,
    caps_dir: String,
}

/// 1-64 bytes of `A-Z a-z 0-9 . _ -`, not starting with `.`, no `..`: usable as a file name, as a printed token and as nothing else.
fn ticket_id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !s.starts_with('.')
        && !s.contains("..")
}

/// Absolute, and no `..` component (the shape rule of both folder options).
fn result_dir_arg_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

/// Step 1: the command line: presence, repeat and missing value only. The shape of the result and caps folders is judged at their own steps,
/// after the ticket and the repo have been judged.
fn parse_args(a: &[String]) -> Result<Args, Stop> {
    let mut map = RepoMap::new();
    let (mut file, mut dir, mut caps): (Option<&str>, Option<&str>, Option<&str>) = (None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--repo-map" => {
                let Some(spec) = it.next() else { return Err(usage_err("--repo-map is missing its value")) };
                map.add(spec).map_err(|e| usage_err(&e))?;
            }
            "--result-dir" => {
                let Some(val) = it.next() else { return Err(usage_err("--result-dir is missing its value")) };
                if dir.replace(val.as_str()).is_some() {
                    return Err(usage_err("--result-dir was given twice"));
                }
            }
            "--caps-dir" => {
                let Some(val) = it.next() else { return Err(usage_err("--caps-dir is missing its value")) };
                if caps.replace(val.as_str()).is_some() {
                    return Err(usage_err("--caps-dir was given twice"));
                }
            }
            s if s.starts_with("--") => return Err(usage_err("unknown option")),
            _ => {
                if file.replace(arg.as_str()).is_some() {
                    return Err(usage_err("more than one ticket file given"));
                }
            }
        }
    }
    let file = file.ok_or_else(|| usage_err("no ticket file given"))?;
    let dir = dir.ok_or_else(|| usage_err("--result-dir is required"))?;
    let caps = caps.ok_or_else(|| usage_err("--caps-dir is required"))?;
    Ok(Args { ticket_file: file.to_string(), map, result_dir: dir.to_string(), caps_dir: caps.to_string() })
}

/// The result folder: absolute, no `..`, a real directory (not a link), canonicalized once, outside the repo.
fn resolve_result_dir(arg: &str, root: &Path) -> Result<PathBuf, Stop> {
    if !result_dir_arg_ok(arg) {
        return Err(err2("--result-dir must be an absolute path with no `..` component"));
    }
    let arg = Path::new(arg);
    let meta = std::fs::symlink_metadata(arg).map_err(|_| err2("the result directory is missing or unreadable"))?;
    if !meta.file_type().is_dir() {
        return Err(err2("the result directory must be a real directory, not a link"));
    }
    let canon = arg.canonicalize().map_err(|_| err2("the result directory cannot be resolved"))?;
    if canon.starts_with(root) {
        return Err(err2("the result directory lies inside the repo"));
    }
    Ok(canon)
}

/// The caps folder: the shape rule here (judged at this step, as for the result folder), then the reader's own `Caps::open` (a real directory,
/// not a link, canonicalized once, outside the repo). Opening creates nothing.
fn open_caps(arg: &str, root: &Path) -> Result<Caps, Stop> {
    if !result_dir_arg_ok(arg) {
        return Err(err2("--caps-dir must be an absolute path with no `..` component"));
    }
    Caps::open(Path::new(arg), root).map_err(err2)
}

/// An existing entry equal to `file` ignoring ASCII case but not exactly is a collision (on a case-insensitive filesystem it IS the target).
/// The scan is by name, on every filesystem.
fn case_collision(dir: &Path, file: &str) -> Result<bool, String> {
    let rd = std::fs::read_dir(dir).map_err(|_| "result directory could not be listed".to_string())?;
    for e in rd {
        let name = e.map_err(|_| "result directory could not be listed".to_string())?.file_name().to_string_lossy().into_owned();
        if name != file && name.eq_ignore_ascii_case(file) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The ticket's recorded result: `None` only when the file is genuinely absent. A link, a non-regular entry, an oversize, non-UTF-8 or
/// malformed note, and a differently-cased twin are all errors, never "none".
fn read_note(dir: &Path, id: &str) -> Result<Option<GateResult>, Stop> {
    let file = format!("{}.json", id);
    if case_collision(dir, &file).map_err(err2)? {
        return Err(err2("a result file whose name differs only by letter case exists"));
    }
    let target = dir.join(&file);
    match std::fs::symlink_metadata(&target) {
        Ok(m) if !m.file_type().is_file() => return Err(err2("the result target exists and is not a regular file")),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(err2("the result target could not be inspected")),
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&target).and_then(|f| f.take(READ_LIMIT).read_to_end(&mut bytes));
    if read.is_err() {
        return Err(err2("the result file cannot be read"));
    }
    if bytes.len() > MAX_BYTES {
        return Err(err2("the result file is over 1 MiB"));
    }
    let text = String::from_utf8(bytes).map_err(|_| err2("the result file is not UTF-8"))?;
    GateResult::from_json(&text).map(Some).map_err(|e| err2(format!("the result file is malformed: {}", e)))
}

/// The one output line, from fixed fields only. `gate_current=yes` only on exact head equality.
fn status_line(ticket_id: &str, repo_id: &str, head: &str, note: Option<&GateResult>, rounds: u8) -> String {
    let (last, gate_head, current, class) = match note {
        None => ("none", "none", "none", "none"),
        Some(r) => (
            match r.status {
                Status::Pass => "pass",
                Status::Fail => "fail",
            },
            r.head_sha.as_str(),
            if r.head_sha == head { "yes" } else { "no" },
            r.failure_class.as_deref().unwrap_or("none"),
        ),
    };
    format!(
        "ticket={} repo_id={} head={} last_gate={} gate_head={} gate_current={} failure_class={} fix_rounds={}",
        ticket_id, repo_id, head, last, gate_head, current, class, rounds
    )
}

fn run(a: &[String]) -> Result<String, Stop> {
    // 1. Command line.
    let args = parse_args(a)?;

    // 2. Read the ticket file (bounded). 3. Parse it (a refusal is exit 1, as `ticket intake`).
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&args.ticket_file).and_then(|f| f.take(READ_LIMIT).read_to_end(&mut bytes));
    if read.is_err() {
        return Err(err2("the ticket file cannot be read"));
    }
    let ticket = Ticket::parse_bytes(&bytes).map_err(|e| Stop(1, format!("harness: ticket refused: {}", e)))?;

    // 4. The ticket id must be usable as a file name and as a printed token.
    if !ticket_id_ok(&ticket.id) {
        return Err(err2("the ticket id cannot be used as a file name"));
    }

    // 5. Intake on the canonical repo folder: the one-entry map of that folder is what is verified, and its head is THE head.
    let repo_id = ticket.repo_id.as_str().to_string();
    let Some(mapped) = args.map.lookup(&ticket.repo_id) else {
        return Err(Stop(1, format!("harness: ticket refused: repo id `{}` is not in the repo map", repo_id)));
    };
    let not_found = || Stop(1, format!("harness: ticket refused: repo not found for repo id `{}`", repo_id));
    let root: PathBuf = mapped.canonicalize().map_err(|_| not_found())?;
    let root_text = root.to_str().ok_or_else(not_found)?;
    let mut one = RepoMap::new();
    one.add(&format!("{}={}", repo_id, root_text)).map_err(|_| not_found())?;
    let head = match intake(&ticket, &one) {
        Ok(ok) => ok.head_sha,
        Err(r) => return Err(Stop(1, format!("harness: ticket refused: {}", r))),
    };

    // 6. Result folder. 7. Caps folder. 8. Result file.
    let rdir = resolve_result_dir(&args.result_dir, &root)?;
    let caps = open_caps(&args.caps_dir, &root)?;
    let note = read_note(&rdir, &ticket.id)?;

    // 9. The cap record, through the public reader only. 10. No record but a note is a deleted record (the gate refuses the same state).
    let rounds = match caps.read(&ticket.id, &repo_id).map_err(err2)? {
        Some(rec) => rec.rounds(),
        None if note.is_some() => return Err(err2("a result file exists but the cap record is missing")),
        None => 0,
    };
    Ok(status_line(&ticket.id, &repo_id, &head, note.as_ref(), rounds))
}

/// `harness status ...` (the arguments after `status`). Exit 0 = a line was printed (including `last_gate=none`); 1 the ticket is refused;
/// 2 any other error. On every non-zero exit stdout is empty.
pub fn status_cmd(a: &[String]) -> i32 {
    match run(a) {
        Ok(line) => {
            println!("{}", line);
            0
        }
        Err(Stop(code, line)) => {
            eprintln!("{}", line);
            code
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C1: &str = "1111111111111111111111111111111111111111";
    const C2: &str = "2222222222222222222222222222222222222222";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn note(status: Status, head: &str, class: Option<&str>) -> GateResult {
        GateResult {
            status,
            gate_id: "ticket-gate".to_string(),
            head_sha: head.to_string(),
            failure_class: class.map(|c| c.to_string()),
            message: "m".to_string(),
            file: None,
            line: None,
        }
    }

    #[test]
    fn a_ticket_id_is_usable_only_as_a_file_name_and_a_token() {
        for ok in ["EX-1", "EX-1.v2", "a_b", &"a".repeat(64)] {
            assert!(ticket_id_ok(ok), "{ok:?} is usable");
        }
        for bad in ["", "../escape", "a/b", ".hidden", "a..b", "a b", "a=b", "a\nb", "a\u{1b}b", "a\\b", &"a".repeat(65), "é"] {
            assert!(!ticket_id_ok(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_result_dir_argument_is_absolute_without_dot_dot() {
        assert!(result_dir_arg_ok("/tmp/results"));
        for bad in ["results", "./results", "/tmp/../etc", "/tmp/a/.."] {
            assert!(!result_dir_arg_ok(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_line_has_the_eight_keys_in_order_and_marks_a_stale_note() {
        let fail = status_line("EX-1", "r", C1, Some(&note(Status::Fail, C1, Some("check-failed"))), 1);
        assert!(
            fail.starts_with(&format!("ticket=EX-1 repo_id=r head={C1} last_gate=fail gate_head={C1} gate_current=yes failure_class=check-failed ")),
            "{fail}"
        );
        let pass = status_line("EX-1", "r", C2, Some(&note(Status::Pass, C2, None)), 0);
        assert!(pass.contains("last_gate=pass") && pass.contains("gate_current=yes") && pass.contains("failure_class=none"), "{pass}");
        let stale = status_line("EX-1", "r", C2, Some(&note(Status::Pass, C1, None)), 0);
        assert!(stale.contains(&format!("gate_head={C1} gate_current=no ")), "{stale}");
        let none = status_line("EX-1", "r", C2, None, 0);
        assert!(none.contains("last_gate=none gate_head=none gate_current=none failure_class=none "), "{none}");
    }

    #[test]
    fn arguments_need_a_file_exactly_one_result_dir_and_exactly_one_caps_dir() {
        let good = s(&["t.json", "--repo-map", "r=/x", "--result-dir", "res", "--caps-dir", "caps"]);
        assert!(parse_args(&good).is_ok(), "twin: a complete command line (the folders' own rules come later)");
        let cases: Vec<Vec<String>> = vec![
            s(&[]),
            s(&["--repo-map", "r=/x", "--result-dir", "/res", "--caps-dir", "/caps"]),
            s(&["t.json", "--repo-map", "r=/x", "--caps-dir", "/caps"]),
            s(&["t.json", "--result-dir", "/res", "--result-dir", "/res2", "--caps-dir", "/caps"]),
            s(&["t.json", "--nope", "--result-dir", "/res", "--caps-dir", "/caps"]),
            s(&["t.json", "u.json", "--result-dir", "/res", "--caps-dir", "/caps"]),
            s(&["t.json", "--result-dir", "--caps-dir", "/caps"]),
            s(&["t.json", "--result-dir", "/res", "--caps-dir", "/caps", "--repo-map"]),
        ];
        for c in &cases {
            assert!(matches!(parse_args(c), Err(Stop(2, _))), "{c:?} must be a usage error");
        }
        let caps_cases: Vec<(&str, Vec<String>)> = vec![
            ("--caps-dir is required", s(&["t.json", "--repo-map", "r=/x", "--result-dir", "/res"])),
            ("was given twice", s(&["t.json", "--result-dir", "/res", "--caps-dir", "/c1", "--caps-dir", "/c2"])),
            ("--caps-dir is missing its value", s(&["t.json", "--result-dir", "/res", "--caps-dir"])),
        ];
        for (want, c) in &caps_cases {
            assert!(matches!(parse_args(c), Err(Stop(2, m)) if m.contains(want)), "{c:?} must name {want}");
        }
        // The shape of --caps-dir is NOT a command-line fault here (it is judged after intake, like --result-dir).
        assert!(parse_args(&s(&["t.json", "--result-dir", "/res", "--caps-dir", "../caps"])).is_ok(), "shape is judged at the folder step");
    }

    #[test]
    fn the_rounds_printed_are_the_readers_formula_over_every_valid_state() {
        use crate::caps::{Record, MAX_FIX_ROUNDS};
        let d = std::env::temp_dir().join(format!("status-unit-caps-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let caps = Caps::open(&d, Path::new("/nonexistent-root")).unwrap();
        let mut seen = 0;
        for fix_rounds in 0..=MAX_FIX_ROUNDS {
            for in_flight in [false, true] {
                if !Record::state_ok(fix_rounds, in_flight) {
                    continue;
                }
                seen += 1;
                let rec = Record { ticket: "EX-1".to_string(), repo_id: "r".to_string(), fix_rounds, in_flight };
                std::fs::write(d.join("EX-1.rounds"), rec.encode()).unwrap();
                let rounds = caps.read("EX-1", "r").unwrap().map(|r| r.rounds()).unwrap();
                assert_eq!(rounds, fix_rounds + u8::from(in_flight));
                let line = status_line("EX-1", "r", C1, None, rounds);
                assert!(line.ends_with(&format!(" fix_rounds={rounds}")), "{line}");
            }
        }
        assert_eq!(seen, 9);
        std::fs::write(d.join("EX-1.rounds"), "{\"v\":1,\"ticket\":\"EX-1\",\"repo_id\":\"r\",\"fix_rounds\":4,\"in_flight\":true}\n").unwrap();
        assert!(caps.read("EX-1", "r").is_err(), "twin: 4 rounds with a run in flight is not a valid state");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_caps_dir_argument_shape_is_the_result_dir_shape() {
        let real = std::env::temp_dir().canonicalize().unwrap();
        let ok = real.to_str().unwrap().to_string();
        assert!(open_caps(&ok, Path::new("/nonexistent-root")).is_ok(), "twin: an absolute real directory outside the root");
        for bad in ["caps", "./caps", &format!("{ok}/../{}", real.file_name().unwrap().to_str().unwrap()), &format!("{ok}/a/..")] {
            assert!(open_caps(bad, Path::new("/nonexistent-root")).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_name_differing_only_by_case_is_a_collision() {
        let d = std::env::temp_dir().join(format!("status-unit-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ex-1.json"), "x").unwrap();
        assert_eq!(case_collision(&d, "EX-1.json"), Ok(true));
        assert_eq!(case_collision(&d, "ex-1.json"), Ok(false), "the exact name is not a collision");
        assert_eq!(case_collision(&d, "other.json"), Ok(false));
        let _ = std::fs::remove_dir_all(&d);
    }
}
