//! The session name check (S-1.6.2, P1B-16). One predicate over the name shapes the live launcher
//! emits, plus the coordinator name, the transitional long forms and the frozen legacy table.
//! Classification is by shape only: no name from any acceptance list is special-cased, and the
//! lane upper bound is always the caller's configured count (`lanes.count`), never a default.
//! A refusal is one line naming the offending part of the name.
//!
//! Families (the closed allowlist; the role word picks the ONE family the rest must match):
//! 1. launcher: `<role>-L<n>-t<slug>-<S|M|L>-<model>-<effort>`
//! 2. `Orchestrator`, exactly.
//! 3. long (transitional): `<role>-l<n>-<slug>`, `planner-ahead-<slug>`,
//!    `(governance|risk-review)-p<digit><slug>-signoff`
//! 4. legacy: the frozen five-row table.
//! `--new` (the launcher's mode) accepts family 1 only.

/// Launcher role words: the live launcher's short seat words.
pub const LAUNCHER_ROLES: [&str; 10] =
    ["plan", "exec", "p-rvw", "e-rvw", "rsch", "ship", "learn", "know", "gov", "push"];
/// Long-form role words (transitional families, running sessions only).
const LONG_ROLES: [&str; 6] = ["planner", "execution", "risk-review", "learning", "knowledge", "governance"];
/// Model words: the live launcher's short model words.
const MODELS: [&str; 7] = ["son", "opu", "fab", "hai", "glm", "glf", "loc"];
/// Effort words: the first three letters of the launcher's effort flags.
const EFFORTS: [&str; 5] = ["low", "med", "hig", "xhi", "max"];
/// The frozen legacy table (rule 4): exact name -> ticket read-back (`None` = no public ticket).
const LEGACY: [(&str, Option<&str>); 5] = [
    ("builder1-p1a13", Some("p1a13")),
    ("lane3-pre6a-builder", Some("pre6a")),
    ("lane2-pre4-planner", Some("pre4")),
    ("orchestration-l2-p1a06", Some("p1a06")),
    ("D16", None),
];
/// Every word a shaped name can start with. `p-rvw` and `e-rvw` are single words, and `plan` never
/// swallows `planner` because a word match always requires the `-` terminator after the word.
const START_WORDS: [&str; 16] = [
    "plan", "exec", "p-rvw", "e-rvw", "rsch", "ship", "learn", "know", "gov", "push",
    "planner", "execution", "risk-review", "learning", "knowledge", "governance",
];

/// The read-back of an accepted name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verdict {
    /// `launcher`, `orchestrator`, `long` or `legacy`.
    pub kind: &'static str,
    /// The matched role word, `-` when the family has none.
    pub role: &'static str,
    /// The lane number when the family carries one.
    pub lane: Option<u64>,
    /// The ticket slug when the family carries one.
    pub ticket: Option<String>,
}

impl Verdict {
    /// The one stdout line of an accepted name. Each family carries exactly the fields its
    /// read-back defines: the launcher form reads back role, lane and ticket; the long form reads
    /// back lane and ticket (`-` when the shape carries neither slot's value); the legacy table
    /// reads back the ticket only; the coordinator name reads back nothing (`lane=- ticket=-`).
    pub fn line(&self) -> String {
        let lane = match self.lane {
            Some(n) => n.to_string(),
            None => "-".to_string(),
        };
        let ticket = self.ticket.clone().unwrap_or_else(|| "-".to_string());
        match self.kind {
            "launcher" => {
                format!("ok kind=launcher role={} lane={} ticket={}", self.role, lane, ticket)
            }
            "legacy" => format!("ok kind=legacy ticket={ticket}"),
            k => format!("ok kind={} lane={} ticket={}", k, lane, ticket),
        }
    }
}

