# Process supervisor

The process supervisor starts a ticket's long-running programs — app servers, browser and test
processes — under **ticket ownership**: a started process and its port are signed out on the ticket's
[resource manifest](resource-manifest.md) before `start` answers, and a start that cannot record its
spawn is rolled back, so a refused start leaves nothing running and nothing listed.

**Stopping a recorded process is not part of this PR** (follow-up ticket #183, stop-by-pid): nothing
here reads, signals or removes a process that a `start` of this run did not create. See
["Not in this PR"](#not-in-this-pr-stop-by-pid) below.

Code: `core/rust/harness/src/supervisor.rs` (unix only) and the `harness process` command in
`core/rust/harness/src/cli.rs`. Nothing new in the manifest format: one existing `add` process line and, with
`--port`, one existing `add` port line — the manifest line IS the durable record, no event kind is added.

## The command

```
harness process start <ticket> --id <id> --cwd <dir> [--port <n>] [--dir <manifest-dir>] -- <program> [args...]
```

`--dir` defaults to `.harness/manifests`, the same default as `harness manifest`. Listing stays
`harness manifest list` (it already prints `id, class, pid, pid_start_ms`); no new list verb exists.
`process stop` is not a verb: any `stop` invocation is the usage refusal (exit 2), and nothing is
signalled, written or removed by it.

## start, in order

1. Validate `--port` in the parser: an integer in 1..=65535, else **exit 2** with nothing run — this runs BEFORE
   the lock, the manifest read, the bind check or the spawn.
2. Take the per-ticket supervisor lock (`<manifest-dir>/<ticket>.proc.lock`, the std exclusive file lock the
   manifest already uses) and hold it for the whole operation.
3. Read the manifest. Missing, torn or corrupt: **exit 2**, nothing started.
4. Refuse (**exit 2**, nothing started) when `--id` — or `<id>.port`, if `--port` is given — is already live,
   when `--cwd` does not resolve inside one of the ticket's roots, when `--port` is already live in the manifest,
   or when `--port` is already bound on 127.0.0.1 (checked with a bind-then-release snapshot). Only `AddrInUse`
   is reported as "already bound": a bind refused for privilege names the error kind instead — ports below 1024
   need privilege for the bind check even on 127.0.0.1 (macOS/BSD give `PermissionDenied`, Linux `EPERM`).
5. Spawn the program in its OWN process group (group id = its pid), stdin closed, stdout and stderr discarded
   (capture is P1B-08), and `PORT=<n>` in its environment when `--port` is given.
6. Read the pid's start time, append the process line, then the port line. If either read or append fails, the
   whole group this start just made is stopped and **exit 2**; a failure AFTER the process line (the port
   append) also appends `remove` for that process line, so a refused start leaves nothing running and nothing
   listed. A rollback that cannot CONFIRM its kill (the kill helper not runnable, the child not provably dead)
   says so in the refusal message instead of claiming success.
7. Print `id<TAB>pid<TAB>pid_start_ms`, exit 0.

## Exit codes

| Case | stdout | exit |
|---|---|---|
| started | `id<TAB>pid<TAB>pid_start_ms` | 0 |
| usage error or any refusal (out-of-range `--port`, bad ticket or id, missing/torn/corrupt manifest, id or port already live, cwd outside every root, port already bound, unreadable start time, refused append) | — (stderr names it) | 2 |

There is exactly one nuance inside exit 2: a refusal that happens AFTER the spawn (step 6) has already
stopped the group that spawn made — such a `2` means "nothing listed and nothing left running", not
"nothing was ever started". When the rollback itself could not confirm the kill, the message names that
too, so no refusal is ever read as a success-shaped answer.

## The rollback

The only signal path in this PR is the start rollback, and it touches only the group the `start` of this
very run created: TERM, a bounded wait (5 s), then KILL to the group, all through `/bin/kill` by
ABSOLUTE path (never through `PATH`). Whatever happened to the group, std `Child::kill()` — which needs
no `PATH` — ends the direct child before a bounded reap (3 s), so a rollback can never block forever on a
child it could not signal. A helper that cannot be RUN is "unknown", never "gone" or "empty": the group
waits are skipped and the refusal names the unconfirmed kill.

## What this does not cover

- **Start-to-record window.** A hard crash between spawn and the manifest append leaves a process the sheet does
  not list; a failed append kills the group, but a hard crash does not. The P2 orphan reaper owns that case.
- **Port check is a snapshot, and 127.0.0.1 only.** "Free at start" can change before the child binds; the
  supervisor records the declared port and does not prove the child listens on it. A port already bound on another
  address (for example `0.0.0.0`) but free on 127.0.0.1 passes the check.
- **The lock file.** A refusal never writes the MANIFEST. Only a start that gets past the parser and the token
  checks takes the per-ticket lock file `<manifest-dir>/<ticket>.proc.lock` (step 2 of `start`); a parser or
  token refusal (bad `--port`, bad ticket or id) exits before the lock, so no lock file is made either. It is a
  std file-lock helper, not a record — it holds no data and is safe to delete between commands.
- **No output capture** (stdout, stderr, exit codes, timing, traces: P1B-08), **no timeouts or step limits**
  (P1B-06), **no worker-death record** (P1B-07), **no orphan reaping** (P2), **no network allow-list** (G1b/P4a),
  **no finalize/teardown** (P1B-14).

## Not in this PR: stop-by-pid

Stopping a recorded process was split into its own follow-up ticket (#183, stop-by-pid) after review found
the round-3 `stop` could answer success without proving the process died. This PR keeps only the start and
record behaviour that passed review; it never signals or removes a process its own `start` did not create.
The follow-up must handle at least:

- **a non-leader pid** listed through the public `harness manifest add --class process` verb: a group signal
  to `-<pid>` hits no group when the pid is not its own group leader, so "stopped" would be a lie while the
  process lives (r3 finding 1);
- **`kill` / `ps` not runnable**: signal and probe by absolute path (as the rollback here does), and treat a
  helper that cannot be run as "unknown" — never as "gone", "empty" or "already dead" — refusing with exit 2
  and keeping the entry (r3 finding 2, all three sites);
- **a post-signal liveness check**: answer 0 "stopped" and append the `remove` lines only after the recorded
  pid is provably dead (`kill -0` failing for the pid itself, not just the group).
