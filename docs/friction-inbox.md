# The friction inbox (K-1 slice 1)

The friction inbox is where the harness writes down what broke while it is being built: one row per friction or
lesson, captured the same turn it is found (T-9). This page is the public contract of the row, the verbs that read
and write it, and the `lesson:` close line. The rule itself is the knowledge-routing rule (K-1, ticket 153); this
ticket (154) builds its first slice: capture first, so the store grows while the harness is built.

**The inbox file is not in this repo.** It lives only in the private overlay (5.2 item 4): lesson text is real
operational text, so it is private data. This repo holds the contract, the reader and the lint — never the rows.

Status: contract plus `harness friction add | lint | close-check` (ticket 154), the pattern half — `kind`,
`repeats`, `friction patterns` and the trial record (ticket 192) — and the T-9.3 event-to-row mapper with
`source` (ticket 193), the four evidence keys and the triage loop `rank | triage | bind | page` (ticket 3601), and the read-only `controls-check` verb. The generated lessons index (`harness find`) is slice 2; see "Not in slice 1" below.

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
weekly sweep (`harness friction sweep`) files them.

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
rule as `patterns --record` (exit 2; the record may not name the inbox).

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
  ignored (never read, locked, renamed or deleted: the friction verbs delete nothing they did not create). A
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
