//! `harness ticket gate` (std only, Unix). Chains the merged parts for ONE ticket: intake (which repo, which head), the path map (which
//! checks watch the changed files), the runner (head-only checks) and the both-ends judge, then writes ONE normalized result naming the head sha.
//! Any FAIL, and any error at all, exits non-zero. That non-zero exit IS "Done is refused"; only an all-green run whose result was written and
//! read back exits 0 and prints `done=allowed`. Nothing here judges a check: it asks the merged modules in order and folds their answers.
//! Every value is checked once against its one root, then only the checked value is used: the repo folder is canonicalized once, the base is a
//! full sha, the result folder is canonicalized once and must lie outside the repo, the file name comes only from the validated ticket id.
//! No error or stdout line repeats a mapped or result folder.

use crate::both_ends;
use crate::caps::{self, Caps, LockError, Record};
use crate::gate_result::{GateResult, Status};
use crate::gates::{self, Gates};
use crate::intake::{intake, RepoMap};
use crate::path_map::{self, PathMap};
use crate::runner::{self, Check};
use crate::ticket::Ticket;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

pub const USAGE: &str = "usage: harness ticket gate <ticket-file> --repo-map <repo_id>=<absolute-dir> [--repo-map ...] --base <full-sha> --result-dir <absolute-dir> --caps-dir <absolute-dir>";
/// The folded result's gate id.
pub const GATE_ID: &str = "ticket-gate";
/// One byte over the 1 MiB ticket cap: an oversize file is read no further than this, then refused by the strict reader.
const TICKET_READ_LIMIT: u64 = 1024 * 1024 + 1;

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
    base: String,
    result_dir: PathBuf,
    caps_dir: PathBuf,
}

fn base_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// 1-64 bytes of `A-Z a-z 0-9 . _ -`, not starting with `.`, no `..`: usable as a file name and as nothing else.
pub(crate) fn ticket_id_ok(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        && !s.starts_with('.')
        && !s.contains("..")
}

/// Absolute, and no `..` component.
fn result_dir_arg_ok(s: &str) -> bool {
    let p = Path::new(s);
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir))
}

fn parse_args(a: &[String]) -> Result<Args, Stop> {
    let mut map = RepoMap::new();
    let (mut file, mut base, mut dir, mut caps_dir): (Option<&str>, Option<&str>, Option<&str>, Option<&str>) = (None, None, None, None);
    let mut it = a.iter();
    while let Some(arg) = it.next() {
        let slot = match arg.as_str() {
            "--repo-map" => {
                let Some(spec) = it.next() else { return Err(usage_err("--repo-map is missing its value")) };
                map.add(spec).map_err(|e| usage_err(&e))?;
                continue;
            }
            "--base" => &mut base,
            "--result-dir" => &mut dir,
            "--caps-dir" => &mut caps_dir,
            s if s.starts_with("--") => return Err(usage_err("unknown option")),
            _ => {
                if file.replace(arg.as_str()).is_some() {
                    return Err(usage_err("more than one ticket file given"));
                }
                continue;
            }
        };
        let Some(val) = it.next() else { return Err(usage_err("an option is missing its value")) };
        if slot.replace(val.as_str()).is_some() {
            return Err(usage_err("an option was given twice"));
        }
    }
    let file = file.ok_or_else(|| usage_err("no ticket file given"))?;
    let base = base.ok_or_else(|| usage_err("--base is required"))?;
    let dir = dir.ok_or_else(|| usage_err("--result-dir is required"))?;
    let caps_dir = caps_dir.ok_or_else(|| usage_err("--caps-dir is required"))?;
    if !base_ok(base) {
        return Err(usage_err("--base must be a full lowercase hex commit sha (a ref name is refused)"));
    }
    if !result_dir_arg_ok(dir) {
        return Err(usage_err("--result-dir must be an absolute path with no `..` component"));
    }
    if !result_dir_arg_ok(caps_dir) {
        return Err(usage_err("--caps-dir must be an absolute path with no `..` component"));
    }
    Ok(Args { ticket_file: file.to_string(), map, base: base.to_string(), result_dir: PathBuf::from(dir), caps_dir: PathBuf::from(caps_dir) })
}

/// One git call: no hooks, no inherited `GIT_*` name, no stdin, pinned to the canonical root's own `.git`. Same shape as `path_map::changed_paths`.
fn git_pinned(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let mut c = Command::new("git");
    c.current_dir(root)
        .arg("--no-optional-locks")
        .arg("--git-dir")
        .arg(root.join(".git"))
        .arg("--work-tree")
        .arg(root)
        .args(["-c", "core.hooksPath=/dev/null"])
        .args(args);
    for (k, _) in std::env::vars_os() {
        if k.to_string_lossy().starts_with("GIT_") {
            c.env_remove(k);
        }
    }
    let out = c.stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(out.stdout)
}

