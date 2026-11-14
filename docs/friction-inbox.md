# The friction inbox (K-1 slice 1)

The friction inbox is where the harness writes down what broke while it is being built: one row per friction or
lesson, captured the same turn it is found (T-9). This page is the public contract of the row, the verbs that read
and write it, and the `lesson:` close line. The rule itself is the knowledge-routing rule (K-1, ticket 153); this
ticket (154) builds its first slice: capture first, so the store grows while the harness is built.

**The inbox file is not in this repo.** It lives only in the private overlay (5.2 item 4): lesson text is real
operational text, so it is private data. This repo holds the contract, the reader and the lint — never the rows.

Status: contract plus `harness friction add | lint | close-check` (ticket 154), the pattern half — `kind`,
`repeats`, `friction patterns` and the trial record (ticket 192) — and the T-9.3 event-to-row mapper with
`source` (ticket 193), the four evidence keys and the triage loop `rank | triage | bind | page` (ticket 3601). The generated lessons index (`harness find`) is slice 2; see "Not in slice 1" below.

## 1. The row (the one contract everything else reads)

One JSON object per line: exactly these six keys, every one required (a missing, empty or extra key is refused), plus
seven OPTIONAL keys — `kind` and `repeats` (ticket 192), `source` (ticket 193) and the four evidence keys `at`,
`impact`, `why` and `proof` (ticket 3601) — each omitted at its default:

```
{"id":"les-001","date":"2026-11-01","ticket":"154","where-it-lives":"memory","breaks":"","text":"the lesson"}
{"id":"pat-001","date":"2026-11-01","ticket":"192","where-it-lives":"memory","breaks":"","text":"the procedure that worked","kind":"pattern"}
{"id":"pat-001-r1","date":"2026-11-02","ticket":"192","where-it-lives":"memory","breaks":"","text":"it worked again","kind":"pattern","repeats":"pat-001"}
{"id":"les-002","date":"2026-11-03","ticket":"3601","where-it-lives":"gate","breaks":"les-001","text":"the lesson, again","at":"2026-11-03T09:30:00Z","impact":"high","why":"suspected: the gate reads a stale head","proof":"bridlework-harness@1a24234:core/rust/harness/src/friction.rs:75; session:s-0042; pr:owner/repo#93; cmd:cargo test; ~/notes/x.md"}
{"id":"evt.replan_required.8_ai-brain.3_193.16_replan-threshold.2026-w45","date":"2026-11-02","ticket":"t-9.3","where-it-lives":"memory","breaks":"","text":"event replan_required repo=ai-brain ticket=193 reason=replan-threshold","source":"event"}
```

