# harness build check

Says whether the engine binary was built from the head of the engine source. The binary carries the
commit it was built from as a build stamp, and the verb compares that commit with the head of a repo
dir. It compares commits, not changes: a merge that changes no file a caller uses still makes an
older binary older.

## Synopsis

```
harness build check
  --repo-dir <absolute-dir>
  --ref <ref>
```

- `--repo-dir`: a folder of the engine repo (the primary clone or a scratch checkout). Absolute. The
  verb does not fetch, so the head it reads is the head as last fetched: the caller fetches first.
- `--ref`: the ref whose commit is the head (for example `origin/master` or `HEAD`). Not empty, not
  starting with a dash. A tag is peeled to its commit.

Both flags are required, each once. A missing or repeated option, an unknown option, a flag with no
value, a relative or missing repo dir, an absolute repo dir that holds `..` (`--repo-dir must be an
absolute path`), a repo dir that is not a folder (`--repo-dir is not a folder`), or an empty ref / a
ref starting with a dash is exit 2 with a
`harness: error:` line and the usage line on stderr, stdout empty.

## The stamp

The stamp is the value of the environment variable `HARNESS_BUILD_COMMIT` read at build time by
`option_env!` in `core/rust/harness/src/build_check.rs`. The package has no build script, no new crate
(std only) and no git call at build time; rustc itself tracks the variable, so a rebuild after an
empty commit takes the new stamp with no source change (a build script reruns only when a path it
lists changes, so a doc-only merge would leave an old stamp in a fresh binary and the check would
call that binary older for good).

The stamp must be 40 lower-case hex characters. A build with the variable not set has no stamp: the
check refuses it as cannot tell, so an unstamped binary is not taken as current. A variable set but
empty, upper case, 39 or 41 characters or a word is refused with the same cannot-tell line. A plain
cargo build or cargo test sets no stamp while `HARNESS_BUILD_COMMIT` is not set in the caller's
environment; if it is exported there, that build picks it up.

`ci/stamped-build.sh` is the one place that sets the stamp. It builds `core/rust` release with the
variable set to the commit of `HEAD` of the checkout it lives in, prints
`stamped-build: built commit=<sha>` and exits 0. It exits 1 with nothing on stdout, and runs no
build, when `HEAD` has no commit, when `git status --porcelain --untracked-files=all` lists a change
(a tracked edit or an untracked file, whatever `status.showUntrackedFiles` says), or when that `git status`
call itself fails (an unreadable or corrupt index). It also exits 1
when an index entry is flagged assume-unchanged or skip-worktree (`git ls-files -v` prints a tag other
than `H`; such a flag hides an edit from `git status`) or when `git ls-files` fails. It also exits 1
when the cargo build itself fails. Both git calls run with `core.fsmonitor=false`,
`core.trustctime=true` and `core.checkStat=default`, so a repo config that sets those keys otherwise
(a hook that reports no change, a loose stat compare) cannot hide an edit; other config keys
are not overridden. Other repo config, for example a clean filter named in `.git/info/attributes`, can
still hide an edit from `git status`, as ignored files can; a fresh clone (the rebuild hand step)
carries no such config. `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_COMMON_DIR` and
`GIT_OBJECT_DIRECTORY` are cleared before the git calls, so the stamp is the commit of `HEAD` of the
checkout the script lives in. The
work tree check does not list files ignored by `.gitignore`, `.git/info/exclude` or
`core.excludesFile`, so an ignored file can still change the build; the
stamp names the commit of `HEAD`, which matches the built code when the tree is otherwise clean. A
stamp set by hand to a commit that was not built is not seen by the verb; the script is the guard.

The release build, the release smoke and the clean-install scripts set no stamp, so a released
tarball holds none (while `HARNESS_BUILD_COMMIT` is not set in the build environment) and its check
says cannot tell. Nothing in the public repo refuses an unstamped build, and the install page
(docs/install.md) builds unstamped: this is by design.

## The compare

The head is `git rev-parse --verify <ref>^{commit}` in the repo dir, run read-only with `GIT_DIR` and
`GIT_WORK_TREE` removed from the environment, no stdin, no replace refs and no lazy fetch (an object
missing from the object store fails the call, it is not fetched).

| case | exit | stdout | stderr |
|---|---|---|---|
| the stamp equals the head | 0 | `build current commit=<first 12 of the stamp>` | empty |
| the stamp is a commit of the repo dir and an ancestor of the head, and not equal | 1 | empty | `harness: build check refused: older: this binary is built from <first 12 of the stamp>, the head is <n> commits ahead; rebuild it` |
| every other case | 2 | empty | `harness: build check refused: cannot tell: <reason>` |

`n` is `git rev-list --count <stamp>..<head>`, at least 1. The cannot-tell reasons, one per line:

- `this binary has no build stamp`
- `the build stamp is not a 40 character lower-case commit id`
- `the head ref cannot be resolved in the repo dir`
- `the build stamp is not a commit of the repo dir`
- `the build stamp is not an ancestor of the head`
- `the commits between the stamp and the head cannot be counted`
- `git is not runnable`

A stamp on a side branch or a stamp ahead of the head gives `not an ancestor`, and a stamp naming a
commit the repo dir has not fetched gives `not a commit of the repo dir`: the verb cannot show that
this binary holds the head, so it is not called current. A folder that is no repo, and is not inside
another repo, gives `the head ref cannot be resolved in the repo dir`; a folder inside another repo
is read against that parent repo.

A caller that must not go on with an older engine refuses on each non-zero exit: the only exit 0 is
an equal stamp.

## What it does not do

- It fetches nothing and writes nothing; it reads one binary constant, the arguments and read-only
  git answers.
- It compares commits, whatever the change is. This is the accepted cost of the check: after each
  engine merge the running binary is reported older until it is rebuilt, even when the merge changes
  nothing the caller uses.
- It does not check that the stamp was really built from that commit (the script is the guard).
- The head is the head as last fetched: a head not yet fetched makes the verb say current for a
  binary that holds it.
- No other verb changes behaviour, and `harness --version` prints the same line as before. The
  top level usage text does change: it names the new `build check` entry.

## The test flag

`--stamp-for-test <sha>` is read only when the test mode flag `--testmode synthetic-test` comes with
the call, and stands in for the compiled stamp, so the compare can be tested against scratch repos
with synthetic commits. Without the test mode flag it is an argument fault (exit 2).