/// The folder must BE the head: no modified, staged or untracked path (ignored files are not listed, by design).
fn clean_tree(root: &Path) -> Result<(), Stop> {
    let out = git_pinned(root, &["status", "--porcelain=v1", "-z", "--untracked-files=all"]).ok_or_else(|| err2("git status could not be read"))?;
    if !out.is_empty() {
        return Err(err2("the repo folder is not clean (a changed or untracked path is present)"));
    }
    Ok(())
}

/// The head of the one-entry map's repo. `code` is the exit code of a refusal: 1 the first time (a ticket refusal), 2 when re-checking.
fn head_of(ticket: &Ticket, one: &RepoMap, code: i32) -> Result<String, Stop> {
    match intake(ticket, one) {
        Ok(ok) => Ok(ok.head_sha),
        Err(r) if code == 1 => Err(Stop(1, format!("harness: ticket refused: {}", r))),
        Err(r) => Err(err2(format!("head could not be re-read: {}", r))),
    }
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

/// The result directory: a real directory (not a link), canonicalized once, outside the repo.
fn resolve_result_dir(arg: &Path, root: &Path) -> Result<PathBuf, Stop> {
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

/// The result file: refused when a differently-cased twin exists, or when the target exists and is not a regular file (a link is never followed).
fn check_target(dir: &Path, file: &str) -> Result<PathBuf, Stop> {
    if case_collision(dir, file).map_err(err2)? {
        return Err(err2("a result file whose name differs only by letter case already exists"));
    }
    let target = dir.join(file);
    match std::fs::symlink_metadata(&target) {
        Ok(m) if !m.file_type().is_file() => Err(err2("the result target exists and is not a regular file")),
        Ok(_) => Ok(target),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(target),
        Err(_) => Err(err2("the result target could not be inspected")),
    }
}

fn fold_pass(passed: &[String], head: &str) -> GateResult {
    GateResult {
        status: Status::Pass,
        gate_id: GATE_ID.to_string(),
        head_sha: head.to_string(),
        failure_class: None,
        message: format!("checks passed: {}", passed.join(", ")),
        file: None,
        line: None,
    }
}

/// The first failing result, carried over: its class, file and line unchanged, its message prefixed with its gate id.
fn fold_fail(r: &GateResult, head: &str) -> GateResult {
    GateResult {
        status: Status::Fail,
        gate_id: GATE_ID.to_string(),
        head_sha: head.to_string(),
        failure_class: r.failure_class.clone(),
        message: format!("{}: {}", r.gate_id, r.message),
        file: r.file.clone(),
        line: r.line,
    }
}

fn no_checks(head: &str) -> GateResult {
    GateResult {
        status: Status::Fail,
        gate_id: GATE_ID.to_string(),
        head_sha: head.to_string(),
        failure_class: Some("no-checks".to_string()),
        message: "the diff touches no path, so no check applies and Done on nothing proves nothing".to_string(),
        file: None,
        line: None,
    }
}

/// Temp file with `create_new` (an existing entry, link or not, is never opened), then a rename over the target, then a read-back.
/// The read-back is defence in depth: no planted test kills it.
fn write_result(dir: &Path, id: &str, target: &Path, r: &GateResult) -> Result<(), Stop> {
    let json = r.to_json().map_err(|e| err2(format!("the result is not valid: {}", e)))?;
    let tmp = dir.join(format!(".{}.json.tmp", id));
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(|_| err2("the result file could not be created"))?;
    if f.write_all(json.as_bytes()).and_then(|_| f.sync_all()).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err2("the result file could not be written"));
    }
    drop(f);
    if std::fs::rename(&tmp, target).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return Err(err2("the result file could not be put in place"));
    }
    let back = std::fs::read_to_string(target).map_err(|_| err2("the result file could not be read back"))?;
    match GateResult::from_json(&back) {
        Ok(ref parsed) if parsed == r && back == json => Ok(()),
        _ => Err(err2("the result file read back differently from what was written")),
    }
}

struct Outcome {
    ticket_id: String,
    head: String,
    result: GateResult,
    /// The gate that decided a FAIL (the first failing result's own gate id).
    failing_gate: String,
    /// True when this FAIL used the last allowed round, so the next run will be refused.
    cap_reached: bool,
}

