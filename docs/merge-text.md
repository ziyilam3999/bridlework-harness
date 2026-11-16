# harness ticket merge-text

Writes a checked squash subject file and body file for one ticket, or refuses.

## Synopsis

```
harness ticket merge-text <ticket-file> --pr <n> --body-file <absolute-file> --out-dir <absolute-dir>
```

All three flags are required, each once. `--pr` is 1 to 9 ASCII digits with no leading 0.
`--body-file` and `--out-dir` are absolute with no `..` component. A bad command line is exit 2 and
prints the usage line.

## Output

On success it writes two new files and prints one stdout line, with stderr empty:

- `<out-dir>/<id>.subject`: the ticket title, a blank, `(#<n>)`, and one LF.
- `<out-dir>/<id>.body`: the body file bytes as read, unchanged.
- stdout: `merge-text ok ticket=<id> pr=<n> body_lines=<count>`

The PR number comes from `--pr`; it is not checked against GitHub. The body file is written unchanged.

## Order of steps

The first failing step gives one stderr line and stops.

1. Usage (exit 2).
2. The privacy list loads before any input file is opened (exit 2).
3. The ticket file is opened with no link follow and no block, the handle is re-checked to be a regular
   file, and at most 1 MiB plus one byte is read (exit 2).
4. The strict ticket reader (exit 1, `harness: ticket refused: <reader message>`).
5. The ticket id must be a plain file name (exit 2).
6. The output folder: it exists, is a folder and not a link, is canonicalized once, and neither it nor any
   folder above it holds an entry named `.git` (exit 2). The verb creates no folder.
7. The body file, opened as the ticket file is; at most 4097 bytes are read (exit 2).
8. The subject checks (exit 1).
9. The body checks (exit 1).
10. The privacy scan (exit 1).
11. The writes (exit 2 on an error).

## Exit 2 inputs

Lines start with `harness: error: ` and name the input, never a path: the privacy list, the ticket file,
the ticket id, the output folder, the body file. The output folder is refused when it is missing, not a
folder, a symbolic link, inside a work tree, not writable, or already holds `<id>.subject` or `<id>.body`.

## Exit 1 lines

After `harness: merge-text refused: `:

- Subject: `title is blank`; `title has a control character`; `subject is over 256 bytes` (counted with
  the `(#<n>)` part).
- Body, in this order: `body is over 4096 bytes`; `body is empty (0 bytes)`; `body is not UTF-8`;
  `body does not end with a newline`; `body has more than 20 lines`; then per line, in order:
  `body line <n> has a control character`; `body line <n> is blank at the start or the end` (first or last
  line only; a blank line in the middle is kept); `body line <n> looks like a trailer`.
- Privacy: `subject has a privacy hit (<classes>)` or `body line <n> has a privacy hit (<classes>)`. The
  subject is scanned as `merge.subject`, the body line by line as `merge.body`, with the privacy list and
  testmode off. No byte of the hit is printed.

A control character is any `char::is_control` character, U+2028 or U+2029. A CR of a CRLF line end and a
NUL byte are refused.

## The trailer rule

A line is refused when, after leading blanks, it starts with two or more runs of ASCII letters or digits
joined by single `-`, then zero or more blanks, then a colon. The squash body becomes the commit message
on the public default branch, and a co-author or sign-off line there names a person. The rule is wide on
purpose: a matching line is refused anywhere in the body, though git reads trailers only in the last
paragraph. Single-word tokens such as `Note: x` pass; git may read them as trailers when they end the body.

## Files and cleanup

The verb writes only into a folder that holds neither target. A caller uses a new folder per run (for
example `mktemp -d`). Each target is created so that an entry made by someone else after the check is not
replaced or written through. An error after this run created a target removes the targets this run
created, so a refusal leaves no file. Nothing is written outside the output folder.

## What it does not do

It runs no gh, git or network call. It does not push, edit a PR, wait for checks or merge. The squash text
goes to the merge call and not into a PR body edit, so no PR text check is started again by it.
