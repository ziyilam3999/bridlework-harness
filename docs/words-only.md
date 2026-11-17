# harness ticket words-only

Reads two commits of one repo and exits 0 when each change between them is comment text in a Rust file or
text of a docs Markdown file. Else it refuses.

## Synopsis

```
harness ticket words-only
  --repo-dir <absolute-dir>
  --from <full-sha>
  --to <full-sha>
```

- `--repo-dir`: a git repo or a linked work tree. Absolute, with no `..` component.
- `--from`: 40 lowercase hex characters, the older commit.
- `--to`: 40 lowercase hex characters, the newer commit.

The three flags are required, each once. An unknown flag (also `--from=x`), a flag with no value, a flag
given twice or a word that is not a flag is exit 2 with the usage line on stderr and stdout empty.

## Words files

- a path ending in `.rs`
- a path under `docs/` ending in `.md`
- the root files `README.md`, `CONTRIBUTING.md` and `SECURITY.md`

Paths compare byte for byte (`Docs/x.md` is not under `docs/`). A shell file, a file with no name ending and
a Markdown file elsewhere are not words files.

## How git is read

Each git call runs on the repo dir, removes each inherited variable whose name starts with `GIT_`, gets no
stdin and runs with `--no-replace-objects`. It reads only: no fetch, no ref, config, index or work tree
write. Changed entries come from a raw tree compare with renames off. File contents come from the object
store as raw bytes, so no text conversion, filter or diff driver runs. The work tree is not read.

## Order of steps

The first failing step gives one stderr line and stops.

1. Usage (exit 2).
2. Setup (exit 2, `harness: error: <what>`, no path printed): the repo dir is missing or not a folder, it is
   not a git repo, git cannot run or a call fails, `--from` or `--to` is not a commit of the repo.
3. `--from` equal to `--to`: `no-change`.
4. `--from` is not an ancestor of `--to`: `not-ancestor`.
5. More than 200 changed entries: `too-many-files`.
6. Each changed entry, in the order git lists them (see the classes).
7. A `.md` words file passes on any text change. A `.rs` file passes when the two versions are equal after the
   lexer removes comments.
8. Done: exit 0.

A refusal is exit 1 with one stderr line `harness: words-only refused: <path or -> <class>` and stdout empty.

## Classes

- `no-change`: `--from` and `--to` are one commit (path `-`).
- `not-ancestor`: `--from` is not an ancestor of `--to` (path `-`).
- `too-many-files`: more than 200 changed entries (path `-`).
- `bad-path`: a path that is not UTF-8 or holds a control character. The line says `<unprintable path>`, not
  the name.
- `new-file`: an added path.
- `deleted-file`: a deleted path. A rename shows as a deleted and an added path, so the first one in git order
  is named.
- `mode`: a mode change or a type change.
- `link`: a symbolic link on either side.
- `submodule`: a submodule entry on either side.
- `not-words-file`: the path is not a words file.
- `too-large`: either blob is over 1 MiB. It is refused, not cut.
- `not-utf8`: either blob is not UTF-8.
- `unparsed`: the lexer cannot read a `.rs` version to its end (an unterminated string, char literal or
  block comment).
- `code-changed`: the two `.rs` versions differ after comments are removed.

## What the lexer removes and keeps

The lexer reads each `.rs` version and removes comments:

- a line comment `//` to its line end (a `/*` inside it opens nothing)
- a block comment `/* */`, with nesting
- the line doc forms `///` and `//!` (not `////`)

It keeps, so a change there is a code change (rustdoc runs these as doctests):

- a block doc comment `/** */` or `/*! */`, whole
- in a run of line doc comments, a fence line (text after the marker and leading blanks starts with three or
  more backticks or three or more tildes) and the lines up to and with the next fence line of the same
  character, or to the end of the run when none closes it
- a line doc comment whose text after the marker and at most one blank starts with four blanks or a tab

A run of line doc comments is a set of consecutive lines that each end in a line doc comment.

Removing a comment also removes the blanks before it on its line. A block comment with a non-blank byte
before it (ahead of those blanks) and a non-blank byte right after it is replaced by one blank, so two
tokens are not joined. A line left with only blanks is dropped with its newline when it held comment text.
A line that held no comment text is kept as it is, blank or not. A CR before an LF stays in the code, so a
line end change on a code line is a code change.

The lexer reads string literals with escapes, byte strings, raw strings (`r"..."`, `r#"..."#` with any
number of `#`), byte and C string forms of both, raw identifiers such as `r#type` (not a raw string), char
and byte literals (with escapes such as a quote, `\n` and `\u{...}`), and lifetimes and loop labels. A quote
starts a char literal when the next character is a backslash, or when the character after the next one is
a quote. Else it is a lifetime or label.

## The ok line

Exit 0 with stderr empty and one stdout line:

```
words-only yes from=<from> to=<to> files=<count of changed entries>
```

## What it does not do

- It decides no review and marks no finding. A reviewer marks a finding as wording only. A caller uses the
  answer as one input beside the reviewer mark.
- It does not replace the gate, because some checks read comment and docs text. As of 1ef1556,
  `ci/lint-rust-tests.sh` and `ci/lint-fixtures.sh` read a code line with a trailing comment and block
  comment lines, and `tests/intake.rs` runs that lint on its own file. `tests/manifest.rs` reads
  `docs/resource-manifest.md`. `ci/install-guide-smoke.sh` reads `README.md` and `docs/install.md`. A comment
  edit can fail a lint.
- It says nothing about whether a sentence is true.
- It reads shell files as `not-words-file` (a later change).
- It reads two commits, not the work tree.
- A fix round that was rebased onto a new base is refused as `not-ancestor`.
