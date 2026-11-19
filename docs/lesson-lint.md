# `harness lesson lint`

```
harness lesson lint <path> [<path> ...]
```

Checks the shape of lesson pages. A `<path>` that is a file is one page. A `<path>` that is a directory means every
file directly inside it whose name is `ISSUE-<digits>-<anything>.md` (other files, such as a README, and sub-folders
are skipped; an entry with such a name that cannot be read as a regular file, such as a broken symbolic link, is an
error). The same page reached twice in one run (the same file given twice, or a file and the directory that holds it,
compared by canonical path) is checked and counted once. Every page is checked even when one fails.

The lint checks shape only: it does not check that a sentence is true, that a link resolves, or that an eval result exists.

## Output

- All pages pass: stdout is exactly one line `lesson-lint ok pages=<n>` (with `<n>` the number of pages checked), exit 0.
- Any page breaks a rule: stdout holds one line per problem, `<file name>: <RULE> <short detail>` (`<file name>` is the last part of the path, without folders), and nothing else;
  exit 1. Problems are listed in path order: the paths in the order they were given, the pages of one directory in
  name order, the problems of one page in rule order.
- In a problem line or a stderr message, a control character (C0, DEL, C1) or a Unicode white-space character
  other than a plain space is written `\u{<hex>}` (a format character such as U+200B or U+FEFF is printed as it is);
  stdout holds no control character except its line ends.

## Exit codes

- `0`: every page passed.
- `1`: at least one problem line was printed.
- `2`: a usage or read error, with a message on stderr and nothing on stdout: no path given; a path that does not
  exist or cannot be read; a page-shaped entry inside a given directory that cannot be read as a regular file; a
  file that is not valid UTF-8; zero pages found. `harness lesson` with no verb or an
  unknown verb prints a usage text that includes `lesson lint <path>` and exits 2.

## Front matter rules

The front matter block: line 1 is `---`, a later line `---` closes the block, and every line inside is `key: value`
(a non-empty key, a colon, then the value; a key the rules below do not name is allowed). In the body (everything
after the closing `---`), a level-1 heading is a line that starts with `# ` and a level-2 heading a line that starts
with `## `. A line that starts with three backticks opens a code fence, another such line closes it, and a line
inside a code fence is never a heading.

- `FRONT-MATTER`: the block is not there (line 1 is not `---`, or no later line closes it) or a line inside it is not `key: value`. When the block is not there, this is the only problem listed for that page, because nothing else can be checked. A key holding a blank or a control character is not `key: value`. A key written twice is refused with `FRONT-MATTER line <n> repeats the key <key>` (`<n>` is the later line).
- `SHARED-FIELD`: one of `kind`, `updated`, `sources`, `verified` is missing, or `updated` is not a real calendar date written `YYYY-MM-DD`, or `verified` is not `yes` or `no`.
- `TYPE`: `type` is missing or is not exactly `lesson`.
- `ID`: `id` is missing, or is not `ISSUE-<digits>`, or the file name does not start with that id followed by `-`.
- `STATUS`: `status` is missing or is not one of `unvalidated`, `validated`, `retired`.
- `TIER`: `minimum_proven_tier` is missing, is empty, or is not exactly one of `T1`, `T2`, `T3`, `T4`, `unproven`,
  `unmeasured` — the six values `docs/fix-tier.md` accepts for this field (the comparison is exact and
  case-sensitive on the value after the trim of the front matter line, which removes only ASCII spaces and tabs
  around the key and the value, so `t2`, `T0`, a quoted `"T2"` or a blank other than an ASCII space or tab (such as a
  no-break space) is refused; the detail names the value).
- `CONTROLS`: `controls` is missing, is not an inline list `[...]` (an empty list, `[]` or `[ ]`, is fine), or holds
  an empty token.
