//! Both-ends judgement (std only, Unix). The declared check must FAIL at base and PASS at head; the result names the head sha it ran on.
//! `judge_reports` is a pure mapping over the two end reports tree preparation returns: it spawns nothing, calls no git, reads no folder and writes nothing.
//! `judge` is the one call a gate makes: it hands everything to tree preparation, passes any refusal through verbatim, and maps the reports.
//! The end reports are the only source of the base and head shas, and `Ended` is read as given (never re-inferred from output text).
//! A check that already passes at base catches nothing and is FAIL. `Ended::Unknown` is never a red at base and never a pass at head.
//! Neighbour suites (`judge_ticket_reports`, `judge_ticket`): a neighbour already red at base is reported as "red at base" and never counts as a pass or a new
//! failure; a neighbour green at base and not green at head fails the ticket. A neighbour can only turn a PASS into a FAIL, never the other way.
//! Known limits: the two ends run one after the other, so a stateful check can differ between them; a check that exits 126 or 127 at base is read as
//! "something it needs is missing", not as red. In `judge_ticket` every suite goes through tree preparation in turn, so a refusal that tree preparation
//! raises for a later suite (its check path, say) can follow the runs of earlier suites; only the suite names are checked before anything runs.

use crate::gate_result::{GateResult, Status};
use crate::runner::Ended;
use crate::tree_prep::{prepare_and_run, EndReport, Reports, Spec};
use std::path::Path;

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The same id rule `GateResult::validate` and the runner's limits check use: lowercase letters, digits, `-`, `_`, `.`.
fn id_ok(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

fn fail(id: &str, head_sha: &str, class: &str, message: String) -> GateResult {
    GateResult { status: Status::Fail, gate_id: id.to_string(), head_sha: head_sha.to_string(), failure_class: Some(class.to_string()), message, file: None, line: None }
}

fn pass(id: &str, head_sha: &str) -> GateResult {
    GateResult { status: Status::Pass, gate_id: id.to_string(), head_sha: head_sha.to_string(), failure_class: None, message: "fails at base and passes at head".to_string(), file: None, line: None }
}

/// Every value is checked once, here, against its one root (the end reports); after this only the checked values are used.
fn check_entry(base: &EndReport, head: &EndReport, recorded_head: &str, gate_id: &str) -> Result<(), String> {
    if !id_ok(gate_id) {
        return Err("gate id must be lowercase letters, digits, '-', '_' or '.'".to_string());
    }
    if !sha_ok(&base.sha) || !sha_ok(&head.sha) {
        return Err("base and head must be full lowercase hex shas".to_string());
    }
    if head.sha != recorded_head {
        return Err("head does not match the recorded sha".to_string());
    }
    if base.sha == head.sha {
        return Err("base and head are the same commit".to_string());
    }
    Ok(())
}

/// Judge the declared check from the two end reports. Err means the request is refused (no result is produced); Ok carries a normalized result naming
/// the head sha taken from the head report.
pub fn judge_reports(reports: &Reports, recorded_head: &str, gate_id: &str) -> Result<GateResult, String> {
    let (base, head) = (&reports.base, &reports.head);
    check_entry(base, head, recorded_head, gate_id)?;

    // At base, before the head is looked at. Only a normal non-zero exit is a red result. 126 and 127 mean something the check needs is missing;
    // a signal, a timeout, a failed start or an unknown ending is a harness problem, never the test catching something.
    match base.ended {
        Ended::Exit(0) => return Ok(fail(gate_id, &head.sha, "passes-at-base", "the check passes at base, so it catches nothing".to_string())),
        Ended::Exit(c) if c != 126 && c != 127 => {}
        _ => return Ok(fail(gate_id, &head.sha, "base-error", "the check did not run cleanly at base, so it is not a red result".to_string())),
    }

    // At head, reached only when base was red. Only a clean exit 0 is a pass.
    Ok(match head.ended {
        Ended::Exit(0) => pass(gate_id, &head.sha),
        Ended::Exit(c) => fail(gate_id, &head.sha, "check-failed", format!("the check still fails at head (exit {}): {}", c, head.output)),
        Ended::Signal(s) => fail(gate_id, &head.sha, "head-signal", format!("the check was killed by signal {} at head", s)),
        Ended::TimedOut => fail(gate_id, &head.sha, "head-timeout", "the check ran past its budget at head".to_string()),
        Ended::NotStarted => fail(gate_id, &head.sha, "head-not-started", "the check could not be started at head".to_string()),
        Ended::Unknown => fail(gate_id, &head.sha, "head-unknown", "how the check ended at head could not be told, so it cannot certify the fix".to_string()),
    })
}

/// The one call a gate makes for both-ends mode: prepare both trees, run the check once in each, map the two reports.
/// Every refusal comes from tree preparation and passes through verbatim; this adds none of its own.
pub fn judge(repo_path: &Path, base: &str, head: &str, recorded_head: &str, spec: &Spec) -> Result<GateResult, String> {
    let reports = prepare_and_run(repo_path, base, head, recorded_head, spec)?;
    judge_reports(&reports, recorded_head, spec.id)
}

// ---------- neighbour suites: "red at base" ----------

/// What one neighbour suite's two end reports mean for the ticket. A neighbour never produces or contributes to a pass.
#[derive(Debug, PartialEq, Eq)]
enum Neighbour {
    /// Already red before the change (a clean non-zero exit at base): reported, never a pass and never a new failure.
    RedAtBase,
    Fine,
    NewFailure(i32),
    HeadError(Ended),
    BaseError,
}

/// Base first, with the declared check's own red rule (only a clean non-zero exit other than 126 or 127 is red).
fn classify_neighbour(base: &EndReport, head: &EndReport) -> Neighbour {
    match base.ended {
        Ended::Exit(c) if c != 0 && c != 126 && c != 127 => Neighbour::RedAtBase,
        Ended::Exit(0) => match head.ended {
            Ended::Exit(0) => Neighbour::Fine,
            Ended::Exit(c) => Neighbour::NewFailure(c),
            other => Neighbour::HeadError(other),
        },
        // 126, 127, a signal, a timeout, a failed start or an unknown ending at base: we cannot tell whether a red head is new, so only a verifiably green head is fine.
        _ => match head.ended {
            Ended::Exit(0) => Neighbour::Fine,
            _ => Neighbour::BaseError,
        },
    }
}

fn ending_text(e: Ended) -> String {
    match e {
        Ended::Exit(c) => format!("exit {}", c),
        Ended::Signal(s) => format!("signal {}", s),
        Ended::TimedOut => "timed out".to_string(),
        Ended::NotStarted => "not started".to_string(),
        Ended::Unknown => "unknown ending".to_string(),
    }
}

/// Suite names, checked once and before anything runs: well formed, pairwise distinct, and distinct from the gate id (one suite, one name).
fn check_ids<'a>(gate_id: &str, neighbour_ids: impl Iterator<Item = &'a str>) -> Result<(), String> {
    if !id_ok(gate_id) {
        return Err("gate id must be lowercase letters, digits, '-', '_' or '.'".to_string());
    }
    let mut seen = vec![gate_id];
    for id in neighbour_ids {
        if !id_ok(id) {
            return Err("neighbour id must be lowercase letters, digits, '-', '_' or '.'".to_string());
        }
        if seen.contains(&id) {
            return Err("suite ids must be distinct from each other and from the gate id".to_string());
        }
        seen.push(id);
    }
    Ok(())
}

