# The goal store (WAITING-228)

A goal is the parent of tickets with its own finish line, one per focus group (WAITING-232 A). A goal that
spans tickets had no record and no finish line: the ticket schema (`ticket.rs`, its key list) and the plan
index (`plan_index.rs`, its eight keys) have no goal or parent field, so work under "video 1" kept going into
new tools. The goal store is the small ledger that fixes that: an add row when the operator says yes to a
goal, and a close row when it is met (with proof pointers) or dropped (with a reason).

**The store file is not in this repo.** It lives in the private overlay, like the owed store and the friction
inbox. It is named by `--store <absolute-file>` on each verb (a relative value is exit 2); there is no config
key. The file is created once by hand as an empty file: no verb creates it, so a wrong `--store` path cannot
start a second store. The file is append-only JSON lines. No clock runs in these verbs: times arrive by `--at`.

## The rows

Two row shapes, each one JSON object of string values, keys written in this order (a reader accepts any key
order but exactly the key set of its op; any other key set is store damage):

```
{"op":"add","id":"gs-video-1","at":"2026-11-26T23:49:00Z","by":"orch-a","title":"video 1 ships","done_when":"the first video is published","source":"pr:owner/repo#7"}
{"op":"close","id":"gs-harness","at":"2026-11-27T01:16:00Z","by":"orch-a","status":"met","ref":"pr:owner/repo#9","reason":""}
```

Both keys `ref` and `reason` are present on a close row; the one that does not apply is empty. The state of
an id is `open` after its add row, and `met` or `dropped` after its close row.

| field | rule |
|---|---|
| `id`, `by` | a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not `.` or `..` |
| `at` | `YYYY-MM-DDTHH:MM:SSZ`, a real UTC instant (so `2026-02-30T00:00:00Z` is refused) |
| `title` | one non-blank line of at most 500 bytes (no control character, no U+2028, no U+2029) |
| `done_when` | the line rule of `title`, and not only `-`, `tbd` or `?` (any case, after trimming): a goal needs a finish line |
| `source` | `; `-separated proof pointers, the proof grammar of `docs/friction-inbox.md` (never empty) |
| `status` | `met` or `dropped` |
| `ref` | `; `-separated proof pointers, the proof grammar of `docs/friction-inbox.md` |
| `reason` | the line rule of `title` |

Pairing: `met` needs `--ref` and takes no `--reason`; `dropped` needs `--reason` and takes no `--ref`.
Pairing is judged on values: an empty `--ref` or `--reason` counts as not given. A close row is checked in the
order id, at, by, status, pairing, then ref (met) or reason (dropped); an add row in the order id, at, by,
title, done_when, source. No refusal message echoes a field value, except the id in the stdout
`goal refused id=<id> reason=<reason>` line of `check` and `show` (the id is scanned against the privacy list
first).

**The id convention.** One id per focus group, named for the group: `gs-video-1`, `bw-harness-running`. The
id is the one goal id rule; the ticket side (T2) uses the same rule. A new goal needs the operator's yes in
`--source`: the pointer names where the yes was given.

**There is no `--ask` and no `--group` flag.** The goal is the focus group, and ask rows name it in the
ledger (WAITING-232 A); there is no friction-group goal (WAITING-233 A). Either one is an unknown option,
exit 2.

## The verbs

Exit codes for the whole family: **0** ok; **1** a refused input; **2** usage, setup or a damaged store.

```
harness goal add --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --title <text> --done-when <line> --source <pointers>
harness goal list --store <absolute-file> [--open]
harness goal show --store <absolute-file> --id <id>
harness goal close --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --status <met|dropped> (--ref <pointers> | --reason <text>)
harness goal check --store <absolute-file> --id <id>
```

| code | meaning | line |
|---|---|---|
| 0 | ok | the verb's output lines on stdout |
| 1 | a refused input | `harness: goal refused: <reason>` on stderr; the `goal refused id=<id> reason=<reason>` line of `check` and `show` is on stdout |
| 2 | usage, setup or a damaged store | `harness: error: <reason>` on stderr |

A non-zero exit prints nothing on stdout, except the `goal refused` line of `check` and `show`.

