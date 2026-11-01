# Resource manifest (v1)

The resource manifest is the sign-out sheet of a ticket: one durable list of everything the ticket owns — worktrees, temp folders, processes, ports, logs, kept evidence — so cleanup may touch only what is listed. Anything inside the ticket's roots that is NOT on the list is a leftover, to be quarantined (later tickets), never silently trusted. The file survives a crash and survives removal of the ticket's worktree, because it lives outside the roots.

It is one append-only JSON Lines file per ticket, `<manifest-dir>/<ticket>.jsonl` (default `<manifest-dir>`: `.harness/manifests/` in the working folder, git-ignored; `--dir` overrides it). `schemas/resource-manifest.schema.json` is the machine mirror of the shapes below; a test pins the two together.

Code: `core/rust/harness/src/manifest.rs` (the format, reader, writer, fence and ownership query) and the `harness manifest` command in `core/rust/harness/src/cli.rs`. The line, read and write rules are the event log's (`docs/event-log.md`), restated here only where the manifest adds to them.

## Shape

One operation per line, compact, fixed key order, every key present. Each line carries `v`, the schema version (an integer, `1` today). A line is at most 4096 bytes including its newline. Every line's `ticket` equals the file's ticket.

```
{"v":1,"op":"root","ticket":"t-1","path":"/scratch/roots/r","ts_ms":1730000000000}
{"v":1,"op":"add","ticket":"t-1","id":"w-1","class":"worktree","path":"/scratch/roots/r/w","ts_ms":1730000000001}
{"v":1,"op":"add","ticket":"t-1","id":"p-1","class":"process","pid":4242,"pid_start_ms":1730000000000,"ts_ms":1730000000002}
{"v":1,"op":"add","ticket":"t-1","id":"n-1","class":"port","port":8080,"ts_ms":1730000000003}
{"v":1,"op":"remove","ticket":"t-1","id":"w-1","ts_ms":1730000000004}
```

| `op` | Keys, in order | Notes |
|---|---|---|
| `root` | `v, op, ticket, path, ts_ms` | Written only by init, only before any other line (one line per root). `path` is an absolute, existing directory, stored resolved; never `/`. |
| `add` (path class) | `v, op, ticket, id, class, path, ts_ms` | `class` is `worktree`, `temp_dir`, `log` or `evidence`. `path` is absolute and stored resolved; it must resolve inside one of the ticket's roots (symlinks and `..` resolved first). It may not exist yet; then its parent must resolve inside a root. |
| `add` (process) | `v, op, ticket, id, class, pid, pid_start_ms, ts_ms` | `class` = `process`. `pid` is an integer > 1 (0, 1 and negatives are refused: signalling them would hit a group or init). `pid_start_ms` is the process's OS start time in ms since the epoch, read by the writer (on Linux from `/proc`; otherwise the system `ps`); a pid whose start time cannot be read is refused. It lets a later reader tell a reused pid from the recorded one. Off Linux the value comes from `ps` elapsed time at whole-second precision and jitters between reads (about a second), so a caller comparing start times needs a tolerance of at least 2000 ms. |
| `add` (port) | `v, op, ticket, id, class, port, ts_ms` | `class` = `port`. `port` is an integer in 1..65535. |
| `remove` | `v, op, ticket, id, ts_ms` | `id` must be a live entry when the line is written. |

