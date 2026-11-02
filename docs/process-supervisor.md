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
harness process stop <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>] [--session <uuid> --checkpoint <sha> --repo <token> [--log <path>] [--root <dir>]]
harness process check <ticket> [--grace-ms <n>] [--dir <manifest-dir>] [--session <uuid> --checkpoint <sha> --repo <token> [--log <path>] [--root <dir>]]
harness process step <ticket> --repo <r> --steps <s> --soft <a> --hard <b> [--grace-ms <n>] [--dir <manifest-dir>] [--log <path>] [--root <dir>] [--session <uuid> --checkpoint <sha>]
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
- **No output capture** (stdout, stderr, exit codes, timing, traces: P1B-08) and **no network allow-list**
  (G1b/P4a). The worker-death record (the `timed_out` event with its continuation recipe, P1B-07) now
  EXISTS and has its own section, [Death record](#death-record), below.

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

1. **Liveness first.** Every visited entry is probed for liveness BEFORE anything about it is read (the
   age of a GONE pid cannot be read either: `ps` does not know it, `/proc/<pid>/stat` is gone). A
   provably-gone pid — most commonly a program that finished on its own inside its limit — resolves
   through the SAME already-gone tail stop uses (stop's stderr line verbatim, the entries removed,
   counted under exit 5), never a refusal. A pid that answers the probe but is not ours to signal
   (`kill -0` EPERM), or a probe that cannot run, is a fail-closed refusal.
2. **The expiry decision, fail closed.** Only for a proven-live pid: its age is the OS's own age of the
   process — on Linux `/proc/uptime` minus the pid's `starttime` (both boot-relative, so a wall-clock
   jump cannot move it; an odd uptime reading — below the pid's own `starttime`, NaN, infinite or
   negative — REFUSES, it never saturates to an age of 0); elsewhere `/bin/ps -o etime=` (whole seconds,
   wall-based), read by absolute path. Expired means `age >= timeout_ms`, measured from the process's OS
   start (the same instant `pid_start_ms` records), so restarting the harness cannot reset the timer. An
   age that cannot be READ, or a wall clock that reads BEFORE the `limit` line's own `ts_ms` (a
   detectable backward jump), REFUSES that entry — nothing signalled, entry kept — and check never
   answers "fine" when it does not know.
3. **Enforcement through the same stop checks.** An expired entry is stopped through stop's checks 2-7
   under the lock check already holds, so a clock jump can change WHEN a process is stopped, never WHOM.

### check exit codes

Precedence `2 > 4 > 5 > 0`; every entry is still visited, so a refusal on one entry never hides an expiry
on another.

| Case | output | exit |
|---|---|---|
| no entry expired or resolved | — | 0 |
| at least one visited entry resolved: an expired one stopped, or any visited entry already-gone / pid-reused | stdout `id<TAB>pid<TAB>timed_out` per stopped entry; already-gone / pid-reused / not-confirmed reuse stop's existing stderr lines verbatim | 5 |
| an expired one was signalled but its death was not confirmed (entry kept) | stop's stderr line verbatim | 4 |
| refusal: usage, unclean manifest, unreadable age of a LIVE pid (`harness: error: <id>: cannot read the age of pid <pid>: <why>; nothing signalled`), a clock before the limit line (`harness: error: <id>: the clock reads before the recorded limit; nothing signalled`), a pid that is not ours to signal, or a liveness probe that cannot run | — (stderr names it) | 2 |

