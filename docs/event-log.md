# Event log (v2)

The event log is where a cap says "I stopped this ticket, and here is why" in a place that survives a crash. It is one append-only file of JSON Lines. `schemas/event.schema.json` is the machine mirror of the shape below; a test pins the two together.

Code: `core/rust/harness/src/events.rs` (the format, reader and writer) and the `harness events` command in `core/rust/harness/src/cli.rs`.

## Shape

One event per line, compact, fixed key order, every key present. Each line carries `v`, the schema version (an integer, `2` today; this is schema version 2). A line is at most 4096 bytes including its newline.

```
{"v":2,"kind":"escalation","ts_ms":1730000000000,"run_id":"run-1730000000000-1234-1","ticket":"t-1","repo":"my-repo","reason":"cap.hit","role":"executor","model":"m-1"}
```

| Key | Shape |
|---|---|
| `v` | the integer `2`; any other value is refused (a `v:1` line names `v` and is never upgraded) |
| `kind` | one of the thirteen kinds below |
| `ts_ms` | non-negative integer, milliseconds since the epoch, supplied by the caller |
| `run_id`, `role`, `model` | the run identity: which run wrote the line, in which role, with which model |
| `ticket`, `repo`, `reason` | a plain token: lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..` |

`run_id`, `role` and `model` are plain tokens too (the same rule as `ticket`, `repo` and `reason`). No other key is allowed, and no key may appear twice. Strings carry no escape sequences: a token never needs one. An event missing `run_id`, `role` or `model` is refused with `EVENT NOT RECORDED`; the fields are never guessed, defaulted or upgraded from a v1 line.

## Kinds

Thirteen kinds today, in two families. Each kind's key set is declared once, in the kind table in `core/rust/harness/src/events.rs`; this doc and `schemas/event.schema.json` mirror that table (a test pins all three together, so a drifted key fails the build).

### The four base kinds (the run-identity envelope)

| Kind | Meaning |
|---|---|
| `escalation` | a fix-round cap was reached and the ticket was escalated |
| `replan_required` | the ticket must be re-planned before more work is done on it |
| `budget_exceeded` | the ticket went over its budget and was stopped |
| `run_issued` | a run identity was self-issued; the first line such an invocation appends |

All four have the same key set: `v, kind, ts_ms, run_id, ticket, repo, reason, role, model`. Every key is a plain token except `v`, `kind` and `ts_ms`.

### The nine P1b kinds (payload kinds)

These carry no run-identity fields yet: the identity (`run_id`, `role`, `model`) joins them by editing the ONE shared envelope declaration in the loader when a run issuer exists (the PRE-B follow-up). Nothing writes them yet; later P1b tickets do, through the library record writer — the `events append` CLI flags cannot express a payload and refuse these kinds outright.

| Kind | Keys (canonical order) | Field shapes after `v`, `kind`, `ts_ms` | Meaning |
|---|---|---|---|
| `session_started` | `v, kind, ts_ms, session_id, source` | `session_id` uuid; `source` `launch` or `hook` | a worker session was launched, or the interactive session started at host hook time |
| `unmanaged_session` | `v, kind, ts_ms, session_id, ticket, reason` | `session_id` uuid; `ticket` token (the ticket or phase id the claimant reported); `reason` `not_supervisor_launched` or `no_launch_record` | a process reported a worker id but was not launched by the supervisor, or a hand-started session had no launch-record row; the one record that must never be lost, so it names no repo |
| `launch_refused` | `v, kind, ts_ms, launch_role, brief_sha256, matched_entry` | `launch_role` token (the refused session's role — the envelope `role` is the writer's); `brief_sha256` hex64; `matched_entry` token, the NAME of the config entry that matched, never the matched text (which can be a private path) | a builder-state entry matched, the launch was refused, nothing was started |
| `timed_out` | `v, kind, ts_ms, ticket, repo, cause, checkpoint, recipe, handover, session_id` | `ticket`, `repo` token; `cause` `timeout` or `died`; `checkpoint` sha (the last pushed checkpoint, or the workspace base sha when none was pushed); `recipe` `commit_push_then_resume`; `handover` relpath; `session_id` uuid (the worker) | the Process Supervisor stopped the run and left a continuation recipe |
| `check_in` | `v, kind, ts_ms, ticket, repo, steps, soft_limit` | `ticket`, `repo` token; `steps`, `soft_limit` u64 | past the soft step limit a check-in was written and the job continued |
| `step_limit_stop` | `v, kind, ts_ms, ticket, repo, steps, hard_limit` | `ticket`, `repo` token; `steps`, `hard_limit` u64 | the hard step limit stopped the job |
| `cleanup_result` | `v, kind, ts_ms, ticket, repo, result, manifest_left` | `ticket`, `repo` token; `result` `pass` or `fail`; `manifest_left` u64 (0 = the manifest's empty state) | the finalize step's cleanup outcome; `result=pass` with `manifest_left` above 0 is refused (a pass that left entries behind is a lie) |
| `quarantined` | `v, kind, ts_ms, ticket, repo, from, to, reason` | `ticket`, `repo` token; `from`, `to` relpath; `reason` `unmanifested` or `retention` | an item was moved to the quarantine folder, not deleted (the "under the harness root" bound is the writer's rule; the loader checks the relpath shape) |
| `human_origin_refused` | `v, kind, ts_ms, target, path` | `target` `config`, `permission` or `control_registry`; `path` relpath of the target file, never its content | a config, permission or control-registry write was refused because no human origin was proven; no `ticket`/`repo` (an agent run may be off-ticket) |

### Value shapes

| Shape | Rule |
|---|---|
| token | lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..` |
| uuid | full lowercase uuid, `8-4-4-4-12` hex; a short id is refused |
| hex64 | exactly 64 lowercase hex digits |
| sha | 40 or 64 lowercase hex digits (either length a repository may use) |
| relpath | token segments joined by `/`: no leading `/`, no empty, `.` or `..` segment, no `~` — so neither an absolute nor a home path can be written |
| u64 | an unsigned integer |
| enum | exactly one of the listed tokens |

