# `harness lesson lint`

```
harness lesson lint <path> [<path> ...]
```

Checks the shape of lesson pages. A `<path>` that is a file is one page. A `<path>` that is a directory means every
file directly inside it whose name is `ISSUE-<digits>-<anything>.md` (other files, such as a README, and sub-folders
are skipped). Every page is checked even when one fails.

The lint checks shape only: it does not check that a sentence is true, that a link resolves, or that an eval result exists.

## Output

- All pages pass: stdout is exactly one line `lesson-lint ok pages=<n>` (with `<n>` the number of pages checked), exit 0.
- Any page breaks a rule: stdout holds one line per problem, `<file name>: <RULE> <short detail>` (`<file name>` is the last part of the path, without folders), and nothing else;
  exit 1. Problems are listed in path order: the paths in the order they were given, the pages of one directory in
  name order, the problems of one page in rule order.

## Exit codes

- `0`: every page passed.
- `1`: at least one problem line was printed.
- `2`: a usage or read error, with a message on stderr and nothing on stdout: no path given; a path that does not
  exist or cannot be read; a file that is not valid UTF-8; zero pages found. `harness lesson` with no verb or an
  unknown verb prints a usage text that includes `lesson lint <path>` and exits 2.

## Front matter rules

The front matter block: line 1 is `---`, a later line `---` closes the block, and every line inside is `key: value`
(a non-empty key, a colon, then the value; a key the rules below do not name is allowed). In the body (everything
after the closing `---`), a level-1 heading is a line that starts with `# ` and a level-2 heading a line that starts
with `## `. A line that starts with three backticks opens a code fence, another such line closes it, and a line
inside a code fence is never a heading.

- `FRONT-MATTER`: the block is not there (line 1 is not `---`, or no later line closes it) or a line inside it is not `key: value`. When the block is not there, this is the only problem listed for that page, because nothing else can be checked. If a key appears twice, the later line counts.
- `SHARED-FIELD`: one of `kind`, `updated`, `sources`, `verified` is missing, or `updated` is not a real calendar date written `YYYY-MM-DD`, or `verified` is not `yes` or `no`.
- `TYPE`: `type` is missing or is not exactly `lesson`.
- `ID`: `id` is missing, or is not `ISSUE-<digits>`, or the file name does not start with that id followed by `-`.
- `STATUS`: `status` is missing or is not one of `unvalidated`, `validated`, `retired`.
- `TIER`: `minimum_proven_tier` is missing or empty.
- `CONTROLS`: `controls` is missing or is not an inline list `[...]` (an empty list, `[]` or `[ ]`, is fine).
- `MATCH`: `match` is present but is not an inline list, is an empty list (`[]` or `[ ]`), or holds an empty token (a missing `match` is fine).
- `DUPLICATE-ID`: two pages of one run carry the same `id`; the problem names the later page.

## Body rules

- `TITLE`: the body does not hold exactly one level-1 heading, or that heading does not start with `# <id>:` (this second check is skipped when the page has no `id` of the form `ISSUE-<digits>`).
- `SECTIONS`: the level-2 headings are not exactly `Issue and symptoms`, `Context`, `Root cause`, `Resolution`,
  `Controls`, `Verification`, `Effectiveness`, each once and in this order (a missing, repeated, extra or
  out-of-order heading is refused and the detail names the heading), or one of the sections holds no non-blank line. A section runs from its heading to the next level-1 or level-2 heading (a `###` line belongs to the section).
- `ROOT-CAUSE`: the text of `Root cause` is the same as the text of `Issue and symptoms` (compared after trimming).
- `EFFECTIVENESS`: the `Effectiveness` section does not hold, ignoring blank lines, exactly the four lines
  `- metric: <text>`, `- value: <text>`, `- date: <YYYY-MM-DD>`, `- source: <text>`, each with a non-empty value
  (a real calendar date for `- date`).
- `VALIDATED-CONTROL`: `status` is `validated` but the `controls` list is empty and the `Controls` section does not
  contain the words `no reusable control`.