`timed_out` IS written as an event when check runs in RECORD MODE (see [Death record](#death-record),
#210): with `--session`/`--checkpoint`/`--repo` (and optional `--log`/`--root`) every death check
resolves — an expired entry stopped, or any visited entry already-gone or pid-reused — leaves a handover
file and one `timed_out` line, plus one `recorded<TAB>id<TAB>cause<TAB>handover` stdout line per death
this run recorded. Without the record flags nothing is written beyond today's manifest lines, stdout
line and exit 5.

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
| a stop REFUSED after the event was written (non-leader pid, missing `/bin/kill`, unreadable start time) | the `step_limit_stop` event IS on disk and the header line was printed; that entry is kept, nothing was signalled for it, and the command still answers 2 — the event says the hard line was reached, the exit says a stop could not be proven | 2 |
| refusal (usage, bad limits, unclean manifest) | — (stderr names it) | 2 |
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
- **A driver that never passes the record flags** gets no death record (see [Death record](#death-record)):
  legacy mode still resolves a gone entry with no `timed_out` line — today's behaviour, kept for
  compatibility. The P1B-14/15 driver must pass them.
- **Existing hazard, noted not fixed (Linux):** a wall-clock jump shifts `/proc/stat` `btime`, so stop's
  exact identity check may read a jumped clock as `pid-reused` and remove the entry without a signal — it
  fails toward "not signalled", never toward a stranger.

## Death record

`process check`, `process stop` and `process step` take an OPT-IN record mode (#210, P1B-07):
`--session <uuid> --checkpoint <sha> --repo <token>` on `check` and `stop` (plus the optional
`--log <path>`/`--root <dir>` the event log needs, the `events append` defaults when absent), and
`--session <uuid> --checkpoint <sha>` on `step` (which already owns `--repo`/`--log`/`--root`). The set
is ALL-OR-NONE and shape-checked in the parser, before the lock: a partial set, an uppercase or short
uuid, a 39-hex checkpoint or a non-token repo is a usage error (exit 2) that leaves no lock file, no log
and no deaths folder. With NO record flags every verb is byte-identical to today. The values are trusted
like `--steps` is: the harness does not prove the checkpoint is a pushed commit or the session real.

Nothing new runs in the background: a death is recorded only when the driver already runs a verb, and
the same death is recorded ONCE (an existing handover of the same process life is reused, and the event
is not appended when the log already holds this death's line). Two writers on one ticket serialise on
the per-ticket lock; the loser finds the entry removed and writes nothing.

### Which ends are recorded, and as what

Every recorded end is the EXISTING `timed_out` event kind; its two `cause` values carry the whole
distinction (no new kind, no `v` bump):

| Where found | Today's answer | `cause` | Recorded? |
|---|---|---|---|
| `check`: expired, stopped by this run | 5, `timed_out` stdout | `timeout` | yes |
| `step`: hard line, stopped by this run | 6, `stopped` stdout | `timeout` (handover `limit` = `step`) | yes |
| any verb: provably gone before our signal (`already-gone`) | 3 / 5 | `died` | yes |
| any verb: `pid-reused` (the recorded process is gone) | 3 / 5 | `died` | yes |
| `stop`: a deliberate stop by the caller | 0 `stopped` | — | NO (asked-for, not a death) |
| signalled, death not confirmed | 4 | — | not yet (entry kept; a later run records it) |
| any refusal | 2 | — | no |

`died` means "ended without a supervisor signal"; whether it exited cleanly is P1B-08's exit record, not
this ticket's claim. A distinct `step_limit` cause would be a schema change and is NOT done; the
handover's `limit` field carries that distinction instead.

### The handover file

One file per death, `<manifest-dir>/<ticket>.deaths/<id>.<pid>.<pid_start_ms>.json` — outside the
roots, so teardown and the quarantine sweep never touch it and it outlives the worktree, like the
manifest itself; the key `(id, pid, pid_start_ms)` names exactly one process life, so a re-used pid gets
a different file. The event's `handover` field is that path RELATIVE TO THE MANIFEST DIR. One
newline-terminated compact JSON line, at most 4096 bytes, fixed key order, every key present:

```
v(1), ticket, repo, id, pid, pid_start_ms, cause(timeout|died), limit(time|step|none),
timeout_ms (0 when no time limit), steps, hard_limit (0 when not a step stop), detected_ms (wall),
checkpoint, recipe(commit_push_then_resume), session_id
```

The recipe, stated once: a restart checks out `ws/<ticket>` at `checkpoint` under a NEW session and
resumes; the old `session_id` is the pointer to what the dead worker knew. NO program name, argv, cwd,
path, ticket title or any free text is written — nothing that could carry a secret or a private folder.

### Order of writes, and how a failure fails

For a death: (a) pre-flight — the event log is read and must be missing or clean, else refuse; (b) the
handover file is written atomically (an exclusive temp file, `sync_all`, then a hard link that FAILS if
the name exists — never a replacing rename); (c) the `timed_out` event is appended; (d) only then the
manifest `remove` lines. For a TIMEOUT, (a) and (b) happen BEFORE any signal, like `step_limit_stop` —
a hard stop is never silent. A crash between any two steps leaves the entry on the sheet, so the next
record-mode run finishes the job. Record mode widens `check` to visit EVERY live process entry for
liveness (an entry with no limit is probed only; gone -> `died`), so a program that vanished is found at
the driver's next check.

Any record failure answers 2 with `harness: error: <id>: <reason>` then `harness: DEATH NOT RECORDED`
on stderr; the entry is KEPT (not removed), so the death is retried, never lost. The fixed reasons:

- `deaths folder is a symlink`
- `deaths folder is not a directory`
- `deaths folder is owned by another user`
- `deaths folder is readable by group or others`
- `handover <relpath> exists and does not match this death`
- `cannot write the handover: <kind>`
- `event log refused: <why>`
- `the clock reads before the process was recorded`

The deaths folder is created 0700 when missing, without following links; handover files are 0600. No
absolute path is ever printed. Exit codes are unchanged: check `0/2/4/5`, step `0/2/4/6`, stop
`0/2/3/4` keep their meanings, and "death not recorded" is a refusal (2, top of check's precedence).
Record mode adds ONE stdout line per death this run recorded, after the verb's own line:
`recorded<TAB><id><TAB><cause><TAB><handover-relpath>`.

### What the death record does not cover

- **A driver that never passes the record flags.** Legacy mode still removes a gone entry with no
  record (today's behaviour, kept for compatibility); the P1B-14/15 driver must pass them.
- **Trusted `--checkpoint` / `--session`.** Shape-checked only; not proven to be a pushed commit of
  `ws/<ticket>` or the real worker session.
- **Nobody runs a verb.** No daemon: a death is recorded at the next check/step/stop, not when it
  happens.
- **Residual folder swap.** The checks are no-follow and the final link refuses an existing name, but a
  same-user writer could swap the deaths folder between its check and the temp create (std has no
  `openat`).
- **Two manifest dirs or two logs for one ticket.** Each records separately (the existing "two `--dir`s
  split a ticket" limit).
- **Orphan reaping** (a process the sheet never listed, or a leaderless group) is P2; **output, exit
  code and timing** of the ended program are P1B-08.
- **Leftover intent files.** A timeout whose stop is refused forever leaves a handover with no event; it
  is an intent record, harmless, and is reused if the entry is later resolved.
- **Zombie window.** A dead but unreaped program answers the probe and is not recorded until reaped.
- **Log read cost.** The idempotence lookup reads the whole event log per death (no index); fine at
  today's sizes.
