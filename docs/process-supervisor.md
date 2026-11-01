# Process supervisor

The process supervisor starts and stops a ticket's long-running programs — app servers, browser and test
processes — under **ticket ownership**: a started process and its port are signed out on the ticket's
[resource manifest](resource-manifest.md) before `start` answers, and `stop` signals a pid only when the manifest
of THAT ticket lists it AND the live process at that pid is provably the one that was recorded. Cleanup can never
reach a process the ticket does not own, and a re-used pid is never signalled.

Code: `core/rust/harness/src/supervisor.rs` (unix only) and the `harness process` command in
`core/rust/harness/src/cli.rs`. Nothing new in the manifest format: one existing `add` process line and, with
`--port`, one existing `add` port line — the manifest line IS the durable record, no event kind is added.

## The command

```
harness process start <ticket> --id <id> --cwd <dir> [--port <n>] [--dir <manifest-dir>] -- <program> [args...]
harness process stop  <ticket> (--id <id> | --pid <n>) [--grace-ms <n>] [--dir <manifest-dir>]
```

`--dir` defaults to `.harness/manifests`, the same default as `harness manifest`. Listing stays
`harness manifest list` (it already prints `id, class, pid, pid_start_ms`); no new list verb exists.

## start, in order

1. Take the per-ticket supervisor lock (`<manifest-dir>/<ticket>.proc.lock`, the std exclusive file lock the
   manifest already uses) and hold it for the whole operation.
2. Read the manifest. Missing, torn or corrupt: **exit 2**, nothing started.
3. Refuse (**exit 2**, nothing started) when `--id` — or `<id>.port`, if `--port` is given — is already live, when
   `--cwd` does not resolve inside one of the ticket's roots, when `--port` is already live in the manifest, or
   when `--port` is already bound on 127.0.0.1 (checked with a bind-then-release snapshot).
4. Spawn the program in its OWN process group (group id = its pid), stdin closed, stdout and stderr discarded
   (capture is P1B-08), and `PORT=<n>` in its environment when `--port` is given.
5. Read the pid's start time, append the process line, then the port line. If either append fails, the whole
   group it just made is stopped and **exit 2** — a refused start leaves nothing running that the sheet does not
   list.
6. Print `id<TAB>pid<TAB>pid_start_ms`, exit 0.

## stop: ownership first, signal second

`stop` signals only when ALL hold:

- the manifest of THIS ticket reads clean (missing, torn or corrupt: **exit 2**, nothing signalled, nothing written);
- the target is a LIVE `process` entry of THIS ticket — by `--id`, or by `--pid` matched against live entries. A
  raw pid that is not listed, including a pid listed only on ANOTHER ticket's manifest, is never signalled (**exit 2**);
- the live process at that pid has the recorded `pid_start_ms` — exact on Linux, within 2000 ms elsewhere
  (`ps` start times jitter at whole-second precision; the manifest doc's tolerance).

Then TERM goes to the process GROUP (through the system `kill` command with a negative group id, so no crate is
needed), `stop` waits up to `--grace-ms` (default 5000) for the group to empty, sends KILL to the group if it
lives, and appends `remove` for the process entry and for `<id>.port` if live.

When the recorded process is provably gone, nothing is signalled and the entries are removed:

| Case | stdout | exit |
|---|---|---|
| started / stopped | `id<TAB>pid<TAB>pid_start_ms` / `stopped<TAB>id<TAB>pid` | 0 |
| recorded pid has no process now | `already-gone` | 3 |
| pid alive but its start time differs (pid re-used) | `pid-reused` | 3 |
| usage error or any refusal (not on this sheet, torn manifest, refused start) | — (stderr names it) | 2 |

Two stops of one id at once serialise on the supervisor lock: one exits 0, the other finds the id not live and
exits 2, and the manifest holds exactly one `remove` line. A crash after the signal but before the `remove` is
safe to repeat: the next stop sees `already-gone` (exit 3) and removes the entries.

## What this does not cover

- **Start-time precision off Linux.** `ps` gives whole seconds; with the 2000 ms tolerance, a pid re-used within
  about two seconds of the original start can pass the check. Linux (`/proc`) is exact.
- **Check-to-signal race.** Between the start-time check and the `kill` the process could exit and the pid be
  re-used. The window is milliseconds; closing it needs pidfds (Linux only, a crate). Documented, not closed.
- **Start-to-record window.** A hard crash between spawn and the manifest append leaves a process the sheet does
  not list; a failed append kills the group, but a hard crash does not. The P2 orphan reaper owns that case.
- **Children that leave the group** (a daemon that calls `setsid`) are not stopped; a group whose leader already
  exited is reported `already-gone` and its remaining members are not signalled (no live leader to prove identity).
- **Port check is a snapshot.** "Free at start" can change before the child binds; the supervisor records the
  declared port and does not prove the child listens on it.
- **No output capture** (stdout, stderr, exit codes, timing, traces: P1B-08), **no timeouts or step limits**
  (P1B-06), **no worker-death record** (P1B-07), **no orphan reaping** (P2), **no network allow-list** (G1b/P4a),
  **no finalize/teardown** (P1B-14).
