# harness session rotation-check

Report only. It says whether an Orchestrator session is due for a handover. It decides and blocks nothing: it writes no file, no
event and no row, starts no child process, and no hook calls it yet. std only.

```
harness session rotation-check --log <abs file> --session-id <uuid> --transcript <abs file> --utc-offset <+HH:MM|-HH:MM> [--now-ms <n>]
```

| Flag | Meaning |
|------|---------|
| `--log` | absolute path of the event log (JSON Lines) that holds the start row |
| `--session-id` | lowercase uuid of the session |
| `--transcript` | absolute path of the transcript, named `<session-id>.jsonl` |
| `--utc-offset` | the zone of the operator as a fixed offset: `+HH:MM` or `-HH:MM`, HH 00 to 14, MM 00 to 59 |
| `--now-ms` | the moment of the check in epoch milliseconds, digits, at most 253401609599000; the system clock when absent |

A missing, repeated or unknown flag, or a stray argument, is exit 2.

## The two marks and the rule

- A new day: the local day of now is after the local day of the start.
- The first compaction: the transcript holds one or more compaction lines.

The session is due when either mark holds. The local day of a moment is `floor((ms / 1000 + offset seconds) / 86400)`.

The zone is a fixed offset the caller gives. A fixed offset does not follow a clock change (daylight saving): a caller whose zone
changes its offset passes the offset in force at the moment of the call.

## The start record

The log is read with the event log reader. The start is the smallest `ts_ms` among the rows of kind `session_started` with
`source` `hook` and `session_id` equal to `--session-id`. Rows of another session id, rows with source `launch` and rows of other kinds
(for example `unmanaged_session`) are ignored.

Nothing here writes a start row. `harness session record-start` or the SessionStart hook does. Example:

```
harness session record-start --session-id <uuid> --log <abs file> --ts-ms <n>
```

The SessionStart matcher also fires on resume, clear and compact, and each firing appends the same kind of row, so the rows cannot
tell the four apart. The earliest row of the id is the start, so a later row of the same id does not move it. This is an OPEN fail-open
path: a row written on resume, clear or compact before any startup row cannot be told from a startup row, so the start can be too
late and a due day can be missed. Closing it needs a change to the shared event schema and to the hook, outside this verb; the later
hooks ticket carries it.

With no hook row for the id the verb refuses; it does not pass. A missing log is a refusal here, while `session sign-off-check`
reads a missing log as empty. A torn tail can show for a moment when another writer appends to the shared log.

## The compaction count

The verb parses each transcript line in memory to count the lines of type `system` and subtype `compact_boundary` (the count
`session tokens` reports), and prints only the count. No byte of a line is printed. Which sessions a caller may run it on is the rule
of the caller.

A half-written last line while the session writes is `line <n> is not valid JSON` (exit 3). An assistant line with no message, no id
or no usage, or with a usage field that is not a whole number, gives exit 3 on each later call while that line stays
in the transcript.

The last part of the transcript path must be exactly `<session-id>.jsonl` (the id as given, lowercase). The name is checked before the
file is touched, so the event log, or the transcript of another session, passed by mistake is refused and not counted as 0.

## Output and exit codes

Stdout, on exit 0 and 10 only, is one line:

```
rotation due=<yes|no> reason=<none|day|compaction|day+compaction> compactions=<n> start_day=<YYYY-MM-DD> now_day=<YYYY-MM-DD>
```

| Exit | Meaning | Stdout |
|------|---------|--------|
| 0 | not due | the line |
| 10 | due | the line |
| 2 | usage error | empty |
| 3 | refusal, cannot judge | empty |

A caller that gates work treats each exit other than 0 as not cleared: 10 is due, 3 is cannot judge (not "not due"), 2 is a bad call.

## Usage errors and refusals

Exit 2 prints two stderr lines: `harness: error: <message>` and the usage line. The checks run in this order; the first that fails
is the one printed.

| Message |
|---------|
| `bad arguments` |
| `--log is required`, `--session-id is required`, `--transcript is required`, `--utc-offset is required` (in that order) |
| `--log must be an absolute path`, `--transcript must be an absolute path` |
| `--session-id must be a uuid` |
| `--utc-offset must be +HH:MM or -HH:MM (HH 00 to 14, MM 00 to 59)` |
| `--now-ms must be digits, at most 253401609599000` |

A system clock before the epoch is exit 2 with `harness: error: clock is before the epoch` and no usage line.

Exit 3 prints one stderr line, `harness: rotation-check refused: <part>: <text>`. The order of checks: usage flags, the start record,
the transcript name, the transcript file.

| Part | Text |
|------|------|
| start record | `cannot read log: <reason>` (permission denied, entity not found) |
| start record | `log refused: not a regular file` |
| start record | `log refused: the path is a symbolic link` |
| start record | the own text of the reader (a log over its byte cap, a changed file) |
| start record | `no start row for the session` |
| start record | `the log has a corrupt line or a torn tail` |
| start record | `the start time is out of range` (start over 253401609599000) |
| start record | `the start time is after now` (a start equal to now passes) |
| transcript | `the file name does not match the session id` |
| transcript | `the transcript cannot be opened: <reason>` |
| transcript | `the transcript is a symbolic link` |
| transcript | `the transcript is not a regular file` |
| transcript | `the transcript is empty` |
| transcript | `line <n> is not valid JSON`, or the reader text for an assistant line with no message, no id, no usage or a bad usage field |

The transcript path kind is checked without following links, and the opened file must be the file that check saw.

## Cost

Each call reads the whole log (up to its byte cap) and the whole transcript (no line cap).

## Worked examples

Two `record-start` rows for one id at 1795140000000 and 1795200000000, a transcript of the two lines
`{"type":"user"}` and `{"type":"system","subtype":"compact_boundary"}`, offset `+08:00`, `--now-ms 1795150000000`:

```
rotation due=yes reason=compaction compactions=1 start_day=2026-11-20 now_day=2026-11-20
```

exit 10: the start is the earlier row.

A log with no hook row for the id: stderr `harness: rotation-check refused: start record: no start row for the session`, exit 3,
stdout empty.

## What it does not do

No block, no write, no hook. It does not write a start row, close the earliest-row path, read any other transcript, or handle a zone
with a clock change.