`ticket` and `id` are plain tokens (lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..` — the event log's token rule). No other key is allowed, no key may appear twice, and strings carry no escape sequences: a path may legally contain a space, but a path holding `"` or `\` or a control character is refused.

**Current state** = the roots plus every `add` whose `id` has no later `remove`. "Manifest empty" means no live entries; roots do not count.

## The fence

Init declares the ticket's roots. Every path entry must resolve inside one of them — a path outside, a `..` that escapes, or a symlink that points out are all refused. The manifest file itself must live outside the roots (`--dir` inside a root is refused), so finalize can remove the roots without removing the record, and the record can still show the empty state afterwards.

## Ownership

- A path is owned when it equals, or lies inside, a live path entry (after resolving).
- A pid or a port is owned when a live entry lists it. `owns --pid` answers by pid alone; comparing `pid_start_ms` against a live process after `list` is the caller's job (the process supervisor and the orphan reaper).
- Anything else is not owned. A manifest that cannot be read cleanly — missing file, torn tail, corrupt line — owns nothing: the query fails closed, and callers treat any non-zero `owns` as "do not touch".

## Reading and writing

Same rules as the event log:

- A final fragment with **no trailing newline** is a **torn tail**: reported, never parsed, even when it happens to be valid JSON.
- A **complete line that is not a valid line of this ticket's manifest** is **corruption**; the read stops at that line. Corruption wins over a torn tail. A line that breaks a state invariant (an `add` before init, a root line after another operation, a duplicate live id, the same path/pid/port twice live, a `remove` of a dead id, a `ticket` that differs from the file's) is corruption too: such a file was not written by this writer, and the read fails closed.
- The file itself is capped at 1 MiB (1,048,576 bytes): a larger file is refused as corrupt — fail closed, never a slow read of an unbounded file.
- The writer opens the file in append mode (creating it if absent), takes an exclusive lock, checks the last byte, writes the whole line with ONE write, then calls `sync_all`. It refuses to append onto a torn tail and never repairs one. A line on disk means the operation happened; the manifest file IS the durable record (no event kind is added for it).

## Refused, writing nothing

A missing, extra or duplicate key; `v` != 1; an unknown `op` or `class`; a non-token `ticket` or `id`; a line whose `ticket` differs from the file's; a relative path, a path with a character that would need escaping, a path resolving outside every root; a root that is relative, missing, not a directory, or `/`; a manifest dir that resolves inside one of the ticket's roots; a second init; an `add` or `remove` before init; a duplicate live id; the same resolved path, pid or port listed twice live; a `remove` of an id that is not live; a pid <= 1; a process add missing `pid_start_ms` or with a non-integer one; a port outside 1..65535; a line over 4096 bytes; an append onto a torn tail.

## The command

```
harness manifest init   <ticket> --root <dir> [--root <dir> ...] [--dir <manifest-dir>] [--ts-ms <n>]
harness manifest add    <ticket> (--path <p> | --pid <n> | --port <n>) --class <c> --id <id> [--dir <manifest-dir>] [--ts-ms <n>]
harness manifest remove <ticket> --id <id> [--dir <manifest-dir>] [--ts-ms <n>]
harness manifest list   <ticket> [--dir <manifest-dir>]
harness manifest owns   <ticket> (--path <p> | --pid <n> | --port <n>) [--dir <manifest-dir>]
harness manifest verify <ticket> [--dir <manifest-dir>]
```

`--class` must match the target flag: a path class with `--path`, `process` with `--pid`, `port` with `--port`. A process `add` reads `pid_start_ms` itself; a pid whose start time cannot be read is refused. Without `--ts-ms`, the wall clock is written.

`list` prints one line per live entry, in add order, nothing when the manifest is empty. A `process` entry prints four tab-separated fields — `id`, `class`, `pid`, `pid_start_ms`, both last ones decimal — so the supervisor and the reaper can compare the start time against the live process. Every other class prints exactly three fields: `id`, `class`, `target`, where `target` is the stored resolved path for the path classes and the decimal `port` for `port`.

`verify` reads the whole file and exits 0 only when it is clean — no torn tail, no corruption — the same contract as `harness events verify`.

Exit codes: `0` ok / owned; `1` a torn tail or corruption on a read (`list`, `verify`, `owns`), or `owns` answers not owned; `2` a usage error or any refusal (including a write onto a damaged file). A missing manifest file: `list` and `verify` exit 2 (it is not a manifest), `owns` exits 1 (it owns nothing).

## What v1 does not promise

- Order among concurrent writers is undefined; "add order" is a single-writer idea.
- Local disk only; no network filesystem is supported or tested.
- No tamper evidence: a hand edit that stays schema-valid AND state-valid is not detected. A hash chain is deferred.
- The lock is advisory: it binds writers that use this module.
- Containers are not a class at v1 (a later ticket adds them with a `v` bump); screenshots, browser profiles and temp databases are recorded as `temp_dir` or `evidence` paths.
- Two `init`s under two different `--dir`s both succeed (each file fences only itself) and split one ticket's record; callers that own a ticket's lifecycle always pass an explicit `--dir`.
