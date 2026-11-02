# The operator-question classifier (G8.4)

Status: contract plus its pure decision function (`core/rust/harness/src/question_class.rs`,
ticket 191). Nothing calls it yet: this page and that function are the rule the orchestration
session will be wired to (the caller that derives the five facts from a real question is later
work). The advisor is the **T3 advisor** and is never named by brand — not in this rule, not in
its inputs, not in its event.

## The rule in one paragraph

When the operator asks the main helper something, most questions are small and safe and the main
helper just answers (T2 only). A question is worth the advisor only when it is BIG (size `L`) AND
risky (`high`) — the one combination that consults the T3 advisor, with the trigger token
`operator-question`. Every question, whatever its facts, is logged once as a `question_class`
event; a status request ("how is it going?") is logged as `rule_hit=status` and never reaches the
advisor; if any fact is missing or spelled outside the contract the classifier plays safe:
`rule_hit=fail_closed`, size and risk `unclassified`, no advisor, and the bad value never appears
in the event.

## Inputs

Five text facts, each optional, each exactly spelled (case-sensitive, no trim), plus the event's
timestamp. The classifier never reads question text, a ticket file or a path map — the caller
supplies the facts:

| Fact | Values |
|---|---|
| `request` | `status` or `question` |
| `ticket_size` | the named ticket's `metadata.size` — `S`, `M`, `L` — or `none` (names no ticket) |
| `plan_change` | `yes` or `no` |
| `section7` | `yes` or `no` (a section 7 decision) |
| `risk_signal` | `none`, `risk-path` (a P5 risk-path name), `release`, `privacy`, `egress`, `destructive` |

Note the vocabulary gap on purpose: a ticket's own `risk` field says `normal`/`high`
(`core/rust/harness/src/ticket.rs`), but the classifier's `risk_signal` is the closed list above —
`normal` planted in the signal slot is a `fail_closed` row, never silently accepted.

## Output

Always exactly one `question_class` event record (the classifier never refuses; every question is
logged once) plus the advisor answer: `none`, or a consult with the trigger `operator-question`.

## The cases

| # | condition | outcome |
|---|-----------|---------|
| 1 | `request = status` | `rule_hit=status`, size and risk `unclassified`, no advisor; the other facts are ignored, however spelled |
| 2 | any fact missing or outside its set (an unknown `request` included) | `rule_hit=fail_closed`, size and risk `unclassified`, no advisor; the bad value never appears in the event |
| 3 | `plan_change=yes` or `section7=yes` | size `L` (whatever the ticket says) |
| 4 | no ticket named (`ticket_size=none`) | size `S` |
| 5 | else | size is the ticket's own size |
| 6 | `section7=yes` or any named risk signal | risk `high`; else `low` |
| 7 | `size=L` AND `risk=high` | `rule_hit=advisor`, advisor consult, trigger `operator-question` |
| 8 | every other size/risk combination | `rule_hit=t2_only`, no advisor |

## The event

The payload kind `question_class` (the shared payload envelope `v, kind, ts_ms`; see
`docs/event-log.md`): `size` one of `S`, `M`, `L`, `unclassified`; `risk` one of `low`, `high`,
`unclassified`; `rule_hit` one of `status`, `advisor`, `t2_only`, `fail_closed`. A reader counting
L+high questions must filter on `rule_hit`, never on size alone (status and fail-closed rows log
`unclassified`, never a guessed size). No model brand appears anywhere in the rule, its inputs or
its event.

## Later work (out of scope here)

- the advisor event kind (its `trigger` field and the five D16 triggers) and the P4a adapter that
  writes it and calls the T3 advisor only on `rule_hit=advisor`;
- the caller that derives the five facts from a real question, the named ticket's `metadata.size`
  and the P5 risk path map;
- moving the risk-signal list into a Risk-Review-owned config key (the rubric is fixed in code
  here: a fixed grammar, nothing to tune that could loosen the rule).
