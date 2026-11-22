# The friction inbox (K-1 slice 1)

The friction inbox is where the harness writes down what broke while it is being built: one row per friction or
lesson, captured the same turn it is found (T-9). This page is the public contract of the row, the verbs that read
and write it, and the `lesson:` close line. The rule itself is the knowledge-routing rule (K-1, ticket 153); this
ticket (154) builds its first slice: capture first, so the store grows while the harness is built.

**The inbox file is not in this repo.** It lives only in the private overlay (5.2 item 4): lesson text is real
operational text, so it is private data. This repo holds the contract, the reader and the lint — never the rows.

Status: contract plus `harness friction add | lint | close-check` (ticket 154), the pattern half — `kind`,
`repeats`, `friction patterns` and the trial record (ticket 192) — and the T-9.3 event-to-row mapper with
`source` (ticket 193), the four evidence keys and the triage loop `rank | triage | bind | page` (ticket 3601), the read-only `controls-check` verb, the read-only `suggest` verb (ask A32, unix only), the two status keys `status` and `status_ref` with the read-only `open` verb (ticket friction-row-status-fields), and `set-status`, the one verb that rewrites a line in place (ticket friction-status-write). The generated lessons index (`harness find`) is slice 2; see "Not in slice 1" below.

## 1. The row (the one contract everything else reads)

One JSON object per line: exactly these six keys, every one required (a missing, empty or extra key is refused), plus
nine OPTIONAL keys — `kind` and `repeats` (ticket 192), `source` (ticket 193), the four evidence keys `at`,
`impact`, `why` and `proof` (ticket 3601) and the two status keys `status` and `status_ref` (ticket
friction-row-status-fields) — each omitted at its default:

```
{"id":"les-001","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"the lesson"}
{"id":"pat-001","date":"2026-11-01","ticket":"192","where-it-lives":"memory","breaks":"","text":"the procedure that worked","kind":"pattern"}
{"id":"pat-001-r1","date":"2026-11-02","ticket":"192","where-it-lives":"memory","breaks":"","text":"it worked again","kind":"pattern","repeats":"pat-001"}
{"id":"les-002","date":"2026-11-03","ticket":"3601","where-it-lives":"gate","breaks":"les-001","text":"the lesson, again","at":"2026-11-03T09:30:00Z","impact":"high","why":"suspected: the gate reads a stale head","proof":"bridlework-harness@1a24234:core/rust/harness/src/friction.rs:75; session:s-0042; pr:owner/repo#93; cmd:cargo test; ~/notes/x.md"}
{"id":"evt.replan_required.12_example-repo.3_193.16_replan-threshold.2026-w45","date":"2026-11-02","ticket":"t-9.3","where-it-lives":"memory","breaks":"","text":"event replan_required repo=example-repo ticket=193 reason=replan-threshold","source":"event","at":"2026-11-02T00:00:00Z","impact":"medium"}
```

| field | rule |
|---|---|
| `id` | a plain token: lowercase letters, digits, `-`, `_`, `.`; never empty, never `.` or `..`; unique in the inbox |
| `date` | `YYYY-MM-DD` naming a real calendar date (`2026-02-30` is refused; leap years are honoured) |
| `ticket` | a plain token, the ticket that captured the row |
| `where-it-lives` | exactly one of `skill`, `gate`, `memory`, `wiki` (K-1.1; see the table below) |
| `breaks` | empty, or the `id` of a lesson this row says broke again (K-1 rule 3); it must name another row that exists in the inbox, never itself |
| `text` | the lesson, non-empty; one line (a newline inside it is stored escaped, the file stays one row per line) |
| `kind` (optional) | empty (= `friction`), `friction` or `pattern`; anything else is refused naming the field. `friction` is a lesson learned the hard way; `pattern` is a repeatable procedure that worked. Omitted at its default, so every six-key row written before this key existed re-encodes byte-identical. A `pattern` row needs no cause tag in `why` |
| `repeats` (optional) | empty, or a plain row id token. The pointer rules mirror `breaks`: it must name a row that exists in the inbox, never itself; the row carrying it must itself be `kind: pattern`; and the row it names must be a `kind: pattern` row with an empty `repeats` — the chain ROOT — so a chain's repeat count is one unambiguous number. A `pattern` row with no `repeats` is legal: it is the root of its chain |
| `source` (optional) | empty (= hand-written) or `event`; anything else is refused naming the field. `event` marks a row the T-9.3 mapper produced from an event (see "Event rows" below). Omitted at its default, so every row written before this key existed re-encodes byte-identical. An `event` row needs no cause tag in `why` |
| `at` (optional) | empty (absent) or `YYYY-MM-DDTHH:MM:SSZ`: UTC, a real calendar instant (hour < 24, minute and second < 60). **The at/date rule:** its date part must equal the row's `date`, or the row is refused naming `at`. Omitted when absent |
| `impact` (optional) | empty (= unrated) or exactly one of `low`, `medium`, `high`, `blocker`; anything else (`urgent`) is refused naming `impact`. Rank weights are 1, 3, 9, 27 (unrated counts as 1) |
| `why` (optional) | one line, at most 1,000 bytes: the cause, stated as suspected or proven. A newline is stored escaped, as in `text`. Over the cap is refused naming `why`. Optional in a stored row; at `add` a hand-written friction row must hold a cause tag (see "The cause tag" under `harness friction add`), while `pattern` rows, `event` rows and rows already stored are not checked |
| `proof` (optional) | one line, at most 2,000 bytes: pointers separated by `; `. **Proof pointer grammar:** each pointer is exactly one of `<repo>@<7-40 lowercase hex>:<path>[:<line>]` (a commit and a repo-relative path, an optional line number), `session:<id-token>`, `pr:<owner>/<repo>#<n>`, `cmd:<text>` (free text, one line) or `~/<path>`. Anything else is refused naming `proof` (the reason never echoes the pointer) |
| `status` (optional) | what became of the row. One of `fixed`, `guarded`, `ticket`, `lesson`, `no-action`; an absent key reads as `open`. An explicit `"status":"open"` or `"status":""` is refused, so a hand-written default cannot slip in: an open row omits the key. An unknown word is refused naming `status`. Omitted at its default, so a row with only the older keys re-encodes byte-identical |
| `status_ref` (optional) | the pointer that backs the status: the proof pointer grammar and the 2,000-byte cap of `proof`, `; `-separated. A status other than open needs it. A `status_ref` with no status (or with an explicit `open`) is refused. The pointer kinds each status needs are in the table below |

**The status words and the pointer each needs.** Each row of the table is checked by the row rules. At least one
pointer of the named kind must be in `status_ref`. A missing `status_ref`, a pointer of the wrong kind and an unknown
status word are each refused with a reason that names the field.

| `status` | needs in `status_ref` |
|---|---|
| open (key absent) | no `status_ref` |
| `fixed` | a `pr:<owner>/<repo>#<n>` pointer or a `<repo>@<sha>:<path>` pointer |
| `guarded` | a `<repo>@<sha>:<path>:<line>` pointer whose path ends in `friction-controls.txt` |
| `ticket` | a `~/.bridlework/tickets/<path>` pointer or a `pr:` pointer |
| `lesson` | a `<repo>@<sha>:<path>` pointer whose path is under `.sources/bridlework/lessons/` |
| `no-action` | a `cmd:<text>` pointer; the text is the reason |

```
{"id":"les-003","date":"2026-11-04","ticket":"3601","where-it-lives":"gate","breaks":"","text":"the lesson","status":"fixed","status_ref":"pr:owner/repo#101"}
```

`status_ref` goes through the privacy engine like `proof` (section 4). A `status_ref` that pushes the line
over the 16 KiB cap is refused. `harness friction set-status` writes a status (section 2); `friction add` stores a
new row with no status (a new row is open). `rank` and `suggest` list rows of any status, on purpose: a
fixed row can still repeat, and the repeat belongs in its group. Only `friction open` filters by status.