/// Check one session name against the closed allowlist. `lanes` is the configured lane count (the
/// upper bound of a legal lane number, `lanes.count`); `new_only` is the launcher's `--new` mode,
/// which accepts the launcher family only. `Ok` carries the verdict and read-back; `Err` is a
/// one-line reason naming the offending part.
pub fn check(name: &str, lanes: u64, new_only: bool) -> Result<Verdict, String> {
    if name.is_empty() {
        return Err("name: the session name is empty".to_string());
    }
    let verdict = match longest_word(name) {
        Some(w) if LAUNCHER_ROLES.contains(&w) => launcher(name, w, lanes)?,
        Some(w) if LONG_ROLES.contains(&w) => long(name, w, lanes)?,
        Some(w) => return Err(format!("role: `{w}` is a start word with no family")),
        None => {
            if name == "Orchestrator" {
                Verdict { kind: "orchestrator", role: "-", lane: None, ticket: None }
            } else if let Some((_, ticket)) = LEGACY.iter().find(|(n, _)| *n == name) {
                Verdict { kind: "legacy", role: "-", lane: None, ticket: ticket.map(|t| t.to_string()) }
            } else {
                return Err(format!(
                    "role: `{}` does not start with a known role word and is not in the legacy table",
                    first_part(name)
                ));
            }
        }
    };
    if new_only && verdict.kind != "launcher" {
        return Err(format!(
            "--new: a `{}` name is not accepted; --new accepts the launcher form only",
            verdict.kind
        ));
    }
    Ok(verdict)
}

/// The longest word in `START_WORDS` the name starts with as a whole word followed by `-`.
fn longest_word(name: &str) -> Option<&'static str> {
    let mut best: Option<&'static str> = None;
    for w in START_WORDS {
        if !starts_word(name, w) {
            continue;
        }
        let longer = match best {
            Some(b) => w.len() > b.len(),
            None => true,
        };
        if longer {
            best = Some(w);
        }
    }
    best
}

/// `name` starts with `w` followed by `-` and has at least one byte after it.
fn starts_word(name: &str, w: &str) -> bool {
    name.len() > w.len() + 1 && &name[..w.len()] == w && name.as_bytes()[w.len()] == b'-'
}

/// Family 1: `<role>-L<n>-t<slug>-<S|M|L>-<model>-<effort>`. Each part is checked in order and the
/// first bad one is named; a part count other than five is a task-part defect by construction
/// (every other part is hyphen-free).
fn launcher(name: &str, word: &'static str, lanes: u64) -> Result<Verdict, String> {
    let rest = &name[word.len() + 1..];
    let segs: Vec<&str> = rest.split('-').collect();
    if segs.len() > 5 {
        return Err(format!(
            "task: {} `-`-separated parts follow the role word, 5 expected (the task slug cannot contain `-`)",
            segs.len()
        ));
    }
    let lane = match segs.first() {
        Some(s) => lane_number(s, 'L', lanes)
            .ok_or_else(|| format!("lane: `{s}` is not `L<n>`, a lane number 1..={lanes} with no leading zero"))?,
        None => return Err("lane: the lane part is missing".to_string()),
    };
    let ticket = match segs.get(1) {
        Some(s) => task_slug(s)
            .ok_or_else(|| format!("task: `{s}` is not `t<slug>`, `t` plus lowercase letters and digits only"))?
            .to_string(),
        None => return Err("task: the `t<slug>` part is missing".to_string()),
    };
    match segs.get(2) {
        Some(&"S") | Some(&"M") | Some(&"L") => {}
        Some(s) => return Err(format!("size: `{s}` is not one of S, M, L")),
        None => return Err("size: the size part is missing".to_string()),
    }
    match segs.get(3) {
        Some(s) if MODELS.contains(s) => {}
        Some(s) => return Err(format!("model: `{s}` is not a known model word")),
        None => return Err("model: the model part is missing".to_string()),
    }
    match segs.get(4) {
        Some(s) if EFFORTS.contains(s) => {}
        Some(s) => return Err(format!("effort: `{s}` is not a known effort word")),
        None => return Err("effort: the effort part is missing".to_string()),
    }
    Ok(Verdict { kind: "launcher", role: word, lane: Some(lane), ticket: Some(ticket) })
}

