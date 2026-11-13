# Runtime evidence reading and the Done link

The process supervisor ([process supervisor](process-supervisor.md), P1B-08) gives every program a
ticket starts a small private notebook: what it printed (`stdout`, `stderr`) and a last page (`exit`)
that says how it ended, inside the ticket's own root and signed out on the ticket's
[resource manifest](resource-manifest.md) before the spawn. This document is the READER half
(P1B-09, #214): two read-only verbs that read those notebooks and answer the Done question.

```
harness evidence show  <ticket> --manifest-dir <absolute-dir>
harness evidence check <ticket> --manifest-dir <absolute-dir>
```

Unix only, like `process`. The argument rules are `workspace check`'s: the ticket is a plain manifest
token (lowercase letters, digits, `-`, `_`, `.`; not empty, not `.` or `..`), `--manifest-dir` is
required, absolute, with no `..` component; an unknown flag, a missing value, a repeat or a second
ticket is a usage error. No default folder, no config file, no current-folder lookup.

Code: `core/rust/harness/src/evidence.rs` (the reader lives with the writer it inverts); the
`harness evidence` dispatch is in `core/rust/harness/src/cli.rs`. Nothing new in the manifest format
and no new event kind: the manifest's full append log IS the input.

## What a "run" is

A run is every `<id>.<ts_ms>` for which the manifest's FULL append log — not just the live entries —
ever holds an `add` of class `evidence` whose id is `<id>.<ts_ms>.<stream>` (the stream one of
`stdout`, `stderr`, `exit`; the id splits from the right, so a process id may itself hold dots) and
whose stored path is exactly `<root>/evidence/proc/<id>.<ts_ms>/<stream>` for one of the manifest's
roots. A REMOVED entry is still a run: crossing the line off the sheet does not erase what happened,
so a worker cannot hide a crash by deleting its line. Any other `evidence` entry (a screenshot added
by hand) is counted as `other`, never judged.

## The state table

Each run gets exactly one state:

| state | meaning |
|---|---|
| `ok` | a valid record, `committed:true`, `exited`, `code` 0 |
| `stopped` | a valid record, `committed:true`, `signalled`, signal 15 or 9 (what a harness stop sends) |
| `attempt` | a valid record with `committed:false` (start answered 2 for it at the time) |
| `failed` | `committed:true` and `exited` with a non-zero code, or `signalled` with any other signal, or `wait_failed` |
| `running` | no `exit`, this is the newest run of its id, and a live `process` entry of that id exists whose pid still is the process the sheet recorded (start time unchanged) |
| `unknown` | no `exit` otherwise (waiter killed, host crash, files removed) — unknown is NEVER success |
| `damaged` | the evidence shape itself is broken, with one reason token (below) |

A record is judged by its SHAPE, strictly: exactly one newline-terminated line in the writer's fixed
key order (the reader's parser is the inverse of the encoder), `v` 1, a known outcome, and no
`committed:true` with `spawn_failed`. Before the record is opened, its name is looked at without
following links, and the opened file's device and inode must equal the looked-at one (a swap in
between is caught). The `damaged` reason tokens:

- `symlink` — `evidence`, `evidence/proc`, the run dir or the `exit`/stream name is a symlink (a symlink is never opened)
- `not-dir` / `not-regular` — the run dir is not a directory; `exit` or a stream is not a regular file (a FIFO is never opened, so nothing hangs)
- `bad-mode` — any group/other permission bit on `evidence`, `evidence/proc`, the run dir or `exit`
- `foreign-owner` — another user owns `evidence`, `evidence/proc`, the run dir or `exit`
- `over-cap` — `exit` is over 4096 bytes; it is judged by its metadata and never read past 4097 bytes
- `unreadable` — the file or dir exists but cannot be read
- `malformed` — the record deviates from the writer's shape in any way
- `missing-stream` — a whole record whose `stdout` or `stderr` file is missing
- `swapped` — the opened `exit` is not the file the no-follow look saw

A run dir that is gone entirely is NOT damaged: with no `exit` it reads `unknown` (which blocks Done
just the same).

## The Done rule

`evidence check` refuses Done (exit 1) when at least one run is `failed`, `running`, `unknown` or
`damaged`. It allows (exit 0) when every run is `ok`, `stopped` or `attempt` — including ZERO runs.
Zero runs is allowed because most tickets never start a process, and P1B-08 evidence exists only for
started processes; whether a ticket NEEDED a runtime run is the acceptance gate's question, not this
check's. `ticket gate` is not changed (it reads a ticket FILE, a different input with a different id
rule): the Done link is this check plus the contract below, exactly as `workspace check` is wired.

**Finalize (P1B-15) runs `harness evidence check <ticket> --manifest-dir <dir>` and refuses Done
unless it exits 0.**