**Privacy behaviour of the evidence keys.** All four pass through the UNCHANGED privacy engine like every other field
(section 4): an unfolded home path anywhere in `proof` — including inside a `cmd:` pointer, which is free text and
guarded only by the engine — is refused `field proof has a privacy hit (home-path)`; a `~/<path>` pointer and a bare
`session:<id>` are stored. Each key is omitted from the canonical line at its default, so every row written before
these keys existed re-encodes byte-identical. `harness friction add` takes `--at`, `--impact`, `--why` and `--proof`.

**What the mapper writes.** An event row (below) now carries `at` (the event's `ts_ms` as a UTC instant, so its date part
equals the row's `date` by construction) and `impact` from one fixed table: `gate_fail` is `high`, `replan_required` is
`medium`, `launch_refused` is `medium`. The mapper still reads no prose and leaves `why` and `proof` empty.

The count of a pattern chain is mechanical, never judged: it is the number of rows whose `repeats` names the root,
counted by the verb, remembered nowhere. When the count reaches N (`friction.pattern_threshold`, the config contract's one
optional key; default 3), `harness friction patterns` lists the chain as a candidate for a reusable control — and listing it creates
zero files under any skill or ticket path (the P8 factory judges what to build, later).

The tag is the ROUTE chosen at capture, nothing more (K-1 rule 2):

- `skill` — a repeatable procedure;
- `gate` — a machine-checkable never-break rule;
- `memory` — an operational lesson (before P3, this inbox row is its home);
- `wiki` — project knowledge.

### Event rows (T-9.3)

The event-to-row mapper (`friction_event` in the crate, ticket 193) translates one event-log line into ONE
inbox row over a fixed, closed list of deterministic event kinds — no prose is ever read. The list, and the
signature (the CAUSE a repeat of which files no second row that week) per kind:

| event kind | maps? | signature |
|---|---|---|
| `replan_required` | yes | `repo`, `ticket`, `reason` |
| `launch_refused` | yes | `launch_role`, `matched_entry` (`brief_sha256` is left out: it differs per brief while the cause stays the same) |
| `gate_fail` | yes | `repo`, `ticket`, `gate_id`, `failure_class` (`head_sha` is left out: it changes with every fix push while the cause stays the same; `result` is constant `fail`) |
| the other twelve kinds | no — zero rows | — |

A sweep `blocked` is not an event kind today, so it cannot map yet (a deferred follow-up to
ticket 193); a gate FAIL maps through `gate_fail` (written by `harness ticket gate ... --log <absolute-path>`, see `docs/event-log.md`). The mapper is pure: it never opens or writes the inbox — `harness friction sweep` (below) feeds it one week of the log and files its rows
through the same checks `add` walks, and because the dedup key below IS the row `id`, the duplicate-id rule of
`add`/`lint` is a second backstop against double-filing.

Every event row carries the constants: `ticket` `t-9.3` (the catcher is the capturer — never the event's own
ticket, which lives in the key and the text, so an event row never changes what a real ticket's `lesson:`
close-check must name), `where-it-lives` `memory`, `source` `event`, `kind` at its default (`friction`),
`breaks` empty, `date` the UTC date of the event's `ts_ms`, and a `text` built only from the kind name and
the signature tokens (`event <kind> <key>=<value> ...`).

The dedup key grammar (the row `id`): `evt.<kind>.` + each signature value as `<byte-length>_<value>`,
joined by `.`, + `.<iso-year>-w<ww>` (ISO 8601 week, Monday-first, the year of the week's Thursday). The
length prefix makes the join injective (tokens may contain `.`, so a bare `.`-join would collide
`a.b`,`c` with `a`,`b.c`); the week part makes every week a fresh row — the same cause twice in one week is
one row (dated the earliest event), Sunday 23:59:59.999 and the next Monday 00:00:00.000 are two. An event
that cannot name a row (torn, invalid, unknown kind, wrong `v`, a `ts_ms` past year 9999) is refused fail
closed; in the batch helper one bad line refuses the whole batch with no rows returned.

## 2. The verbs

The inbox path comes from the config (`friction.inbox`, config contract v3), resolved exactly as `harness config
check` resolves the file: `--config <file>`, else `HARNESS_CONFIG`, no third source, an empty value refused and
never defaulted. The path is never printed.

Exit codes for the whole family: **0** ok; **1** a refusal; **2** a usage or setup error. Nothing is written to
stdout when the exit is non-zero. A refusal names the field and the reason on stderr and **never** echoes the
lesson text, the matched privacy bytes, or the inbox path. A refused write leaves the inbox byte-identical — and
if the inbox did not exist yet, it is still not created.

### `harness friction add`

```
harness friction add --id <id> --date <YYYY-MM-DD> --ticket <t> \
  --where-it-lives <skill|gate|memory|wiki> [--breaks <row-id>] \
  [--kind <friction|pattern>] [--repeats <row-id>] [--source event] \
  [--at <YYYY-MM-DDTHH:MM:SSZ>] [--impact <low|medium|high|blocker>] --why <cause> [--proof <pointers>] \
  --text <lesson> [--config <file>]
```

Appends one row. The checks run before anything is written, in this order: the flags, the row rules
(section 1), the cause tag of `why` (the next paragraph), the fields of the new row through the same privacy
engine as the rest of the harness, then — under
one exclusive writer lock (section 4) — the same privacy engine over the stored rows and the inbox cross rules
(a duplicate `id`; a `breaks` naming no lesson or naming itself; the `repeats` pointer rules of section 1; the
existing inbox must itself be clean, untorn and privacy-clean). Only then is one whole line (plus newline) appended and synced; the file is created when
absent, its folder is not; a refused row never creates the file (a first row whose only possible refusal is a
`breaks` with nothing to name is refused before the file is opened). On success stdout is `stored id=<id>`. The new row is scanned before any cross rule
runs, so a refusal can never echo an unscanned byte of it. `--source` accepts only `event` (anything else is
a row refusal naming the field); the flag exists so the T-9.3 mapper's rows are fileable exactly as the
weekly sweep (`harness friction sweep`) files them.

**The cause tag.** A hand-written friction row — `kind` empty or `friction`, `source` empty, a new row or a
repeat filed with `--breaks` — must hold its cause in `why`: the FIRST `cause:` of the why (the byte before it
is not checked), then one space, then `Orchestrator`, `builder` or `system` byte for byte (letter case
included), then the end of the line or one byte that is not an ASCII letter, digit, underscore or hyphen. The
text before and after the tag is free (`the brief was short. cause: builder.` holds a tag), a later `cause:` is
not read (`cause: ticket; cause: system` holds no tag), and a wrong case (`cause: Builder`), no space
(`cause:builder`), a second space or a non-breaking space holds no tag. A why that holds no tag is refused:
exit 1, two stderr lines (`harness: friction refused: why must hold a cause tag: cause: Orchestrator, cause:
builder or cause: system` and `harness: ROW NOT RECORDED`), the inbox byte-identical and still not created when
absent. The check runs after the row rules and before the privacy list is loaded, and it reads only the why of
the new row: a tag in `--text` or `--proof` does not count. A `kind: pattern` row and a `--source event` row
need no tag, rows already stored are not re-checked, and `friction lint`, `sweep`, `suggest` and the other
verbs are unchanged by this rule.

**Recording a repeat.** A friction that happened again is filed as a new row with `--breaks <id of the first row>`;
`harness friction rank` groups rows by that `breaks` chain, so the repeat counts toward the first row's group.
`--repeats` is for rows of `kind: pattern` only (a pattern chain, counted by `harness friction patterns`): a row
carrying `repeats` that is not `kind: pattern` is refused.

A missing required field flag (`--where-it-lives` absent) is a row refusal (1) naming the field, not a usage
error: K-1.1 counts "no tag" as refused. With no privacy list configured the verb fails closed (exit 2) — an
unscanned row is never stored, and a usage error never echoes the raw flag or argument bytes either: it names
the problem, not the value, on every verb's unknown-option and stray-positional paths. One refusal names a
value by contract: a CONFIG refusal may name a lowercase token-shaped key (never a value), because that is the
shared config contract (`docs/config-contract.md`) — so the never-echo claim here is scoped to the friction
verbs' own arguments, rows and note lines.

### `harness friction lint`

```
harness friction lint [--config <file>]
```

Lints the whole inbox with the same rules: per-row rules with 1-based line numbers, then the cross-row rules
(duplicate `id`, a `breaks` naming no lesson, a `breaks` naming its own row), then the same privacy engine over
every field of every row — a hand-written row holding private text is refused exactly like a new row would be
(line, field and class on stderr, never the bytes). Exit 0 clean (stdout `inbox ok rows=<n>`); 1 any bad row, a
privacy hit or a torn tail, each named with its line on stderr. With no privacy list configured lint fails
closed (exit 2): an inbox is never linted clean unscanned. Hand-written rows from before this contract and the
154b weekly script share this one checker. A missing inbox is a setup error (2): `lint` is how a real inbox is
audited, so it must exist. A recovery copy next to the missing inbox gives the recovery refusal (1) instead.

### `harness friction open`

```
harness friction open [--config <file>]
```

Lists the rows whose status is open (an absent `status`). It runs the full lint first, as `rank` does: a torn tail,
any bad row or a privacy hit exits 1 with nothing on stdout. A missing inbox or no privacy list is exit 2. On exit 0
stdout is one line per open row, in file order, then one count line:

```
open id=<id> date=<date> impact=<impact or -> ticket=<ticket>
rows open=<n> fixed=<n> guarded=<n> ticket=<n> lesson=<n> no-action=<n> total=<n>
```

The word `rows` marks the count line as per row. The `open=<n>` of `triage` counts groups. The open lines hold no
`text`, `why`, `proof` or `status_ref`. The verb writes nothing.

### `harness friction set-status`

```
harness friction set-status --id <row id> --set <fixed|guarded|ticket|lesson|no-action|open> [--ref <pointers>] [--config <file>]
```

Sets the `status` and `status_ref` of the one row with that id, in place, and changes nothing else. It is the
verb that rewrites a line of the inbox. The lines it does not change are written back from their original bytes
(a row that is valid but not canonical stays as it is). The changed line is the canonical encoding of the stored row
with the new status. `--set open` removes both keys and refuses a `--ref`. Any other status needs a `--ref` that the
status rules of section 1 accept. A row that already has a status gets both keys replaced. A set equal to the stored
values writes nothing and exits 0. On success stdout is one line: `status set id=<id> status=<word>`.

Exit 1 is a refusal. The inbox is byte-identical, stdout is empty and stderr is one line. It says the status was not
set, except in the recovery copy refusal, whose line is the shared recovery line and names the copy instead. The line names at most a line number, a field, a privacy class and byte counts (the line cap refusal prints two numbers, the row size and the cap; the inbox cap refusal prints the cap). It holds no row, no `--id`, no `--ref` and no path.
The refusals are: no row with that id (a missing inbox included, and the file is not created; a recovery copy next to a
missing inbox gives the recovery refusal below instead); an `--id` that is not
a plain token; a problem the full read finds (a torn tail, a bad or CRLF line, a duplicate id, a privacy hit in a
stored row, the same read as `add`); a privacy hit in the new `--ref` (scanned first, under the scope
`friction.set-status`); a status and pointer pair the status rules refuse; a new line over the 16 KiB line cap; a new
inbox over the 4 MiB cap; a hard-linked inbox; a recovery copy next to the inbox (below). Exit 2 is a usage or
setup error, or a failed write after the recovery copy exists (see the crash windows below): a missing or repeated flag, an unknown flag, an extra word, no config, no privacy list, an inbox that
cannot be read or written, an inbox over the cap or not UTF-8, a lock busy past the wait, a folder that cannot take the
recovery copy. A setup error leaves the inbox unchanged. It leaves no recovery copy, with one exception: when the
removal of a partly made copy fails, the copy stays and the exit 2 line names it (below).

**How the write goes.** The flag checks, the scan of the new `--ref` and the status rules run first, before the lock.
Then one exclusive lock on the inbox file covers the read, the stored-row checks, the id lookup, the hard-link check,
the line cap check, the inbox cap check and the write. Those checks have passed before the recovery copy is made. Then
the verb creates `<inbox>.status-write` (exclusive create, with the inbox's permission bits) in the real file's
folder, writes the whole old inbox to it and syncs the copy and the folder. If the copy cannot be made, or its
permission bits cannot be set, no copy is left: a partly made copy is removed, and when that removal fails the exit 2
line names the path of the copy. Then the verb truncates the locked file, writes the new content and syncs. Then it
removes the copy and syncs the folder. If the removal succeeds and only the folder sync fails, the exit 2 line says the
status was written and the copy was removed. The file keeps its inode: no temp file is renamed onto the path, so a
symlink to the inbox sees the new content. A hard link would keep the old bytes under the other name, so a
hard-linked inbox is refused. The recovery path is the canonical inbox path plus `.status-write`, so a symlinked file
or folder spelling of the inbox path finds the same copy.

**Crash windows.** A failure after the copy exists leaves the copy in place and exits 2, except the folder sync failure
after the copy was removed (above). A kill has the same effect as a failure. The verbs that read the inbox check for the
copy after they hold their lock, and refuse while it exists (exit 1; exit 2 for `close-check`), with one fixed stderr
line that names the recovery copy and no path. When the inbox file is missing, there is no lock to take, and the
copy is still reported the same way: `lint`, `open`, `patterns`, `close-check`, `add`, `sweep`, `set-status` and the
verbs that read through the same reader give the recovery line, and none of them creates the inbox. A `sweep` week that
maps no row reads no inbox and does not check for the copy. The windows are:

| window | state on disk | how it shows |
|---|---|---|
| while the copy is written | inbox whole, copy partial | the copy exists |
| after the truncate | inbox empty, copy whole | the copy exists |
| a partial write that ends on a newline | inbox missing rows, copy whole | the copy exists |
| a partial write that ends mid-line | inbox with a torn tail, copy whole | the copy exists |

To recover by hand, compare the two by row id. Keep the inbox only when it lints clean and holds every row id of the
copy. Otherwise put the copy back as the inbox. Then move the leftover file aside with `mv`. Length alone is not a
test: a partial write can lint clean, and a grown `status_ref` line can outweigh a missing tail. The syncs use `File::sync_all`, which is `fsync` and, on macOS, `F_FULLFSYNC`. This is a recovery aid, not a
claim that the write is all or nothing.

### `harness friction patterns`

```
harness friction patterns [--config <file>] [--record <file>]
```

The N-threshold listing (ticket 192). It runs the FULL lint first — any lint failure is the same exit and the same
stderr as `friction lint`, with nothing on stdout — then prints one line per chain-root pattern row whose count of
`repeats` rows is >= N, sorted by id:

```
pattern id=<id> repeats=<n>
patterns ok threshold=<N> listed=<k>
```

Ids and counts only: a row's `text` is never printed, and the ids it prints have all passed the same privacy
engine as the lint. N is `friction.pattern_threshold` from the config (the one optional key in the config
contract; absent = 3). Being listed creates NOTHING: no skill, no ticket, no file under any skill or ticket path —
the listing is a candidate list for the P8 factory, which judges what to build.

The verb writes nothing, with one exception: when `--record <file>` is given, exactly one JSONL line is APPENDED
to that file — `{"date":"YYYY-MM-DD","threshold":N,"rows":<rows>,"listed":["<id>",...]}` (`schemas/friction-trial.schema.json`
is the shape; the date is the run's UTC day, written by the harness). The trial record is the keep-or-remove
evidence for this feature: if four weekly records show nothing ever reaching N, Governance removes `kind` and
`repeats` (the 4-run ablation decision is Governance's, not a verb's). Exit codes as the family: 0 ok; 1 the lint
leg refused; 2 a usage or setup error — including a failed record append, so a half-written trial never reads as
a clean run. The record path is checked by the guard below, and `--record` may not name the inbox.

**The `--record` guard** (one function, used by `patterns`, `rank` and `suggest`). It runs after the config is
loaded and before the privacy list is loaded and before anything is read, ranked, printed or written. Refused in
this order, exit 2, stdout empty, one stderr line `harness: error: <message>`, nothing created or appended; the
messages are constant and no path bytes are echoed:

1. a path that does not start with `/`: `--record must be an absolute path`;
2. a path that cannot be resolved (a symbolic link whose target does not resolve, a missing file in a folder that
   cannot be canonicalised, a missing path that ends in `..` or has no file name): `--record cannot be resolved`
   (an existing path that ends in `..`, such as `<existing folder>/..`, is a folder and falls under (5));
3. the friction inbox: `--record must not name the friction inbox`;
4. the `--config` value and the `HARNESS_CONFIG` value (both, when both are set), and the `HARNESS_PRIVACY_LIST`
   and `HARNESS_PRIVACY_OLD_LIST` values when set and not empty, also when the verb does not read the list file
   (with `HARNESS_PRIVACY_LIST_SECRET` set): `--record must not name a config file`; a protected value that is
   set, not empty and cannot be resolved (for example a dangling symbolic link) is refused with `--record cannot
   be resolved`, because it cannot be compared; a value that is not UTF-8 is compared as it is;
5. the file type is read with `symlink_metadata` before any open or read: an existing path that is not a regular
   file (a folder, a FIFO, a device such as `/dev/null`) is refused without being opened, so a FIFO cannot block
   the verb; then an existing non-empty regular file whose first byte is not `{` is refused, and so is a regular
   file that cannot be opened or read: `--record names a file that is not a JSON-lines record`. The guard checks
   only the first byte, and the same text is used for a folder, a FIFO, a device and an unreadable file.

**How a path is compared.** A relative protected value is taken against the working folder. When the path is
found (a link counts, also a dangling one) it must canonicalise, else it is refused as (2); when it is not
found, its folder must canonicalise and it needs a file name, and the resolved form is that folder plus the
name. A protected value that does not resolve is refused as (2) after the match test. A protected value matches the record when the inbox check says so (same text, same device and inode, or
both canonicalise to one path) or when the two resolved forms are equal, so a symbolic link, a hard link, a
`<folder>/sub/../config.yaml` form, a relative list value and a protected path that does not exist yet are
matched too (the missing file is not made). On a case-insensitive file system a case variant of a name is the
same file and is caught by the inode test.

**The shape rule.** The engine is not handed the config file of a wrapper. The record lines of `patterns`,
`rank` and `suggest` are each one JSON object, so a record file that exists and is not empty starts with `{`;
a key=value config, a YAML config or a deny list does not and is refused by rule (5). What it misses: any
non-empty file that starts with `{` and is none of the files of (3) and (4), for example the `friction triage
--state` file (JSON lines), another JSON-lines record or log, the inbox of another config, or a JSON settings
file; the files of (3) and (4) are caught by path whatever they hold. Other limits: the check and the open are
two steps, so a file swapped in between is not seen. A record file whose first byte is not `{` must be
moved aside.

### `harness friction close-check`

```
harness friction close-check <close-note> --ticket <t> [--config <file>]
```

K-1.5: at ticket end, the close note names each lesson the ticket captured, or says none. Exit 0 the note matches
the inbox (stdout `close-check ok ticket=<t> lessons=<n>`); 1 refused (stdout empty); 2 a usage or setup error. The
`--ticket` value is a plain token and is also run through the privacy engine BEFORE anything is printed: a hit exits
1 with `field ticket has a privacy hit (<class>)` on stderr (the class only, never the bytes) and nothing on stdout,
so the success line only ever carries a ticket that passed the scan. A
missing inbox reads as no rows (a fresh install), so only `lesson: none` can pass; a damaged inbox (torn, bad
rows, unreadable, a privacy hit in a stored row) fails the check closed (2), because it cannot be compared
against. The `lesson:` lines themselves go through the same privacy engine before the check, so an id the check
names back on a refusal has always passed the scan; with no privacy list configured the verb fails closed
(exit 2), exactly like `add` and `lint`.

Orchestration runs this by hand at the S-1.6 session-removal step until P1b automates removal (154c); a non-zero
exit blocks removal.

### `harness friction sweep`

```
harness friction sweep --log <event-log> --week <iso-year>-w<ww> [--config <file>]
```

The weekly sweep (ticket 200): it feeds the T-9.3 mapper one week of the event log and files its rows. The
whole log is read no further than 4 MiB (section 4) and checked by the event reader's own parser: a final
fragment with no newline is a torn tail, a complete-but-invalid line is corruption, and either refuses the
whole sweep (exit 1, `the event log has a torn tail at line N; nothing was filed` / `the event log is
corrupt at line N; nothing was filed`) — one bad line files no rows. The mapped rows whose id ends
`.<iso-year>-w<ww>` then walk the same door `add` walks: every field of every row through the privacy
engine, then one exclusive writer lock (section 4) holding the same stored-row checks as `add`, a skip of
each row whose id is already stored, the cross rules, and only then the appends — all checks before the
first append, so a refused sweep leaves the inbox byte-identical and never creates a missing one (a sweep
that would file zero rows does not open it). On success stdout is one `stored id=<id>` per filed row (id
order), then `sweep week=<week> filed=<n> already=<m>`.

`--log` and `--week` are required and never defaulted: no wall clock runs in this verb, so every run is
deterministic and any past week can be re-swept. The week grammar is exactly the dedup-key suffix
(`<iso-year>-w<ww>`, digits only); a bad or absent value is a usage error (2) whose value is never echoed.
Because the id carries the week and already-stored ids are skipped (not refused), a second run in the same
week exits 0 with `filed=0` — and the inbox's own duplicate-id refusal stays the second backstop. A missing
or unreadable log, a log over the cap, or no privacy list configured is a setup error (2), nothing filed.

### `harness friction rank`

```
harness friction rank [--top <k>] [--min-occurrences <n>] [--record <file>] [--config <file>]
```

The ranked list of friction GROUPS (ticket 3601). It runs the FULL lint first (any lint failure is the same exit and
stderr as `friction lint`, nothing on stdout). Grouping is by the **transitive `breaks` closure**: `breaks` may name
any other row, so chains, forward pointers and cycles are all lint-legal, and the walk follows them iteratively.
- `root(r)` is `r` when its `breaks` is empty, else the root of the row it names. A walk that revisits a row is a
  cycle: rank refuses, exit 1, stderr `rank: breaks cycle through <id>` (the lowest id on the cycle), empty stdout.
- the **group key** is the root's *signature* (the id before `.<yyyy>-w<ww>`) when the root is a `source: event`
  row, else the root's id. Every row lands in exactly one group, so `les-001 <- les-002 <- les-003` (each breaking
  the one before) is ONE group `les-001`, break count 2.
- **occurrences**: a hand group = its closure size; an event group = its distinct event weeks plus the hand rows
  that break into it; a group rooted at a `kind: pattern` row = its break count (the pattern root is a success, not
  an occurrence). A pattern-rooted group nothing broke is not ranked (`patterns` lists it).
- **score** = weight of the group's highest-rated row (unrated 1, `low` 1, `medium` 3, `high` 9, `blocker` 27) x
  occurrences. Order: score desc, last-seen date desc, group id asc (a total order).

Output is ids and numbers only, never `text`, `why` or `proof`:

```
rank group=<id> score=<n> impact=<low|medium|high|blocker|unrated> occurrences=<n> first=<date> last=<date>
rank ok groups=<n> listed=<k>
```

`--top` defaults to `friction.triage_top` (config contract, the second optional key; absent = 3). `--record <file>`
appends one JSONL trial line `{"date","groups","listed":[ids]}` BEFORE anything is printed, with the same failure
rule as `patterns --record` (exit 2; the record may not name the inbox, and the `--record` guard under `patterns` applies).

`--min-occurrences <n>` (a positive whole number, parsed like `--top`; `0`, `abc` or an empty value is exit 2 with
nothing on stdout) lists only groups whose `occurrences` is `n` or more. The filter runs first and `--top` then caps
what is left. Without the flag every group passes, so the output is unchanged. The filter never changes the counts
of the whole inbox: in `rank ok groups=<n> listed=<k>`, `groups` stays the number of ALL ranked groups and `listed`
is the number of `rank group=` lines printed. The `--record` line follows the same split: `groups` is the number of
all ranked groups and `listed` holds only the ids of the groups printed.

### `harness friction triage`

```
harness friction triage --week <iso-year>-w<ww> --state <file> --handoff-dir <dir> \
  [--log <event-log>] [--closed <file>] [--controls <file> --lessons-dir <dir>] \
  [--as-of <YYYY-MM-DDTHH:MM:SSZ>] [--top <k>] [--config <file>]
```

The loop's one deterministic step: no wall clock runs, the week is an argument. In order: (1) with `--log`, the
existing `sweep` for that week (the same function); (2) lint + rank; (3) the `--state` file, an append-only JSONL of
nine string keys (`group`, `status` of `proposed|resolved|regressed|escalated`, `ticket_file`, `proposed_week`,
`task_id`, `task_session`, `resolved_pr`, `resolved_date`, `attempts`; the last row per group wins), read under the
4 MiB cap; a torn or bad state refuses (exit 1) and nothing is written; (4) with `--controls <file> --lessons-dir
<dir>` (both flags together, either one alone is exit 2, after the state and the closed feed are read; both inputs
are read before the `--log` sweep, so a controls refusal leaves the inbox unchanged) each line of the controls file: blank
lines and lines that start with `#` after a trim are skipped (a control character other than a tab, a CR that does not end the line, or non-UTF-8 bytes
refuse any line, a comment too), other lines are three parts split on ` | ` — a group id with no white space,
`control: <pointer>` or `not machine-checkable: <reason>` with non-blank text, and `lesson: ISSUE-NNN` (exactly
three ASCII digits) naming a regular file `ISSUE-NNN-*.md` (a symbolic link or a folder does not count) directly in
the lessons folder. A bad line refuses the run (exit 1): stderr names the line number, no line text, and the state
and the hand-off folder stay byte-identical. A missing controls file, a folder as the controls path, a controls
file over 1 MiB or a missing lessons folder is a setup error (exit 2) with nothing written. The engine is stricter
than `bw start` here: a control character other than a tab, or a lone CR, inside a line refuses; the file cap is
1 MiB; no refusal or note prints line text. A valid line whose id is a group root with no state row seeds one
`resolved` state row: `task_id`, `task_session`, `resolved_pr` and `ticket_file` empty, `attempts` 0,
`proposed_week` the `--week` value, and `resolved_date` the latest `at` among the member rows on the group's last
date, else that date. The group is not proposed in that run, and a seeded group is not seeded again, also after it
regresses. A group whose last row is `proposed` with an empty `task_id` and `attempts` 0 is seeded the same way and
keeps its `attempts`, `proposed_week` and `ticket_file`; a group in another status, `proposed` with a `task_id`,
or `proposed` with `attempts` 1 or more, gets no row. A line whose id is a row inside another group prints
`control-not-root line=<n> group=<root id>`; an id that is no row prints `control-unknown line=<n>`. Both note
lines seed nothing, print no text of the line, and do not change the exit code; (5) with `--closed` (a JSON object whose
`closed_with_evidence` array holds ticket-reconcile rows `id`, `repo`, `at` and `pr` (digits) or, when `pr` is absent or an empty string,
`head` (7 to 40 hex characters), plus an optional `session`; a row with neither, or a `pr` that is not a string or a
number (`true`, an object, an array, `null`), refuses the whole feed, exit 1; `resolved_pr`
becomes `<repo>#<pr>` or `<repo>@<head>`; `at` as `YYYY-MM-DDTHH:MM:SSZ` is kept whole as `resolved_date`, a date-only
`at` is kept as the date; any other `at` (`T10:00:00z`, an offset such as `+08:00`, a fraction such as `T10:00:00.5Z`, `T25:00:00Z`,
`2026-11-05junk`) refuses the whole feed, exit 1) a `proposed` group resolves ONLY when its bound `task_id` equals a row's `id` (and, when both
carry a session, the sessions match); the hand-off id is not compared, and a group with no bound task is not resolved by the feed;
(6) a `resolved` group with a member row dated after its `resolved_date` becomes `regressed` (`attempts` + 1; when
`resolved_date` is a full time and the row has an `at`, the two times are compared, so a later `at` on the same day
regresses and an earlier one does not; otherwise the row's date is compared with the date), and
at `attempts >= 2` becomes `escalated` instead, which is never auto-proposed again; (7) proposals: every `regressed`
group (first), every group with break count >= 2 (always, regardless of rank) and the top K ranked groups, minus
groups already `proposed`, `resolved` or `escalated`; (8) one ticket-contract file `<handoff-dir>/<week>-<n>.json`
per proposal (id `friction-<week>-<n>`, owner `friction_learning`, size `M` / `L` and risk `normal` / `high` for a
`blocker`), each round-tripped through the ticket reader before any write; (9) the state rows are appended under the
exclusive lock (section 4) and ids/numbers are printed:

```
seed group=<id> line=<n>
control-not-root line=<n> group=<id>
control-unknown line=<n>
propose group=<id> file=<basename> reason=<rank|twice-broken|regressed>
open group=<id> age_weeks=<n> bound=<yes|no>
signature group=<id> weeks=<n>
pattern id=<id> repeats=<n>
escalate group=<id> attempts=<n>
triage week=<w> proposed=<n> open=<n> resolved=<n> escalated=<n>
```

`open` lists every `proposed` group with its age (`bound=no` makes a forgotten bind visible each week);
`signature` lists an event signature seen in 2 or more weeks; `pattern` is the `patterns` listing; `proposed`
counts this run's proposals, `resolved` its feed resolutions (a seed is not counted), `open` and `escalated`
the standing groups. A second run for the same week proposes 0. Every check precedes every write: a refused run
leaves the state and the hand-off folder byte-identical.

**The `--as-of` numbers.** `--as-of <YYYY-MM-DDTHH:MM:SSZ>` (a UTC instant; a bad value is exit 2 and a value
given twice is exit 2) appends the hand-over numbers after the `triage week=` line (ids, counts, status words and the `--as-of` value; no row text):
one `unsettled group=<id> rows=<n> rows_over_24h=<k> age_hours=<h>` line per ranked group with no state row
(a group seeded or proposed in the same run has a state row), one `after group=<id> status=<s>
bound=<yes|no> rows_after=<n>` line per group with a non-empty `resolved_date` anywhere in its history
(`rows_after` counts member rows later than the LAST such date, `bound` is the page's bound rule), then the
closing `due as_of=<v> unsettled=<n> unsettled_rows=<n> unsettled_over_24h=<n> after_ge3=<n>
after_ge3_unbound=<n> escalated=<n> state_no_rows=<n>` line (a state group with no inbox row counts in
`state_no_rows` and prints no other line). A row's instant is its `at`, else 00:00:00Z of its date; the age of
a row is `--as-of` minus that instant in whole hours, and `rows_over_24h` counts member rows 24 hours old or
more. `--as-of` is not compared with `--week` and writes nothing beyond the run's own writes. An inbox row
whose instant is later than `--as-of` refuses the run (exit 1, `--as-of is before row <id>`, the state and the
hand-off folder byte-identical; with `--log` the sweep runs first, so the swept row is in the inbox).

### `harness friction controls-check`

```
harness friction controls-check --controls <file> --lessons-dir <dir> [--config <file>]
```

Read only: nothing is written. It refuses while a repeat group has no valid line in the controls file. The first step
that fails decides: (1) the flags (exit 2); (2) `--controls` or `--lessons-dir` missing (exit 2, one line naming the
flag); (3) the config (exit 2); (4) the privacy list, as `rank` (exit 2); (5) the controls file and the lessons folder
through the same reader as `triage --controls` (setup errors exit 2, a refused line exit 1); (6) the inbox lint, as
`rank` (exit 2 for an unreadable inbox, exit 1 for a lint, torn-tail or privacy problem); (7) the grouping (exit 1,
the `rank: breaks cycle through <id>` line); (8) the check.

**The rule.** A repeat group is a ranked group (one that `rank` counts in `groups=`) whose `occurrences` is 2 or more (a group rooted at a
`kind: pattern` row counts its breaks, not its root; a pattern nothing broke is not ranked). It is covered when at
least one valid controls line has a first part byte-equal to the group id. A valid line that names a member row that
is not the root, an event row id, or no row covers nothing and is not reported.

Output. Exit 0: stdout is one line, stderr is empty:

```
controls-check ok groups=<ranked groups> repeat=<repeat groups> lines=<valid controls lines>
```

Exit 1 and 2 leave stdout empty. When repeat groups are not covered the exit is 1 and stderr is one line:

```
harness: friction refused: repeat groups with no valid line: <ids in byte order, joined by a comma and a blank>. Add one line per group to the controls file: <the line shape of the triage section>
```

It holds no line text and no path. `lines=` counts the valid lines, so two equal lines
count twice.

Where the private wrapper's python check and this verb differ (the wrapper is not edited here):

1. A bad line: the wrapper collects all bad lines and the missing groups in one message; the verb stops at the first
   bad line, exit 1, also for a group seen once.
2. A carriage return inside a line: the wrapper splits the line in two; the verb refuses the line (stricter). A CRLF
   line end is read by both.
3. A control character other than a tab: the wrapper reads it; the verb refuses (stricter).
4. The error text: the wrapper echoes the line text and the controls path; the verb prints line numbers, group
   ids and fixed text, and no controls line text and no controls path.
5. A controls file over 1 MiB is exit 2 here and has no cap in the wrapper (stricter); a missing lessons folder is
   exit 2 here and exit 1 in the wrapper; a missing inbox file is exit 2 here and exit 1 in the wrapper.
6. A controls file that is not UTF-8 is a named exit 1 here and a python traceback with exit 1 in the wrapper.
7. The wrapper refuses past 100000 ranked groups (its `--top`); the verb has no limit. The 4 MiB inbox cap holds fewer
   than 47000 rows, so the bound is not reachable.
8. The line shape, the three-digit lesson number and the lesson page rule (a regular file `ISSUE-NNN-*.md` directly in
   the folder; a symbolic link or a folder does not count) are the same in both.
9. A form feed or U+0085 inside a line: the wrapper reads it; the verb refuses the line as a control character
   (stricter).

Apart from item 7, none of these makes the verb pass where the wrapper refuses. A U+FEFF in front of the first line
is part of its first part in both: a valid line then names an id that begins with U+FEFF and covers no group, and a
comment line there is a malformed line.

### `harness friction bind`

```
harness friction bind --state <file> --group <id> --task-id <id> --task-session <token> [--config <file>]
```

The one writer of the task binding: it appends one state row (a copy of the group's last row, or a fresh `proposed` row, see below) with `task_id` and
`task_session` set, so the closed feed can match the bound id.
`--task-id` is ASCII letters, digits, `.`, `-` and `_`, starting with a letter or digit, at most 64 bytes (for example `admission-two-lanes`).
A group with inbox rows and no state row is filed as one `proposed` row bound to the task, with an empty
`ticket_file` and the ISO week of its last row as `proposed_week` (the inbox is read, so the config is needed).
Bind does not check that the task fixes the group; a repeat after the resolve still regresses it.
Refused (exit 1, state byte-identical): a group with no state row and no inbox row, a last status other than
`proposed`, a binding to a different task, a `--task-id` outside that set, a `--task-session` that is not a plain
token, a privacy hit in the group, the task id or the session, and, on the fresh-row path, an inbox that fails lint or has a
privacy hit and a new row the state reader would refuse (for example a last row dated 9999-12-31). A `--config` that
cannot be loaded exits 2; on the fresh-row path a missing or unreadable inbox file exits 2 and, with no `--config`, so does a missing or unloadable config. Re-binding the same pair prints `bind already group=<id>`; success prints `bind group=<id> task=<id>`.

### `harness friction page`

```
harness friction page --week <iso-year>-w<ww> --state <file> --out <file> [--config <file>]
```

Writes the weekly report page to `--out` only: front-matter `kind: note`, `updated`, `sources`, `verified: no`, the
title `# Friction triage <w>`, and one `## <group>` section per `proposed` or `escalated` group listing each row's
`at`, `impact`, `text`, `why` and `proof`. Stdout is exactly `page written groups=<n>`, so the lesson text never
reaches a printed list (K-1 rule 8). `--out` may not name the inbox or the state file.

## 3. The `lesson:` close-line grammar

- A close-line is a note line whose trimmed form begins `lesson:` (case-sensitive; where it sits in the note does
  not matter — the check is mechanical).
- Exactly one close line per note: none is refused, two are refused.
- The value is `none`, or a comma-separated list of lesson ids (`lesson: les-001, les-002`); an empty list, an
  empty element, a duplicate id or a non-token id is refused.
- `lesson: none` passes only when the inbox holds NO row with this ticket.
- `lesson: <ids>` passes only when the named ids are exactly the inbox rows whose `ticket` is this ticket:
  an unknown id, an id belonging to another ticket, or a captured row missing from the line is refused.

## 4. Privacy, caps, torn tails

- Every field of a new row is scanned by the existing privacy engine (`harness privacy`'s classes); a hit refuses
  the row naming the field and the class, never the bytes. These verbs scan the stored rows: `add`, `set-status`, `lint`,
  `open`, `patterns`, `close-check`, `rank`, `controls-check`, `triage`, `page`, `suggest`, `sweep` (when the
  week maps at least one row), `bind` (for a group with no state row) and `lesson replay` (with its answer key). `close-check` scans its `lesson:` lines too.
  The scan covers `status` and `status_ref` as it covers `proof`. `set-status` also scans its new `--ref`.
- One writer at a time: `add`, `sweep` and `set-status` take ONE exclusive kernel advisory lock (`flock(2)`, the std
  file lock) on the inbox file itself, and do the read, scan, cross check and write through that same open
  descriptor, so two concurrent adds do not both store the same id (the check and the write are one critical
  section). The readers that go through `read_inbox` take the SHARED lock on the file they
  read, for the length of the read, and release it when the read returns (`suggest` holds no lock across its Jev
  call). `read_inbox_locked` takes no lock of its own: it reads on a descriptor its caller has already locked
  (shared inside `read_inbox`, exclusive in `add`, `sweep` and `set-status`). A reader waits for a writer, and a writer waits for the readers. Readers do not wait
  for each other. The lock lives on the inbox's inode, so the spellings named here share it: a symlinked
  folder, a symlinked file, a `./` or doubled-slash detour, a hard
  link, and a not-yet-created file behind a symlink (the first append creates the target and locks it). `set-status`
  refuses a hard-linked inbox, because it rewrites the file in place. There is
  no side lock file and no path built for locking. The recovery copy path (above) is built from the
  canonical inbox path. The kernel releases the lock the moment
  the holder exits — return, panic, or a kill -9 — so a crashed writer does not leave the inbox locked: there is
  no steal rule and no stale-lock age. A file or folder named `<inbox>.lock` is not a lock and is simply
  ignored (not read, locked, renamed or deleted: the friction verbs delete nothing they did not create). A
  lock held longer than 10 seconds by a live writer fails the verb closed (exit 2, `the inbox is busy`, no
  path); a reader waits the same 10 seconds and then exits 2, with `the inbox is busy (it is locked)` because the
  holder may be a reader or a writer. The lock calls (`File::try_lock` and
  `File::try_lock_shared`) need Rust 1.89 or newer; the workspace pins 1.99.1. The lock is advisory, so it protects
  against other harness verbs, not against an editor that ignores it. The engine's `close-check` reads under the shared
  lock. The private `bw` script reads the inbox without the lock through one function, `fr_rows`, which three steps use.
  `fr_rows` lists the rows for the id
  lookup: a row it misses gives a refusal at that step, and a duplicate next id is refused by `add`. `expected_close_line`
  runs `lint` and then reads the rows with no lock held across the two steps. The hint can be wrong in two
  windows of a `set-status` write: the emptied inbox (it prints `lesson: none`) and a read that ends on a newline
  with rows missing (it prints a short id list). The refusal itself comes from the
  engine's `close-check`. A refused row does not create the inbox file.
- Input bounds, all failing closed past the bound: one row line is capped at 16 KiB; the inbox is read no further
  than 4 MiB; a close note no further than 1 MiB; the event log a `sweep` reads no further than 4 MiB (each
  event line is separately capped at 4 KiB by the event reader's own rules). Over the bound the verb refuses
  (1 for a row, 2 for a file; a log one byte over the cap is refused with nothing filed, a log exactly at it
  is accepted).
  `patterns` never writes the inbox at all, and its only output file is the one `--record` line; the listing
  itself cannot loop or grow past its bound (one pass over the already-capped inbox, one line per root).
- Each row is written as one whole line plus newline. A last line with no newline is a torn tail (a crashed
  append): reported by `lint` with its line number, never read as a row; `add` refuses to append onto it.
- A recovery copy next to the inbox (`<inbox>.status-write`, left by a `set-status` write that did not finish) is the
  other crash sign. The verbs that read the inbox check for it after they hold their lock (a missing inbox has no
  lock; the check runs there without one), and refuse (1; `close-check` 2) with one line that names the copy and no
  path, until the copy is moved aside by hand (see `set-status`). `bind` makes the check also for a group that has
  a state row (it reads no inbox there, but it refuses without writing). The check needs a config: `bind` uses
  `--config` or `HARNESS_CONFIG`, and with neither no inbox is named and no check runs.
  `lesson replay` makes it when it builds its answer key (`--config`); without `--config` it reads no inbox.
  An inbox behind a dangling symlink is checked beside the file the link points to, so `add` and `sweep` find a
  copy there and create nothing. An inbox path whose recovery path cannot be built (a link loop, a folder that cannot
  be read) is a setup error for `add`, `lint` and `set-status` (exit 2, nothing on stdout, the inbox cannot be read or
  written), and `close-check` gives the recovery line there (exit 2), since a copy next to the real file cannot be
  ruled out. A missing folder is not that case: no copy can sit in it.

### `friction suggest` (ask A32)

`harness friction suggest --text <t> [--why <w>] [--top <k>] [--before <row id>] [--record <file>] [--config <file>] [--jev-send <program> --jev-key-file <file> --jev-ledger-dir <dir> [--jev-deadline-ms <n>]]`
ranks the existing groups against a row that is not stored yet, so the new row can be filed with `--breaks`.
One line per group, best first: `suggest: rank=<n> group=<group id> row=<best row id> score=<2 decimals> via=bm25`
(default `--top 3`; an empty inbox, or no row before `--before`, prints `suggest: none`; exit 0). The group ids are
the ones `friction rank` prints (a hand or pattern root's row id, an event group's signature); `row=` is always a
stored row id. Free with no `--jev` flag: no model, no network.

- Token: a run of `[a-z0-9]` in the lowercased text after `f-YYYYMMDD-NN`, `issue-N` and `#N` are removed, kept
  when longer than 2 and not all digits. The query is the DISTINCT tokens of `text + ' ' + why`; each ranked row
  is a document over its own `text + ' ' + why`. N, df and the average length are over the ranked rows only (all
  stored rows, or the rows before `--before`), never the query.
- Row score = sum over query tokens of `ln(1 + (N - df + 0.5)/(df + 0.5)) * tf * 2.2 / (tf + 1.2 * (0.25 + 0.75 *
  len/avg))`; group score = its best row's score (max, not sum); a tie goes to the group seen first in inbox
  order. `--before <row id>` ranks only the rows before that row (an unknown id refuses, exit 1).
- The query is scanned by the privacy engine before ranking (a hit refuses, exit 1, naming the field and class,
  never the bytes); a damaged inbox refuses like `rank` (exit 1); no privacy list configured is exit 2.
- Read-only on the inbox. `--record <file>` appends one line `{ts, query_sha256, top, via}` (a second one with the Jev arm, below) (sha256 of
  `text + newline + why`, the top group ids, `bm25`) and never any text; a `--record` naming the inbox, by path, by a symbolic link, or by a hard link (the same
  device and inode), is exit 2. The same check guards `friction rank --record`, and the whole `--record` guard under `patterns` (an
  absolute path, a path that resolves, no config or privacy list file, a new, empty or `{`-led regular file)
  applies to this verb too; the Jev arm's second line goes to the same checked path. The verb exists on unix only.

**The Jev arm** (ticket friction-suggest-jev). Four more flags turn a second ranking on:

```
[--jev-send <program> --jev-key-file <file> --jev-ledger-dir <dir> [--jev-deadline-ms <n>]]
```

The arm is on exactly when `--jev-send` is given; there is no config key and no mode flag. The three path flags
must be absolute, `--jev-send` needs both `--jev-key-file` and `--jev-ledger-dir`, a `--jev` flag without
`--jev-send` is exit 2, and `--jev-deadline-ms` is a whole number from 1 to 600000 (default 12000). With no
`--jev` flag the outcomes documented above are unchanged (stdout, exit codes and record line, byte for byte);
only the usage line and the unknown-option line change.

With the arm on, each step of today runs first and unchanged (the checks and refusals, the BM25 record line,
then the BM25 lines or `suggest: none`), stdout is flushed, and then one more line is printed, the last one (none
when the second record line cannot be written, below; a refusal prints no Jev line). A worked example, three BM25 lines then a pick:

```
suggest: rank=1 group=les-001 row=les-001 score=2.10 via=bm25
suggest: rank=2 group=evt.gate_fail.3_abc row=evt.gate_fail.3_abc.2026-w44 score=2.02 via=bm25
suggest: rank=3 group=les-009 row=les-012 score=0.72 via=bm25
suggest: jev pick=les-001 row=les-001 p=0.62
```

The same run with no credit left ends:

```
(the same three BM25 lines)
jev: skipped reason=no-credit
```

The pick line names the picked group, the best row of that group (the row whose text was sent) and the model's
probability with 2 decimals. The skip line names one token of this closed list:

| token | cause |
|---|---|
| `no-candidates` | fewer than 2 candidate groups are left (an empty inbox, one group only, or the code rule left fewer than 2) |
| `code-skipped` | the `--text` or `--why` value, as given, holds a code fence or an indented line |
| `payload` | the request is over 199999 bytes |
| `scan-refused` | the privacy engine refuses the request as a whole, or the program exited 7 |
| `no-key` | the `--jev-key-file` path names no regular file, or the program exited 5 |
| `ledger` | the ledger path is not a folder or cannot be made, or the program exited 8 without the cap text, 11 or 12 |
| `spawn-failed` | the temp folder or request file cannot be made, the program cannot be started, or a wait error |
| `timeout` | the program did not end within `--jev-deadline-ms` |
| `no-credit` | the program exited 10 and its last `HTTP <3 digits>` line says 402 |
| `http-<3 digits>` | another HTTP status on that line |
| `http-unknown` | exit 10 with no `HTTP <3 digits>` line |
| `transport` | the program exited 9 |
| `cap` | the program exited 8 and its stderr holds `SPEND-CAP reached` |
| `send-<exit code>` | any other exit code (for example `send-6`) |
| `send-signal` | the program was ended by a signal |
| `bad-answer` | the answer file is missing or not the fixed shape |
| `flat` | all probabilities are equal, so the model names no best option |

The verb exits 0 for each Jev outcome, pick or skip; the one exception is a failed write of the second record
line (below), which is exit 2 after the BM25 lines. No byte the program prints reaches the verb's streams, with
one exception: the three digits in `http-<code>`.

- **Candidates.** The groups in the BM25 order (score, then first seen), taken BEFORE the `--top` cut, the first
  20 of them; `--top` changes the printed lines only, and `--before` limits the candidates as it limits the
  ranking. Each candidate stands for one group and is shown by its best row.
- **The clean rule.** The shown text of a row and of the query is `clean(text)`, one line feed, `Why: `,
  `clean(why)` (the Why part is there also when why is empty). `clean` replaces the three id shapes `f-YYYYMMDD-NN`,
  `issue-N` and `#N` (matched in upper or lower case) by one blank, folds each run of 2 or more blanks or tabs
  into one blank, and cuts blanks, tabs and line ends from both ends. The text is not lower-cased: each other
  byte is kept as stored.
- **The letter rule.** With n candidates the letters are the first n of A to T. A candidate's letter is its rank
  in the order of the lower-case hex sha256 of `<query sha256 hex>:<group id>`, smallest first (the query sha256
  is the value the record line holds: sha256 of `text`, a line feed, `why`). For the same query and the same set
  of candidates, each group gets the same letter whatever the inbox order.
- **The request.** One JSON object, no blanks between tokens, no final line feed, keys in this order (strings
  escaped as the row lines are):

  ```json
  {"model":"typesafe/jev-1.13","state":{"query":"<shown query>"},"questions":{"group":{"type":"choice","instructions":"<the fixed 324-byte prompt>","criteria":{"A":"<shown text of the candidate with letter A>","B":"..."}}}}
  ```

  A small real one, for the query `cache warm zeta` with the why `reruns are slow` and two candidates (written on
  one line; each `\n` is the two characters backslash and n):

  ```json
  {"model":"typesafe/jev-1.13","state":{"query":"cache warm zeta\nWhy: reruns are slow"},"questions":{"group":{"type":"choice","instructions":"The query above is a new friction report: a problem someone hit while working, with why it matters. Each option is an earlier friction report that stands for one group of reports about the same underlying problem. Which option describes the same underlying problem that the new report repeats? Choose the single best option.","criteria":{"A":"cold cache on every rerun\nWhy: the build is slow","B":"stale lock file\nWhy: a second run hangs"}}}}
  ```

- **The checks before the run**, in this order; the first that holds prints its skip line with nothing run;
  nothing is made at (1) to (5), and at (7) a missing ledger folder was already made by (6): (1) code in the
  query; (2) candidates with code in their row are left out, and fewer than 2 left is `no-candidates`; (3) the
  byte cap; (4) the whole request through the privacy engine with test mode OFF whatever the run was given (the
  stored rows and the query already passed it field by field before the ranking); (5) the key file must name a
  regular file (a link to a file counts); the engine does not open it; (6) the ledger folder must be there, or
  missing and makable (only the last path part, mode 0700); the engine does not make the ledger file; (7) the
  temp folder and request file must be makable.
- **The send program contract.** The engine writes the request to a fresh folder under the system temp folder
  (mode 0700; `request.json` inside it, mode 0600) and runs exactly:
  `<program> --payload <temp>/request.json --surface prose --spend-ledger <ledger dir>/spend-friction-suggest-<UTC date>.jsonl --out <temp>/answer.json`
  - these 8 arguments and no other. The child runs in the temp folder, in its own process group, with stdin and
  stdout closed to the null device and its stderr read by the engine (the last 4096 bytes are kept). Its
  environment is cleared, then `PATH`, `HOME`, `LANG` and `LC_ALL` pass through when the caller has them,
  `TMPDIR` is the temp folder of the run, and `OPENROUTER_KEY_FILE` holds the `--jev-key-file` value; no other
  name is passed. The engine opens no socket and reads no key file: the program makes the call and only it can
  spend. The program's exit codes and stderr texts the engine reads: 0 the answer counts; 5 no key; 7 scan refused;
  8 `cap` when the kept stderr holds `SPEND-CAP reached`, else `ledger`; 9 transport; 10 the LAST stderr line
  ending in `HTTP ` plus exactly 3 digits (402 is `no-credit`, another code is `http-<code>`, no such line is
  `http-unknown`); 11 and 12 ledger; any other code n is `send-n`; a signal is `send-signal`. The answer file is opened
  on exit 0 only (no other end touches it) and counts only then: at most 1048576 bytes, a regular file (a link does not count), one JSON object (nesting of at
  most 16 levels, no surrogate-pair escape) whose
  `answers.group.probabilities` holds exactly the asked letters (upper case, byte-equal) as JSON numbers from 0
  to 1; the highest value picks the letter, ties go to the earlier letter, and all-equal is `flat`. The `choice`
  field is not read.
- **The deadline.** When the program has not ended `--jev-deadline-ms` after its start, the engine kills the
  whole process group with the KILL signal, reaps the child and prints `timeout`; after a normal end it waits at
  most 1000 ms for stderr to close; when stderr is still open then, the kept stderr counts as empty (exit 10
  gives `http-unknown`, exit 8 gives `ledger`). The engine removes the temp folder and all in it on each path
  after it was made (pick, each skip, timeout, a failed record write); a removal that fails is not reported. Not removed when a signal ends the engine
  itself during the run (the engine has no signal handler): a folder with mode 0700 and a child that runs on.
- **The record.** With `--record`, the line of today is written as today, and with the Jev arm on a second line
  is appended after the Jev outcome is known and before the Jev line is printed:
  `{"ts":...,"query_sha256":...,"via":"jev","status":"pick|skipped","reason":"<token>","letter":"<pick or empty>","group":"<pick or empty>","p":<number or null>,"cost_usd":...,"latency_ms":...}`
  - the same ts and query sha256 as the first line (the clock is read once), no row text, no query text and no
  path. `cost_usd` is an UPPER ESTIMATE, not a booked spend: 0.000000 when the program did not run; the answer's
  `usage.cost` (a number from 0 to 1) when the program ended 0 and the answer holds it; 0.001400 in each other
  case. `latency_ms` is the wall time from the start of the program to its end or its kill (the wait for stderr
  after a normal end is not in it; 0 when the program did not run). The ledger of the send program is the
  truth. An example of a pick (the sha256 is a sample value):

  ```json
  {"ts":"2026-11-20T09:15:00Z","query_sha256":"0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0","via":"jev","status":"pick","reason":"","letter":"B","group":"les-001","p":0.62,"cost_usd":0.000145,"latency_ms":1830}
  ```

  A failed append of the second line is exit 2 with
  `harness: error: the suggest record cannot be written` on stderr, and the Jev line is not printed.
- **What leaves the machine**, through the send program only: the model name, the fixed prompt, the cleaned
  query (text and why) and the cleaned text and why of at most 20 stored rows, one per candidate group. What
  does not: row ids, group ids, scores, the paths of the run (temp, ledger, key file, inbox), the privacy list;
  the key is not read by the engine. A source test fails when one of the two engine files holds `std::net`, a TCP
  or UDP socket type name, or an http or https URL scheme.

Limits worth knowing: the code check is two byte rules (three grave accents, or a line that starts with 4 blanks
or a tab), so code with no fence and no indent still leaves; a query with no word in common with a stored row
(BM25 score 0) still sends its candidates; a run killed at the deadline is not booked in the ledger of the send
program, so the spend cap can be passed by one call per timeout; the letter order rule and the id strip are a
new form of what the trial used (the trial seeded its shuffle by the row id); the wrapper does not pass the flags yet;
a program that ignores its arguments is not detected (the engine reads only its exit code, its stderr and the
answer file); an exit seen at the very edge of the deadline may be read as `timeout` or as the exit; whether the
pick of the model is right is not checked; the send script books its worst-case cost on a non-200 answer, so a
day with no credit reaches its spend cap early, and this verb does not change that; a program that writes
outside its `--out` path is not detected; a background child of the program that leaves its process group is not
ended by the deadline; the answer file is checked as a regular file and then opened, so a program that exited 0 and
left a background child that swaps that file for a pipe in between can make the open wait with no deadline (the
other ends never open it).

## 5. Not in slice 1 (where each piece went)

- The weekly printout (lessons at 2 `breaks`, open tickets with their age, the pattern candidates at N repeats) —
  154b / 194, now BUILT here as `rank`, `triage`, `bind` and `page` (ticket 3601). The generic mechanism lives in this
  public repo (with the `wiki/friction/` templates: the weekly runner, the closed-feed builder, the smoke tests and
  the plist template); the DATA (inbox, state, pages) lives only in the private friction wiki. What remains: the
  operator creating that wiki and installing the schedule (Leg B), and the human review of any `escalated` group.
- Wiring `close-check` into the automated S-1.6 removal step — 154c (until then it runs by hand).
- The generated lessons index, `harness find` over the inbox — slice 2 (K-1 rule 4, X-81a).
- Promotion of lessons to the wiki / W3 Lesson Store, and the tag-vs-`controls` lint (K-1.3) — P3.
- Automatic gate-ticket filing and skills extraction — the P8 factory (a `triage` hand-off file is a ticket-contract
  file the orchestrator files; nothing files itself).
- Migrating existing hand-written inbox rows to this shape — a private-overlay chore by hand (`friction lint`
  names each bad line; no public code touches the file).
