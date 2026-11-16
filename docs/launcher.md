# Session launcher (`harness session launch`)

Starts a named background session from the harness, with a launch record written first. Unix only.

```
harness session launch --name <n> --brief <file> --cwd <dir> --host <abs-path>
    [--records <file>] [--config <file>] [--sign-off] [--provider <name>]
    [--turn <name>[,<name>]] [--turn-reason <text>] [--start-timeout-ms <n>] [--map-timeout-ms <n>]
harness session launches [--records <file>]
harness session next-provider --providers <name>[,<name>] [--records <file>]
```

`--provider <name>` records which provider ran the seat. A name is 1 to 16 bytes: a lowercase ASCII letter, then
lowercase ASCII letters or digits (`claude`, `zai` and `gpt5` pass; `Zai`, `zAi`, `z-ai`, an empty value and a 17-byte name do not).
A refused name, the flag given twice or the flag with no value is exit 2 before any write; the host is not run.
Any role may carry a provider. A launch without `--turn` does not read or parse earlier rows (the torn-tail check of
the append step still runs) and prints the same stdout line as without it.

`--turn <name>[,<name>]` (the list form of `--providers`) asks for a turn check on a builder launch, and
`--turn-reason <text>` (1 to 200 bytes, no control character, not only blanks) lets a launch out of turn go on; see step 3c.
`--turn` needs `--provider`, the provider must be in the list, the name must have the role `exec` and be a round 1 name;
`--turn-reason` needs `--turn`. Each refusal is exit 2 before any write.

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
3c. Turn check, for a launch given `--turn`; the same lock and handle as step 4. The launcher takes the exclusive lock
   on the records file, reads the whole file through that handle (a file that did not exist is empty), computes the next
   provider as `next-provider` does for the `--turn` list, and appends the `launch` row through the same handle before the
   lock is let go. No second lock is taken, so with two names, two launches at once cannot both take the turn. The launch goes on
   when the provider equals the next provider, or when `--turn-reason` is given. Otherwise it exits 7 with no row written
   and nothing started; stderr starts with `launch refused: turn:` and names the case: `provider <p> is not the turn, next
   is <q>`, `the last counted launch <run id> carries no provider`, or `the last counted launch <run id> carries provider
   <x>, not in --turn`. With a reason, those last two cases go on and write no `turn_next`. Records that cannot be read
   (torn tail, corrupt line, over the read cap) are exit 2 with no row written. After exit 7 an existing file is
   byte-unchanged; a file that did not exist may be left as an empty file. `--turn` takes round 1 names only: a name
   with `r2` or more is exit 2 (a fix round goes to a provider typed by hand). Without `--turn` the launch does not read or
   parse earlier rows.
4. Write the `launch` row first. Any failure exits 2 with `launch refused: launch record not written: <cause>`
   and the host is never run.
5. Start `<host> --bg -n <name> <brief>`: the brief is ONE argv element, byte-identical to the file, run from the
   canonical `--cwd`, in its own process group, with a bounded wait (`--start-timeout-ms`, default 30000).
6. Map the short id the host printed to the full uuid by polling `<host> agents --json --all` (default bound
   10000 ms, tunable with `--map-timeout-ms`, every 250 ms) for exactly one entry with that short id and the same
   name. Append a `launch_ids` row with the same `run_id`. A record never carries a uuid the host did not print.
7. Print `launched run_id=.. name=.. role=.. short_id=.. session_id=..`.

## Host output shape

The short id is read from the `--bg` stdout by two rules. `agents --json --all` is a JSON array of objects with
`id` (short id), `sessionId` (full uuid) and `name`. All of this lives in one place in `launcher.rs`
(`parse_short_id` and the `AGENT_KEY_*` constants).

1. If a stdout line starts with `backgrounded` (leading whitespace ignored), the first such line carries the id:
   `backgrounded · <8 lowercase hex> · <name>`, fields separated by the middle dot U+00B7. The id is the second
   field and must be exactly 8 lowercase hex characters; other lines, before or after it, are ignored. If that
   line has no such id, the launcher reads no short id: exit 3, a `launch_failed` row with `stage=start`, and no
   `stop` call to the host.
2. With no `backgrounded` line, the id is the first whitespace token of the last non-empty stdout line. A host
   that prints further lines after the id line, without a `backgrounded` line, is read by its last line: if that
   line's first token is not token-shaped (letters, digits, `-` and `_` only, at most 64 characters, no leading
   `-`) the launcher reads no short id (exit 3, no `stop` call); otherwise it gets a token it cannot map (exit 4
   or 5, see the exit-code table below) and the stop targets that token. In both cases check `<host> agents` and
   stop the real session by hand.