/// Family 3 (transitional): `<role>-l<n>-<slug>`, `planner-ahead-<slug>`,
/// `(governance|risk-review)-p<digit><slug>-signoff`.
fn long(name: &str, word: &'static str, lanes: u64) -> Result<Verdict, String> {
    let rest = &name[word.len() + 1..];
    if rest.starts_with('l') {
        let Some((seg, tail)) = rest.split_once('-') else {
            return Err("task: the slug after the lane is missing".to_string());
        };
        let lane = lane_number(seg, 'l', lanes)
            .ok_or_else(|| format!("lane: `{seg}` is not `l<n>`, a lane number 1..={lanes} with no leading zero"))?;
        if !lower_slug(tail) {
            return Err(format!("task: `{tail}` is not a slug of lowercase letters, digits and `-`"));
        }
        return Ok(Verdict { kind: "long", role: word, lane: Some(lane), ticket: Some(tail.to_string()) });
    }
    if word == "planner" {
        if let Some(slug) = rest.strip_prefix("ahead-") {
            if !lower_slug(slug) {
                return Err(format!("task: `{slug}` is not a slug of lowercase letters, digits and `-`"));
            }
            return Ok(Verdict { kind: "long", role: word, lane: None, ticket: Some(slug.to_string()) });
        }
    }
    if word == "governance" || word == "risk-review" {
        if let Some(mid) = rest.strip_prefix('p') {
            if let Some(inner) = mid.strip_suffix("-signoff") {
                let ok = !inner.is_empty()
                    && inner.as_bytes()[0].is_ascii_digit()
                    && inner.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
                if ok {
                    return Ok(Verdict { kind: "long", role: word, lane: None, ticket: None });
                }
                return Err(format!("task: `{rest}` is not `p<digit><slug>-signoff`"));
            }
        }
    }
    let extra = if word == "planner" { " or `ahead-<slug>`" } else { " or `p<digit><slug>-signoff`" };
    Err(format!(
        "lane: `{}` does not match the long form for `{word}` (`l<lane>-<slug>`{extra})",
        first_part(rest)
    ))
}

/// `<letter><digits>`: 1..=`lanes`, digits only, no leading zero, at most 15 digits (the config's number rule).
fn lane_number(seg: &str, letter: char, lanes: u64) -> Option<u64> {
    let digits = seg.strip_prefix(letter)?;
    if digits.is_empty() || digits.len() > 15 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') {
        return None;
    }
    let n: u64 = digits.parse().ok()?;
    if n < 1 || n > lanes {
        return None;
    }
    Some(n)
}

/// `t` plus a non-empty run of lowercase letters and digits.
fn task_slug(seg: &str) -> Option<&str> {
    let slug = seg.strip_prefix('t')?;
    let ok = !slug.is_empty() && slug.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
    if ok {
        Some(slug)
    } else {
        None
    }
}

