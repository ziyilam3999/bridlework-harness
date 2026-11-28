# harness ticket body-cites

Checks each strict line cite of a PR body against the repo and prints one line per cite. It exists because cited
lines were once not printed on the final head before a body was set (friction group f-20261120-60). It decides no
review and writes no file; a caller uses its output as one input. It runs no network call.

```
harness ticket body-cites --repo-dir <absolute dir> --head <full sha> --base <full sha> \
  --body-file <absolute file> [--cite-ok "<cite or token>"]...
```

`--head` and `--base` are 40 lowercase hex characters and commits of the repo. `--repo-dir` is absolute, has no
`..` part and is a git work tree. Git runs read only: no inherited `GIT_` name, no stdin, `--no-replace-objects`,
`--no-lazy-fetch`.

## The cite form

```
<sha>:<path>:<n>[-<m>] "<fragment>"
```

- `sha`: 7 to 40 hex characters.
- `path`: repo relative, no `..` part, not absolute, no empty or `.` part, no ASCII white space (space, tab, line feed, form feed or CR), no straight `"` and no backtick. The path ends
  at the first such character.
- `n` or `n-m`: a line number or a range (lines count from 1).
- Exactly one space or tab between `<n>[-<m>]` and the opening quote. A curly quote does not open a fragment.
- `fragment`: not empty, no line break, ends at the next straight `"`.
- The `<sha>:<path>:<n>[-<m>]` part may be wrapped in one pair of backticks (an opening backtick just before the
  sha, a closing one just after the range, then the one space or tab and the quote). It is the same cite: every
  check runs on it, and it is printed with its backticks. A closing backtick with no opening one is not a strict
  cite. A second backtick just before the opening one (two backticks before the sha) is not one pair either: the
  locator is not wrapped, no strict cite is read, and the token is printed as unparsed.

## Output

For each cite, in body order, one line: `OK <cite>`, or `FLAG <reason>[,<reason>...] <cite>`. The cite is printed as
written in the body. The reasons come in this fixed order, and a cite can carry more than one. The last line is
`cites=<n> ok=<k> flag=<f> unparsed=<u>`. `cites` counts every strict cite (also cleared ones); `flag` counts cites
only; unparsed lines count only in `unparsed`.

## The reasons

The file is read at the cite sha: the blob at `<sha>:<path>`.

1. `sha-not-head-or-base`: the cite sha does not resolve to `--head` or `--base` (it is a prefix of neither). This
   catches a cite left on an older head. Such a cite is not read further.
2. `file-missing`: no such path at the cite sha.
3. `not-a-file`: the path is a folder, a symlink or a submodule, or the blob is over 1048576 bytes or not UTF-8.
   Its text is not checked (a blob that is not UTF-8 is fetched and then refused). A path that is not UTF-8 is not
   listed, so a cite to it reads as `file-missing`.
4. `out-of-range`: `n < 1`, `m < n`, or `m` past the last line. The last line is the last newline-ended line (a
   final line with no newline counts), so `m` one past it is out of range, not blank. A cite with this reason
   carries no other.
5. `blank-line`: the first or the last line of the range is blank (empty, or only space, tab or CR).
6. `fragment-absent`: the fragment, compared without case, is not on lines `n` to `m`.

## Unparsed

After the strict cites are taken out, each `:<digits>` (or `:<digits>-<digits>`) left counts as unparsed when its
name holds a `.` or a `/`, or when the character before the colon is not a letter or digit. The name is the run of
characters before the colon back to the last space, tab, `"`, `'`, backtick, `(`, `)`, `[`, `]`, comma, one of the
curly quotes U+201C, U+201D, U+2018, U+2019, or the start of the line. `{`, `}`, `<` and `>` do not stop it. It prints `FLAG unparsed <name>:<digits>[-<digits>]`, as written.

| Text | Unparsed? | Why |
| --- | --- | --- |
| `a1b2c3d:src/x.rs:12` (no fragment) | yes | name `a1b2c3d:src/x.rs` holds `.` and `/` |
| `x.rs:12` | yes | name holds `.` |
| `see :12` | yes | the character before the colon is a space |
| ``15dc27f:src/a.rs:1` "x"`` | yes | a closing backtick with no opening one is not a strict cite |
| `` ``` ``15dc27f:src/a.rs:1` "x" ``` `` | yes | two opening backticks, one closing: the second backtick before the sha means not one pair, so no strict cite |
| `src/x:3-4` | yes | name holds `/` |
| `at 16:35 UTC` | no | a clock time: name `16` has no `.` or `/`, a digit comes before the colon |
| `std::fmt:12` | no | name has no `.` or `/`, a letter comes before the colon |

## Clearing

`--cite-ok "<cite or token as printed>"` (repeatable). A FLAG line whose cite or token equals one `--cite-ok`
exactly prints `CLEARED <cite>` instead and counts neither as a flag nor as unparsed. A `--cite-ok` that matches no
FLAG line of this run exits 1 with one line `FLAG cite-ok-unused <value>` (printed before the count line, not
counted in `flag`).

## Exit codes

- 0: no FLAG and `unparsed=0` (after clearing). A body with no cite and no unparsed token gives
  `cites=0 ok=0 flag=0 unparsed=0`.
- 1: at least one FLAG or `unparsed > 0` or an unused `--cite-ok`.
- 2: one stderr line and nothing on stdout: a missing, repeated or unknown flag; `--repo-dir` not absolute, holding
  a `..` part, or not a git work tree; `--head` or `--base` not 40 lowercase hex or not a commit there; the body
  file missing, a link, not a regular file, over 1048576 bytes or not UTF-8. Also exit 2: `--body-file` not
  absolute or holding a `..` part (the usage line); a `--cite-ok` with no value (the usage line); the repo dir
  missing or not a folder; git cannot run; a git call failed; the body file cannot be read.

## Worked example

Head `1111111...` has `src/a.rs` with five lines: `fn alpha() {}`, a blank line, `fn beta() {}`, a blank line,
`fn gamma() {}`. Base `2222222...` has `src/a.rs` with `fn base_line() {}`. The body (the sha is written in full in
real use):

```
Fixed in 1111111:src/a.rs:1 "fn alpha" and 1111111:src/a.rs:2-3 "fn beta" (was
2222222:src/a.rs:1 "base_line"). See 1111111:src/a.rs:5 "FN GAMMA", a.rs:12, at 16:35.
```

Output, exit 1:

```
OK 1111111:src/a.rs:1 "fn alpha"
FLAG blank-line 1111111:src/a.rs:2-3 "fn beta"
OK 2222222:src/a.rs:1 "base_line"
OK 1111111:src/a.rs:5 "FN GAMMA"
FLAG unparsed a.rs:12
cites=4 ok=3 flag=1 unparsed=1
```

Running again with `--cite-ok 'a.rs:12' --cite-ok '1111111:src/a.rs:2-3 "fn beta"'` prints `CLEARED` for those two
lines and exits 0 with `cites=4 ok=3 flag=0 unparsed=0`.