## Rows

All rows are one compact JSON line in an append-only file, taken under an exclusive lock; a torn tail refuses the write.

- `{"v":1,"kind":"launch","ts_ms":..,"run_id":..,"name":..,"role":..,"brief_sha256":..,"owner":"orchestration"}`
- `{"v":1,"kind":"launch_refused","ts_ms":..,"run_id":..,"name":..,"role":..,"brief_sha256":..,"matched_entry":..}` (a signer brief named builder state; `matched_entry` is the entry NAME, never its text; the row stands alone)
- `{"v":1,"kind":"launch_ids","ts_ms":..,"run_id":..,"short_id":..,"session_id":..}`
- `{"v":1,"kind":"launch_failed","ts_ms":..,"run_id":..,"stage":"start|map","reason":..}` (best effort)

`harness session launches` joins rows by `run_id` and prints one line per launch with `state=bound|failed|pending|refused`.
A corrupt line stops the read (exit 2, the line number named). `launcher::read_launches` is the same join as a function.

A `--sign-off` launch writes `"sign_off":true` after `owner` in its `launch` row, before any `provider` field (a launch
without the flag writes the row it wrote before the flag). `launches` adds ` sign_off=yes` to that launch's line only. Any other `sign_off` value in a row
(`false`, a string, a number) is a corrupt line. `harness session sign-off-check` reads the mark; see `docs/signoff.md`.

A launch given `--provider` writes `"provider":"<name>"` after the fields of the plain row (after `"sign_off":true` when
that is present); a launch without it writes the row it wrote before the flag. In a launch row, a `provider` field that is not a
string in the provider name form, or `provider` written twice in one row, is a corrupt line (other row kinds do not read the field): `session launches`,
`session sign-off-check` and `next-provider` refuse the file, and `lane stale` refuses it on its first read.
`launches` appends ` provider=<name>` to the line of a launch that carries it, after `sign_off=yes` when present.

A launch that goes on with `--turn` writes `"turn_next":"<name>"` after `provider`, when a next provider could be
computed, and `"turn_reason":"<text>"` after that, when `--turn-reason` was given (also when the provider equals the
turn). A launch without `--turn` writes the same row bytes as before the flag. In a launch row, a `turn_next` that is not a
string in the provider name form, a `turn_reason` that is not a string, either one written twice, or either one in a row with
no `provider`, is a corrupt line, with the same effect as a bad `provider`. `launches` appends ` turn_next=<name>` after
`provider=<name>` to the line of a launch that carries it; `turn_reason` is not printed.

## Next provider (`harness session next-provider`)

```
harness session next-provider --providers <name>[,<name>] [--records <file>]
```

The `--providers` value is one name or two different names joined by one comma, each in the provider name form. The default records
path is the one of `session launches`. The verb reads and reports; it writes no file and logs no event. It does not
reserve a turn: two callers at once get the same answer.

Output, exit 0, one line: `next_provider=<name> last_provider=<name|-> last_run_id=<run id|-> basis=<turn|first|single>`.

- `basis=turn`: the last counted launch carries a provider in the list; the other name is printed.
- `basis=first`: no launch counts (an empty file, or no row that counts); the first name is printed, the last parts are `-`.
- `basis=single`: the list holds one name; it is printed whatever the last provider was (the file is still read).
  `last_provider` is `-` when the last counted launch has none.

Counted launches: role `exec`, state `bound` or `pending`, and a name that reads back as round 1 (a name with no
round token counts as round 1; a name that does not read back does not count). `failed` and `refused` launches,
other roles and fix rounds (`r2` or more) do not count. The last one in file order counts, not by `ts_ms`.
A pending row left by a killed launcher moves the turn once.
`session launch --turn` applies this rule to a launch; see step 3c.

| code | meaning |
|---|---|
| 0 | printed |
| 2 | usage: no `--providers`, a flag twice, a missing value, an unknown argument, a bad list |
| 3 | the records cannot be used: `reason=unreadable` and the cause (missing file, over the read cap, torn last line, corrupt line) |
| 4 | two names, the last counted launch has no provider: `reason=no-provider run_id=<id>` |
| 5 | two names, the last counted launch has a provider not in the list: `reason=unknown-provider provider=<name> run_id=<id>` |

On exit 2 to 5 stdout is empty.

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
| 7 | a `--turn` launch was refused as out of turn with no `--turn-reason` given (step 3c; no row is written, nothing started) |

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
