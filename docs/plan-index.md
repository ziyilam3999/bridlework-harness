# Plan index

The plan index is a file of JSON lines. Each line is one row of the plan. `harness ticket intake --plan-index <file>` reads it to check that a ticket names rows that exist. The reader is `core/rust/harness/src/plan_index.rs` (std only, runs nothing, writes nothing).

The index is private data of the operator. It is passed by path and is not kept in this repo.

## Row format

One JSON object per line, with these 8 required keys and one optional key `left_estimate`, each once:

| Key | Shape |
|---|---|
| `id` | 1 to 64 bytes of ASCII letters, digits, `.`, `_` or `-` |
| `title` | non-empty string |
| `plan_line` | JSON integer from 0 to 2^64-1, written with digits only |
| `state` | one of `not-built`, `in-build`, `built`, `deviation`, `later`, `unknown` |
| `tickets` | array of strings with the id character rule, may be empty |
| `deviation` | `null`, or an object whose keys are among `reason`, `decided_by`, `date`, `source`, each once and each a string |
| `trigger` | `null` or a string |
| `code` | array of strings |
| `left_estimate` (optional) | JSON integer from 0 to 1000000, written with digits only: points of work on the row that has no ticket file yet (S=1, M=3, L=9) |

A `left_estimate` that is a string, a decimal, negative, `-0`, written with an exponent, `null`, a boolean, an array, an object or over 1000000 is refused as `line <n>: field `left_estimate`: wrong type or value`. A repeated one is refused as `line <n>: field `left_estimate`: given twice`. Rows with and without the key may be mixed.

`unknown` means no source shows the state yet. `null` is told apart from a number, `true`, `false`, an array and an object: a `deviation` of `1.5`, `true` or `[]` is a wrong type, not `null`.

## Read rules and refusals

- At most 1 MiB + 1 bytes are read; a larger file is refused without being read to its end.
- A line ends at LF. The last row may have no newline after it.
- A line ending in CR LF is read (the CR counts as JSON whitespace). A test pins this.
- An empty or blanks-only line is refused (a lone newline is one).
- A line must be one JSON object with nothing after it.
- Refused in any object: a repeated key, a lone surrogate escape, a raw control byte or NUL in a string, a byte order mark, a trailing comma, nesting deeper than 32.
- Refused: a missing required key, an unknown key (any key outside the nine), a wrong type, an `id` outside the character rule, a `state` outside the six.
- Refused for the file: missing, a folder or not readable; over 1 MiB; not UTF-8; zero rows.
- A function lists the ids that appear on more than one row. Intake refuses an index with one.

A refusal of a line names the line number, and the field when it has one; a refusal of the whole file names no line. No refusal prints a value or a path. Each reason text is distinct for: missing or unreadable file, over the cap, not UTF-8, zero rows, and a line that does not parse.

## Intake flag

```
harness ticket intake <ticket-file> --repo-map <id>=<dir> [--repo-map ...] [--plan-index <absolute-file>]
```

`--plan-index` is optional and at most once. Given twice, given with no value, written as `--plan-index=<x>`, or not absolute or holding a `..` part: exit 2, stdout empty, one stderr line `harness: error: <what>`.

Order, with the exit code of each step:

1. Usage errors: exit 2.
2. The index read (each refusal of the reader, and an index with a repeated id): exit 2, stderr `harness: error: plan index: <reason>`.
3. The ticket file read: exit 2.
4. The ticket parse (both link keys, an empty list, a bad element or reason): exit 1.
5. The plan link, only when `--plan-index` is given: exit 1 with `harness: ticket refused: no plan link: main_plan_ref or off_plan is required`, or with `harness: ticket refused: main_plan_ref names <element>, which is not in the plan index` for the first element that is not an id of the index. An `off_plan` ticket passes this step.
6. The repo lookup.

Without `--plan-index`, no link is required and none is checked against an index; the shape of a link that is given is still checked at step 4. The accept line is the same in both modes: `ticket=<id> repo_id=<id> head=<sha>`.

## plan check and plan status

```
harness plan check|status --index <absolute-file> --tickets <absolute-dir> --done <absolute-dir> [--ticket <id>]
```

`--index`, `--tickets` and `--done` are required, each once. `--ticket` is optional, at most once, and its value follows the id rule below. An unknown flag, a flag with no value, a flag given twice, a path that is not absolute or holds a `..` part, a bad `--ticket` value, a missing subcommand or any other word: exit 2, the usage line on stderr, stdout empty. Both subcommands read only. The code is `core/rust/harness/src/plan.rs`.

### Inputs

