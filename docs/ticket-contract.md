# Ticket contract (v1)

A ticket is one JSON file with a fixed shape. This page is the contract in plain words. `schemas/ticket.schema.json` is the machine mirror of the same field set.

Status: contract only. No reader exists yet; the id type, the strict reader and their tests come in later tickets, and each rule below gains the name of the test that enforces it when that reader lands.

## The eleven fields

Every field is required. There are no optional fields and no defaults.

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

No other key is allowed, and no key may appear twice.

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

A reader of this contract refuses, and runs nothing, on: a missing field, an unknown or duplicate field, a wrongly typed value, a value outside a closed list, a malformed repo id, trailing text after the object, input that is not UTF-8, and a file over 1 MiB. A refusal names the field.

## Future fields

The build spec v1.1 lists more ticket fields than the eleven above. They are not v1 fields. They are recorded here so none is silently lost; adding any of them is a later schema version, not an edit to v1.

- novelty
- related main-plan objective
- related milestone
- related Wiki pages
- related Skills and Hooks
- model tier
- status
- verification state

A protected-acceptance-criteria digest is also planned as a later addition (PRE-4d).