- `MATCH`: `match` is present but is not an inline list, is an empty list (`[]` or `[ ]`), or holds an empty token (a missing `match` is fine).
- `ALWAYS`: `always` is present but its value is empty, is over 200 bytes, or holds a control character or a
  white-space character other than a plain space (the value is read as the front matter reader gives it, ASCII
  spaces and tabs trimmed, and a retired page is checked as any page), or the page carries both `always` and
  `match` (`ALWAYS always and match on one page`). The value is one rule sentence, a habit that holds for most
  changes: no `match` token picks the tickets such a page fits, so it says so with this key instead of a loose
  token that would offer it by chance. A page with neither key is fine.
- `ALWAYS-CAP`: more than 5 pages that are not retired carry `always`. The cap counts pages, not rule
  sentences, in the order the lint checks them (a page reached twice counts once); the sixth and each later
  such page gets one problem line `ALWAYS-CAP more than 5 pages carry always`, placed directly after its
  `ALWAYS` line or where that line would be. A retired page with `always` does not count.
- `DUPLICATE-ID`: two pages of one run carry the same `id`; the problem names the later page.

## Body rules

- `TITLE`: the body does not hold exactly one level-1 heading, or that heading does not start with `# <id>:` (this second check is skipped when the page has no `id` of the form `ISSUE-<digits>`).
- `SECTIONS`: the level-2 headings are not exactly `Issue and symptoms`, `Context`, `Root cause`, `Resolution`,
  `Controls`, `Verification`, `Effectiveness`, each once and in this order (a missing, repeated, extra or
  out-of-order heading is refused and the detail names the heading), or one of the sections holds no text: every line
  of it is blank or a code-fence line (a line that starts with three backticks; a fenced block with at least one
  non-blank line inside it is text). A section runs from its heading to the next level-1 or level-2 heading (a `###` line belongs to the section).
- `ROOT-CAUSE`: the text of `Root cause` is the same as the text of `Issue and symptoms` (compared after trimming).
- `EFFECTIVENESS`: the `Effectiveness` section does not hold, ignoring blank lines, exactly the four lines
  `- metric: <text>`, `- value: <text>`, `- date: <YYYY-MM-DD>`, `- source: <text>`, each with a non-empty value
  (a real calendar date for `- date`).
- `VALIDATED-CONTROL`: `status` is `validated` but the `controls` list is empty and the `Controls` section does not
  contain the words `no reusable control`.

## `harness lesson lookup`

```
harness lesson lookup <lessons-dir> --ticket <ticket-file> --scope-file <file>
```

Lists the lesson pages that match a ticket. The exit 1 lines are printed after all pages are read.

Output (exit 0): one line `lesson: <id> <heading> <file name>` per matching page, in byte order of the file names, then
`lessons: scanned=<n> matched=<ids joined by a comma>` (`matched=none` when no page matches). `<id>` is the
`ISSUE-<3 digits>` start of the file name; `<n>` is the number of pages read. Two pages with one id are both listed.

Always pages: a page that carries the front matter key `always` (one rule sentence, at most 5 such pages that are not
retired, see `ALWAYS` and `ALWAYS-CAP` above) is listed by both modes with its own line shape: one line
`always: <id> <rule>` per such page, in byte order of the file names, printed before the `lesson:` lines; `<id>` is the
start of the file name as on a `lesson:` line and `<rule>` the `always` value, printed through the escape of the lint.
Such a page is not listed as a `lesson:` line and is not in `matched=` (a page that carries both `always` and `match`
is refused, see the refusals below). When one or more `always:` lines were printed, the receipt holds one more field,
`always=<ids joined by a comma>`, directly after `matched=<...>` and before `retired=<r>`; with no `always` page the
stdout of both modes is as before.

Retired pages: both modes skip a page with `status: retired` (a lesson that no longer holds is not offered). The page is
still checked as any page (a `FRONT-MATTER`, `STATUS`, `MATCH` or `ALWAYS` problem refuses it) and still counts in
`scanned`; a retired `always` page prints no `always:` line, does not count toward the cap and counts in `retired=<r>`.
When `r` pages read are retired (they hit or not, with a match key or not), the receipt ends with the last field
`retired=<r>` (`lessons: scanned=<n> matched=<ids|none> [always=<ids>] retired=<r>`); with no retired page the field is
not printed. A caller that parses the receipt line has to accept the optional fields in this order: `always=<ids>` (only
when an `always:` line was printed), then `retired=<r>`; no reader in this repo parses it.

