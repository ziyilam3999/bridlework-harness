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

Lists the lesson pages that match a ticket. Every file is read before anything is printed.

Output (exit 0): one line `lesson: <id> <heading> <file name>` per matching page, in byte order of the file names, then
`lessons: scanned=<n> matched=<ids joined by a comma>` (`matched=none` when no page matches). `<id>` is the
`ISSUE-<3 digits>` start of the file name; `<n>` is the number of pages read. Two pages with one id are both listed.

Heading: the text after `# ` of the first line below the front matter that starts with `# `, trimmed of Unicode white
space. With no such line, or an empty text, it is the part of the file name between `ISSUE-<3 digits>-` and `.md` with
each `-` turned into a blank. A code fence is not looked at. The heading and the file name are printed through the escape
of the lint (a control character or a white-space character other than a plain space is written `\u{<hex>}`).

Exit codes: `0` done; `1` a page or the ticket is refused (stdout empty, one stderr line per refusal,
`harness: ticket refused: <reader message>` or `harness: lesson lookup refused: <file name>: <RULE> <detail>`); `2` a usage
or read error (stdout empty; the usage text, or `harness: error: <message>`). When a cause of `2` and a cause of `1`
are both present, the run exits `2`.

Page rule: a page is a regular file directly inside `<lessons-dir>` whose name is `ISSUE-<exactly 3 digits>-<anything>.md`.
Other entries (a README, a sub-folder, a symbolic link, a name with another digit count) are skipped and not counted.
A missing folder, a file given as the folder, a page that is not valid UTF-8 and a folder with no page exit 2.

Front matter: read with the reader of the lint. A page for which the lint lists a `FRONT-MATTER` or a `MATCH` problem is
refused with the rule word and detail the lint prints. No other lint rule is checked. A page with no `match` key is
counted and matches nothing. The ticket goes through the ticket reader; only `repo_id`, `title` and `acceptance_criteria`
are used. The scope file is read as `lane claim` reads it (at most 1 MiB, UTF-8, `parse_scope`).

Tokens: the parts of the `match` list split on commas, trimmed, then with the leading and trailing `"` and `'` removed. A
token that is empty after that is refused (`MATCH match has a token with no text between its quotes`); the lint does not
flag it. A page matches when one token hits in one of three ways:

- repo: the token equals `repo_id`, letter case included.
- path: token and scope entry are split on `/`, empty parts dropped; the token has at least one part and its parts equal
  the first parts of one scope entry (whole parts, letter case included).
- word: the token occurs in the title or in one acceptance criterion, letter case ignored per character
  (`char::to_lowercase`), with no word character (`char::is_alphanumeric` or `_`) directly before or after it.

Where the verb is stricter than the wrapper lookup it replaces:

- a page with no or a bad front matter, a bad or repeated `match` line, an empty token
- a page that is not valid UTF-8, an empty folder
- a missing or empty scope file, and what `parse_scope` refuses (a blank-only line, a `..` part, a leading `/`, a control
  character, `a//b`)

Where the verb reads more than the wrapper:

- an indented `match` line inside the block, and `match : [x]` with a blank before the colon
- a file name that holds a newline (the wrapper skips it)

Other differences: the escape writes a tab, a no-break space and other control characters of a heading or file name as
`\u{<hex>}` where the wrapper prints them raw; the word-character and letter-case rules are the Rust ones above, which
differ from the wrapper for a few letters outside ASCII (for example the long s).