### `harness goal add`

Checks, in this order: (1) flags: an unknown option, an unexpected argument, a flag given twice or missing
its value is exit 2; a missing required flag is exit 2 in usage order (store, id, at, by, title, done-when,
source); (2) `--store` absolute, else exit 2; (3) the field rules, exit 1; (4) the privacy list loads, else
exit 2: an unscanned row is never stored; (5) the privacy scan of each field of the new row (id, at, by,
title, done_when, source), a hit is exit 1 naming the field and the classes; (6) the store path stat:
missing (or under a file) is exit 2 `the goal store does not exist`; anything but a regular file (a symbolic
link, a folder, a fifo) is exit 2 `the goal store is not a regular file`; (7) the open, read and append, no
create, with `O_NOFOLLOW` and `O_NONBLOCK`: the open itself refuses a link, so a link swapped in between the
stat and the open is exit 2 `the goal store cannot be opened` and nothing is written through it; the handle
is re-checked to be a regular file; (8) a blocking exclusive lock on that handle, held from the read to the
write, no time limit; (9) the store lint, exit 2; (10) the id is already in the store (open or closed), exit
1; (11) one line is written and synced. Success: exit 0, stdout `goal added id=<id> state=open`. A refusal
in steps 1 to 10 leaves the store byte for byte unchanged and creates no file. Parent folders of the store
path are followed, so a linked parent folder is accepted; only the last part must not be a link: the stat
refuses it in step 6, and the open of step 7 is the backstop for a link swapped in after the stat. No test
can drive a verb to the open with a link in place, because the stat refuses it first, so that backstop
(`O_NOFOLLOW`: `0x100` on macOS) is pinned by a unit test of the open alone (`goal.rs`,
`the_open_itself_refuses_a_link`), for a read as for a write.

### `harness goal list`

Checks: flags, store path, the privacy list, the stat, a read-only open, a shared lock, the store lint: any
failure is exit 2 as in add. Output, one line per goal in the order of its add row, then one count line:

```
goal id=<id> state=<open|met|dropped>
goal open=<o> met=<m> dropped=<d>
```

With `--open`, only the open goals' lines, then `goal open=<o>`. Ids, state words and numbers only, as
`owed list`: no title, done_when, source, ref or reason text is printed. `list` writes nothing.

### `harness goal show`

Checks: flags, store path, the id rule (exit 1, on stderr), the privacy list, the scan of the id, the stat,
the read-only open, the shared lock, the store lint. Output for an open goal, exactly:

```
goal id=<id> state=open
title: <title>
done_when: <done_when>
added <at> by <by> source: <source>
```

For a closed goal the first line has `state=<met|dropped>` and one last line follows:
`closed <at> by <by> ref: <ref>` (met) or `closed <at> by <by> reason: <reason>` (dropped).

**A chosen difference from the owed store:** `show` prints stored text (title, done_when, source, ref,
reason), which `owed list` never does. The rows were scanned on write, and every read scans every stored row
again (see Privacy), so nothing reaches stdout that has not passed the scan under the list of this run.

### `harness goal close`

Checks, in this order: flags (required order store, id, at, by, status); `--store` absolute; the close-row
field rules (a `--status` other than `met` or `dropped`, a bad `--ref` pointer, a broken pairing, a
`--reason` that is blank, holds a control character or is over 500 bytes: exit 1 naming the field); the
privacy list; the privacy scan of id, at, by, status, ref, reason; steps (6) to (9) of add; then, exit 1
with the store unchanged: `id is not in the store`, `id is already closed`, `at is before the time the id
was added` (both times have one fixed format, so a string compare is a time compare; an open goal has one
earlier row, its add row); then one close row is written and synced. Success: exit 0, stdout
`goal closed id=<id> state=<met|dropped>`.

A close of `met` needs a valid `--ref` (plan 5.14: "a close row is `met` with proof pointers (the inbox proof
grammar)"; the WAITING-229 pass field): the gs-video-1 goal closes only with a proof pointer. `met` with no
`--ref`, an empty `--ref`, or a `--ref` that is not a pointer list is exit 1.