| field | rule |
|---|---|
| `id` | a plain token: lowercase letters, digits, `-`, `_`, `.`; never empty, never `.` or `..`; unique in the inbox |
| `date` | `YYYY-MM-DD` naming a real calendar date (`2026-02-30` is refused; leap years are honoured) |
| `ticket` | a plain token, the ticket that captured the row |
| `where-it-lives` | exactly one of `skill`, `gate`, `memory`, `wiki` (K-1.1; see the table below) |
| `breaks` | empty, or the `id` of a lesson this row says broke again (K-1 rule 3); it must name another row that exists in the inbox, never itself |
| `text` | the lesson, non-empty; one line (a newline inside it is stored escaped, the file stays one row per line) |
| `kind` (optional) | empty (= `friction`), `friction` or `pattern`; anything else is refused naming the field. `friction` is a lesson learned the hard way; `pattern` is a repeatable procedure that worked. Omitted at its default, so every six-key row written before this key existed re-encodes byte-identical |
| `repeats` (optional) | empty, or a plain row id token. The pointer rules mirror `breaks`: it must name a row that exists in the inbox, never itself; the row carrying it must itself be `kind: pattern`; and the row it names must be a `kind: pattern` row with an empty `repeats` — the chain ROOT — so a chain's repeat count is one unambiguous number. A `pattern` row with no `repeats` is legal: it is the root of its chain |
| `source` (optional) | empty (= hand-written) or `event`; anything else is refused naming the field. `event` marks a row the T-9.3 mapper produced from an event (see "Event rows" below). Omitted at its default, so every row written before this key existed re-encodes byte-identical |
| `at` (optional) | empty (absent) or `YYYY-MM-DDTHH:MM:SSZ`: UTC, a real calendar instant (hour < 24, minute and second < 60). **The at/date rule:** its date part must equal the row's `date`, or the row is refused naming `at`. Omitted when absent |
| `impact` (optional) | empty (= unrated) or exactly one of `low`, `medium`, `high`, `blocker`; anything else (`urgent`) is refused naming `impact`. Rank weights are 1, 3, 9, 27 (unrated counts as 1) |
| `why` (optional) | one line, at most 1,000 bytes: the cause, stated as suspected or proven. A newline is stored escaped, as in `text`. Over the cap is refused naming `why` |
| `proof` (optional) | one line, at most 2,000 bytes: pointers separated by `; `. **Proof pointer grammar:** each pointer is exactly one of `<repo>@<7-40 lowercase hex>:<path>[:<line>]` (a commit and a repo-relative path, an optional line number), `session:<id-token>`, `pr:<owner>/<repo>#<n>`, `cmd:<text>` (free text, one line) or `~/<path>`. Anything else is refused naming `proof` (the reason never echoes the pointer) |

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
ticket 193); a gate FAIL maps through `gate_fail` (whose writer is a later ticket). The mapper is pure: it never opens or writes the inbox — a later weekly sweep files its rows
through `harness friction add`, and because the dedup key below IS the row `id`, the duplicate-id rule of
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
  --where-it-lives <skill|gate|memory|wiki> [--breaks <lesson-id>] \
  [--kind <friction|pattern>] [--repeats <row-id>] [--source event] \
  [--at <YYYY-MM-DDTHH:MM:SSZ>] [--impact <low|medium|high|blocker>] [--why <cause>] [--proof <pointers>] \
  --text <lesson> [--config <file>]
```

Appends one row. Everything is checked before anything is written, in this order: the flags, the row rules
(section 1), every field of the new row through the same privacy engine as the rest of the harness, then — under
one exclusive writer lock (section 4) — the same privacy engine over every stored row and the inbox cross rules
(a duplicate `id`; a `breaks` naming no lesson or naming itself; the `repeats` pointer rules of section 1; the
existing inbox must itself be clean, untorn and privacy-clean). Only then is one whole line (plus newline) appended and synced; the file is created when
absent, its folder is not; a refused row never creates the file (a first row whose only possible refusal is a
`breaks` with nothing to name is refused before the file is opened). On success stdout is `stored id=<id>`. The new row is scanned before any cross rule
runs, so a refusal can never echo an unscanned byte of it. `--source` accepts only `event` (anything else is
a row refusal naming the field); the flag exists so the T-9.3 mapper's rows are fileable exactly as the
weekly sweep will file them.

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
audited, so it must exist.

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
a clean run.

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
harness friction rank [--top <k>] [--record <file>] [--config <file>]
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
rule as `patterns --record` (exit 2; the record may not name the inbox).

### `harness friction triage`

```
harness friction triage --week <iso-year>-w<ww> --state <file> --handoff-dir <dir> \
  [--log <event-log>] [--closed <file>] [--top <k>] [--config <file>]