- Index: read with the reader above. Each refusal of it is exit 2. A repeated id is not an input refusal here; check reports it as red.
- Tickets: each entry directly in `--tickets` whose name ends in `.json` is one ticket (other names such as `<id>.json.part` are skipped; no sub-folder is read). It is read with the ticket reader. A file that is not a regular file, does not parse, has a name outside the id rule, or has an id that differs from its name is exit 2.
- Done records: each entry of `--done` whose name ends in `.done`. Its first line must be exactly `ticket=<name without .done>`; that is the line `bw` writes after finalize, so this reader is coupled to that wrapper format. A ticket is done when its done record exists. A record that is not a regular file, has a name outside the id rule, or has another first line is exit 2.
- The id rule on file names: 1 to 64 bytes of ASCII letters, digits, `.`, `_` or `-`, not starting with a dot, no two dots in a row. A name outside it is not printed.
- A missing or unreadable `--tickets` or `--done` folder is exit 2.
- Each input refusal prints one stderr line `harness: error: <what>`, with at most a file name and a line number, no folder and no value. Stdout is empty.

### Links

A ticket links to a row when its `main_plan_ref` names the row id, or the row lists the ticket id in `tickets`. Either direction is enough. A ticket that names an id that two rows share links to both rows. A `main_plan_ref` element that is not an index id links to nothing. An `off_plan` ticket that a row lists is linked.

### plan check

One stdout line per finding, `red <class> <id>`, then the last line `plan check red findings=<n>` and exit 1. Order: rows in index order, then tickets in byte order of the id, then done records in byte order of the name without `.done`. With no finding: one line `plan check ok rows=<n> tickets=<n> done=<n>` and exit 0. Stderr is empty.

| Class | Meaning |
|---|---|
| `duplicate-id <row id>` | an id on more than one row; named once, at its second row |
| `unknown-state <row id>` | state `unknown` (fail closed: unknown is not a pass) |
| `open-row-no-estimate <row id>` | state `not-built` or `in-build` and no `left_estimate` key (fail closed: a missing number is not zero; `0` passes); whole-index mode only, named after the other findings of the same row, once per sighting |
| `built-no-done-ticket <row id>` | state `built` and no linked ticket has a done record (a listed ticket with no ticket file does not count) |
| `deviation-no-reason`, `deviation-no-decided-by`, `deviation-no-date <row id>` | state `deviation` and that key missing, empty or blanks only, or the `deviation` null |
| `deviation-bad-decided-by <row id>` | `decided_by` is given and is not `operator` or `orchestrator-reading` |
| `ticket-names-unknown-row <ticket id> <element>` | a `main_plan_ref` element that is not an index id, on any ticket |
| `done-ticket-no-row <ticket id>` | a done ticket with no `off_plan` and no linked row |
| `done-record-no-ticket <name>` | a `.done` file with no ticket file |

### Ticket mode

`plan check --ticket <id>` is meant for the `bw finish --merge` wrapper (a later ticket), to be called before the merge and before the done record is written. `<tickets>/<id>.json` must be a ticket file (else exit 2). The three inputs are read and refused as above, so one broken file in either folder is exit 2. Findings are only about this ticket and the rows it links to: `no-plan-link <ticket id>` (no `off_plan` and no linked row), `ticket-names-unknown-row`, and for each linked row `duplicate-id`, `unknown-state` and the four `deviation-` classes. A done record is not needed. Red ends with `plan check red findings=<n>` (exit 1); green is exactly `plan check ok ticket=<id>` (exit 0). A finding about another row or ticket does not change the answer.

### plan status

Exit 0 when the inputs read; it does not go red. Lines, in this order:

- six lines `state <s> rows=<n> ids=<id,id,...>` for `not-built`, `in-build`, `built`, `deviation`, `later`, `unknown` (ids in index order, `-` when none);
- one line `off-plan ticket=<id> done=<yes|no> reason=<reason>` for each `off_plan` ticket that no row lists;
- one line `leaned-on row=<id> tickets=<id,...>` for each `later` row that has a linked ticket;
- `progress done=<points> left=<points> percent=<n|none>`: done is the points of linked tickets with a done record, left the points of linked tickets with none;
- `on-plan done=<points> all-done=<points> percent=<n|none>`: done as above, all-done the points of the tickets with a done record, linked or not;
- one line `no-estimate row=<id>` for each `not-built` or `in-build` row with no `left_estimate`, in index order;
- last, `progress-estimated done=<D> left=<L> estimate=<E> percent=<n|none>`: D is the done of the progress line, E the sum of `left_estimate` over rows in state `not-built` or `in-build` (a row counts once per sighting), L the left of the progress line plus E. Percent is the floor of 100 x D / (D + L); it is `none` when D + L is 0 or when at least one `no-estimate` line was printed. Estimates on rows in other states are read and not added.

