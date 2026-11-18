# Ticket contract (v2)

A ticket is one JSON file with a fixed shape. This page is the contract in plain words. `schemas/ticket.schema.json` is the machine mirror of the same field set. The reader is `core/rust/harness/src/ticket.rs`.

## Changes from v1

v2 adds two optional plan link keys, `main_plan_ref` and `off_plan`. A v1 ticket (the 11 keys) reads unchanged.

## The eleven required fields

The ticket has 11 required keys, two optional plan link keys and the optional `removed_tests` (below). There are no defaults.

| Field | Shape |
|---|---|
| `id` | non-empty string, the ticket's own id |
| `repo_id` | which repository the ticket is for; see below |
| `title` | non-empty string |
| `group` | non-empty string |
| `size` | exactly `S`, `M` or `L` |
| `objective` | non-empty string |
| `acceptance_criteria` | non-empty array of strings |
| `dependencies` | array of strings, may be empty |
| `risk` | exactly `normal` or `high` |
| `owner_role` | exactly one of the six role ids below |
| `evidence_links` | array of strings, may be empty |

No key outside these 11, the two plan link keys and `removed_tests` below is allowed, and no key may appear twice.

## The plan link (optional)

A ticket may carry one of two optional keys, not both:

| Field | Shape |
|---|---|
| `main_plan_ref` | non-empty array of strings, each 1 to 64 bytes of ASCII letters, digits, `.`, `_` or `-`, none repeated: the plan index rows the ticket builds |
| `off_plan` | string of 1 to 300 bytes, not blank, with no control character (U+0000 to U+001F, U+007F, also when written as an escape): one line saying why the ticket is off plan |

The reader refuses a bad link shape with an error that names the field, so each caller that parses a ticket refuses it. A ticket with neither key parses as before.

At this version only `harness ticket intake --plan-index <file>` checks the link against an index (see `docs/plan-index.md`): it requires one of the two keys and requires each `main_plan_ref` element to be an id of the index. The other callers check its shape (through the ticket parser), not the index.

## Removed tests (optional)

A ticket may carry the optional key `removed_tests`: an array of 1 to 200 strings, each `<test id> -- <reason>`, split at the first space-dash-dash-space (a reason may hold a second one).

- The test id is 1 to 300 bytes with no blank and no control character; a test id is given once.
- The reason is 1 to 300 bytes, not blank, with no control character.
- Control characters are U+0000 to U+001F and U+007F, also when written as an escape.

The reader refuses an empty array, a non-string element, null, a string in place of the array, an element with no separator, a bad test id or reason, more than 200 elements, and a repeated test id, naming `removed_tests`. The key given twice is refused like the other keys. A ticket without it reads as before.

`harness ticket gate` hands the test ids to each head-only gate program as `HARNESS_GATE_REMOVED_TESTS` (joined by a newline, in ticket order; set and empty when the ticket has no key), beside `HARNESS_GATE_BASE` (the `--base` sha) and `HARNESS_GATE_HEAD` (the head judged). A value of these names in the caller environment does not reach the program. For a both-ends program the engine sets none of the three; a value inherited from the caller is not removed.

An engine built before this key existed refuses a ticket that holds it (unknown key).

## The repo id

`repo_id` names exactly one repository. It is a plain name: lowercase letters, digits, `-`, `_` and `.`, never empty, never a path, never `.` or `..`. A ticket carries one repo id, not a list.

The id says which repository; it does not say where that repository is on disk. That mapping is not part of the ticket and is decided elsewhere (the config work, PRE-5). A reader must take the repo from the ticket and never from the folder it is run in.

## Closed lists

`size`: `S`, `M`, `L`.

`risk`: `normal`, `high`. The build spec's prose also mentions low-risk and "high/critical" without a field value list; `low` and `critical` are not v1 values and need a later schema version.

`owner_role`: the six logical owners of the build spec v1.1 (section "Logical owners", L32-58):

- `orchestration_portfolio` (Orchestration and Portfolio)
- `execution_verification` (Execution and Verification)
- `risk_review` (Risk and Independent Review)
- `friction_learning` (Friction and Harness Learning)
- `knowledge_lessons` (Knowledge and Lessons)
- `harness_governance` (Harness Governance)

The plan for this work assumed five roles; the spec lists six, so all six are allowed.

## Refusal

A reader of this contract refuses, and runs nothing, on: a missing field, an unknown or duplicate field, both plan link keys, a bad plan link value, a bad `removed_tests` value, a wrongly typed value, a value outside a closed list, a malformed repo id, trailing text after the object, input that is not UTF-8, and a file over 1 MiB. A refusal names the field.

## Future fields

The build spec v1.1 lists more ticket fields than the eleven above. They are not fields yet. They are recorded here so none is silently lost; adding any of them is a later schema version.

The item related main-plan objective is now met by `main_plan_ref`.

- novelty
- related milestone
- related Wiki pages
- related Skills and Hooks
- model tier
- status
- verification state

A protected-acceptance-criteria digest is also planned as a later addition (PRE-4d).
