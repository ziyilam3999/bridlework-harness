# Process supervisor

The process supervisor starts a ticket's long-running programs — app servers, browser and test
processes — under **ticket ownership**: a started process and its port are signed out on the ticket's
[resource manifest](resource-manifest.md) before `start` answers, and a start that cannot record its
spawn is rolled back, so a refused start leaves nothing running and nothing listed. Every started
program's **runtime evidence** — stdout, stderr, exit code and timing — is captured into the ticket's
own root and signed out ahead of the spawn ([runtime evidence](#runtime-evidence-p1b-08) below).

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
Code: `core/rust/harness/src/supervisor.rs` and `core/rust/harness/src/evidence.rs` (unix only) and the
`harness process` command in `core/rust/harness/src/cli.rs`. Nothing new in the manifest format: one existing
`add` process line and, with `--port`, one existing `add` port line, with `--timeout-ms` (#207) one existing
`limit` line, plus three existing `add` evidence lines per start — the manifest line IS the durable record,
no event kind is added.

## The command

```
harness process start <ticket> --id <id> --cwd <dir> [--port <n>] [--timeout-ms <n>] [--worker [--log <path>] [--root <dir>]] [--dir <manifest-dir>] -- <program> [args...]
harness process stop <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>] [--session <uuid> --checkpoint <sha> --repo <token> [--log <path>] [--root <dir>]]
harness process check <ticket> [--grace-ms <n>] [--dir <manifest-dir>] [--session <uuid> --checkpoint <sha> --repo <token> [--log <path>] [--root <dir>]]
harness process step <ticket> --repo <r> --steps <s> --soft <a> --hard <b> [--grace-ms <n>] [--dir <manifest-dir>] [--log <path>] [--root <dir>] [--session <uuid> --checkpoint <sha>]
harness process claim <ticket> --session <uuid> [--dir <manifest-dir>] [--log <path>] [--root <dir>]
harness process sessions <ticket> [--dir <manifest-dir>]
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
5. Make the evidence layout and the write-ahead: the manifest root that contains `--cwd` (the longest such
   root when roots nest) gets `evidence/` and `evidence/proc/` (real dirs, mode 0700, owned by the running
   user — an existing dir that is a symlink, not a directory, another user's, or readable by group or others
   refuses: **exit 2**, nothing spawned, nothing appended, the symlink target untouched), then ONE run dir
   `evidence/proc/<id>.<ts_ms>/` (created exclusively at 0700; an existing name refuses — retryable, the next
   millisecond is a new run), then THREE existing-class `evidence` entries (`<id>.<ts_ms>.stdout`,
   `.stderr`, `.exit`) appended AHEAD of the spawn: the captured files can never be unlisted orphans.
   They stay live after a stop — evidence outlives the process (retention is P1B-09/P1B-14).
6. Spawn the WAITER: the harness binary itself, in the hidden `process waiter` subcommand, in ITS OWN process
   group, with a commit pipe in and a one-line handshake pipe out. The waiter re-verifies the run dir, creates
   the three files (0600, exclusively — a pre-planted name, a symlink included, is refused and never written
   through), spawns the PROGRAM in the program's OWN process group (group id = its pid, exactly as before, so
   `stop`'s checks 5-7 are unchanged), stdin closed, both streams piped to itself, `PORT=<n>` in its
   environment when `--port` is given, and reports `ok <pid> <pid_start_ms>` — start records the PROGRAM's
   pid and start time on the process line, exactly what that line has always meant. The waiter's refusal
   (`fail <message>`, e.g. today's `cannot start <program>: <kind>`), silence past 5000 ms, or garbage each
   refuse with **exit 2** and the evidence entries stay listed (they record the failed attempt).
7. Append the process line, then — when `--timeout-ms` was given (#207) — one `limit` line for that id, then
   the port line. If any append fails, the whole group this start just made is stopped and **exit 2**; a
   failure AFTER the process line (the limit or port append) also appends `remove` for that process line (a
   remove ends the limit too), so the sheet never lists a start that was refused — but the attempt's three
   evidence entries stay listed. A rollback that cannot CONFIRM its kill says so in the refusal message
   instead of claiming success.
8. Write the `commit` word to the waiter and close its pipe — the commit handshake: a waiter whose input ends
   WITHOUT that word kills its program (see [the rollback](#the-rollback)), so a start that refuses or dies
   after the spawn leaves nothing running unless the waiter died too. Then print `id<TAB>pid<TAB>pid_start_ms`,
   exit 0.

## Exit codes

| Case | stdout | exit |
|---|---|---|
| started | `id<TAB>pid<TAB>pid_start_ms` | 0 |
| usage error or any refusal (out-of-range `--port` or `--timeout-ms`, bad ticket or id, missing/torn/corrupt manifest, id or port already live, cwd outside every root, port already bound, `evidence refused:` ... — a symlinked / non-directory / foreign-owned / group-readable evidence dir, `run dir already exists`, `cannot create <rel>: <kind>`, `the waiter did not report the spawn within 5000 ms`, `the waiter reported garbage` — unreadable start time, refused append, `cannot start <program>: <kind>` on a spawn failure) | — (stderr names it; no new message prints an absolute path) | 2 |

There is exactly one nuance inside exit 2: a refusal that happens AFTER the spawn (steps 6-7) has already
stopped everything that spawn made — such a `2` means "nothing left running and no process or port entry
listed", not "nothing was ever started"; the attempt's three evidence entries STAY listed (they record the
failed attempt). When the rollback itself could not confirm the kill, the message names that too, so no
refusal is ever read as a success-shaped answer.

## The rollback

The only signal path in this verb is the start rollback, and it touches only what the `start` of this
very run created. Since #209 the spawn is a WAITER (in its own group) whose child is the program (in the
program's own group), and the rollback runs: the commit pipe closes UNCOMMITTED first — the end-of-input
is what makes a LIVE waiter kill its program through `/bin/kill` by ABSOLUTE path (never through `PATH`;
the std `Child::kill` needs the Child handle, which the waiter's event-driven reaping `wait()` holds) —
then a short bounded grace (2 s) in which the waiter does that kill itself and ends. The grace matters
most on a refused HANDSHAKE, where the group the rollback would signal is the WAITER's own: a signal
that lands before the waiter has read the end-of-input kills the waiter (and its commit watcher with
it) while the program it already spawned keeps running. Only what is still alive past the grace gets
signalled: TERM, a bounded wait (5 s), then KILL to the group the handshake named (the program's group)
through `/bin/kill` by absolute path;
then a bounded reap (3 s) of the WAITER, and the std kill of the waiter as the last resort, so a rollback
can never block forever on something it could not signal. A helper that cannot be RUN is "unknown", never
"gone" or "empty": the group waits are skipped and the refusal names the unconfirmed kill.

## What this does not cover

- **Start-to-record window (narrowed).** The commit handshake closes most of it: a start that refuses
  after the spawn, or dies hard after it, no longer leaves a running unlisted program — its waiter sees
  the uncommitted end-of-input and kills the program. What remains: the waiter ITSELF killed (SIGKILL,
  host crash) before the record lands — then there is no `exit` file (outcome unknown, never fabricated)
  and a program whose start died before committing may outlive it unlisted. The P2 orphan reaper owns
  that case.
- **Port check is a snapshot, and 127.0.0.1 only.** "Free at start" can change before the child binds; the
  supervisor records the declared port and does not prove the child listens on it. A port already bound on another
  address (for example `0.0.0.0`) but free on 127.0.0.1 passes the check.
- **The lock file.** A refusal never writes the MANIFEST. Only a start that gets past the parser and the token
  checks takes the per-ticket lock file `<manifest-dir>/<ticket>.proc.lock` (step 2 of `start`); a parser or
  token refusal (bad `--port`, bad ticket or id) exits before the lock, so no lock file is made either. It is a
  std file-lock helper, not a record — it holds no data and is safe to delete between commands.
- **No orphan reaping for stop escapees** (P2) and **no finalize that serialises a ticket; teardown calls stop for
  every live process of a ticket** (see [Teardown](workspace.md#teardown-p1b-14)).
- **Evidence privacy is not handled here** (P1B-10): capture is raw, byte-for-byte, secret-shaped text kept
  verbatim. The evidence READER verbs and the Done link are P1B-09 (`harness evidence show|check`, see
  [runtime evidence reading](evidence.md); a missing `exit` file reads as unknown, never success).
  **No evidence retention/teardown** (P1B-14). The worker-death record (the `timed_out` event with its
  continuation recipe, P1B-07) now EXISTS and has its own section, [Death record](#death-record), below.
  **No network allow-list** (G1b/P4a).
- **Waiter limits.** The waiter is killed by SIGKILL = no record (above). If the harness binary is replaced
  between `start` and the waiter spawn, a different binary runs as the waiter (same user only; `current_exe`).
  A same-user writer could swap the run dir between the waiter's re-check and its file creates (0700 dirs;
  closing that needs `openat`, not std). Grandchildren that leave the group keep running after the program
  ends, and their output past the 2000 ms drain window is lost. Each start adds three manifest lines (~250
  bytes each) against the 1 MiB cap — roughly 1,300 starts of one ticket before the manifest refuses.

## Runtime evidence (P1B-08)

Every started program gets a small private notebook inside the ticket's own root, listed before it
exists:

- **Layout.** `<root>/evidence/proc/<id>.<ts_ms>/` holding exactly `stdout`, `stderr`, `exit`. `<root>` is
  the manifest root containing `--cwd` (longest when nested); `<id>` the validated token; `<ts_ms>` the
  start's wall ms — the run stamp lets the same `--id` run again after a stop. No ticket title or free
  text ever appears in a path. Dirs 0700, files 0600, files created exclusively.
- **The waiter.** One helper per started program, the harness binary itself (`process waiter`, hidden),
  in its own group; it creates the files, spawns the program in the program's own group, reports
  `ok <pid> <pid_start_ms>`, reaps the program the moment it dies (an event-driven `wait()`, not a poll —
  a zombie must not outlive the death, `stop`'s proof would read it as alive), drains both pipes (at most
  2000 ms past the exit for grandchildren that hold them — the bound holds even when a grandchild keeps a
  pipe open FOREVER: the copiers publish their progress and never hold a lock across a read, so the
  record lands with `output_open_after_exit:true` and the waiter ends), writes `exit` and ends. Not a
  daemon: nothing
  runs when no ticket program runs. The waiter is not in the program's group, so `stop`'s TERM/KILL to the
  group does not reach it and it records the signal.
- **The exit record.** `exit` is ONE line, compact JSON, fixed key order, newline-terminated, every key
  present: `v` (1), `pid`, `committed` (bool), `outcome` (`exited` | `signalled` | `spawn_failed` |
  `wait_failed`), `code` (-1 when not `exited`), `signal` (0 when not `signalled`), `start_ms`, `end_ms`
  (wall clock, for humans), `duration_ms` (the waiter's own MONOTONIC span, between its spawn returning
  and its wait returning, inside one process), `stdout_bytes`, `stderr_bytes` (total seen, past the cap
  too), `stdout_truncated`, `stderr_truncated`, `stdout_error`, `stderr_error` (`none` or the I/O error
  kind), `output_open_after_exit`. It is written to a temp name, synced, renamed: a reader sees a whole
  record or no `exit` file. NO `exit` file means outcome UNKNOWN, never success.
- **Cap and marker.** Per stream the first 4,194,304 bytes (4 MiB, a constant, no flag) are kept
  byte-for-byte; then exactly this 46-byte marker once: `\n[harness: output truncated at 4194304 bytes]\n`.
  The pipe keeps being drained past the cap, so a program that prints forever never blocks on it.
- **Raw bytes.** Binary and non-UTF-8 output is copied as bytes, never decoded or rewritten.
- **Write errors.** A failing stream stops being written but keeps being drained, and the error kind
  lands in `<stream>_error`; if `exit` itself cannot be written, there is no `exit` file (unknown).

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

A cause is decided by what the verb OBSERVED about the process before any record was written, never by
which verb is running: at `check`'s expiry and at `step`'s hard line, a `timeout` handover is staged only
for a pid the run has just proven live AND still the recorded process. An entry the probe finds gone, or
whose pid was re-used, is never staged as a timeout — it records `died` (limit from its own entry)
through the stop's already-gone / pid-reused arms, in every verb.

An entry that check or step signals for a LIMIT is staged first or not signalled at all. The pre-stage
probes must be CONCLUSIVE: an inconclusive one (a liveness or identity probe that could not run or proved
nothing) refuses the entry — exit 2, nothing signalled, the entry kept, the cause named in the message —
because the stop re-probes on its own, and a limit stop whose own later probe succeeds with nothing
staged would be read as a deliberate stop and remove the entry with NO record. As a second line, a limit
stop that ever reaches the stopped arm unstaged refuses with `DEATH NOT RECORDED` and keeps the entry
(instead of the stopped arm reading "nothing staged" as a deliberate stop).

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
`steps` and `hard_limit` name the RUN that found the death, so a `died` found at `step`'s hard line
carries that run's numbers even though the process was not stopped by the step line (the entry's own
`limit` field stays the honest one: `none`).

### Order of writes, and how a failure fails

For a death: (a) pre-flight — the event log is read and must be missing or clean, else refuse; (b) the
handover file is written atomically (an exclusive temp file, `sync_all`, then a hard link that FAILS if
the name exists — never a replacing rename); (c) the `timed_out` event is appended; (d) only then the
manifest `remove` lines. For a TIMEOUT, (a) and (b) happen BEFORE any signal, like `step_limit_stop` —
a hard stop is never silent. A crash between any two steps leaves the entry on the sheet, so the next
record-mode run finishes the job. Record mode widens `check` to visit EVERY live process entry for
liveness (an entry with no limit is probed only; gone -> `died`), so a program that vanished is found at
the driver's next check.

A path at the final handover name is accepted only if the no-follow metadata says it is a REGULAR file:
a symlink (even to a valid same-life handover outside the folder), a FIFO, a socket or a directory is
the planted-name refusal below, is never opened, and cannot hang the verb with the ticket lock held.
The open itself is non-blocking only (portable no-follow open FLAGS do not exist — the per-platform
`O_NOFOLLOW` values differ), so a same-user SWAP of the final name between that gate and the open is
caught one step later: the opened handle must itself be a regular file carrying the gate's own device
and inode, else the same planted-name refusal — nothing swapped in at the name is ever parsed.

When an existing handover of the same life is REUSED, decision 7 pins only the cause and limit: the
appended `timed_out` event carries THIS run's `--checkpoint`/`--session` values, so the event and the
reused handover may name different resume points — the handover's are the durable ones.

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
- `a limit stop reached the stopped arm with no staged handover`

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
  today's sizes. Both record-mode log reads (the pre-flight and the idempotence lookup) go through the
  same capped, regular-file-proving reader as every other log read (64 MiB cap, a FIFO or directory at
  the log path refused without being opened).

## Worker sessions (#3706)

`process start --worker [--log <path>] [--root <dir>]` gives the started program a session id. BEFORE any
spawn the supervisor mints a v4 uuid from 16 bytes of `/dev/urandom` (an unreadable source refuses with exit
2, nothing spawned or listed) and sets `HARNESS_WORKER_SESSION=<uuid>` for the program. After the process (and
limit) line it writes one `session` line ([resource manifest](resource-manifest.md)), and after the port line
it appends `session_started {session_id, source: launch}` to the log (default `.harness/events.jsonl`, same
resolver as the record-mode verbs). A failure of either write runs the start rollback: the entry is removed
and the spawned group stopped, so a refused worker start leaves nothing running and nothing live. The
append-only manifest keeps the `session` line, so `process sessions` shows that attempt as `closed`.
Success prints one extra stdout line, `worker <id> session <uuid>`. `--log` and `--root` without `--worker`
are a usage error (exit 2, nothing spawned). Without `--worker` the start is byte-for-byte unchanged.

`process claim <ticket> --session <uuid>` is how a process says "I am this ticket's worker". The claimant is
identified by OS ancestry, never by an argument: the walk starts at the claimant's OWN pid (a shell that
`exec`s the claim holds the launched pid itself), then its parents, at most 32, stopping at pid <= 1. The
first pid that equals a live `process` entry's pid AND passes the recorded start-time identity (the same proof
`stop` uses) is the claimant's launch. The whole claim runs under the per-ticket supervisor lock, so it waits
out an in-flight `start` of the same ticket and sees either the finished lines or nothing.

| Case | stdout | exit |
|---|---|---|
| launched worker, entry has a session equal to `--session` | `counted <id> <uuid>` (nothing written) | 0 |
| launched worker, entry's session differs from `--session` | — (stderr: `session id mismatch: launched <id> holds a different session id`; nothing written, the launched id is not echoed) | 2 |
| no ancestor is a live entry, or the matching entry has no session (not a worker): `unmanaged_session {reason: not_supervisor_launched}` is appended | `unmanaged: not counted` | 7 |
| an ancestor's parent or start time cannot be read, damaged manifest, bad ticket or non-uuid `--session`, or the `unmanaged_session` append failed | — (stderr names it; "unknown" is never read as "not launched") | 2 |

`process sessions <ticket>` is read-only: one `<id> <uuid> live|closed` line per `session` line ever written
(closed = a later `remove` of that entry, e.g. after `stop`), then `counted <n>` = the live sessions.

Honest limits: (a) any descendant of the worker (a shell it spawned) claims as the worker — the family is the
unit, as with `stop`; (b) the id is the harness's own launch handle; whether the host CLI reports the same id
is the live check of a later row; (c) the finalize join (closing the session at ticket finalize) is not wired
here — `stop` closes it.
