# `harness session done-check` and `harness session close` (P1B-17)

Read the host's session listing (`<host> agents --json --all`, a JSON array of entries with string `id` and `name`, and optionally `state`),
work out each session's ticket from its NAME, and compare it with a done set. The ticket comes from `session_name::check` (the P1B-16 predicate)
with the config's `lanes.count`. The S-1.6 slug of a ticket id is the id lower-cased with every character outside `[a-z0-9]` dropped; an empty slug is refused.

```
harness session done-check --host <abs> --config <file> --done <ticket-id> [--done <ticket-id> ...]
harness session close      --host <abs> --config <file> --ticket <ticket-id>
```

`--host` is an absolute path to an existing regular file (no PATH lookup). `--config` falls back to `HARNESS_CONFIG`, as `session check-name` does.
Every argument passed to the host is a validated token (an id of at most 64 characters from `[A-Za-z0-9_-]`, not starting with `-`).
At most 64 `--done` values and 10000 listed entries are read; more is refused.

## `done-check` (S-1.6.1, read-only)

Never calls `stop` or `rm`.

- Each listed session whose ticket equals a `--done` slug: `session=open name=<n> id=<id> ticket=<slug>`, then `done-check=fail open=<k>`, exit 1.
- No such session: `done-check=pass open=0`, exit 0.
- A hyphenated read-back that equals a done slug once its `-` are removed is `unresolved`: `session=unresolved name=<n> id=<id> ticket=<read-back>`.
  With no open session, `done-check=cannot-check unresolved=<k>` and exit 3; it never prints `pass` while one is listed. An open session still wins (exit 1).
- A listing it cannot read (non-zero exit, timeout, non-JSON, not an array, an entry without string `id` and `name`, a bad `id`): exit 3.
- Usage or config error: exit 2.

## `close`

For each listed session whose ticket equals the slug: run `stop <id>` unless its `state` is `done`, then always `rm <id>`. A step passes only on exit 0.

- `closed name=<n> id=<id>` per removed session, then `close=pass ticket=<slug> closed=<k>`, exit 0.
- The first failing step: `close=fail name=<n> id=<id> step=<stop|rm>`, exit 1; the remaining matches are not tried.
- An unresolved entry gets no host call: `unresolved name=<n> id=<id>`; the exit code is unchanged (finalize's re-read refuses).
- Names the predicate refuses and names with no ticket (`Orchestrator`, `D16`) are never matched and never touched.
- Listing error: exit 3. Usage error: exit 2.

`harness ticket finalize` runs `close` and then `done-check` as its `sessions` step (see `docs/finalize.md`).

## Limits

- Host verbs are tested against stub hosts only until the S-1.6 host re-probe (`claude agents --json --all`, `claude rm`, `claude stop`).
- A hyphenated long-form read-back that de-hyphenates to a done slug is reported `unresolved` (exit 3) and must be removed by hand: the legacy long form
  cannot tell `q1-1` from `q11` (the spec's known gap).
- The standing pusher `governance-l4-pusher<n>` reads back ticket `pusher<n>`, so a ticket whose slug is `pusher<n>` would match and remove it.
- Finalize must not run inside one of the ticket's own sessions: it would end its own session partway through. This is documented only, not enforced.
- The done set inside finalize is exactly the ticket being finished; Done is not stored.
