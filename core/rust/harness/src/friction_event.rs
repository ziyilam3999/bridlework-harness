//! The T-9.3 event-to-row mapper (ticket 193): the pure translator from event-log lines to friction-inbox
//! rows. A fixed, closed list of deterministic event kinds — `replan_required` (the G3a cap signal),
//! `launch_refused` (a dispatcher refusal) and `gate_fail` (a gate check's FAIL) — writes ONE inbox row
//! per (event kind, signature) per ISO week through the friction verbs; every other kind maps to nothing
//! and no prose is ever read or written. The
//! signature names the CAUSE of the event, not its incident: a repeat of the same cause in the same week
//! files no second row, and next week starts a fresh row.
//!
//! The mapper only translates. It never opens the inbox, never scans (the friction verbs scan every field
//! of every row they store or lint, so a privacy hit refuses the row, fail closed) and never writes: the
//! weekly sweep that feeds it the log and files its rows through `harness friction add` is a later ticket.
//! Because the dedup key is the row `id`, the inbox's own duplicate-id rule is a second backstop against
//! double-filing. An event row is never a ticket: every event row carries the fixed ticket `t-9.3` (the
//! catcher is the capturer), never the event's own ticket — the event's ticket lives in the key and the
//! text, so an event row never changes what a real ticket's `lesson:` close-check must name.

use crate::events::{Field, Kind, Record};
use crate::friction::Row;

/// The `ticket` of every event row: the T-9.3 catcher itself, never the event's own ticket.
pub const EVENT_ROW_TICKET: &str = "t-9.3";
/// The `where-it-lives` tag of every event row (K-1.1: before P3, an operational lesson's home is memory).
pub const EVENT_ROW_TAG: &str = "memory";
/// The only legal non-default `source` of a row: filed from an event, not hand-written.
pub const SOURCE_EVENT: &str = "event";
/// The newest year an event may name: past it the row's `date` could not stay `YYYY-MM-DD` (four digits),
/// so the event is refused, never truncated or guessed.
const YEAR_MAX: i64 = 9999;
/// Milliseconds in one day; the only unit the civil-calendar arithmetic below needs.
const MS_PER_DAY: u64 = 86_400_000;

// ---------------------------------------------------------------- the closed list (the one mapping table)

/// The signature of a mapped event: the named values that identify its CAUSE. A repeat of the same values
/// in the same week files no second row. `launch_refused` deliberately leaves `brief_sha256` out (it differs
/// per brief while the cause stays the same), and `gate_fail` leaves `head_sha` out the same way (it changes
/// with every fix push while the cause stays the same); `replan_required` names the cap check's whole condition.
fn signature(rec: &Record) -> Result<Option<Vec<(&'static str, String)>>, String> {
    // The match is exhaustive over `Kind` on purpose (no `_` arm): a new event kind does not compile until
    // someone decides map / no-map and writes the decision here.
    Ok(match rec.kind {
        Kind::ReplanRequired => Some(vec![
            ("repo", field_str(rec, "repo")?),
            ("ticket", field_str(rec, "ticket")?),
            ("reason", field_str(rec, "reason")?),
        ]),
        Kind::LaunchRefused => Some(vec![
            ("launch_role", field_str(rec, "launch_role")?),
            ("matched_entry", field_str(rec, "matched_entry")?),
        ]),
        Kind::GateFail => Some(vec![
            ("repo", field_str(rec, "repo")?),
            ("ticket", field_str(rec, "ticket")?),
            ("gate_id", field_str(rec, "gate_id")?),
            ("failure_class", field_str(rec, "failure_class")?),
        ]),
        // The twelve kinds off the closed list (spec T-9.3): zero rows, and no error either.
        Kind::Escalation
        | Kind::BudgetExceeded
        | Kind::RunIssued
        | Kind::SessionStarted
        | Kind::UnmanagedSession
        | Kind::TimedOut
        | Kind::CheckIn
        | Kind::StepLimitStop
        | Kind::CleanupResult
        | Kind::Quarantined
        | Kind::HumanOriginRefused
        | Kind::HumanOriginAccepted
        | Kind::QuestionClass => None,
    })
}

/// One string field of a record, fail closed: a key the record does not carry (a hand-built record, not one
/// the reader or `Record::build` produced) is an error, never a default.
fn field_str(rec: &Record, key: &str) -> Result<String, String> {
    match rec.field(key) {
        Some(Field::Str(s)) => Ok(s.clone()),
        _ => Err(format!("a {} event is missing its {key} (fail closed)", rec.kind.as_str())),
    }
}

// ---------------------------------------------------------------- the weekly dedup key

/// The ISO 8601 week of a day-from-the-epoch: (iso year, week 1-53). A week runs Monday to Sunday, and the
/// year of a week is the year of its Thursday — so 2026-12-31 and 2027-01-03 are both week 53 of 2026, and
/// 2024-12-30 is already week 1 of 2025.
fn iso_week(days: i64) -> (i64, u32) {
    let (y, m, d) = civil_from_days(days);
    let ordinal = day_of_year(y, m, d);
    // 1970-01-01 was a Thursday, so Monday=0 is (days + 3) mod 7.
    let iso_dow = (days + 3).rem_euclid(7) + 1; // 1 = Monday .. 7 = Sunday
    let mut week = (ordinal as i64 - iso_dow + 10) / 7;
    let mut year = y;
    if week < 1 {
        // The day belongs to the last week of the previous year.
        year = y - 1;
        week = if iso_long_year(year) { 53 } else { 52 };
    } else if week > 52 && !(week == 53 && iso_long_year(y)) {
        // Week 53 exists only in a long year; otherwise the day is already week 1 of the next year.
        year = y + 1;
        week = 1;
    }
    (year, week as u32)
}

