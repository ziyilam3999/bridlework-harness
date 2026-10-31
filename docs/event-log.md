# Event log (v1)

The event log is where a cap says "I stopped this ticket, and here is why" in a place that survives a crash. It is one append-only file of JSON Lines. `schemas/event.schema.json` is the machine mirror of the shape below; a test pins the two together.

Code: `core/rust/harness/src/events.rs` (the format, reader and writer) and the `harness events` command in `core/rust/harness/src/cli.rs`.

## Shape

One event per line, compact, fixed key order, every key present. Each line carries `v`, the schema version (an integer, `1` today). A line is at most 4096 bytes including its newline.

```
{"v":1,"kind":"escalation","ts_ms":1730000000000,"ticket":"t-1","repo":"my-repo","reason":"cap.hit"}
```

| Key | Shape |
|---|---|
| `v` | the integer `1`; any other value is refused |
| `kind` | one of the three kinds below |
| `ts_ms` | non-negative integer, milliseconds since the epoch, supplied by the caller |
| `ticket`, `repo`, `reason` | a plain token: lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..` |

No other key is allowed, and no key may appear twice. Strings carry no escape sequences: a token never needs one.

## Kinds

| Kind | Meaning |
|---|---|
| `escalation` | a fix-round cap was reached and the ticket was escalated |
| `replan_required` | the ticket must be re-planned before more work is done on it |
| `budget_exceeded` | the ticket went over its budget and was stopped |

All three have the same key set today. A new kind, or a new key, is an additive change that bumps `v`. A reader that sees a `v` it does not know refuses the line and names the line and `v`; it never guesses.

## Reading: torn tail versus corruption

- A final fragment with **no trailing newline** is a **torn tail**: a crash cut a write short. The reader returns every earlier event, reports the tail, and never parses it as an event, even when the fragment happens to be valid JSON.
- A **complete line (ends in a newline) that is not a valid event** is **corruption**. The reader stops at that line. `harness events list` prints the intact events before it and nothing after, and exits non-zero. A refused line is never skipped.
- Corruption wins: if a log has a corrupt line and a torn tail after it, only the corrupt line is reported.

## Writing

- The writer opens the file in append mode (creating it if absent), takes an exclusive lock on it, checks the last byte, writes the whole line with ONE write, then calls `sync_all`. Success means the line is on disk.
- If the log ends in a torn tail, the writer **refuses** and changes nothing. It never repairs the tail and never self-heals: appending onto a fragment would fuse a new event into garbage. Recovery of a torn tail is a separate, explicit step that is not built yet.
- The writer's check is tail-only. It does not validate the rest of the log, so a corrupt middle is caught by `verify` and `list`, not by `append`.
- A line over 4096 bytes, a bad token, or a bad log path (a directory, or a path that resolves outside the root) is refused and writes nothing.
- A failed write never turns a cap's refusal into a pass. The command exits non-zero and prints `EVENT NOT RECORDED` on stderr.

## The command

```
harness events append [--log <path>] [--root <dir>] --kind <kind> --ticket <t> --repo <r> --reason <x> [--ts-ms <n>]
harness events list   [--log <path>]
harness events verify [--log <path>]
```

The default log is `.harness/events.jsonl` in the working folder, which is git-ignored. Without `--root`, the root is the log's parent folder. Without `--ts-ms`, the wall clock is written. Exit codes: `0` ok; `1` the log has a torn tail or corruption (`list` still prints the intact events first); `2` a usage error or a refusal.

## What v1 does not promise

- Order among concurrent writers is undefined: any interleaving of complete lines is valid. "Append order" is a single-writer idea.
- Local disk only. No network or remote filesystem is supported or tested.
- No tamper evidence: a hand edit that stays schema-valid is not detected. A hash chain is deferred.
- The lock is advisory: it binds writers that use this module, not a foreign process.
- No rotation, size bound or retention.

## Replay into a database later

The file is append-only and never rewritten, so (file, line number) is a stable, ordered key. Each line is self-describing (`v`, `kind`, exact keys), so a later reader can map each kind to a table by reading top to bottom, and an idempotent re-import is "skip rows already loaded". Nothing here depends on a database.
