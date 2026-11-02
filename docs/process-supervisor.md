# Process supervisor

The process supervisor starts a ticket's long-running programs — app servers, browser and test
processes — under **ticket ownership**: a started process and its port are signed out on the ticket's
[resource manifest](resource-manifest.md) before `start` answers, and a start that cannot record its
spawn is rolled back, so a refused start leaves nothing running and nothing listed.

**Stopping a recorded process** is `harness process stop` (#183): it signals a pid only when the sheet
proves the ticket still owns that exact process, and it answers "stopped" only after the death is proven.
See ["stop, in order"](#stop-in-order) below.

**Honest limits** (#207) add two one-shot alarms on top, with NO always-on watcher: `process start
--timeout-ms` records a `limit` line on the manifest, `process check` stops anything past its time through
the same stop checks, and `process step` enforces a soft check-in line and a hard stop line on the step
count the driver reports. See [check](#check) and [step](#step) below.

Code: `core/rust/harness/src/supervisor.rs` (unix only) and the `harness process` command in
`core/rust/harness/src/cli.rs`. Nothing new in the manifest format: one existing `add` process line and, with
`--port`, one existing `add` port line — the manifest line IS the durable record, no event kind is added.

## The command

```
harness process start <ticket> --id <id> --cwd <dir> [--port <n>] [--timeout-ms <n>] [--dir <manifest-dir>] -- <program> [args...]
harness process stop <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>]
harness process check <ticket> [--grace-ms <n>] [--dir <manifest-dir>]
harness process step <ticket> --repo <r> --steps <s> --soft <a> --hard <b> [--grace-ms <n>] [--dir <manifest-dir>] [--log <path>] [--root <dir>]
```

`--dir` defaults to `.harness/manifests`, the same default as `harness manifest`. Listing stays
`harness manifest list` (it already prints `id, class, pid, pid_start_ms`); no new list verb exists.
`process stop` addresses one entry by `--id`, or by a `--pid` matched against the ticket's live process
entries.

## start, in order

1. Validate `--port` in the parser: an integer in 1..=65535, else **exit 2** with nothing run — this runs BEFORE
   the lock, the manifest read, the bind check or the spawn. `--timeout-ms` is validated in the same place:
   an integer in 1..=604800000 (7 days), else **exit 2** with nothing run, no lock file, nothing spawned.
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
6. Read the pid's start time, append the process line, then — when `--timeout-ms` was given — one `limit`
   line for that id, then the port line. If any read or append fails, the whole group this start just made
   is stopped and **exit 2**; a failure AFTER the process line (the limit or port append) also appends
   `remove` for that process line (a remove ends the limit too), so a refused start leaves nothing running
   and nothing listed. A rollback that cannot CONFIRM its kill (the kill helper not runnable, the child not
   provably dead) says so in the refusal message instead of claiming success.
7. Print `id<TAB>pid<TAB>pid_start_ms`, exit 0.

## Exit codes

| Case | stdout | exit |
|---|---|---|
| started | `id<TAB>pid<TAB>pid_start_ms` | 0 |
| usage error or any refusal (out-of-range `--port` or `--timeout-ms`, bad ticket or id, missing/torn/corrupt manifest, id or port already live, cwd outside every root, port already bound, unreadable start time, refused append) | — (stderr names it) | 2 |

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
- **No output capture** (stdout, stderr, exit codes, timing, traces: P1B-08), **no worker-death record**
  (P1B-07 — the `timed_out` event with its continuation recipe), **no network allow-list** (G1b/P4a).

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

## check

`process check` (#207) is ONE-SHOT: whoever drives the ticket runs it before every step and at finalize.
The bound is therefore "stopped at the first check at or after the deadline", never "stopped at the
deadline". It holds the per-ticket supervisor lock for the whole command, reads the manifest (missing,
torn or corrupt: **exit 2**, nothing signalled, byte-identical), then visits EVERY live process entry
that carries a `limit` line:

1. **The expiry decision, fail closed.** The entry's age is the OS's own age of the process — on Linux
   `/proc/uptime` minus the pid's `starttime` (both boot-relative, so a wall-clock jump cannot move it);
   elsewhere `/bin/ps -o etime=` (whole seconds, wall-based), read by absolute path. Expired means
   `age >= timeout_ms`, measured from the process's OS start (the same instant `pid_start_ms` records), so
   restarting the harness cannot reset the timer. An age that cannot be READ, or a wall clock that reads
   BEFORE the `limit` line's own `ts_ms` (a detectable backward jump), REFUSES that entry — nothing
   signalled, entry kept — and check never answers "fine" when it does not know.
2. **Enforcement through the same stop checks.** An expired entry is stopped through stop's checks 2-7
   under the lock check already holds, so a clock jump can change WHEN a process is stopped, never WHOM.

### check exit codes

Precedence `2 > 4 > 5 > 0`; every entry is still visited, so a refusal on one entry never hides an expiry
on another.

| Case | output | exit |
|---|---|---|
| no entry expired | — | 0 |
| at least one expired, every expired one resolved (stopped, already-gone or pid-reused) | stdout `id<TAB>pid<TAB>timed_out` per stopped entry; already-gone / pid-reused / not-confirmed reuse stop's existing stderr lines verbatim | 5 |
| an expired one was signalled but its death was not confirmed (entry kept) | stop's stderr line verbatim | 4 |
| refusal: usage, unclean manifest, unreadable age (`harness: error: <id>: cannot read the age of pid <pid>: <why>; nothing signalled`), or a clock before the limit line (`harness: error: <id>: the clock reads before the recorded limit; nothing signalled`) | — (stderr names it) | 2 |

`timed_out` is NOT written as an event by this ticket: its required continuation recipe
(`checkpoint`, `handover`, `session_id`, `recipe`) is P1B-07's, and inventing it would be a lie. The
durable record of a timeout is the manifest (the `limit` line, then the `remove` lines), the stdout line
and exit 5, which P1B-07 consumes.

## step

`process step <ticket> --repo <r> --steps <s> --soft <a> --hard <b>` (#207) is STATELESS on purpose: the
driver owns the count (an honest limit — a driver that lies about `--steps` is not caught). Every input is
validated in the parser before the lock, the manifest read or any event write — `1 <= a < b <= 1000000`,
numeric `--steps`, a plain-token `--repo` — so a usage error leaves the log and the manifest untouched.
`--log`/`--root` follow the `events append` defaults (`.harness/events.jsonl` with root `.`). The command
holds the per-ticket lock for its whole run and reads the manifest first (unclean: **exit 2**, nothing
written). The contract for the driver: ONLY exit 0 means "take another step".

| Case | output | exit |
|---|---|---|
| `s < a` | — | 0 |
| `a <= s < b`: one `check_in` event (ticket, repo, steps, soft_limit) is appended per call, the job continues | stdout `check_in<TAB>s<TAB>a` | 0 |
| `s >= b`: the `step_limit_stop` event is appended FIRST (a hard stop is never silent), then every live process entry of the ticket is stopped through stop's checks 2-7; all proven dead or already resolved (or none live) | stdout starts `step_limit_stop<TAB>s<TAB>b`, then stop's `id<TAB>pid<TAB>stopped` per stopped entry | 6 |
| `s >= b`, any death not confirmed (entry kept) | as above plus stop's stderr line | 4 |
| the event could not be written (log outside the root, torn tail, unwritable) | stderr `harness: error: ...` + `harness: EVENT NOT RECORDED`; NOTHING signalled, the manifest byte-identical | 2 |
| refusal (usage, bad limits, unclean manifest) | — (stderr names it) | 2 |

### What check and step do not cover

- **Nobody runs check.** There is no daemon: a process overruns until the next check the driver runs. The
  bound is "stopped at the first check at or after the deadline", never "stopped at the deadline".
- **A lying driver.** `--steps` is trusted; the harness does not count steps itself.
- **A forward wall-clock jump off Linux** can stop a process early (the off-Linux age is wall-based `ps
  etime`); it is still identity-safe — every stop passes the identity and leadership checks, so a jump
  changes WHEN, never WHOM.
- **Start-to-record crash** (the pre-existing honest limit): a hard crash between the spawn and the
  manifest append leaves a process the sheet does not list, so it carries no limit either.
- **No `timed_out` event** (P1B-07): exit 5 plus the manifest lines are the record until then.
- **Existing hazard, noted not fixed (Linux):** a wall-clock jump shifts `/proc/stat` `btime`, so stop's
  exact identity check may read a jumped clock as `pid-reused` and remove the entry without a signal — it
  fails toward "not signalled", never toward a stranger.