/// A non-empty slug of lowercase letters, digits and `-` (the long-form read-back keeps it whole).
fn lower_slug(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The name up to its first `-` (echoed only inside a refusal that already names the whole name).
fn first_part(name: &str) -> &str {
    name.split('-').next().unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK_ERR: &str = "must be accepted";

    fn accept(name: &str, lanes: u64) -> Verdict {
        check(name, lanes, false).unwrap_or_else(|e| panic!("{OK_ERR}: {name}: {e}"))
    }

    fn refuse(name: &str, lanes: u64, part: &str) {
        match check(name, lanes, false) {
            Ok(v) => panic!("must be refused: {name}: {v:?}"),
            Err(e) => assert!(e.contains(part), "{name}: refusal must name the part `{part}`: {e}"),
        }
    }

    // ---- the word tables, one acceptance per word (a dropped word is caught here) ----

    #[test]
    fn every_launcher_role_word_accepts() {
        for w in LAUNCHER_ROLES {
            let v = accept(&format!("{w}-L1-t7-M-son-low"), 4);
            assert_eq!(v.kind, "launcher");
            assert_eq!(v.role, w);
        }
    }

    #[test]
    fn every_long_role_word_accepts() {
        for w in LONG_ROLES {
            let v = accept(&format!("{w}-l2-slug1"), 4);
            assert_eq!(v.kind, "long");
            assert_eq!(v.role, w);
        }
    }

    #[test]
    fn every_model_word_accepts() {
        for m in MODELS {
            accept(&format!("exec-L1-t7-M-{m}-low"), 4);
        }
    }

    #[test]
    fn every_effort_word_accepts() {
        for e in EFFORTS {
            accept(&format!("exec-L1-t7-M-son-{e}"), 4);
        }
    }

    #[test]
    fn every_size_letter_accepts() {
        for s in ["S", "M", "L"] {
            accept(&format!("exec-L1-t7-{s}-son-low"), 4);
        }
    }

    // ---- the launcher family, part by part ----

    #[test]
    fn launcher_read_back() {
        assert_eq!(accept("exec-L1-t113-M-son-med", 4).line(), "ok kind=launcher role=exec lane=1 ticket=113");
        assert_eq!(
            accept("e-rvw-L4-t120-M-opu-hig", 4).line(),
            "ok kind=launcher role=e-rvw lane=4 ticket=120"
        );
        // a long task slug reads back whole: t + the slug, digits and letters only
        assert_eq!(accept("plan-L2-tp1a13-S-hai-max", 4).line(), "ok kind=launcher role=plan lane=2 ticket=p1a13");
    }

    #[test]
    fn lane_at_the_configured_count_is_accepted() {
        // the upper bound is inclusive: lane == count passes
        accept("exec-L4-t7-M-son-low", 4);
    }

    #[test]
    fn lane_above_the_count_is_refused() {
        refuse("exec-L5-t7-M-son-low", 4, "lane");
        refuse("execution-l5-x", 4, "lane");
        // the same name at the higher count is fine: the bound comes from the caller only
        accept("exec-L5-t7-M-son-low", 5);
        accept("execution-l9-x", 9);
        refuse("execution-l9-x", 4, "lane");
    }

    #[test]
    fn lane_zero_and_leading_zero_are_refused() {
        refuse("exec-L0-t7-M-son-low", 4, "lane");
        refuse("exec-L01-t7-M-son-low", 4, "lane");
        refuse("execution-l01-x", 4, "lane");
    }

    #[test]
    fn lowercase_lane_letter_is_refused_in_the_launcher_form() {
        refuse("exec-l1-t7-M-son-low", 4, "lane");
    }

    #[test]
    fn non_digit_lane_is_refused() {
        refuse("push-n3-S-son-low", 4, "lane");
        refuse("plan-ahead-p1a10-M-son-med", 4, "lane");
        refuse("exec-L1x-t9-M-son-med", 4, "lane"); // non-digit inside the lane digits
    }

    #[test]
    fn missing_t_prefix_is_refused() {
        refuse("exec-L1-p1a15-M-son-med", 4, "task");
    }

    #[test]
    fn uppercase_task_slug_is_refused() {
        refuse("exec-L1-tP1a15-M-son-med", 4, "task");
    }

    #[test]
    fn hyphen_inside_the_task_slug_is_refused() {
        refuse("exec-L1-t11-5-M-son-med", 4, "task");
    }

    #[test]
    fn empty_task_slug_is_refused() {
        refuse("exec-L1-t-M-son-med", 4, "task");
    }

    #[test]
    fn bad_size_is_refused() {
        refuse("exec-L1-t7-X-son-low", 4, "size");
    }

    #[test]
    fn bad_model_is_refused() {
        refuse("exec-L1-t7-M-ops-low", 4, "model");
        refuse("exec-L1-t7-M-gpt-low", 4, "model");
    }

    #[test]
    fn bad_effort_is_refused() {
        refuse("exec-L1-t7-M-son-high", 4, "effort");
        refuse("exec-L1-t7-M-son-hi", 4, "effort");
    }

    #[test]
    fn trailing_or_missing_parts_are_refused() {
        refuse("exec-L1-t7-M-son-low-extra", 4, "task");
        refuse("exec-L1-t7-M-son-low-", 4, "task");
        refuse("exec-L1-t7-M-son", 4, "effort");
        refuse("exec-L1-t7-M", 4, "model");
        refuse("exec-L1-t7", 4, "size");
        refuse("exec-L1", 4, "task");
        // a lone word with nothing after its `-` never counts as a word match
        refuse("exec-", 4, "role");
    }

    // ---- the coordinator name and the long family ----

    #[test]
    fn orchestrator_exact_only() {
        let v = accept("Orchestrator", 4);
        assert_eq!(v.line(), "ok kind=orchestrator lane=- ticket=-");
        refuse("orchestrator", 4, "role");
        refuse("Orchestrator-2", 4, "role");
    }

    #[test]
    fn long_lane_form_read_back() {
        assert_eq!(accept("execution-l1-p1a13", 4).line(), "ok kind=long lane=1 ticket=p1a13");
        // a hyphenated slug is returned whole (the rule file's known gap)
        assert_eq!(accept("planner-l2-pre4", 4).line(), "ok kind=long lane=2 ticket=pre4");
        assert_eq!(accept("governance-l4-pusher3", 4).line(), "ok kind=long lane=4 ticket=pusher3");
        accept("risk-review-l3-q1", 4);
        accept("learning-l1-x", 4);
        accept("knowledge-l2-x", 4);
    }

    #[test]
    fn long_ahead_and_signoff_forms() {
        assert_eq!(accept("planner-ahead-pre5", 4).line(), "ok kind=long lane=- ticket=pre5");
        let v = accept("governance-p1a-signoff", 4);
        assert_eq!(v.line(), "ok kind=long lane=- ticket=-");
        accept("risk-review-p3-signoff", 4);
        accept("risk-review-p12-ab-9-signoff", 4);
    }

    #[test]
    fn long_form_refusals() {
        refuse("planner-L2-x", 4, "lane"); // uppercase L in the long form
        refuse("learn-l2-x", 4, "lane"); // a launcher word takes the launcher family
        refuse("planner-l2-", 4, "task"); // empty slug
        refuse("planner-ahead-X9", 4, "task"); // uppercase slug
        refuse("governance-pXa-signoff", 4, "task"); // the phase must start with a digit
        refuse("governance-p1a-Signoff", 4, "lane"); // suffix mismatch falls through
        refuse("risk-review-p1a-signoff-extra", 4, "lane");
        refuse("governance-", 4, "role"); // nothing after the word
        refuse("governance-p-signoff", 4, "task"); // empty phase after `p`
        refuse("governance-p1A-signoff", 4, "task"); // uppercase inside the phase
        refuse("execution-l-x", 4, "lane"); // empty lane digits
        refuse("execution-l1x-y", 4, "lane"); // non-digit lane characters
        refuse("execution-l1000000000000000-x", 4, "lane"); // lane digits over the 15-digit cap
    }

    // ---- the legacy table ----

    #[test]
    fn legacy_table_is_exact() {
        assert_eq!(accept("builder1-p1a13", 4).line(), "ok kind=legacy ticket=p1a13");
        assert_eq!(accept("lane3-pre6a-builder", 4).line(), "ok kind=legacy ticket=pre6a");
        assert_eq!(accept("lane2-pre4-planner", 4).line(), "ok kind=legacy ticket=pre4");
        assert_eq!(accept("orchestration-l2-p1a06", 4).line(), "ok kind=legacy ticket=p1a06");
        assert_eq!(accept("D16", 4).line(), "ok kind=legacy ticket=-");
    }

    #[test]
    fn legacy_twins_are_refused() {
        refuse("builder1-p1a14", 4, "role");
        refuse("orchestration-l2-p1a07", 4, "role");
        refuse("d16", 4, "role");
        refuse("lane3-pre6a-planner", 4, "role");
        refuse("D16-extra", 4, "role");
        refuse("builder2-p1a20", 4, "role");
    }

    // ---- routing and everything else ----

    #[test]
    fn unknown_role_words_are_refused() {
        refuse("rvw-L2-t115-M-opu-hig", 4, "role"); // rule-file-only short word
        refuse("random-session", 4, "role");
        refuse("executor-L4-t106-M-ops-med", 4, "role"); // pre-117 launcher shape
        refuse("Exec-L1-t7-M-son-low", 4, "role"); // uppercase role word
        refuse(" exec-L1-t7-M-son-low", 4, "role"); // leading space
    }

    #[test]
    fn empty_name_is_refused() {
        let r = check("", 4, false);
        assert!(r.is_err(), "the empty name must be refused");
        assert!(r.unwrap_err().contains("empty"));
    }

    #[test]
    fn whitespace_around_a_name_is_refused() {
        refuse("exec-L1-t7-M-son-low ", 4, "effort"); // trailing space
        refuse("exec-L1-t7-M-son-low\n", 4, "effort"); // trailing newline
    }

    #[test]
    fn rule_file_only_short_shapes_are_refused() {
        refuse("gov-p1a-signoff-S-opu-hig", 4, "lane");
        refuse("push-n3-S-son-med", 4, "lane");
        refuse("plan-ahead-p1a10-M-son-med", 4, "lane");
    }

    #[test]
    fn new_mode_accepts_the_launcher_form_only() {
        accept("exec-L1-t113-M-son-med", 4);
        let e = check("exec-L1-t113-M-son-med", 4, true);
        assert!(e.is_ok(), "{e:?}");
        for name in ["execution-l1-p1a13", "builder1-p1a13", "Orchestrator", "planner-ahead-pre5", "governance-p1a-signoff"] {
            match check(name, 4, true) {
                Ok(v) => panic!("--new must refuse: {name}: {v:?}"),
                Err(e) => assert!(e.contains("--new"), "{name}: {e}"),
            }
        }
    }
}