/// What a call ended with: a verdict, or a refusal to run at all (the fix-round cap).
enum Ran {
    Verdict(Outcome),
    Refused { ticket_id: String, head: String, reason: &'static str },
}

fn run(a: &[String]) -> Result<Ran, Stop> {
    // 1. Arguments.
    let args = parse_args(a)?;

    // 2. Ticket and repo. The mapped folder is canonicalized ONCE; the one-entry map built from it is what intake verifies.
    let mut bytes = Vec::new();
    let read = std::fs::File::open(&args.ticket_file).and_then(|f| f.take(TICKET_READ_LIMIT).read_to_end(&mut bytes));
    if read.is_err() {
        return Err(err2("the ticket file cannot be read"));
    }
    let ticket = Ticket::parse_bytes(&bytes).map_err(|e| Stop(1, format!("harness: ticket refused: {}", e)))?;
    let repo_id = ticket.repo_id.as_str().to_string();
    let Some(mapped) = args.map.lookup(&ticket.repo_id) else {
        return Err(Stop(1, format!("harness: ticket refused: repo id `{}` is not in the repo map", repo_id)));
    };
    let not_found = || Stop(1, format!("harness: ticket refused: repo not found for repo id `{}`", repo_id));
    let root: PathBuf = mapped.canonicalize().map_err(|_| not_found())?;
    let root_text = root.to_str().ok_or_else(not_found)?;
    let mut one = RepoMap::new();
    one.add(&format!("{}={}", repo_id, root_text)).map_err(|_| not_found())?;
    let head = head_of(&ticket, &one, 1)?;
    if args.base == head {
        return Err(err2("--base is the same commit as the head"));
    }

    // 3. Result target.
    if !ticket_id_ok(&ticket.id) {
        return Err(err2("the ticket id cannot be used as a file name"));
    }
    let rdir = resolve_result_dir(&args.result_dir, &root)?;
    let target = check_target(&rdir, &format!("{}.json", ticket.id))?;

    // 3b. The cap (point B): the caps folder and record name pass the same rules, a stale result temp is refused BEFORE a round is paid, the
    // per-ticket lock is taken without waiting and held to the end of the run, and the record is read strictly. A ticket already at the cap is refused.
    let caps = Caps::open(&args.caps_dir, &root).map_err(err2)?;
    match std::fs::symlink_metadata(rdir.join(format!(".{}.json.tmp", ticket.id))) {
        Ok(_) => return Err(err2("a stale result temp file is present in the result directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(err2("the result temp name could not be inspected")),
    }
    caps.check_names(&ticket.id).map_err(err2)?;
    let lock = caps.lock(&ticket.id).map_err(|e| match e {
        LockError::Busy => err2("a run for this ticket is in progress"),
        LockError::Other(m) => err2(m),
    })?;
    let used = match caps.read(&ticket.id, &repo_id).map_err(err2)? {
        Some(r) => r.rounds(),
        None => {
            // No record is 0 rounds only when the ticket has never produced a result; a result without its record is a deleted record.
            if std::fs::symlink_metadata(&target).is_ok() {
                return Err(err2("a result file exists but the cap record is missing"));
            }
            0
        }
    };
    if let Err(refusal) = caps::may_start(used) {
        caps.escalate(&refusal, &ticket.id, &repo_id)
            .map_err(|m| Stop(2, format!("harness: ESCALATION NOT RECORDED reason={} ({})", refusal.reason, m)))?;
        return Ok(Ran::Refused { ticket_id: ticket.id, head, reason: refusal.reason });
    }

    // 4. Clean tree: head-only checks run in the repo folder, so the folder must BE the head.
    clean_tree(&root)?;

    // 5. Which checks. One changed_paths call; the path map judges coverage.
    let map = PathMap::load(&root).map_err(err2)?;
    let changed = path_map::changed_paths(&root, &args.base, &head).map_err(err2)?;
    let verdict = path_map::judge(&map, &changed, &head).map_err(err2)?;

    let mut decided: Option<(GateResult, String)> = None;
    let mut passed: Vec<String> = Vec::new();
    let mut plan: Option<(Vec<String>, Gates, BTreeMap<String, Check>)> = None;
    if verdict.status == Status::Fail {
        decided = Some((fold_fail(&verdict, &head), verdict.gate_id.clone()));
    } else if changed.is_empty() {
        decided = Some((no_checks(&head), GATE_ID.to_string()));
    } else {
        let mut ids: Vec<String> = changed.iter().flat_map(|p| map.checks_for(p).iter().cloned()).collect();
        ids.sort();
        ids.dedup();
        let all = Gates::load(&root).map_err(err2)?;
        gates::resolve_map_ids(&all, &ids).map_err(err2)?;
        // Every head-only gate is turned into its Check BEFORE the first run, so a bad program path refuses with nothing run.
        let mut checks: BTreeMap<String, Check> = BTreeMap::new();
        for id in &ids {
            let gate = all.get(id).ok_or_else(|| err2("a selected gate is missing"))?;
            if !gate.both_ends {
                checks.insert(id.clone(), gates::to_check(&root, gate).map_err(err2)?);
            }
        }

        plan = Some((ids, all, checks));
    }

    // 5b. PAY the round (point C), durably, before the first check runs. If the write fails nothing runs. Only a decided PASS refunds it.
    caps.write(&Record { ticket: ticket.id.clone(), repo_id: repo_id.clone(), fix_rounds: used, in_flight: true }, &lock).map_err(err2)?;

    // 6. Run, fail fast, sorted id order.
    if let Some((ids, all, checks)) = &plan {
        for id in ids {
            let gate = all.get(id).ok_or_else(|| err2("a selected gate is missing"))?;
            let r = if gate.both_ends {
                gates::with_spec(gate, |spec| both_ends::judge(&root, &args.base, &head, &head, spec)).and_then(|x| x).map_err(err2)?
            } else {
                let check = checks.get(id).ok_or_else(|| err2("a selected check is missing"))?;
                let r = runner::run(&root, check, &head);
                clean_tree(&root)?;
                r
            };
            if r.status == Status::Fail {
                decided = Some((fold_fail(&r, &head), r.gate_id.clone()));
                break;
            }
            passed.push(id.clone());
        }
    }

    // 7. Head did not move, tree still clean. No result of either kind is written over a moved or dirtied folder.
    clean_tree(&root)?;
    if head_of(&ticket, &one, 2)? != head {
        return Err(err2("the repo head moved while the checks ran"));
    }

    // 8. The verdict's count first (a FAIL keeps its round, a PASS refunds it), THEN one result.
    let (result, failing_gate) = match decided {
        Some((r, g)) => (r, g),
        None => (fold_pass(&passed, &head), GATE_ID.to_string()),
    };
    let fixed = if result.status == Status::Pass { used } else { used + 1 };
    caps.write(&Record { ticket: ticket.id.clone(), repo_id: repo_id.clone(), fix_rounds: fixed, in_flight: false }, &lock).map_err(err2)?;
    write_result(&rdir, &ticket.id, &target, &result)?;
    drop(lock);
    Ok(Ran::Verdict(Outcome { ticket_id: ticket.id, head, result, failing_gate, cap_reached: fixed >= caps::MAX_FIX_ROUNDS }))
}

/// `harness ticket gate ...` (the arguments after `gate`). Exit 0 only for a written PASS; 1 a FAIL (written) or a ticket refusal; 2 any error.
pub fn gate_cmd(a: &[String]) -> i32 {
    match run(a) {
        Ok(Ran::Refused { ticket_id, head, reason }) => {
            println!("ticket={} head={} result=refused done=refused reason={}", ticket_id, head, reason);
            eprintln!("harness: gate REFUSED reason={}", reason);
            1
        }
        Ok(Ran::Verdict(o)) => {
            // The line is built from fixed fields only, never from a check's output.
            if o.result.status == Status::Pass {
                println!("ticket={} head={} result=pass done=allowed", o.ticket_id, o.head);
                0
            } else {
                println!("ticket={} head={} result=fail done=refused", o.ticket_id, o.head);
                eprintln!("harness: gate FAIL class={} gate={}", o.result.failure_class.as_deref().unwrap_or("unknown"), o.failing_gate);
                if o.cap_reached {
                    eprintln!("harness: fix-round cap reached; the next run is refused and escalated");
                }
                1
            }
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
    use std::sync::atomic::{AtomicU64, Ordering};

    static N: AtomicU64 = AtomicU64::new(0);
    const SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn fail_of(class: &str, gate: &str, file: Option<&str>, line: Option<u32>) -> GateResult {
        GateResult {
            status: Status::Fail,
            gate_id: gate.to_string(),
            head_sha: SHA.to_string(),
            failure_class: Some(class.to_string()),
            message: "boom".to_string(),
            file: file.map(|f| f.to_string()),
            line,
        }
    }

    #[test]
    fn base_must_be_a_full_lowercase_sha() {
        assert!(base_ok(SHA) && base_ok(&"a".repeat(64)));
        for bad in ["master", "abc1234", &SHA.to_uppercase(), &"a".repeat(39), &"a".repeat(41), ""] {
            assert!(!base_ok(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_ticket_id_is_usable_only_as_a_file_name() {
        for ok in ["EX-1", "EX-1.v2", "a_b", &"a".repeat(64)] {
            assert!(ticket_id_ok(ok), "{ok:?} is usable");
        }
        for bad in ["", "../escape", "a/b", ".hidden", "a..b", "a b", "a\\b", &"a".repeat(65), "é"] {
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
    fn arguments_need_every_flag_exactly_once() {
        let b = SHA.to_string();
        let good = s(&["t.json", "--repo-map", "r=/x", "--base", &b, "--result-dir", "/res", "--caps-dir", "/caps"]);
        assert!(parse_args(&good).is_ok(), "twin: a complete command line");
        // Every vector carries a valid --caps-dir, so each one fails for its OWN fault (named in the message), not for a missing option.
        let cases: Vec<(&str, Vec<String>)> = vec![
            ("no ticket file", s(&["--caps-dir", "/caps"])),
            ("no ticket file", s(&["--repo-map", "r=/x", "--base", &b, "--result-dir", "/res", "--caps-dir", "/caps"])),
            ("--base is required", s(&["t.json", "--repo-map", "r=/x", "--result-dir", "/res", "--caps-dir", "/caps"])),
            ("--result-dir is required", s(&["t.json", "--repo-map", "r=/x", "--base", &b, "--caps-dir", "/caps"])),
            ("given twice", s(&["t.json", "--base", &b, "--base", &b, "--result-dir", "/res", "--caps-dir", "/caps"])),
            ("given twice", s(&["t.json", "--base", &b, "--result-dir", "/res", "--result-dir", "/res2", "--caps-dir", "/caps"])),
            ("unknown option", s(&["t.json", "--nope", "--base", &b, "--result-dir", "/res", "--caps-dir", "/caps"])),
            ("more than one ticket file", s(&["t.json", "u.json", "--base", &b, "--result-dir", "/res", "--caps-dir", "/caps"])),
            ("missing its value", s(&["t.json", "--caps-dir", "/caps", "--base"])),
            ("--base must be", s(&["t.json", "--base", "master", "--result-dir", "/res", "--caps-dir", "/caps"])),
            ("--result-dir must be", s(&["t.json", "--base", &b, "--result-dir", "res", "--caps-dir", "/caps"])),
        ];
        for (needle, c) in &cases {
            match parse_args(c) {
                Err(Stop(2, m)) => assert!(m.lines().next().unwrap().contains(needle), "{c:?} must fail for its own fault `{needle}`: {m}"),
                _ => panic!("{c:?} must be a usage error"),
            }
        }
        // The new option's own faults.
        let without = s(&["t.json", "--repo-map", "r=/x", "--base", &b, "--result-dir", "/res"]);
        assert!(matches!(parse_args(&without), Err(Stop(2, m)) if m.contains("--caps-dir is required")));
        for bad in ["caps", "/caps/../x"] {
            let v = s(&["t.json", "--base", &b, "--result-dir", "/res", "--caps-dir", bad]);
            assert!(matches!(parse_args(&v), Err(Stop(2, m)) if m.contains("--caps-dir must be")), "{bad:?}");
        }
    }

    #[test]
    fn the_fold_carries_the_first_failure_over_unchanged() {
        let f = fold_fail(&fail_of("unmapped-path", "path-map", Some("x/y.txt"), Some(3)), SHA);
        assert_eq!((f.status.clone(), f.gate_id.as_str()), (Status::Fail, GATE_ID));
        assert_eq!((f.failure_class.as_deref(), f.file.as_deref(), f.line), (Some("unmapped-path"), Some("x/y.txt"), Some(3)));
        assert!(f.message.starts_with("path-map: "), "{}", f.message);
        assert!(f.validate().is_ok());
        let p = fold_pass(&s(&["p", "t"]), SHA);
        assert_eq!(p.status, Status::Pass);
        assert!(p.message.contains("p, t") && p.validate().is_ok());
        assert!(no_checks(SHA).validate().is_ok());
    }

    #[test]
    fn a_name_differing_only_by_case_is_a_collision() {
        let d = std::env::temp_dir().join(format!("tgate-unit-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("ex-1.json"), "x").unwrap();
        assert_eq!(case_collision(&d, "EX-1.json"), Ok(true));
        assert_eq!(case_collision(&d, "ex-1.json"), Ok(false), "the exact name is a replace, not a collision");
        assert_eq!(case_collision(&d, "other.json"), Ok(false));
        let _ = std::fs::remove_dir_all(&d);
    }
}