### The version rule

The loader keeps a table of each kind's exact key set and the only `v` it exists at; the four base kinds are at `v` 2 with the PRE-B run-identity envelope (bytes exactly as when that envelope landed), the nine P1b kinds are at `v` 2 with the plain envelope and no identity fields. Any other `(v, kind)` pair — a kind at a `v` it never had, a `v` this reader does not know at all — is refused naming `v`, and a `v:1` line is never upgraded. A new kind, or a new key, is an additive change that bumps `v`. A reader that sees a `v` it does not know refuses the line and names the line and `v`; it never guesses.

## Run identity (`run_id`)

Every event belongs to exactly one run, named by its `run_id`.

- **Self-issue.** An invocation that writes an event and has no `HARNESS_RUN_ID` in its environment issues its own id (`run-<ms>-<pid>-<seq>`, unique per issue — the timestamp is never the id alone) and appends a `run_issued` line first, carrying the same `ts_ms`, `ticket`, `repo`, `role` and `model` as the event. Read-only commands (`events list`, `events verify`, `status`) issue nothing and write nothing.
- **Inheritance.** An invocation with `HARNESS_RUN_ID` set writes under that id only when the same log already holds a `run_issued` line for exactly that id. An id that was never issued in this log — or is malformed — is refused with `EVENT NOT RECORDED` and nothing is written. Passing the variable down to children is each spawner's job, not the log's.
- **Role and model.** `role` and `model` come from the caller (`--role`/`--model` flags on `events append`, or `HARNESS_ROLE`/`HARNESS_MODEL` in the environment for the caps' refusals); missing or not a plain token refuses the event. They are never defaulted.
- A cap's refusal stays a refusal when its event is not recorded: a failed write never turns a refusal into a pass.

## Reading: torn tail versus corruption

- A final fragment with **no trailing newline** is a **torn tail**: a crash cut a write short. The reader returns every earlier event, reports the tail, and never parses it as an event, even when the fragment happens to be valid JSON.
- A **complete line (ends in a newline) that is not a valid event** is **corruption**. The reader stops at that line. `harness events list` prints the intact events before it and nothing after, and exits non-zero. A refused line is never skipped. `events list` prints the stored lines; it is not a summary format.
- Corruption wins: if a log has a corrupt line and a torn tail after it, only the corrupt line is reported.

## Writing

- The writer opens the file in append mode (creating it if absent), takes an exclusive lock on it, checks the last byte, writes the whole line with ONE write, then calls `sync_all`. Success means the line is on disk.
- If the log ends in a torn tail, the writer **refuses** and changes nothing. It never repairs the tail and never self-heals: appending onto a fragment would fuse a new event into garbage. Recovery of a torn tail is a separate, explicit step that is not built yet.
- The writer's check is tail-only. It does not validate the rest of the log, so a corrupt middle is caught by `verify` and `list`, not by `append`.
- A line over 4096 bytes, a bad token, or a bad log path (a directory, or a path that resolves outside the root) is refused and writes nothing.
- A failed write never turns a cap's refusal into a pass. The command exits non-zero and prints `EVENT NOT RECORDED` on stderr.

## The command

```
harness events append [--log <path>] [--root <dir>] --kind <kind> --ticket <t> --repo <r> --reason <x> --role <r> --model <m> [--ts-ms <n>]
harness events list   [--log <path>]
harness events verify [--log <path>]
```

`--role` and `--model` may also come from `HARNESS_ROLE` and `HARNESS_MODEL`; the run id comes from `HARNESS_RUN_ID` or is self-issued. The default log is `.harness/events.jsonl` in the working folder, which is git-ignored. Without `--root`, the root is the working folder when the default log is used, and the log's parent folder when `--log` is given. Without `--ts-ms`, the wall clock is written (on both the `run_issued` line and the event). Exit codes: `0` ok; `1` the log has a torn tail or corruption (`list` still prints the intact events first); `2` a usage error or a refusal.

## What v2 does not promise

- Order among concurrent writers is undefined: any interleaving of complete lines is valid. "Append order" is a single-writer idea.
- Local disk only. No network or remote filesystem is supported or tested.
- No tamper evidence: a hand edit that stays schema-valid is not detected. A hash chain is deferred.
- The lock is advisory: it binds writers that use this module, not a foreign process.
- No rotation, size bound or retention.
- `HARNESS_RUN_ID` acceptance is checked against the log as it is read: an id issued in a different log is not inherited here.

## Replay into a database later

The file is append-only and never rewritten, so (file, line number) is a stable, ordered key. Each line is self-describing (`v`, `kind`, exact keys, run identity), so a later reader can map each kind to a table by reading top to bottom, and an idempotent re-import is "skip rows already loaded". Nothing here depends on a database.
