# The fix-round tier rule (G8.3)

Status: contract plus its pure decision function (`core/rust/harness/src/fix_tier.rs`, ticket 190).
Nothing dispatches anything yet: this page and that function are the rule the dispatcher will be
wired to (P4a, later work). `factory.design_tier` (config v4, `docs/config-contract.md`) is the
companion setting.

## The rule in one paragraph

When a fix round starts, the harness looks for a lesson that matches the problem. A **validated**
lesson carries a `minimum_proven_tier`: the cheapest tier that has actually been measured to fix
this class of problem reliably. The fix round runs at that tier. Running **above** it is allowed
only with a written escalation reason, and writes exactly one `escalation` event. Running **below**
it is refused. Everything else — no match, an `unvalidated` or `retired` lesson, or a lesson whose
tier was never measured (`unproven` / `unmeasured`) — runs at `default_main` (the G8 registry's
default, an input here: G8 is another ticket). Escalations above `default_main` are G8.2's job and
are neither emitted nor suppressed by this rule.

## Inputs

All as text, all validated before any branch is taken (fail closed on the envelope first):

- the matched lesson, or none: its `id`, its `status` (`unvalidated` / `validated` / `retired`) and
  its `minimum_proven_tier` (a tier, or `unproven` / `unmeasured`);
- `default_main`, a tier;
- an optional requested tier;
- an optional escalation reason;
- the run identity the escalation event carries (`run_id`, `ticket`, `repo`, `role`, `model`,
  `ts_ms`) — never guessed, never defaulted.

A tier is exactly one of `T1`, `T2`, `T3`, `T4` (T1 the cheapest, T4 the most capable). No model
brand appears in any slot of this rule, ever: a tier slot holding a brand name is refused, not
interpreted.

## Output

Either a dispatch — a tier, the matched lesson's id (carried in the dispatch input whenever a lesson
matched, even when its tier is not honoured), and the one escalation event when one was required —
or a refusal naming its reason class and never echoing an input value.

## The cases

| # | condition | outcome |
|---|-----------|---------|
| 1 | validated lesson, proven tier P, no request | dispatch at P, no escalation |
| 2 | validated lesson, P, request above P, with a reason | dispatch at the request, plus exactly one `escalation` event whose reason token is `above-proven-tier` |
| 3 | validated lesson, P, request above P, no reason | **refused** (G8.3.1 "FAILS": no dispatch) |
| 4 | validated lesson, P, request below P | **refused** (downgrades come from the factory rewriting the lesson, P8 step 9) |
| 5 | request at P | dispatch at P, no escalation |
| 6 | no match, `unvalidated`, `retired`, `unproven`, `unmeasured` | dispatch at `default_main`, lesson id still carried when a lesson matched |
| 7 | any tier text outside the contract, a status outside the three W3 values, a malformed lesson id or identity token | **refused**, naming the slot and the class, never the value |

The escalation event is the base `escalation` kind: it passes `Event::validate` and round-trips
through the event decoder. Its reason is the fixed token `above-proven-tier` (the caller's written
reason is the event's cause, not its reason field: an event reason must be a plain lowercase token,
and lesson ids such as `ISSUE-004` are uppercase).

## `factory.design_tier`

The tier the factory itself designs new fixes at (factory step 5). One of `T1`, `T2`, `T3` — never
`T4`, never a brand — required, ships as `T3` in the public example. See
`docs/config-contract.md` section 3.

## Honest limits

- No lesson is `validated` today and today's lesson notes carry no `status` field, so the validated
  branch is exercised by planted fixtures only; live coverage arrives with the W3 lesson reader and
  the known-issue lookup (X-81b).
- Below-proven refusal (case 4) is a planner choice recorded here: the spec is silent on a request
  below the proven tier, and "proven means the cheapest tier the evidence supports" reads as a floor.
- Nothing in the harness calls this function yet; when the dispatcher is wired, this page is the
  contract it is wired to.