Heading: the text after `# ` of the first line below the front matter that starts with `# `, trimmed of Unicode white
space. With no such line, or an empty text, it is the part of the file name between `ISSUE-<3 digits>-` and `.md` with
each `-` turned into a blank. A code fence is not looked at. The heading and the file name are printed through the escape
of the lint (a control character or a white-space character other than a plain space is written `\u{<hex>}`).

Exit codes: `0` done; `1` a page or the ticket is refused (stdout empty, one stderr line per refusal,
`harness: ticket refused: <reader message>` or `harness: lesson lookup refused: <file name>: <RULE> <detail>`); `2` a usage
or read error, or a page-shaped entry that is not a page and not a folder (stdout empty; the usage text, or one or more
`harness: error: <message>` lines). When a cause of `2` and a cause of `1` are both present, the run exits `2`.

Page rule: a page is a regular file directly inside `<lessons-dir>` whose name is `ISSUE-<exactly 3 digits>-<anything>.md`.
A page-shaped entry is an entry directly inside `<lessons-dir>` whose name is `ISSUE-<1 or more digits>-<anything>.md`
(the lint rule; a name that is not UTF-8 is checked in its lossy form). A folder is skipped and not counted, whatever
its name. Each other page-shaped entry that is not a page is refused; the first check that hits gives the one line
`harness: error: the entry <name> <reason>`:

- (a) the name is not valid UTF-8: `has a name that is not valid UTF-8`
- (b) not exactly 3 digits after `ISSUE-`: `does not have exactly 3 digits after ISSUE-`
- (c) a symbolic link (also a dangling one, or one to a file or a folder): `is a symbolic link`
- (d) not a regular file (a FIFO, a socket): `is not a regular file`

No entry is opened by this check. A name that is not page-shaped (a README, `ISSUE-001.md`, `issue-001-x.md`,
`ISSUE-001-x.MD`) is skipped and not counted. An entry whose file type cannot be read prints
`harness: error: the entry <name> cannot be inspected` and nothing else.

Order when several problems meet: usage, ticket file read, scope file read and parse, the folder checks (missing, a file,
unreadable), the bad entries (one line each, in byte order of the raw file names, then exit 2; a folder whose page-shaped
entries are all bad prints these and not the no-page line), the no-page check, the page reads (cannot be read, not
valid UTF-8), then the exit 1 refusals (the ticket line first, then the pages in byte order of their names). A missing
folder, a file given as the folder, a page that is not valid UTF-8 and a folder with no page exit 2.

Front matter: read with the reader of the lint. A page for which the lint lists a `FRONT-MATTER`, `MATCH` or `ALWAYS`
problem is refused with the rule word and detail the lint prints. The `STATUS` check is made too: a `status` value that
is not `unvalidated`, `validated` or `retired` (an empty value, a quoted value and `Retired` included) is refused with
the lint line, not read as active and not read as retired; a missing `status` key reads as active (the lint reports it).
On one page the lines come in the lint order: `FRONT-MATTER`, `STATUS`, `MATCH`, `ALWAYS`. The `ALWAYS-CAP` check is
made too: more than 5 pages that carry `always` and are not retired (a missing `status` and a refused `always` value
count, a retired page does not) refuse the run with one line
`harness: lesson lookup refused: ALWAYS-CAP <n> pages carry always, at most 5`, printed after the ticket line and the
page lines. No other lint rule is checked. A page with
no `match` key is counted and matches nothing. The ticket goes through the ticket reader; only `repo_id`, `title` and `acceptance_criteria`
are used. The scope file is read as `lane claim` reads it (at most 1 MiB, UTF-8, `parse_scope`).

Tokens: the parts of the `match` list split on commas, trimmed, then with the leading and trailing `"` and `'` removed. A
token that is empty after that is refused (`MATCH match has a token with no text between its quotes`); the lint does not
flag it. A page matches when one token hits in one of three ways:

- repo: the token equals `repo_id`, letter case included.
- path: token and scope entry are split on `/`, empty parts dropped; the token has at least one part and its parts equal
  the first parts of one scope entry (whole parts, letter case included).
- word: the token occurs in the title or in one acceptance criterion, letter case ignored per character
  (`char::to_lowercase`), with no word character (`char::is_alphanumeric` or `_`) directly before or after it.

### Diff mode

```
harness lesson lookup <lessons-dir> --repo-dir <absolute-dir> --from <full-sha> --to <full-sha>
```

Lists the pages that match the committed diff between two commits of one repo, with the two output line shapes above
(the `always:` line of an always page, the `always=` field of the receipt and the `ALWAYS` refusals apply here as in
ticket mode).
A later review brief is meant to list the matched lessons as checks (that brief line is not built). Tokens are read as above (quotes removed, an empty
token refused). A token hits when:

- path: its parts (split on `/`, empty parts dropped, letter case included) equal the first parts of one changed path. A
  changed path is added, changed, deleted or type-changed; a rename reads as a delete plus an add; a gitlink change counts.
  A path is read whole (a blank in a path is part of it).
- word: it occurs, by the word rule of ticket mode (letter case ignored per character), in one added line of the diff. A removed line, a context line, a file header
  line and a no-newline marker line do not count. An added line whose text starts with `++` is an added line, and a
  removed line whose text starts with `--` is a removed line (the patch is read by its hunk counts). A CR at the end of an
  added line is not part of a word.
- a binary file counts by its path only (no line of it is read). Binary is one fixed rule, read by the code: a NUL byte
  in the first 8000 bytes of the old or the new blob (the default rule of git). The patch is asked for with `--text`, so
  no attribute (`.gitattributes`, `info/attributes`, a user attributes file, a diff driver), no `core.bigFileThreshold`
  and no checked-out file decides which lines are read; the answer rests on the two commits.
- the repo rule does not apply (there is no ticket). Two different commits with equal trees give no changed path and no
  added line. The git settings color.ui, diff.noprefix, diff.mnemonicPrefix, diff.external and a textconv driver do not
  change the result (the ones the tests cover).

Refusals, usage (exit 2, the usage text): `--repo-dir` relative or with a `..` part, a sha that is not 40 or 64 lowercase
hex, a missing flag, `--ticket` or `--scope-file` beside the diff flags. A flag given twice is read with the arguments
(exit 2, one `harness: error:` line, before any git call). Git faults (exit 2, one `harness: error: <message>` line, no
partial list): a folder that is not the top folder of a git work tree (a plain folder inside a repo is refused), a sha
that is not a commit of the repo, `--from` equal to `--to`, a `--from` that is not an ancestor of `--to`, a failed git
call, a diff over 8 MiB. A bad page is refused as in ticket mode (exit 1).

Order: the usage checks, then the git checks and the diff read, then the lessons folder and the pages. A git or size
fault exits 2 even when a page is bad.

Git runs read only (as `harness ticket words-only` does): no inherited `GIT_` name, no stdin, no replace refs, no lazy
fetch, no hook, no external diff, no textconv, no color, submodules not ignored. The mode writes no file in the repo
folder, its `.git` folder, the lessons folder or the current folder.

Known gaps:

- a token matches added text only, so a lesson about a removal is not offered
- a page with no `status` key is matched like an active page (the lint flags it)
- a status key in other letter case (`Status:`) is another key, so that page reads as having no status
- a base that is not an ancestor exits 2, so a caller passes the merge base of the branch and its target
- the patch is read as text (`--text`), so a large binary file counts toward the 8 MiB limit

### Ticket mode and the wrapper lookup

Where the verb is stricter than the wrapper lookup it replaces:

- a page with no or a bad front matter, a bad or repeated `match` line, an empty token
- a page that is not valid UTF-8, an empty folder
- a symbolic link, another digit count, a FIFO or other non-regular file, a page-shaped name that is not UTF-8 (the wrapper skips a
  symbolic link, another digit count and a non-regular file; it reads a name that is not UTF-8 as a page)
- a missing or empty scope file, and what `parse_scope` refuses (a blank-only line, a `..` part, a leading `/`, a control
  character, `a//b`)
