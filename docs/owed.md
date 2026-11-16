# The owed store (OW-1)

A promise made to the operator in a report — "I owe you a lesson page", "I will re-run that gate" — is
chat text that no store holds, so a promised item can be lost when the session ends. The owed store is
the small ledger that fixes that: an add row when the promise is made, and a close row when it is kept
(`done`, with proof pointers) or set aside (`dropped`, with a reason).

**The store file is not in this repo.** It lives in the private overlay, like the friction inbox. It is
named by `--store <absolute-file>` on each verb (a relative value is exit 2); there is no config key. The
file is created once by hand as an empty file — no verb creates it, so a wrong `--store` path cannot
start a second store. The file is append-only JSON lines.

## The rows

Two row shapes, each one JSON object of string values, keys written in this order (a reader accepts any
key order but exactly the key set of the op):

```
{"op":"add","id":"ow-1","at":"2026-11-16T00:48:00Z","by":"orch-a","what":"write the lesson page for the menu stall","home":""}
{"op":"close","id":"ow-2","at":"2026-11-16T09:00:00Z","by":"orch-a","status":"done","ref":"pr:owner/repo#7","reason":""}
```

Both keys `ref` and `reason` are present on a close row; the one that does not apply is empty. The state
of an id is `open` after its add row, and `done` or `dropped` after its close row.

| field | rule |
|---|---|
| `id`, `by` | a plain token: lowercase letters, digits, dash, underscore or dot, at most 64 bytes, not `.` or `..` |
| `at` | `YYYY-MM-DDTHH:MM:SSZ`, a real UTC instant (no wall clock runs in these verbs; the time arrives by flag) |
| `what`, `reason` | one non-blank line of at most 500 bytes (no control character, no U+2028, no U+2029) |
| `home` | empty, `ticket:<id>`, `ledger:<row>` (1 to 32 ASCII letters or digits) or `pr:<owner>/<repo>#<n>`, at most 200 bytes |
| `status` | `done` or `dropped` |
| `ref` | `; `-separated proof pointers, the proof grammar of `docs/friction-inbox.md` |

Pairing: `done` needs `--ref` and takes no `--reason`; `dropped` needs `--reason` and takes no `--ref`.
Pairing is judged on values: an empty `--ref` or `--reason` counts as not given. A close row is checked
in the order id, at, by, status, pairing, then ref (done) or reason (dropped); an add row in the order
id, at, by, what, home. No refusal message echoes a field value.

## The verbs

Exit codes for the whole family: **0** ok; **1** a refused input; **2** usage, setup or a damaged store.
Nothing is written to stdout when the exit is non-zero.

```
harness owed add --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --what <text> [--home <home>]
harness owed list --store <absolute-file> [--open]
harness owed close --store <absolute-file> --id <id> --at <YYYY-MM-DDTHH:MM:SSZ> --by <tag> --status <done|dropped> (--ref <pointers> | --reason <text>)
```

### `harness owed add`

Checks, in this order: (1) flags — a missing required flag is exit 2 in usage order (store, id, at, by,
what); (2) `--store` absolute; (3) the field rules; (4) the privacy list loads, else exit 2 — an
unscanned row is never stored; (5) the privacy scan of each field of the new row (id, at, by, what,
home), a hit is exit 1 naming the field and the classes; (6) the store path stat — missing (or under a
file) is exit 2 `the owed store does not exist`; any other stat error is exit 2 `the owed store cannot be
opened`; anything but a regular file (a symbolic link, also one to a regular file, a dangling link, a
folder, a fifo) is exit 2 `the owed store is not a regular file`; (7) the open, read and append,
no create, with `O_NOFOLLOW` and `O_NONBLOCK`, the handle re-checked to be a regular file; (8) a
blocking exclusive lock on that handle, no time limit — a stuck holder is freed by killing it; (9) the
store lint; (10) the id is already in the store (open or closed) is exit 1; (11) one line is written and
synced, and on a write or sync error the file is cut back to its old length. Success: exit 0, stdout
`stored id=<id>`. A refusal in steps 1 to 10 leaves the store byte for byte unchanged and creates no
file. Parent folders of the store path are followed, so a linked parent folder is accepted; only the
last part must not be a link.

### `harness owed list`

Checks: flags, store path, the privacy list, the stat, a read-only open (the same flags), a shared lock,
the store lint — any failure is exit 2 as in add; a missing store is exit 2 (it does not read as zero
open items). Output, one line per added id in the order of its add row, then one count line:

```
owed id=<id> status=<open|done|dropped>
owed open=<o> done=<d> dropped=<r>
```

With `--open`, only the open ids, each `owed id=<id> status=open`, then `owed open=<o>`. Ids, status
words and numbers only: no what, by, home, ref or reason text is printed. `list` writes nothing.

### `harness owed close`

Checks, in this order: flags (required order store, id, at, by, status); `--store` absolute; the
close-row field rules; the privacy list; the privacy scan of id, at, by, status, ref, reason; steps (6)
to (9) of add; then, exit 1 with the store unchanged: `id is not in the store`, `id is already closed`,
`at is before the time the id was added` (both times have one fixed format, so a string compare is a time
compare); then one close row is written and synced, with the write and sync error rule of add. Success:
exit 0, stdout `closed id=<id> status=<done|dropped>`.

## The store lint

One criterion for add, list and close: at most 4194305 bytes are read through the locked
handle, and a store over 4194304 bytes is refused. Each failure is exit 2, one stderr line, no row byte echoed, checked in this order:

- `harness: error: the owed store is over 4194304 bytes`
- `harness: error: the owed store is not utf-8`
- `harness: error: the owed store has a torn last line at line <n>` (a last line with no newline)
- `harness: error: the owed store has a bad row at line <n>: <reason>` for the first bad line, where
  `<reason>` is one of: `not one JSON object of strings` (a CR LF line end is such a row),
  `op must be add or close`, `the keys are not the key set of its op`, a field rule reason, `id is added
  twice`, `close names no earlier added id`, `id is closed twice`, `close is before the time the id was
  added`
- `harness: error: the owed store has a privacy hit at line <n> in field <name> (<classes>)` for the
  first hit over the stored fields in line order

An empty file is a store with no row. A torn last line after a crash blocks each verb until the line is
trimmed by hand.

## Differs from the friction inbox

- The store path is a flag, not a config key.
- A symbolic link store is refused (the inbox opener follows links).
- A missing store is exit 2 in each verb (add creates a missing inbox, close-check reads it as empty).
- Store damage is exit 2 in each verb (the inbox: exit 1 for a torn line, a bad row or a privacy hit in
  add and lint, exit 2 in close-check).
- A missing required flag is exit 2 (exit 1 in friction add).
- The lock waits with no time limit (the inbox waits 10 seconds): a stuck holder is freed by killing it.
- CR LF rows are bad rows.

A value that starts with `--` is taken as the value, except the word `--testmode` and the word after it,
which the global flag takes first.

## Later

- OW-2 adds `harness owed check`, the `left open:` line and the `--as-of` count of open items with no home.
- A config key for the store path.
- A verb to set the home of an open item.
- The backfill of the tracker left-overs as rows.
