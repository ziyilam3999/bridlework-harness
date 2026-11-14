# Session launcher (`harness session launch`)

Starts a named background session from the harness, with a launch record written first. Unix only.

```
harness session launch --name <n> --brief <file> --cwd <dir> --host <abs-path>
    [--records <file>] [--config <file>] [--sign-off] [--start-timeout-ms <n>] [--map-timeout-ms <n>]
harness session launches [--records <file>]
```

## Order of steps

1. Validate everything before any write: `--host` is an absolute path to an existing file (no PATH lookup);
   `--cwd` is an existing directory (canonicalised); the brief is read once and must be non-empty, valid UTF-8,
   free of NUL bytes, not start with `-`, and be at most 102400 bytes. The bytes are never trimmed.
2. Name check: the P1B-16 predicate in `--new` mode, with the lane bound from the config (`--config`, else
   `HARNESS_CONFIG`). The record's `role` is that verdict's role.
   `--sign-off` is a value-less flag (given twice is exit 2). It marks the launch as a sign-off and is allowed for
   `gov` and `e-rvw` names only (any other role: exit 2, `--sign-off needs a gov or e-rvw name`). A sign-off launch
   MUST pass it; an ordinary review must not (a routine `e-rvw` brief may name the builder's work).
3. sha256 of the exact brief bytes, and a fresh run id.
3b. Signer check, after the records path is resolved (the default `.harness/` folder is created first) and before
   the `launch` row. It runs for every `gov` launch and for any launch given `--sign-off`; a plain `e-rvw` launch is
   not checked. With no `builder_state` entries in the config: exit 2, nothing written or started (fail closed).
   Otherwise the FIRST entry (file order) whose text is a byte substring of the brief refuses the launch: one
   `launch_refused` row, no `launch` row, nothing started, exit 6. stderr and the row name the entry, never its text.
4. Write the `launch` row first. Any failure exits 2 with `launch refused: launch record not written: <cause>`
   and the host is never run.
5. Start `<host> --bg -n <name> <brief>`: the brief is ONE argv element, byte-identical to the file, run from the
   canonical `--cwd`, in its own process group, with a bounded wait (`--start-timeout-ms`, default 30000).
6. Map the short id the host printed to the full uuid by polling `<host> agents --json --all` (default bound
   10000 ms, tunable with `--map-timeout-ms`, every 250 ms) for exactly one entry with that short id and the same
   name. Append a `launch_ids` row with the same `run_id`. A record never carries a uuid the host did not print.
7. Print `launched run_id=.. name=.. role=.. short_id=.. session_id=..`.

## Host output shape

The short id is the first whitespace token of the last non-empty stdout line of `--bg`. `agents --json --all`
is a JSON array of objects with `id` (short id), `sessionId` (full uuid) and `name`. Both live in one place in
`launcher.rs` (`parse_short_id` and the `AGENT_KEY_*` constants).

## Rows

All rows are one compact JSON line in an append-only file, taken under an exclusive lock; a torn tail refuses the write.

- `{"v":1,"kind":"launch","ts_ms":..,"run_id":..,"name":..,"role":..,"brief_sha256":..,"owner":"orchestration"}`
- `{"v":1,"kind":"launch_refused","ts_ms":..,"run_id":..,"name":..,"role":..,"brief_sha256":..,"matched_entry":..}` (a signer brief named builder state; `matched_entry` is the entry NAME, never its text; the row stands alone)
- `{"v":1,"kind":"launch_ids","ts_ms":..,"run_id":..,"short_id":..,"session_id":..}`
- `{"v":1,"kind":"launch_failed","ts_ms":..,"run_id":..,"stage":"start|map","reason":..}` (best effort)

`harness session launches` joins rows by `run_id` and prints one line per launch with `state=bound|failed|pending|refused`.
A corrupt line stops the read (exit 2, the line number named). `launcher::read_launches` is the same join as a function.

A `--sign-off` launch writes `"sign_off":true` as the last field of its `launch` row (a launch without the flag writes the
row it always wrote). `launches` adds ` sign_off=yes` to that launch's line only. Any other `sign_off` value in a row
(`false`, a string, a number) is a corrupt line. `harness session sign-off-check` reads the mark; see `docs/signoff.md`.

## Records path

With `--records`, the path is used as typed, resolved against the harness process cwd; its parent folder must
already exist (nothing is created, a missing parent is exit 2). Without it the default is
`.harness/launch-records.jsonl` relative to the harness process cwd, never relative to `--cwd` (which only sets the
host's working directory); `.harness/` is created when missing.

## Exit codes

| code | meaning |
|---|---|
| 0 | launched and bound |
| 1 | the name is refused by P1B-16 |
| 2 | usage or validation error, or the launch record could not be written (nothing started) |
| 3 | the host failed to start: non-zero exit, no short id, spawn error (for example E2BIG), or timeout |
| 4 | the id could not be mapped; the session was stopped (`<host> stop <short-id>` exited 0) |
| 5 | the id could not be mapped and the stop failed; stderr names the short id, stop it by hand |
| 6 | a signer launch was refused: its brief names a `builder_state` entry (a `launch_refused` row is written, nothing started) |

## Brief cap

`BRIEF_MAX_BYTES = 102400`. Linux allows one argv element of at most 131072 bytes including its NUL; the cap leaves
margin and is checked before any write. A residual E2BIG (for example from a huge inherited environment) still fails
closed: exit 3 with a `launch_failed` row (`stage=start`), nothing runs.

## Timeout

When `--start-timeout-ms` expires the launcher sends SIGKILL to the host's process group, reaps the child, appends
`launch_failed` (`stage=start`, `reason=timeout`) and exits 3. Killing the host does not prove no session was
started, so stderr always names the session: check `<host> agents` and stop it by hand.

## Closing a ticket's sessions

`harness session done-check` and `harness session close` (P1B-17) read the same host listing and remove a finished ticket's sessions; see `docs/session-close.md`.