Points come from the ticket `size`: S=1, M=3, L=9. A ticket counts once, however many rows link it and in either direction. Percent is the floor of 100 x done / (done + left), or of 100 x done / all-done; `none` when the divisor is 0. A printed title or reason has each control character replaced by one blank.

`plan status --ticket <id>` prints one line `row=<id> state=<s> plan_line=<n> title=<title>` per linked row in index order; with no linked row, `off-plan reason=<reason>` or `no-plan-link ticket=<id>`. A missing ticket file is exit 2.

### Exit codes

0 ok (check green, or status); 1 check red; 2 usage error or an input refusal.

## plan stale-notes

```
harness plan stale-notes --index <absolute-file> --tickets <absolute-dir> --done <absolute-dir> <absolute-file> [<absolute-file> ...]
```

A line such as "until ticket G-3 is built" goes stale when G-3 is built, and a stale line sends the next agent on a side quest. This sub-verb lists such lines in the named files, and lists in-build rows with nothing left to do. It reports; it writes and decides nothing. Flags and files come in any order; a word that starts with `--` is a flag. The code is `core/rust/harness/src/plan.rs`.

Inputs: the three plan inputs are read as for `plan check`, then each named file in argument order. Done ids are the names of the done records; built ids are the ids of rows in state `built`. Rows in state `not-built`, `in-build`, `deviation`, `later` or `unknown` give no id.

**Pending phrases** (a fixed list, ASCII letter case ignored): `until`, `being built`, `not built`, `not yet built`, `waits for`, `todo`. The words of a phrase are split by one or more ASCII spaces or tabs; a hyphen does not split them (`not-built` is not a phrase). No ASCII letter, digit or underscore may stand directly before or after a phrase, so `untill`, `todos`, `xuntil` and `notbuilt` are not phrases.

**Id hit.** An id hits in exact letter case when the character before it is not an ASCII letter, digit, dot, underscore or hyphen, and the character after it is not an ASCII letter, digit, underscore or hyphen and is not a dot followed by one of those. So `T-1.` at a sentence end, `(T-1)` and a backticked `T-1` hit; `T-10`, `T-1.5`, `T-1.done`, `xT-1` and `T-1-b` do not hit `T-1`.

A line (split on LF, a CR at its end dropped, numbered from 1) is listed when it holds a pending phrase and a done or built id. An id found twice on one line gives one output line.

Output, in this order:

- `stale-note id=<id> by=<done|built|done,built> line=<n> file=<path as given, control characters as blanks>`, one per hit, in file argument order, then line order, then id byte order;
- `stale-row id=<row id> tickets=<count> left_estimate=<n|none>`, one per in-build row (one per sighting) that lists one or more tickets, each with a done record, and whose `left_estimate` is 0 or absent, in index order;
- last, `stale-notes notes=<a> rows=<b> files=<f>`.

Refusals (exit 2, stdout empty, no partial list): a missing flag, a flag given twice, a flag with no value, an unknown flag, `--ticket`, a flag or file path that is not absolute or holds a `..` part, no file, and one file given twice (same argument text) print exactly the usage line above on stderr. An input refusal of the plan readers prints its usual stderr line. A named file that is missing, a folder, unreadable, over 4 MiB (4194304 bytes is read, 4194305 is refused) or not valid UTF-8 prints `harness: error: file <n>: <what>`, with no path; n counts files only, from 1. Order of checks: usage, the three plan inputs, then the files in argument order; the files are read before the first stdout line.

Exit codes: 0 none found, 1 found (a or b above 0), 2 usage or input error.

Known gaps:

- A stale line that uses a phrase outside the list (for example the state word `not-built`) is not found.
- A phrase and an id on one line that are not about each other are listed; a human or the review judges. A long paragraph line gives many such hits, so run it on short-line pages such as the day card.
- A note split over two lines is not found.
- Neither `plan::USAGE` nor the top-level usage line names the sub-verb.

## What it does not do

- `plan stale-notes` reports and changes no file.
- It does not check that a row fits the work. Intake with `--plan-index` checks that each `main_plan_ref` element is an id of the index.
- `plan check` reads links and states. It does not read whether code does what a row says.
- The `progress` line counts linked tickets only; the `progress-estimated` line adds the row estimates. An estimate is a number kept by hand in the index: it is not checked against the work, and a ticket filed for estimated work counts twice until the estimate is lowered.
- A ticket made before intake took a plan link carries no link, so it counts only when a row lists it.
- On an index with rows marked `built` with no linked done ticket (by either direction), or rows in state `unknown`, `plan check` is red.
- One ticket that does not parse or one bad done record in the folders makes check, status and ticket mode exit 2.
- The MCP intake tool passes no index.
- An `off_plan` reason is not judged.