/// A year whose ISO year has 53 weeks: the standard closed-form test (p(y) = 4 or p(y-1) = 3).
fn iso_long_year(y: i64) -> bool {
    let p = |y: i64| (y + y / 4 - y / 100 + y / 400).rem_euclid(7);
    p(y) == 4 || p(y - 1) == 3
}

/// The day-of-year (1-based) of a civil date.
fn day_of_year(y: i64, m: u32, d: u32) -> u32 {
    let leap = y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    let cum = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let mut n = cum[(m - 1) as usize] + d;
    if leap && m > 2 {
        n += 1;
    }
    n
}

/// The civil date of a day-from-the-epoch (the standard civil-calendar arithmetic; `days_from_civil`'s
/// inverse, the same algorithm `friction::today_utc` uses).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Days since 0000-03-01, the calendar the arithmetic below is written against.
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

/// The one fixed impact table (ticket 3601): the rating an event row carries. The match is exhaustive over the
/// three mapped kinds' names; no prose is read.
fn event_impact(kind: Kind) -> &'static str {
    match kind {
        Kind::GateFail => "high",
        _ => "medium",
    }
}

// ---------------------------------------------------------------- the mapper

/// Map one event to its inbox row, or to no row: `Ok(None)` for a kind off the closed list, `Ok(Some(row))`
/// for a mapped kind, `Err` for an event that cannot name a row (a hand-built record missing a signature
/// field, a `ts_ms` whose date does not fit `YYYY-MM-DD`). Pure: reads the record, touches no file.
pub fn map_record(rec: &Record) -> Result<Option<Row>, String> {
    let Some(sig) = signature(rec)? else {
        return Ok(None);
    };
    let days = (rec.ts_ms / MS_PER_DAY) as i64;
    let (year, month, day) = civil_from_days(days);
    let (iso_year, week) = iso_week(days);
    if year > YEAR_MAX || iso_year > YEAR_MAX {
        return Err(format!("ts_ms is past year {YEAR_MAX}: a row date is YYYY-MM-DD, four digits, never truncated"));
    }
    // The dedup key IS the row id: `evt.<kind>.` + each signature value as `<byte-length>_<value>` joined by
    // `.` + `.<iso-year>-w<ww>`. The length prefix makes the join injective (tokens may contain `.`, so a
    // bare `.`-join would collide `a.b`,`c` with `a`,`b.c`), and the week part makes every week a fresh row.
    let id = format!(
        "evt.{}.{}.{}-w{:02}",
        rec.kind.as_str(),
        sig.iter().map(|(_, v)| format!("{}_{}", v.len(), v)).collect::<Vec<_>>().join("."),
        iso_year,
        week
    );
    // The text is a fixed template built only from the kind name and the signature tokens: no prose is read
    // from anywhere, and nothing but these tokens lands in the inbox.
    let text = format!(
        "event {} {}",
        rec.kind.as_str(),
        sig.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ")
    );
    let row = Row {
        id,
        date: format!("{year:04}-{month:02}-{day:02}"),
        ticket: EVENT_ROW_TICKET.to_string(),
        tag: EVENT_ROW_TAG.to_string(),
        breaks: String::new(),
        text,
        kind: String::new(),
        repeats: String::new(),
        source: SOURCE_EVENT.to_string(),
        at: format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", (rec.ts_ms % MS_PER_DAY) / 3_600_000, (rec.ts_ms % 3_600_000) / 60_000, (rec.ts_ms % 60_000) / 1000),
        impact: event_impact(rec.kind).to_string(),
        why: String::new(),
        proof: String::new(),
    };
    // The mapper's rows are ordinary rows: a template that somehow broke the row contract is refused here,
    // fail closed, before any verb ever sees it.
    crate::friction::validate_row(&row)?;
    Ok(Some(row))
}

/// Map one event-log line (WITHOUT its newline, the same shape `Record::decode` reads) to its inbox row, or
/// to no row. Strict: the line is decoded by the event reader's own rules first, so a torn, invalid, unknown
/// or wrong-version line is an error, never a guess.
pub fn map_line(line: &str) -> Result<Option<Row>, String> {
    let rec = Record::decode(line).map_err(|e| format!("the event line is refused: {e}"))?;
    map_record(&rec)
}

/// The batch helper the weekly sweep will call: map many lines, keep ONE row per key (the earliest `ts_ms`
/// of the week; a tie keeps the first-seen, so input order among equals never matters), and return the rows
/// sorted by id. Fail closed on the whole batch: one bad line is an error with NO rows returned, so a
/// half-mapped week can never be filed. Linear in the input: one decode and at most one linear scan of the
/// kept rows per line, and each kept row's line was already capped by the event reader's own `LINE_CAP`.
pub fn map_lines(lines: &[&str]) -> Result<Vec<Row>, String> {
    let mut kept: Vec<(u64, Row)> = Vec::new();
    for line in lines {
        let rec = Record::decode(line).map_err(|e| format!("the event line is refused: {e}"))?;
        let Some(row) = map_record(&rec)? else {
            continue;
        };
        match kept.iter_mut().find(|(_, r)| r.id == row.id) {
            Some((ts, r)) => {
                if rec.ts_ms < *ts {
                    *ts = rec.ts_ms;
                    *r = row;
                }
            }
            None => kept.push((rec.ts_ms, row)),
        }
    }
    let mut rows: Vec<Row> = kept.into_iter().map(|(_, r)| r).collect();
    rows.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(rows)
}