/// Judge the declared check and fold in the neighbour suites. The declared verdict comes from `judge_reports`; a neighbour can only turn a PASS into a
/// FAIL (the first finding's class), never a FAIL into a PASS, and never rewrites a declared FAIL's class. Every red-at-base neighbour is reported in the
/// message on PASS and FAIL alike. An empty neighbour list gives exactly the `judge_reports` result.
pub fn judge_ticket_reports(reports: &Reports, neighbours: &[(&str, &Reports)], recorded_head: &str, gate_id: &str) -> Result<GateResult, String> {
    check_ids(gate_id, neighbours.iter().map(|(id, _)| *id))?;
    check_entry(&reports.base, &reports.head, recorded_head, gate_id)?;
    for (id, r) in neighbours {
        check_entry(&r.base, &r.head, recorded_head, id)?;
        // One base commit and one head commit for the whole ticket: a disagreement is two roots for "which code ran".
        if r.base.sha != reports.base.sha || r.head.sha != reports.head.sha {
            return Err("a neighbour ran against a different base or head commit than the declared check".to_string());
        }
    }
    let mut out = judge_reports(reports, recorded_head, gate_id)?;

    let mut red_at_base: Vec<&str> = Vec::new();
    let mut findings: Vec<(&str, String)> = Vec::new();
    for (id, r) in neighbours {
        match classify_neighbour(&r.base, &r.head) {
            Neighbour::RedAtBase => red_at_base.push(id),
            Neighbour::Fine => {}
            Neighbour::NewFailure(c) => findings.push(("neighbour-new-failure", format!("neighbour {} broke at head (exit {}): {}", id, c, r.head.output))),
            Neighbour::HeadError(e) => {
                findings.push(("neighbour-head-error", format!("neighbour {} could not be verified at head ({}) after a green base", id, ending_text(e))))
            }
            Neighbour::BaseError => findings.push((
                "neighbour-base-error",
                format!("neighbour {} could not be read at base ({}) and is not green at head, so a new failure cannot be ruled out", id, ending_text(r.base.ended)),
            )),
        }
    }
    if !findings.is_empty() {
        let text = findings.iter().map(|f| f.1.as_str()).collect::<Vec<_>>().join("; ");
        if out.status == Status::Pass {
            out.status = Status::Fail;
            out.failure_class = Some(findings[0].0.to_string());
            out.message = text;
        } else {
            out.message = format!("{}; {}", out.message, text);
        }
    }
    if !red_at_base.is_empty() {
        out.message = format!("{}; red at base: {}", out.message, red_at_base.join(", "));
    }
    Ok(out)
}