```

The loop's one deterministic step: no wall clock runs, the week is an argument. In order: (1) with `--log`, the
existing `sweep` for that week (the same function); (2) lint + rank; (3) the `--state` file, an append-only JSONL of
nine string keys (`group`, `status` of `proposed|resolved|regressed|escalated`, `ticket_file`, `proposed_week`,
`task_id`, `task_session`, `resolved_pr`, `resolved_date`, `attempts`; the last row per group wins), read under the
4 MiB cap; a torn or bad state refuses (exit 1) and nothing is written; (4) with `--closed` (a JSON object whose
`closed_with_evidence` array holds ticket-reconcile rows `id`, `repo`, `pr`, `at` plus an optional `session`) a
`proposed` group resolves ONLY when its bound `task_id` equals a row's `id` (and, when both carry a session, the
sessions match); the hand-off id `friction-<week>-<n>` never matches, and an unbound group never resolves;
(5) a `resolved` group with a member row dated after its `resolved_date` becomes `regressed` (`attempts` + 1), and
at `attempts >= 2` becomes `escalated` instead, which is never auto-proposed again; (6) proposals: every `regressed`
group (first), every group with break count >= 2 (always, regardless of rank) and the top K ranked groups, minus
groups already `proposed`, `resolved` or `escalated`; (7) one ticket-contract file `<handoff-dir>/<week>-<n>.json`
per proposal (id `friction-<week>-<n>`, owner `friction_learning`, size `M` / `L` and risk `normal` / `high` for a
`blocker`), each round-tripped through the ticket reader before any write; (8) the state rows are appended under the
exclusive lock (section 4) and ids/numbers are printed:

```
propose group=<id> file=<basename> reason=<rank|twice-broken|regressed>
open group=<id> age_weeks=<n> bound=<yes|no>
signature group=<id> weeks=<n>
pattern id=<id> repeats=<n>
escalate group=<id> attempts=<n>
triage week=<w> proposed=<n> open=<n> resolved=<n> escalated=<n>
```

`open` lists every `proposed` group with its age (`bound=no` makes a forgotten bind visible each week);
`signature` lists an event signature seen in 2 or more weeks; `pattern` is the `patterns` listing; `proposed` and
`resolved` in the summary count this run's transitions, `open` and `escalated` the standing groups. A second run
for the same week proposes 0. Every check precedes every write: a refused run leaves the state and the hand-off
folder byte-identical.

### `harness friction bind`

```
harness friction bind --state <file> --group <id> --task-id <digits> --task-session <token> [--config <file>]
```

The one writer of the task binding: it appends one state row copying the group's last row with `task_id` and
`task_session` set, so the closed feed can match the TaskCreate id (the hand-off file's `id` is a different token).
Refused (exit 1, state byte-identical): no state row for the group, a last status other than `proposed`, a binding to
a different task, a `--task-id` that is not all digits, or a `--task-session` that is not a plain token. Re-binding
the same pair prints `bind already group=<id>`; success prints `bind group=<id> task=<digits>`.

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
  the row naming the field and the class, never the bytes. Every reader of the inbox runs the same scan over
  every stored row (`add`, `lint`, `close-check`), and `close-check` scans its `lesson:` lines too.
- One writer at a time: `add` takes ONE exclusive kernel advisory lock (`flock(2)`, the std file lock) on the inbox
  file itself, and does the read, scan, cross check and append through that same open descriptor, so two
  concurrent adds can never both store the same id (the check and the append are one critical section) — however
  many writers arrive at once and however the inbox path is spelled. The lock lives on the inbox's inode, so every
  spelling of the path shares it: a symlinked folder, a symlinked file, a `./` or doubled-slash detour, a hard
  link, and a not-yet-created file behind a symlink (the first append creates the target and locks it). There is
  no side lock file, no path built for locking and nothing canonicalized. The kernel releases the lock the moment
  the holder exits — return, panic, or a kill -9 — so a crashed writer can never leave the inbox locked: there is
  NO steal rule and no stale-lock age at all. A file or folder named `<inbox>.lock` is not a lock and is simply
  ignored (never read, locked, renamed or deleted: the harness never deletes anything it did not create). A
  lock held longer than 10 seconds by a live writer fails the verb closed (exit 2, `the inbox is busy`, no
  path). The lock call (`File::try_lock`) needs Rust 1.89 or newer; the workspace pins 1.99.1. `lint` and
  `close-check` only read and take no lock; the lock is advisory, so it protects against other harness writers,
  not against an editor that ignores it. A refused row never creates the inbox file.
- Input bounds, all failing closed past the bound: one row line is capped at 16 KiB; the inbox is read no further
  than 4 MiB; a close note no further than 1 MiB; the event log a `sweep` reads no further than 4 MiB (each
  event line is separately capped at 4 KiB by the event reader's own rules). Over the bound the verb refuses
  (1 for a row, 2 for a file; a log one byte over the cap is refused with nothing filed, a log exactly at it
  is accepted).
  `patterns` never writes the inbox at all, and its only output file is the one `--record` line; the listing
  itself cannot loop or grow past its bound (one pass over the already-capped inbox, one line per root).
- Each row is written as one whole line plus newline. A last line with no newline is a torn tail (a crashed
  append): reported by `lint` with its line number, never read as a row; `add` refuses to append onto it.

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
