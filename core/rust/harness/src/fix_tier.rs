//! The G8.3 fix-round tier rule (std only): one pure decision function that answers "which tier does
//! this fix round run at, and is an escalation event required?" — `docs/fix-tier.md` is the contract
//! in plain words. No model brand lives here or in any of its inputs: tiers are T1..T4 and nothing
//! else. The function never dispatches anything itself and never touches the filesystem: the caller
//! passes the result on (wiring it into the P4a dispatcher is later work).
//!
//! Inputs are TEXT (a matched lesson or none, `default_main`, an optional requested tier, an
//! optional escalation reason, plus the run identity the escalation event needs). Any text outside
//! the contract is a refusal that names its reason class and never echoes an input value.

use crate::events::token_ok;
use crate::events::{Event, Kind};

/// The tier ladder: T1 is the cheapest, T4 the most capable. Derived ordering (T1 < T2 < T3 < T4) is
/// the whole notion of "above"/"below" in this module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    T1,
    T2,
    T3,
    T4,
}

/// The exact spellings the contract accepts; anything else (a brand, a lowercase tier, `T0`) is refused.
pub const TIERS: [&str; 4] = ["T1", "T2", "T3", "T4"];

/// The two `minimum_proven_tier` texts that mean "no tier was ever measured" (W3 lesson shape): the
/// lesson exists but its tier claim is refused — the dispatch falls back to `default_main`.
pub const PROVEN_ABSENT: [&str; 2] = ["unproven", "unmeasured"];

/// The three W3 lesson statuses. Anything else is a refusal (fail closed).
pub const STATUSES: [&str; 3] = ["unvalidated", "validated", "retired"];

/// The reason token of the one `escalation` event a dispatch above the proven tier writes. It names
/// the cause and is a plain token, so the event passes `Event::validate` (an uppercase lesson id
/// would not: the caller's reason text is never the event's reason).
pub const ESCALATION_REASON: &str = "above-proven-tier";

impl Tier {
    /// Parse one exact tier spelling; `None` for anything else (case-sensitive, no trim).
    pub fn parse(s: &str) -> Option<Tier> {
        match s {
            "T1" => Some(Tier::T1),
            "T2" => Some(Tier::T2),
            "T3" => Some(Tier::T3),
            "T4" => Some(Tier::T4),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Tier::T1 => "T1",
            Tier::T2 => "T2",
            Tier::T3 => "T3",
            Tier::T4 => "T4",
        }
    }
}

/// A matched W3 lesson: its id, its status and its `minimum_proven_tier`, all as text, all validated
/// by `decide` (never trusted on faith).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lesson<'a> {
    pub id: &'a str,
    pub status: &'a str,
    pub minimum_proven_tier: &'a str,
}

/// The run identity the escalation event carries (the v2 event envelope). Never guessed, never
/// defaulted: `decide` refuses a missing or malformed identity rather than write an event without one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunIdentity<'a> {
    /// Caller-supplied milliseconds since the epoch (the same convention as `Event`).
    pub ts_ms: u64,
    pub run_id: &'a str,
    pub ticket: &'a str,
    pub repo: &'a str,
    pub role: &'a str,
    pub model: &'a str,
}

/// Everything one decision needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchInput<'a> {
    /// The matched lesson, or `None` when nothing matched.
    pub lesson: Option<Lesson<'a>>,
    /// The G8 registry's default tier (an input here: G8 is another ticket's build).
    pub default_main: &'a str,
    /// An optional requested tier, as text (a brand-shaped request is refused, never guessed).
    pub requested: Option<&'a str>,
    /// The escalation reason for a request above the proven tier; `None` or blank means absent.
    pub escalation_reason: Option<&'a str>,
    /// The identity of the escalation event this decision may produce.
    pub identity: RunIdentity<'a>,
}

/// Why a decision refused. Each message is a fixed string that names the class and the slot, never an
/// input value (a value may be a brand or a private folder name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// A tier slot held text outside T1..T4 (or, for the lesson slot, the two proven-absent words).
    BadTierText { slot: &'static str },
    /// The lesson status is not one of the three W3 values.
    BadStatus,
    /// The lesson id is not a short plain id.
    BadLessonId,
    /// One identity field of the escalation event is not a plain token.
    BadIdentity { field: &'static str },
    /// A request above the proven tier with no escalation reason.
    AboveProvenWithoutReason,
    /// A request below the proven tier (downgrades come from the factory rewriting the lesson).
    BelowProven,
}

impl Refusal {
    /// The refusal text: names the reason class, never a value.
    pub fn message(&self) -> String {
        match self {
            Refusal::BadTierText { slot } => match *slot {
                "lesson minimum_proven_tier" => {
                    format!("refused: {slot} must be one of T1, T2, T3, T4, unproven, unmeasured")
                }
                _ => format!("refused: the {slot} tier must be one of T1, T2, T3, T4"),
            },
            Refusal::BadStatus => "refused: the lesson status must be one of unvalidated, validated, retired".to_string(),
            Refusal::BadLessonId => {
                "refused: the lesson id must be a short plain id (1..=64 ascii characters, no space or control character)".to_string()
            }
            Refusal::BadIdentity { field } => {
                format!("refused: the escalation event's {field} must be a plain token (lowercase letters, digits, '-', '_', '.')")
            }
            Refusal::AboveProvenWithoutReason => {
                "refused: a request above the proven tier needs an escalation reason".to_string()
            }
            Refusal::BelowProven => "refused: a fix round never runs below the lesson's proven tier".to_string(),
        }
    }
}

/// The answer: a dispatch (at a tier, carrying the matched lesson id, with the one escalation event
/// when one was required) or a refusal. `escalation` is `Some` exactly when the dispatch runs above a
/// validated lesson's proven tier with a reason; above-`default_main` escalation is G8.2's job and is
/// neither emitted nor suppressed here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Dispatch { tier: Tier, lesson_id: Option<String>, escalation: Option<Event> },
    Refused(Refusal),
}

