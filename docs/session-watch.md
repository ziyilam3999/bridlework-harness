# harness session watch

Report only. One verdict line per check for a builder seat. It writes no file, stops no session, changes no lane, writes no event, and
calls the host for the listing only (never `stop` or `rm`). A seat stopped by `bw stop` or at the host is `stopped`, never `stalled`.

```
harness session watch --host <abs> --records <launch records file> --name <seat name> --ticket <ticket file> --repo-map <id>=<tree>
  --base <sha> --result-dir <dir> --caps-dir <dir> --transcript-root <abs dir> --idle-s <n> --silence-s <n>
  [--stopped-at <epoch s> --stop <stopped|absent>] [--follow --poll-s <n> --max-s <n>] [--seat-log <abs file>]
```

Numbers are whole seconds, 1 to 86400. `--base` is a 40-hex sha. `--stopped-at` and `--stop` come together. `--poll-s` and `--max-s` go
with `--follow` and both are required there. `--seat-log` is optional and must be an absolute path. A missing, repeated or bad flag
(a relative or empty `--seat-log` included) is exit 2 with the usage line.

## Join

The newest launch record of `--name` (largest `ts_ms`, later row on a tie) must be bound. `bw seat --restart` reuses the round-1 name,
so the name alone does not pick the entry. The listing entry is the one whose `id` equals the record `short_id` and whose `sessionId`
equals the record `session_id`. None: `gone`. Two or more: refusal. An entry with no string `id` or no string `name`, or whose `id` is
not a plain token, is left out of the join when it cannot be the watched one: its `sessionId` is not the record `session_id`, its `id`
is not the record `short_id`, its `name` is not the record name and its `cwd`, canonicalized, is not the tree. Such an entry that could
be the watched one — its `sessionId` equals the record `session_id`, its `id` equals the record `short_id`, its `name` equals the
record name, or its `cwd` equals the tree — is a refusal. The output line
counts the entries left out in `skipped_entries`. The entry `cwd`, canonicalized, must equal the canonicalized tree of
`--repo-map`, else refusal. The records file is opened without following a link and must be a regular file.

## Verdicts

Decided in this order on each check.

| # | Condition | Verdict | Exit |
|---|-----------|---------|------|
| v1 | `--stopped-at` given, or host state `stopped` | `stopped by=bw stop=<stopped\|absent>` or `stopped by=host` | 14 |
| v2 | no joined entry | `gone` | 15 |
| v3 | state `working`, tree age below `--idle-s` | `working` | 0 |
| v3 | state `working`, transcript age below `--silence-s` | `slow-alive` | 10 |
| v3 | state `working`, past both limits, entry pid a live process, a commit in base..HEAD at or after the launch second, `last_gate=pass`, `gate_current=yes` | `finished` | 13 |
| v3 | state `working`, entry pid is a live process, otherwise | `stalled` | 11 |
| v4 | state `blocked` | `stopped-early why=blocked block=<dialog\|asked\|other> waiting_for=<text>` | 12 |
| v5 | state `done` or `idle`, a commit in base..HEAD at or after the launch second, and `gate_current=yes` | `finished gate=<pass\|fail>` | 13 |
| v5 | otherwise | `stopped-early why=no-commit` (wins) or `why=no-gate` | 12 |

Follow mode ends with 16 (`verdict=watch-limit last=<verdict>`) at the time limit. Refusals (cannot judge) are exit 3 with empty stdout
and one stderr line. Exit 1 is not used.

The v3 `finished` row is a seat the host still lists as working. The seat may still be running. It needs `last_gate=pass`, which the v5
row does not. Its line carries `state=working/<status>`, which tells it from a v5 finish.

Tree age is now minus the newest of: the mtime of every file and folder of the work tree outside `.git` (no link followed, over 200000
entries is a refusal), the mtime of the git index, and the committer time of HEAD. Transcript age is now minus the transcript mtime.

## Output line

`verdict= state=<host state>/<host status or -> tree_age= transcript_age= commits_since_launch= gate=<pass|fail|none>/<yes|no> session=<short id> skipped_entries=<n>`
then `why=` or `by=` where named. `skipped_entries` counts the listing entries left out of the join (0 when none). A value the verdict
did not need is `-` (tree age and the commit and gate facts are read only
where the verdict uses them; `blocked` shows them when they can be read; `stalled` shows the commits and gate it read). No path,
transcript text or file name is printed; the detail line below prints scanned seat text, never a path of its own.

### Blocked kind

The listing parse keeps the host `waitingFor` string of an entry (absent when the host leaves it out or it is not a string). The blocked
line adds `block=` and `waiting_for=`:

- `block=dialog`: status `waiting` and a non-empty `waitingFor` (a real dialog, for example a permission prompt).
- `block=asked`: status `idle` and no `waitingFor` (the seat ended its turn on a question).
- `block=other`: any other blocked entry.

