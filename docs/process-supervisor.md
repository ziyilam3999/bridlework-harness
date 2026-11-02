# Process supervisor

The process supervisor starts a ticket's long-running programs — app servers, browser and test
processes — under **ticket ownership**: a started process and its port are signed out on the ticket's
[resource manifest](resource-manifest.md) before `start` answers, and a start that cannot record its
spawn is rolled back, so a refused start leaves nothing running and nothing listed.

**Stopping a recorded process** is `harness process stop` (#183): it signals a pid only when the sheet
proves the ticket still owns that exact process, and it answers "stopped" only after the death is proven.
See ["stop, in order"](#stop-in-order) below.

Code: `core/rust/harness/src/supervisor.rs` (unix only) and the `harness process` command in
`core/rust/harness/src/cli.rs`. Nothing new in the manifest format: one existing `add` process line and, with
`--port`, one existing `add` port line — the manifest line IS the durable record, no event kind is added.

## The command

```
harness process start <ticket> --id <id> --cwd <dir> [--port <n>] [--dir <manifest-dir>] -- <program> [args...]
harness process stop <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>]
```

`--dir` defaults to `.harness/manifests`, the same default as `harness manifest`. Listing stays
`harness manifest list` (it already prints `id, class, pid, pid_start_ms`); no new list verb exists.
`process stop` addresses one entry by `--id`, or by a `--pid` matched against the ticket's live process
entries.

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
- **No orphan reaping for stop escapees** (P2) and **no finalize/teardown that calls stop for every live
  process of a ticket** (P1B-14).
- **No output capture** (stdout, stderr, exit codes, timing, traces: P1B-08), **no timeouts or step limits**
  (P1B-06), **no worker-death record** (P1B-07), **no network allow-list** (G1b/P4a).

## stop, in order

`process stop` holds the same per-ticket supervisor lock for the WHOLE stop, then checks, in order — a stop
SIGNALS only when every check passes:

1. Read the manifest; missing, torn or corrupt: **exit 2**, nothing signalled, the file byte-identical.
2. The target must be a LIVE `process` entry of THIS ticket (by `--id`, or a `--pid` matched against the
   live entries). A pid not listed here — including one listed only on ANOTHER ticket — is **exit 2**
   (`... is not a live entry of ticket ...`), nothing signalled, the manifest byte-identical.
3. Liveness, asked through `/bin/kill` by absolute path. Provably no such process: **exit 3**
   `already-gone`, nothing signalled, the entries removed. A helper that could not RUN is "unknown" —
   never "gone": **exit 2**, entry kept, nothing written.
4. Identity: the live pid's start time must equal the recorded `pid_start_ms` (exact on Linux, within
   2000 ms elsewhere — the [resource manifest](resource-manifest.md) tolerance rule). Mismatch means the
   recorded process exited and the number was re-used: **exit 3** `pid-reused`, nothing signalled, the
   entries removed. Unreadable: **exit 2**, entry kept.
5. Group leadership: the live pid must be its own process-group leader (pgid == pid), so a signal to the
   group provably ends the family it heads. A non-leader pid (for example one signed out through the
   public `harness manifest add --class process`) is **exit 2**, nothing signalled, entry kept — the
   message says a group signal could not prove it ends this process.
6. TERM to the whole group, then a bounded wait (`--grace-ms`, default 5000). The KILL gate, before any
   KILL goes: a group already provably empty gets none; a leader still alive must still pass check 4; and
   a leader that is PROVABLY dead with the group still non-empty still gets the KILL — while any member
   lives, the group id cannot be re-issued as a pid, so that KILL cannot hit a stranger either. (Before
   fix r2 this last case got no KILL: the leader's own TERM had killed it, and the gate demanded a live
   leader's identity — so a member that trapped TERM survived a "stopped" answer.)
7. Post-signal proof: the answer 0 needs the WHOLE GROUP provably empty (`kill -0 -- -<pid>` failing) AND
   the recorded pid PROVABLY dead (`kill -0 <pid>` failing) — the leader alone dying proves nothing about
   a member that ignored TERM — before the `remove` lines (the process entry, and `<id>.port` when live).
   A survivor, a leader still answering — an unreaped zombie does — or a probe that could not run:
   **exit 4** "signalled, death not confirmed", the entry KEPT, so a later stop can retry (and a retry
   after a reap answers `already-gone`).

### stop exit codes

| Case | stdout | exit |
|---|---|---|
| stopped and removed | `id<TAB>pid<TAB>stopped` | 0 |
| refusal (dirty manifest, not a live entry of this ticket, unknown liveness, unreadable identity, non-leader pid) | — (stderr names it) | 2 |
| `already-gone` / `pid-reused` (nothing signalled, entries removed) | — (stderr names it) | 3 |
| signalled, death not confirmed (entry kept so a later stop can retry) | — (stderr names it) | 4 |

A pid re-used by another user's process answers the liveness probe "no permission", read as not ours:
**exit 2**, nothing signalled — the safe direction. The identity read itself runs `/bin/ps` by ABSOLUTE
path (never through `PATH`), so a PATH-shadowed `ps` cannot bless a re-used pid.

## What stop does not cover

- **Check-to-signal race.** Between the identity check and TERM the process could exit and the pid be
  re-used. Closing that needs pidfds (Linux only) or a crate; std only here. Shrunk (the identity is
  re-checked before KILL), not closed.
- **Start-time precision off Linux.** `ps etime` is whole seconds; with the 2000 ms tolerance a pid
  re-used within ~2 s of the original start could pass. Linux is exact.
- **A non-leader entry can never be stopped by this verb** (always exit 2): an honest refusal over a false
  "stopped"; the message says why. The orphan reaper (P2) owns what a group signal cannot reach.
- **Children that leave the group** (`setsid`) and a group whose leader already exited BEFORE the stop
  began (`already-gone`: the entry is stale and is removed with nothing signalled; the orphan reaper P2
  owns leaderless groups) are not stopped. A leader the stop's OWN TERM ended is different — that group
  is still provably ours and gets the KILL (check 6).
- **`kill -0` EPERM** names a pid of another user: read as not ours, exit 2, no signal.