/// A lesson id shape: 1..=64 ascii characters, no space, no control character, not `.` or `..`.
/// W3 ids are uppercase (`ISSUE-004`), so — unlike an event token — upper case is allowed.
fn lesson_id_ok(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.is_ascii()
        && id != "."
        && id != ".."
        && !id.bytes().any(|b| b.is_ascii_control() || b == b' ')
}

/// The G8.3 decision, pure over its input. The input texts are validated before any branch is taken
/// (fail closed on the envelope first): the identity tokens, then `default_main`, then the request,
/// then the lesson (id, status, tier text). Only then:
///
/// - no lesson, an `unvalidated`/`retired` lesson, or a lesson whose tier was never measured
///   (`unproven`/`unmeasured`) -> dispatch at `default_main` (escalation above it is G8.2's, not
///   this function's);
/// - a `validated` lesson -> dispatch at its proven tier P; a request above P needs a reason and
///   writes exactly one `escalation` event; a request below P is refused (downgrades come from the
///   factory rewriting the lesson, P8 step 9); a request at P dispatches at P with no event.
pub fn decide(input: &DispatchInput) -> Decision {
    // The envelope first: an escalation event written with a guessed identity would be worse than
    // refusing here.
    for (field, v) in [
        ("run_id", input.identity.run_id),
        ("ticket", input.identity.ticket),
        ("repo", input.identity.repo),
        ("role", input.identity.role),
        ("model", input.identity.model),
    ] {
        if !token_ok(v) {
            return Decision::Refused(Refusal::BadIdentity { field });
        }
    }
    let default_main = match Tier::parse(input.default_main) {
        Some(t) => t,
        None => return Decision::Refused(Refusal::BadTierText { slot: "default_main" }),
    };
    let requested = match input.requested {
        None => None,
        Some(text) => match Tier::parse(text) {
            Some(t) => Some(t),
            None => return Decision::Refused(Refusal::BadTierText { slot: "requested" }),
        },
    };
    let lesson = match &input.lesson {
        None => {
            return Decision::Dispatch { tier: default_main, lesson_id: None, escalation: None };
        }
        Some(l) => l,
    };
    if !lesson_id_ok(lesson.id) {
        return Decision::Refused(Refusal::BadLessonId);
    }
    if !STATUSES.contains(&lesson.status) {
        return Decision::Refused(Refusal::BadStatus);
    }
    let proven = match Tier::parse(lesson.minimum_proven_tier) {
        Some(t) => Some(t),
        None => {
            if !PROVEN_ABSENT.contains(&lesson.minimum_proven_tier) {
                return Decision::Refused(Refusal::BadTierText { slot: "lesson minimum_proven_tier" });
            }
            None
        }
    };
    let lesson_id = Some(lesson.id.to_string());
    // No measured proven tier, or a lesson that is not validated: the tier claim is refused and the
    // dispatch falls back to default_main. The lesson id is still carried (a lesson did match).
    let proven = match proven {
        None => return Decision::Dispatch { tier: default_main, lesson_id, escalation: None },
        Some(p) => p,
    };
    if lesson.status != "validated" {
        return Decision::Dispatch { tier: default_main, lesson_id, escalation: None };
    }
    match requested {
        None => Decision::Dispatch { tier: proven, lesson_id, escalation: None },
        Some(r) if r > proven => {
            let reason_present = input.escalation_reason.is_some_and(|r| !r.trim().is_empty());
            if !reason_present {
                return Decision::Refused(Refusal::AboveProvenWithoutReason);
            }
            let i = &input.identity;
            let escalation = Event {
                kind: Kind::Escalation,
                ts_ms: i.ts_ms,
                run_id: i.run_id.to_string(),
                ticket: i.ticket.to_string(),
                repo: i.repo.to_string(),
                reason: ESCALATION_REASON.to_string(),
                role: i.role.to_string(),
                model: i.model.to_string(),
            };
            Decision::Dispatch { tier: r, lesson_id, escalation: Some(escalation) }
        }
        Some(r) if r < proven => Decision::Refused(Refusal::BelowProven),
        Some(_) => Decision::Dispatch { tier: proven, lesson_id, escalation: None },
    }
}