/// The one call a gate makes for a ticket with neighbour suites: the declared check and every neighbour each go through tree preparation (one tree pair per
/// suite), every refusal passes through verbatim, and the reports are folded by `judge_ticket_reports`. Suite names are checked before any tree exists.
pub fn judge_ticket(repo_path: &Path, base: &str, head: &str, recorded_head: &str, spec: &Spec, neighbour_specs: &[Spec]) -> Result<GateResult, String> {
    check_ids(spec.id, neighbour_specs.iter().map(|s| s.id))?;
    let reports = prepare_and_run(repo_path, base, head, recorded_head, spec)?;
    let mut neighbour_reports: Vec<Reports> = Vec::new();
    for s in neighbour_specs {
        neighbour_reports.push(prepare_and_run(repo_path, base, head, recorded_head, s)?);
    }
    let pairs: Vec<(&str, &Reports)> = neighbour_specs.iter().map(|s| s.id).zip(neighbour_reports.iter()).collect();
    judge_ticket_reports(&reports, &pairs, recorded_head, spec.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::time::Duration;

    const BASE: &str = "0123456789abcdef0123456789abcdef01234567";
    const HEAD: &str = "89abcdef0123456789abcdef0123456789abcdef";
    const OTHER: &str = "fedcba9876543210fedcba9876543210fedcba98";
    const ID: &str = "ticket-test";

    fn rp(base: Ended, head: Ended, head_out: &str) -> Reports {
        Reports {
            base: EndReport { sha: BASE.to_string(), ended: base, output: "base output".to_string() },
            head: EndReport { sha: HEAD.to_string(), ended: head, output: head_out.to_string() },
        }
    }

    fn red_green() -> Reports {
        rp(Ended::Exit(1), Ended::Exit(0), "")
    }

    #[test]
    fn a_neighbour_red_at_base_is_reported_and_never_changes_the_ticket_result() {
        for (declared, label) in [(red_green(), "declared fixes it"), (rp(Ended::Exit(0), Ended::Exit(0), ""), "declared passes at base")] {
            let alone = judge_ticket_reports(&declared, &[], HEAD, ID).unwrap();
            for head in [Ended::Exit(0), Ended::Exit(1), Ended::Exit(2)] {
                let nb = rp(Ended::Exit(1), head, "neighbour output");
                let r = judge_ticket_reports(&declared, &[("nb-suite", &nb)], HEAD, ID).unwrap();
                assert_eq!((&r.status, &r.failure_class), (&alone.status, &alone.failure_class), "{} / neighbour head {:?}", label, head);
                assert!(r.message.contains("red at base: nb-suite"), "{}", r.message);
                assert!(r.validate().is_ok());
            }
        }
        assert_eq!(judge_ticket_reports(&red_green(), &[], HEAD, ID).unwrap().status, Status::Pass);
    }

    #[test]
    fn a_neighbour_green_at_base_and_red_at_head_fails_the_ticket() {
        let nb = rp(Ended::Exit(0), Ended::Exit(3), "line\nNB-MARKER\n[output cut]");
        let r = judge_ticket_reports(&red_green(), &[("nb-suite", &nb)], HEAD, ID).unwrap();
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.failure_class.as_deref(), Some("neighbour-new-failure"));
        assert!(r.message.contains("nb-suite") && r.message.contains("NB-MARKER"), "{}", r.message);
        assert_eq!(r.head_sha, HEAD);
        assert!(r.validate().is_ok());
        // a declared FAIL keeps its own class; the neighbour finding is appended
        let declared_fail = rp(Ended::Exit(0), Ended::Exit(0), "");
        let r = judge_ticket_reports(&declared_fail, &[("nb-suite", &nb)], HEAD, ID).unwrap();
        assert_eq!(r.failure_class.as_deref(), Some("passes-at-base"));
        assert!(r.message.contains("NB-MARKER"), "{}", r.message);
    }

    #[test]
    fn an_unverifiable_neighbour_head_after_a_green_base_is_never_green() {
        for head in [Ended::Signal(9), Ended::TimedOut, Ended::NotStarted, Ended::Unknown] {
            let nb = rp(Ended::Exit(0), head, "");
            let r = judge_ticket_reports(&red_green(), &[("nb-suite", &nb)], HEAD, ID).unwrap();
            assert_eq!(r.status, Status::Fail, "head {:?}", head);
            assert_eq!(r.failure_class.as_deref(), Some("neighbour-head-error"), "head {:?}", head);
            assert!(r.validate().is_ok());
        }
    }

    #[test]
    fn a_neighbour_base_that_cannot_be_read_fails_closed_unless_the_head_is_green() {
        let alone = judge_ticket_reports(&red_green(), &[], HEAD, ID).unwrap();
        for base in [Ended::Exit(126), Ended::Exit(127), Ended::Signal(9), Ended::TimedOut, Ended::NotStarted, Ended::Unknown] {
            let red_head = rp(base, Ended::Exit(1), "");
            let r = judge_ticket_reports(&red_green(), &[("nb-suite", &red_head)], HEAD, ID).unwrap();
            assert_eq!(r.status, Status::Fail, "base {:?}", base);
            assert_eq!(r.failure_class.as_deref(), Some("neighbour-base-error"), "base {:?}", base);
            let green_head = rp(base, Ended::Exit(0), "");
            let r = judge_ticket_reports(&red_green(), &[("nb-suite", &green_head)], HEAD, ID).unwrap();
            assert_eq!((&r.status, &r.failure_class), (&alone.status, &alone.failure_class), "base {:?}", base);
            assert!(!r.message.contains("red at base"), "an unreadable base is not red at base: {}", r.message);
        }
    }

    #[test]
    fn neighbour_disagreement_and_bad_ids_are_refused() {
        let ok = red_green();
        let nb = |base: &str, head: &str| {
            let mut r = rp(Ended::Exit(0), Ended::Exit(0), "");
            r.base.sha = base.to_string();
            r.head.sha = head.to_string();
            r
        };
        let (head_differs, base_differs) = (nb(BASE, OTHER), nb(OTHER, HEAD));
        assert!(judge_ticket_reports(&ok, &[("a", &head_differs)], HEAD, ID).is_err(), "neighbour head differs");
        assert!(judge_ticket_reports(&ok, &[("a", &base_differs)], HEAD, ID).is_err(), "neighbour base differs");
        let fine = nb(BASE, HEAD);
        assert!(judge_ticket_reports(&ok, &[("a", &fine)], OTHER, ID).is_err(), "recorded head matches neither");
        assert!(judge_ticket_reports(&ok, &[("a", &fine), ("a", &fine)], HEAD, ID).is_err(), "two neighbours share an id");
        assert!(judge_ticket_reports(&ok, &[(ID, &fine)], HEAD, ID).is_err(), "neighbour id equals the gate id");
        assert!(judge_ticket_reports(&ok, &[("bad id!", &fine)], HEAD, ID).is_err(), "malformed neighbour id");
        assert!(judge_ticket_reports(&ok, &[("a", &fine)], HEAD, ID).is_ok(), "the well-formed control is accepted");
    }

    #[test]
    fn an_empty_neighbour_list_reproduces_the_single_check_judgement() {
        for declared in [red_green(), rp(Ended::Exit(0), Ended::Exit(0), ""), rp(Ended::Exit(1), Ended::Exit(3), "still red")] {
            assert_eq!(judge_ticket_reports(&declared, &[], HEAD, ID), judge_reports(&declared, HEAD, ID));
        }
    }

    // ---------- end to end through the real tree preparation ----------

    fn scratch(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("hbe-nb-{}-{}", name, std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let o = Command::new("git")
            .current_dir(dir)
            .env("GIT_AUTHOR_NAME", "Tester")
            .env("GIT_AUTHOR_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
            .env("GIT_COMMITTER_NAME", "Tester")
            .env("GIT_COMMITTER_EMAIL", ["1+t", "@", "users.", "noreply.example"].concat())
            .args(["-c", "init.defaultBranch=main", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(o.status.success(), "git failed: {:?} {}", args, String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn put(dir: &Path, rel: &str, body: &str, mode: u32) {
        let f = dir.join(rel);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, body).unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    /// Base commit: value.txt "bad", other.txt = `nb_base`, the neighbour suite. Head commit: value.txt "good", other.txt = `nb_head`, the declared check added.
    fn repo(name: &str, nb_base: &str, nb_head: &str, declared_check: &str) -> (PathBuf, String, String) {
        let repo = scratch(name);
        git(&repo, &["init", "-q"]);
        put(&repo, "value.txt", "bad", 0o644);
        put(&repo, "other.txt", nb_base, 0o644);
        put(&repo, "ci/nb.sh", "#!/bin/sh\ntest \"$(cat other.txt)\" = ok\n", 0o755);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let base = git(&repo, &["rev-parse", "HEAD"]);
        put(&repo, "value.txt", "good", 0o644);
        put(&repo, "other.txt", nb_head, 0o644);
        put(&repo, "ci/check.sh", declared_check, 0o755);
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "-m", "head"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        (repo, base, head)
    }

    fn only_the_main_tree(repo: &Path) {
        let list = git(repo, &["worktree", "list", "--porcelain"]);
        assert_eq!(list.lines().filter(|l| l.starts_with("worktree ")).count(), 1, "stale worktrees:\n{}", list);
    }

    fn spec<'a>(id: &'a str, program: &'a str) -> Spec<'a> {
        Spec { id, program, args: &[], budget: Duration::from_secs(20), max_output: 4000 }
    }

    const FIX_CHECK: &str = "#!/bin/sh\ntest \"$(cat value.txt)\" = good\n";

    #[test]
    fn end_to_end_with_neighbours_on_a_scratch_repo() {
        // the neighbour was broken before the change and still is: reported, and the ticket still passes
        let (a, base, head) = repo("red", "broken", "broken", FIX_CHECK);
        let r = judge_ticket(&a, &base, &head, &head, &spec(ID, "ci/check.sh"), &[spec("nb-suite", "ci/nb.sh")]).unwrap();
        assert_eq!(r.status, Status::Pass, "{:?} {}", r.failure_class, r.message);
        assert_eq!(r.head_sha, git(&a, &["rev-parse", "HEAD"]));
        assert!(r.message.contains("red at base: nb-suite"), "{}", r.message);
        assert!(r.validate().is_ok());
        only_the_main_tree(&a);

        // the neighbour was fine before and the head commit broke it: the ticket fails
        let (b, base, head) = repo("new", "ok", "broken", FIX_CHECK);
        let r = judge_ticket(&b, &base, &head, &head, &spec(ID, "ci/check.sh"), &[spec("nb-suite", "ci/nb.sh")]).unwrap();
        assert_eq!(r.status, Status::Fail);
        assert_eq!(r.failure_class.as_deref(), Some("neighbour-new-failure"));
        assert!(r.message.contains("nb-suite"), "{}", r.message);
        only_the_main_tree(&b);

        // suite names are refused before any tree exists: the declared check would leave a marker if it ran
        let marker = scratch("marker").join("ran");
        let marking = format!("#!/bin/sh\ntouch {}\ntest \"$(cat value.txt)\" = good\n", marker.display());
        let (c, base, head) = repo("ids", "ok", "ok", &marking);
        let e = judge_ticket(&c, &base, &head, &head, &spec(ID, "ci/check.sh"), &[spec("nb-suite", "ci/nb.sh"), spec("nb-suite", "ci/nb.sh")]).unwrap_err();
        assert!(e.contains("distinct"), "{}", e);
        assert!(!marker.exists(), "the declared check ran before the id refusal");
        let e = judge_ticket(&c, &base, &head, &head, &spec(ID, "ci/check.sh"), &[spec("bad id!", "ci/nb.sh")]).unwrap_err();
        assert!(e.contains("neighbour id"), "{}", e);
        assert!(!marker.exists(), "the declared check ran before the malformed-id refusal");
        only_the_main_tree(&c);

        for d in [a, b, c, marker.parent().unwrap().to_path_buf()] {
            let _ = std::fs::remove_dir_all(d);
        }
    }
}