### `harness goal check`

Checks, in this order: flags; `--store` absolute; the id rule (a malformed id such as an upper-case token is
exit 1 `harness: goal refused: id must be a plain token ...` on stderr, the value not echoed, and this comes
before the store is read); the privacy list; the scan of the id; the stat, the read-only open, the shared
lock, the store lint. Then:

```
goal ok id=<id> state=open            (exit 0, an open goal)
goal refused id=<id> reason=unknown   (exit 1, no such id)
goal refused id=<id> reason=closed    (exit 1, a met or dropped goal)
```

The two refusal lines are on stdout with nothing on stderr. A damaged store is exit 2 with nothing on stdout:
`check` never prints `goal ok` for a store it could not read whole.

## Privacy

Every verb, reads included, needs the privacy list (`overlay::load`: `HARNESS_PRIVACY_LIST_SECRET` wins when
set, else `HARNESS_PRIVACY_LIST`; `HARNESS_PRIVACY_OLD_LIST` adds the drift check). Without a list every verb
exits 2 for a call with valid flags and fields. As in `owed add`, the field rules come first: a call with a
bad field or a malformed id exits 1 before the privacy list is looked for.

- add and close scan every field of the new row before the store is opened: a hit is exit 1 naming the field
  and the classes, and nothing is written.
- Every verb scans every stored row on every read, as the owed store lint does. A stored row that holds a
  token of the list (for example a token added to the list after the row was written) makes every verb exit
  2 with `the goal store has a privacy hit at line <n> in field <name> (<classes>)`, naming the line and the
  field, never the value. A newly listed token stops every verb until the row is fixed by hand. This holds
  for a call with valid flags and fields, as above: the field rules (exit 1) and the scan of the new row or
  of the id (exit 1) come before the store is read.
- `show` and `check` print the id, so the id is scanned too: an id holding a deny token is exit 1 on stderr
  and is never printed.

## The store lint

One criterion for every verb: at most 4194305 bytes are read through the locked handle, and a store over
4194304 bytes is refused. Each failure is exit 2, one stderr line, no row byte echoed, checked in this order:

- `harness: error: the goal store is over 4194304 bytes`
- `harness: error: the goal store is not utf-8`
- `harness: error: the goal store has a torn last line at line <n>` (a last line with no newline)
- `harness: error: the goal store has a bad row at line <n>: <reason>` for the first bad line, where
  `<reason>` is one of: `not one JSON object of strings` (a CR LF line end is such a row), `op must be add or
  close`, `the keys are not the key set of its op`, a field rule reason (a blank `done_when`, or one that is
  only `-`, `tbd` or `?`, is one: add never writes such a row; a blank or multi-line `reason` on a stored
  close row is one too), `id is added twice`, `close names no earlier added id`, `id is closed twice`,
  `close is before the time the id was added`
- `harness: error: the goal store has a privacy hit at line <n> in field <name> (<classes>)` for the first
  hit over the stored fields in line order

An empty file is a store with no row. A torn last line after a crash blocks each verb until the line is
trimmed by hand.

## Differs from the owed store

- The row shapes: an add row carries `title`, `done_when` and `source` (no `home`); a close row carries
  `status` `met` or `dropped` (not `done` or `dropped`).
- `done_when` has its own rule: a finish line that is blank or a placeholder is refused.
- `source` is required and is a pointer list: a new goal needs the operator's yes.
- Two more verbs: `show` prints the stored text; `check` gives the open or not-open verdict of one goal on
  stdout.
- No `--ask` and no `--group` flag.

A value that starts with `--` is taken as the value. The one exception is the global flag: `harness` takes
the first `--testmode` of the command line, and the word after it when there is one, before the verb parses
its flags (the verb runs in test mode when that word is `synthetic-test`). A second `--testmode` is not
taken by the global flag: it reaches the verb like any other word, where it is an unknown option (exit 2)
or the value of the flag before it.

## Not in scope here

`goal reword` (T1b); any change to the ticket schema or intake (T2); bw (T3); the live store and the first
goal records (T4). Tracking is in the plan (plan 5.14: "Tracked in the Job log rows of the goal tickets").