- a page with an unknown `status` value (the verb refuses it; the wrapper does not read status)
- a page with `status: retired` (the verb skips it and counts it in `retired=<r>`; the wrapper still offers it)
- a page with an `always` key: the wrapper matcher does not read `always`, the verb lists it on an `always:` line
  and refuses it when the `ALWAYS` rule does

Where the verb reads more than the wrapper:

- an indented `match` line inside the block, and `match : [x]` with a blank before the colon
- a file name that holds a newline (the wrapper skips it)

The lint reads a name with another digit count and follows a link to a regular file, skips a link to a folder, and exits 2
with `the file <name> cannot be read` for a dangling link, a link loop, a FIFO and a socket; the lookup refuses each of these.

Other differences: the escape writes a tab, a no-break space and other control characters of a heading or file name as
`\u{<hex>}` where the wrapper prints them raw; the word-character and letter-case rules are the Rust ones above, which
differ from the wrapper for a few letters outside ASCII (for example the long s).

## `harness lesson replay`

```
harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir> [--config <absolute-file> --controls <absolute-file> [--rows <absolute-file>]]
```

Replays the ticket-mode lookup over a folder of tickets and counts the offered pages. Read only: the run writes no
file in the lessons folder, the tickets folder, the scopes folder, the folders of the config, the controls file and
the rows file, or the current folder, and the same input twice
gives the same stdout. A ticket is one `<id>.json` file directly inside the tickets folder (other entries are
skipped); its scope file is `<id>.paths` directly inside the scopes folder, read as the lookup reads a scope file. A
ticket whose scope file is missing is skipped, not run. The pages are read once, as the lookup reads them (the walk,
the front matter rules, the retired skip with `retired=<r>` counted once per run), then matched against each ticket.

Output (exit 0), three line shapes in this order:

- one line `replay: ticket=<id> offered=<n> pages=<ids joined by a comma, or none>` per ticket run, in byte order of
  the id; `<n>` counts page files (retired pages are not offered) and the `pages=` list is the `matched=` list of the
  lookup over that ticket.
- one line `page: <page id> hits=<n>` per page read, in byte order of the file names; a page offered to no ticket
  and a retired page are listed with `hits=0`.