`waiting_for=` is the `waitingFor` lowercased (ASCII), each character outside `a-z 0-9` made `-`, at most 40 characters; `-` when it is
absent, not a string or empty. The verdict, exit code 12 and `why=blocked` do not change. These host values come from the WAITING-149
research (six probe sessions, each case seen once), not from a log. Lines of other verdicts do not change.

### Detail line (`--seat-log`)

With `--seat-log <abs file>`, a blocked verdict prints one more stdout line right after the verdict line (in follow mode too, after the
final blocked line; exit codes unchanged):

- `block=dialog`: the newest record of event `PermissionRequest`, as `detail tool=<tool> command=<command>`.
- `block=asked`: the newest record of event `Stop`, as `detail message=<message>`.
- `block=other`: the newest record of either kind, shown as its own event is.

`command=` and `message=` are the last field and run to the end of the line. Only records with `ts_ms` at or after the launch record
`ts_ms` count. The newest is the largest whole-number `ts_ms` (the later line on a tie). A record whose `ts_ms` is not a whole number or
whose `event` is not a string is skipped. An absent or non-string `tool`, `command` or `message` prints as `-`. No such record, or a
missing file: `detail none`. A link, a folder, a FIFO, a socket, a device or a file that cannot be read: `detail unreadable`. The log never
makes a refusal and never changes the verdict or exit code. A verdict other than blocked reads no log and prints no detail line.

Record format, the contract for the writer: one JSON object per line, with the fields `ts_ms` (whole number, milliseconds since the
epoch), `event` (`PermissionRequest` or `Stop`), `tool` (string, for `PermissionRequest`), `command` (string, for `PermissionRequest`) and
`message` (string, for `Stop`, the seat's last message).

Reading: the file is opened with `O_NOFOLLOW|O_NONBLOCK` and must be a regular file by fstat on the opened handle, so a FIFO never blocks.
Only the last 65536 bytes are read; the first, possibly partial, line of that window is skipped; a line that is not valid UTF-8 or not a
JSON object is skipped.

Text scan: the text is cut to one line (a control character becomes a space) and to at most 200 bytes on a UTF-8 character boundary, then
run through the engine privacy scan with the list `overlay::load` resolves (`HARNESS_PRIVACY_LIST_SECRET`, else `HARNESS_PRIVACY_LIST`;
`HARNESS_PRIVACY_OLD_LIST` drift is a scan error). A hit, a scan error or no list prints `withheld` in place of the text. The tool name is
not scanned text: each character outside `A-Za-z0-9_` is made `-`, at most 40 characters.

The seat log is not the transcript (WAITING-150): it is the file the seat hooks write, and the transcript stays read by metadata only.
The code that reads it lives in `session_watch_log.rs`; `session_watch.rs` opens and reads no file.

## Refusals

The listing cannot be run, exits non-zero, times out, is not a JSON array, or the joined entry has no string state, cwd or sessionId;
a listing entry with no string id or name, or an id that is not a plain token, that could be the watched one (its sessionId equals the
record session_id, its id equals the record short_id, its name equals the record name, or its cwd equals the tree); an entry of that
kind that cannot be the watched one is left out and counted in `skipped_entries`;
no launch record for the name, or the newest is not bound; two or more joined entries; cwd not the tree; the transcript check fails; a
working seat past both limits with a pid that is missing, not a whole number or not a live process (one `ps -p` run); a working seat past
both limits also reads git base..HEAD and `harness status`, and their failures refuse it; the tree walk is
over its cap; git cannot report HEAD, the index path, base..HEAD or the commit times; `harness status` exits 1 or 2; a host state outside
working, done, blocked, idle, stopped. A refusal is never printed as working or slow-alive.

## Transcript

Path: `<transcript root>/<entry cwd with each character outside [A-Za-z0-9] made ->/<sessionId>.jsonl`. It is read by metadata only
(`symlink_metadata`: kind and mtime, what `ls -l` shows). The code opens no transcript and reads no byte of it. It must be a regular
file (a link, a folder or a missing file is a refusal) and its mtime must not be more than 5 s ahead of the clock. The operator
approved a 10-minute silence watch on 2026-11-16, built on the session file mtime (plan rev 9g), and ruled by menu on 2026-11-19
(WAITING-67) that this approval covers this metadata (plan 5.14 Seat watch). The session id must be a plain token (letters, digits,
`-`, `_`), else the verb refuses, so the path cannot leave the transcript root.

## Follow mode

The check runs at start and then every poll interval. A line is printed at start, on each verdict change and at the end. The run ends
with the exit code of the first verdict other than `working` or `slow-alive` (a refusal included), or with 16 at the time limit
(monotonic clock). The end line carries `max_transcript_age=`, the largest transcript age seen with host state working (`-` if none).
`--stopped-at` is read once at start: a `bw stop` during a follow run reads `stopped by=host` or `gone` (the host state or listing
changes), not `by=bw`.
