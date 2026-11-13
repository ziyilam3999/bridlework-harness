# Sign-off id check (P1B-20, S-1.5 part 5)

```
harness session sign-off-check --ticket <t> --role <gov|e-rvw> --session <uuid-or-short-id> --claim <verdict|done>
    [--records <file>] [--manifest-dir <dir>] [--log <path>] [--root <dir>]
```

A sign-off is accepted only when the signer's session id matches a bound launch-record row for that ticket and role,
and is not the id of any build or worker session of that ticket or of the interactive session. The check applies from
the P1b cut-over until the P7 gate takes over; callers run it before recording a review verdict or a Done claim.

Defaults: `--records` is `.harness/launch-records.jsonl`; `--manifest-dir` is `.harness/manifests`.

## Which log

One resolved event log is both read (for hook-source `session_started` rows) and written (the `unmanaged_session`
append). The order is the SessionStart hook's: `--log`; else `HARNESS_EVENT_LOG`, which must be an absolute path
(otherwise exit 2, nothing read or written); else `.harness/events.jsonl` under the working folder. `--root` defaults
to the log's parent.

When the logs differ: if `HARNESS_EVENT_LOG` is set and no `--log` is given, the `unmanaged_session` row goes to the
`HARNESS_EVENT_LOG` file, NOT to the working folder's `.harness/events.jsonl`. `harness process claim` writes its own
`unmanaged_session` rows (`not_supervisor_launched`) to its `--log` or the default, so the two kinds of row can land in
different files. A reader of `unmanaged_session` rows must read both logs.

## The ticket token

`--ticket` must be the ticket slug as the session name reads it back: the part after `t` in the name's `t<slug>`
segment, for example `3704` for `...-t3704-...`. Supervisor workers must be started with `process start --ticket <the
same slug>`. A worker manifest filed under any other token is not found, so it counts as no workers and its build ids
are not checked.

## Steps

The first refusal stops the check.

- (a) Validate; exit 2 on any failure, nothing written. `--ticket` a plain token, `--role` `gov` or `e-rvw`, `--claim`
  `verdict` or `done`, `--session` a full lowercase uuid or a short-id token of at most 64 bytes, each flag at most once.
- (b) Read the launch records (missing or corrupt: exit 2), the ticket's manifest (missing means no workers; damaged:
  exit 2) and the event log (missing means no hook sessions; corrupt or torn: exit 2).
- (c) Resolve a short id against every bound launch. No match: refuse `no-launch-record`, no event written. Two
  different uuids: exit 2.
- (d) Refuse `build-id` if the uuid is the session id of a bound launch of this ticket in a build role (`plan`, `exec`,
  `ship`, `push`) or of any worker `session` line in the ticket's manifest. Refuse `orchestration-id` if it is any
  `session_started source=hook` id in the log. This runs before the signer's own row is looked up.
- (e) Find the signer's bound launch row for this ticket. None: refuse `no-launch-record` and append one
  `unmanaged_session` (`reason=no_launch_record`) to the resolved log; if that append fails: exit 2. A row of another
  role: `role-mismatch`. An `e-rvw` Done claim whose launch lacked `--sign-off`: `not-marked-sign-off` (`gov` needs no
  mark; an unmarked `e-rvw` verdict is accepted).
- (f) Accept: exit 0, one line `sign-off=accepted ticket=<t> role=<r> claim=<c> run_id=<run> session_id=<uuid>`.

## Exit codes and reasons

- 0 accepted. 1 refused: exactly one stdout line `sign-off=refused ticket=<t> reason=<token>: <sentence>` and nothing on
  stderr. 2 cannot check (usage, unreadable input, failed append); no `sign-off=` line.
- Reason tokens: `build-id`, `orchestration-id`, `no-launch-record`, `role-mismatch`, `not-marked-sign-off`.
- A refusal line never echoes a build, worker or hook uuid.
