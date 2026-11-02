//! The G8.4 operator-question classifier (std only): one pure decision function that answers "what
//! size and risk is this operator question, and does the T3 advisor see it?" —
//! `docs/question-class.md` is the contract in plain words. No model brand lives here or in any of
//! its inputs or outputs: the advisor is "the T3 advisor", never named. The function never
//! dispatches anything itself and never touches the filesystem: the caller passes the record on
//! (wiring it into the orchestration session is later work).
//!
//! Inputs are TEXT: five optional facts, each exactly spelled (case-sensitive, no trim). The
//! classifier never reads question text, a ticket file or a path map — the caller derives the facts.
//! It never refuses: every question is still logged once. A missing or unknown fact is `fail_closed`
//! — size and risk `unclassified`, no advisor — and the bad value never appears in the event.

use crate::events::{Field, Kind, Record};

/// The two request kinds: a status request or a question.
pub const REQUESTS: [&str; 2] = ["status", "question"];
/// The named ticket's size, or `none` when no ticket is named.
pub const TICKET_SIZES: [&str; 4] = ["S", "M", "L", "none"];
/// The two yes/no facts (a plan change, a section 7 decision).
pub const YES_NO: [&str; 2] = ["yes", "no"];
/// The risk signals: a P5 risk-path name, a release, privacy or egress, a destructive action.
pub const RISK_SIGNALS: [&str; 6] = ["none", "risk-path", "release", "privacy", "egress", "destructive"];

/// The trigger token of the one advisor consult the rule can produce (a plain token, lowercase).
pub const TRIGGER_OPERATOR_QUESTION: &str = "operator-question";

/// The five text facts, plus the timestamp of the one event record. Each fact is `None` when
/// missing; validation is exact (case-sensitive, no trim) and happens before any branch is taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionInput<'a> {
    /// Caller-supplied milliseconds since the epoch (the same convention as `Record`).
    pub ts_ms: u64,
    /// `status` or `question`.
    pub request: Option<&'a str>,
    /// `S`, `M`, `L` or `none` (names no ticket).
    pub ticket_size: Option<&'a str>,
    /// `yes` or `no`.
    pub plan_change: Option<&'a str>,
    /// `yes` or `no`.
    pub section7: Option<&'a str>,
    /// one of `RISK_SIGNALS`.
    pub risk_signal: Option<&'a str>,
}

/// The advisor answer: `none`, or a consult with one trigger token. The advisor is the T3 advisor
/// and is never named by brand here or in the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Advisor {
    None,
    Consult { trigger: &'static str },
}

/// The classification: the one `question_class` event record plus the advisor answer. Always
/// produced — the classifier never refuses (every question is logged once, whatever its facts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub record: Record,
    pub advisor: Advisor,
}

/// Build the one event record from fixed tokens the kind table declares (this cannot fail: every
/// value below is a literal from the declared `OneOf` sets, never an input value).
fn record(ts_ms: u64, size: &str, risk: &str, rule_hit: &str) -> Record {
    Record::build(
        Kind::QuestionClass,
        ts_ms,
        &[("size", Field::Str(size.to_string())), ("risk", Field::Str(risk.to_string())), ("rule_hit", Field::Str(rule_hit.to_string()))],
    )
    .expect("the classifier only emits tokens the kind table declares")
}

/// The G8.4 decision, pure over its input:
///
/// 1. `request = status` -> `rule_hit=status`, size and risk `unclassified`, no advisor (the other
///    facts are ignored, however spelled);
/// 2. any fact missing or outside its set -> `rule_hit=fail_closed`, size and risk `unclassified`,
///    no advisor, and the bad value never appears in the event;
/// 3. size: `plan_change=yes` or `section7=yes` -> `L`; else `ticket_size=none` -> `S`; else the
///    named ticket's size;
/// 4. risk: `section7=yes` or any named risk signal -> `high`; else `low`;
/// 5. the advisor rule: consult (trigger `operator-question`) only when `size=L` AND `risk=high`;
///    every other combination is `t2_only`.
pub fn classify(input: &QuestionInput) -> Outcome {
    // 1. A status request never reaches the advisor, whatever its other facts say or misspell.
    if input.request == Some("status") {
        return Outcome { record: record(input.ts_ms, "unclassified", "unclassified", "status"), advisor: Advisor::None };
    }
    // 2. Fail closed on the envelope first: every fact present and exactly spelled, or no branch
    //    is taken at all (no bad value ever reaches the record).
    let ok = |v: Option<&str>, set: &[&str]| v.is_some_and(|v| set.contains(&v));
    if !ok(input.request, &REQUESTS)
        || !ok(input.ticket_size, &TICKET_SIZES)
        || !ok(input.plan_change, &YES_NO)
        || !ok(input.section7, &YES_NO)
        || !ok(input.risk_signal, &RISK_SIGNALS)
    {
        return Outcome { record: record(input.ts_ms, "unclassified", "unclassified", "fail_closed"), advisor: Advisor::None };
    }
    // 3. Size.
    let size = if input.plan_change == Some("yes") || input.section7 == Some("yes") {
        "L"
    } else if input.ticket_size == Some("none") {
        "S"
    } else {
        input.ticket_size.unwrap()
    };
    // 4. Risk.
    let risk = if input.section7 == Some("yes") || input.risk_signal != Some("none") { "high" } else { "low" };
    // 5. The advisor rule: L AND high, nothing less.
    let (rule_hit, advisor) = if size == "L" && risk == "high" {
        ("advisor", Advisor::Consult { trigger: TRIGGER_OPERATOR_QUESTION })
    } else {
        ("t2_only", Advisor::None)
    };
    Outcome { record: record(input.ts_ms, size, risk, rule_hit), advisor }
}