- the last line `replay ok tickets=<n> skipped_no_scope=<k> pages=<p> offered_total=<t> offered_max=<m> zero=<z>`;
  `pages=` counts the pages read (the lookup's `scanned`, retired ones included); `offered_total=` is the sum and
  `offered_max=` the maximum of the per-ticket `offered=`; `zero=` counts the tickets with `offered=0`; with at
  least one retired page the line ends with ` retired=<r>`. No decimal number, no median and no mean is printed (a
  caller computes those from the lines).

Exit codes: `0` done; `1` a page the lookup refuses (its own `harness: lesson lookup refused:` lines), a ticket file
the ticket reader refuses, or a scope file `parse_scope` refuses — a ticket or scope file that is a symbolic link or
over the 1 MiB reader limit is refused the same way, with one line `harness: lesson replay refused: <file name>`
(the file name only, no folder); stdout is empty and no partial count is printed; `2` a usage or read error: a
`<lessons-dir>`, `--tickets` or `--scopes` value that is relative or holds a `..` part (the usage text), a flag
given twice, a missing folder, a lessons folder the lookup's folder walk refuses (its own `harness: error:` lines),
a tickets folder with no `<id>.json` file, or no ticket left after the no-scope skip. A tickets argument or a scopes
argument that names a file is a `2` too. A refused ticket file or scope file (`1`) is reported before the check "no
ticket left after the no-scope skip" (`2`): one ticket with no scope file plus one ticket file that is a symbolic
link exits `1`.

A `<id>.json` that is a symbolic link or a folder is refused (`1`) also when it has no scope file. A tickets folder
given as a symbolic link is followed.

Known limit: ticket mode only — the diff-mode lookup has no replay. The replay does not yet count `always` pages
apart (the replay fields for them are a later change); in plain words:

- A page with `always` has no match tokens, so the replay shows it with `hits=0` and does not count it as offered.
- The replay refuses a page the `ALWAYS` rule refuses (it reads pages with the same page reader as the lookup).
- The replay does not check `ALWAYS-CAP`: with 6 such pages that are not retired the replay exits 0 where the lookup
  exits 1.

An empty ticket id (a file named `.json`) and a
ticket file name that is not UTF-8 are not handled apart.

### The answer key of `harness lesson replay`

```
harness lesson replay <lessons-dir> --tickets <absolute-dir> --scopes <absolute-dir> --config <absolute-file> --controls <absolute-file> [--rows <absolute-file>]
```

With `--config` and `--controls` (only together; `--rows` only beside them) the replay also prints the answer
key: whether the page a friction row's group was bound to was offered on that row's own ticket, so a change of the
pages can be judged on rows that were not used to make the change. The inbox named by the config is read as
`harness friction rank` reads it (the full lint, the privacy scan of the stored rows with the test mode off, the
`breaks` grouping): exit 2 for a config that is missing or refused, no privacy list configured, an inbox that is
missing, a folder, over its cap or not UTF-8; exit 1 with the same stderr lines as `friction rank` for a bad inbox
row, a torn last line, a privacy hit in a stored row and a `breaks` cycle. The controls file is read as
`harness friction controls-check` reads it, over the same `<lessons-dir>`, with its refusals and exit codes kept
(a path that is missing, not a file or over the cap exits 2; a malformed line, a line that is not UTF-8 or has a
control character, a line whose lesson has no regular file in the lessons folder exit 1 with the line number and
no line text), plus one more refusal: two lines that name one group id refuse the file (exit 1, the later line
number). A line that names a retired page is read. The groups follow `breaks` links only (a `repeats` pointer is
not a group edge), and a line that names a row id that is not a group id covers no row. The config, the inbox and
the controls file are opened as their readers open them today (a symbolic link is followed).

Output (after the `page:` lines and before the last line): one line `key: <page id> rows=<n> offered=<h>` per page
named by a counted row, in byte order of the page id, then one line `key ok rows=<n> offered=<h> no_ticket=<t>
no_line=<u>`, which ends with ` listed=<k>` when `--rows` was given. The rule: a row is tied to the ticket its
`ticket` field names, byte for byte; a ticket was replayed when it has a `replay:` line (a ticket skipped for no
scope file was not); a row counts when the group of the row (its `breaks` root) has a controls line and its ticket
was replayed, and it is offered when the page id of that line is in the `pages=` list of that ticket's replay
line; `no_ticket=` counts the rows whose group has a line but whose ticket was not replayed, and `no_line=` the
rows whose group has no line. Each row looked at lands in exactly one of `rows`, `no_ticket` and `no_line`, so the
three add up to the number of inbox rows (with `--rows`, to `listed=<k>`). A row of a line that names a retired
page counts in `rows=` and is not offered (a retired page is in no `pages=` list). A run without the three flags
prints no `key:` line and no `key ok` line. No row text and no ticket text is printed.

`--rows <absolute-file>` names a file of row ids, one id per line; only the named rows are looked at, and the
`key ok` line then ends with ` listed=<k>` (the ids in the file). The groups are built from the whole inbox
first and the listed rows are picked after, so a listed row is counted under its root's controls line also when
the root itself is not listed. Lines end with a line feed; one line feed at the end of the file is not a blank
line; nothing is trimmed (a CR, a blank before or after an id makes the line not an id). The file is read as a
regular file up to 1 MiB: a file that is missing, not a regular file, a symbolic link, over the cap or not UTF-8
exits 2. A line that is not the id of an inbox row, an id given twice, a blank line and an empty file exit 1 with
one line `harness: lesson replay refused: rows line <n> <reason>` (no id is printed).

The three flags are usage errors (exit 2, the usage text): `--config` without `--controls`, `--controls` without
`--config`, `--rows` without both, a flag given twice, and a value that is relative or holds a `..` part.

Known limit (the answer key): a row is tied to the ticket it was written on, which may not be the change that
caused it.