## Output

`show` and `check` print no captured byte, no tail, and no path — captured output is never echoed by
them (only `quote` reads a stream, scanned first, see Evidence privacy); ids are manifest tokens, and every number in
the output is re-printed from the parsed record's integers, never copied text.

`show` (exit 0 read, whatever the states) prints one tab-separated line per run, sorted by `<ts>`
then `<id>`, every field present (`-` when unknown):

```
run <id> <ts> <state> <reason|-> <outcome|-> <code|-> <signal|-> <committed|-> <duration_ms|-> <stdout_bytes|-> <stderr_bytes|-> <stdout_truncated|-> <stderr_truncated|->
```

then exactly one summary:

```
evidence ticket=<t> runs=<n> ok=<a> stopped=<b> attempt=<c> failed=<d> running=<e> unknown=<f> damaged=<g> other=<h>
```

`check` prints `evidence=allowed ticket=<t> runs=<n>` on allow. On refuse it prints one
`refused <id> <ts> <state> <reason|outcome-detail>` line per blocking run (same order as `show`),
then `evidence=refused ticket=<t> blocking=<n>`, and exactly one stderr line
`harness: error: Done refused by evidence: <n> run(s) failed, running, unknown or damaged (first: <id>.<ts> <state>)`.

## Exit codes

- `show`: `0` read (whatever the states); `2` cannot check.
- `check`: `0` Done allowed by evidence; `1` Done refused; `2` cannot check.

"Cannot check" is a usage error, a missing, torn or corrupt manifest, an unreadable manifest dir or
an unreadable running user: stdout empty and one folder-free `harness: error:` stderr line. A MISSING
manifest is never zero runs — fail closed (2 is the crate's refusal code; 1 matches `ticket gate`'s
"Done refused").

## Evidence privacy (P1B-10)

Evidence never leaves private state unscanned. Every renderer builds its whole stdout and stderr text
and hands it to `evidence::emit_scanned`, the only sanctioned way an evidence byte leaves the process.
It loads the privacy list, scans both texts with the same engine as `privacy scan --text` and
`friction add`, and on any hit prints nothing on stdout and one stderr line naming the surface, the hit
count, the class ids and the line numbers (never a scanned byte). **No list configured means exit 2**
(`harness: error: no privacy list configured: evidence is never printed unscanned`): unscanned is never
clean. Later renderers (finalize, status/report cards, briefs) must call `emit_scanned`.

`show` and `check` go through it too: a hit in their rendered text, or no list, exits 2 (never 0, so
Done is never allowed unscanned). Clean output is byte for byte as before.

`harness evidence quote <ticket> --run <id>.<ts> --stream stdout|stderr [--last <n>] --manifest-dir
<absolute-dir>` quotes captured lines. The run must be one the ticket's manifest signed out. The stream
is opened no-follow, must be a regular private file owned by the user (a FIFO or symlink is refused,
never opened), and only its last 64 KiB are read. Only WHOLE finished lines are quoted (a fragment of a
secret scans clean): the truncation marker is stripped first, a window that does not start at offset 0
drops its partial first line, a final run with no newline is dropped, and only then must the rest be
UTF-8 (never a lossy conversion). A stream cut at capture adds the fixed line
`[harness: stream was truncated at capture; the cut line is not quoted]`. `--last` defaults to 20, max
200 (0 or more is a usage error). Exit codes: `0` quoted, `1` a privacy hit (stdout empty), `2` cannot
quote (usage, unsigned run, bad stream, non-UTF-8, no list). `manifest::Class::exportable()` is false
for `Evidence`: any future export or publish path must check it.

## Read-only

`show` and `check` write nothing: neither writes, renames, locks, chmods or deletes anything (the manifest included), takes no
supervisor lock file, never opens a FIFO, and no read is unbounded: the manifest through its capped
(1 MiB) reader, `exit` at most 4097 bytes, streams never read by them.

## What this does not cover

- Redaction: there is none by design. Evidence is scanned and refused, never edited (see Evidence privacy above).
- Retention and teardown of evidence entries and run dirs (P1B-14, ticket 185), run rotation.
- Wiring the check into `harness ticket finalize` and any operator override for `unknown` runs (P1B-15).
- Whether a ticket NEEDED a runtime run (zero runs is allowed).
- Distinguishing a harness stop from an external TERM/KILL, or a timeout stop from a normal stop: a
  program that catches TERM and exits non-zero after a harness stop reads `failed`; an external
  SIGKILL (OOM killer included) reads `stopped`. A stop marker the reader can trust is a follow-up.
- Authorship: a same-uid worker can write a fake `exit` with `code 0` or edit the manifest by hand;
  the checks catch shape, mode, owner and swap, not authorship (the manifest has no tamper evidence
  either).
