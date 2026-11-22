# harness review recall

Scores a replay of the fresh reviewer over stored FAIL heads and clean controls: recall per stage
and false blocks per set. It reads two inputs (a case file and a folder of result files), prints the
score, and writes nothing. The stored FAIL heads come from the `verdict=fail` entries of
`~/.bridlework/logs/<ticket>.review-log` and their answer keys from the named friction rows; picking
the cases and marking the results is done outside the verb.

## Synopsis

```
harness review recall <cases-file> --results <dir>
```

- `--results <dir>`: the folder holding one result file per case and stage.
- `<cases-file>`: the JSON lines case file.

Both paths must be absolute with no `..` component. A relative path or a `..` part is exit 2 with the
usage line on stderr and stdout empty.

## The case file

JSON lines, one case per line, each line one JSON object of string fields:

| field | shape |
| --- | --- |
| `id` | 1 to 32 characters of `a-z`, `0-9` and `-` |
| `kind` | `fail` or `control` |
| `set` | `tune` or `test` |
| `repo` | the repo the head lives in (non-empty) |
| `head` | 40 lowercase hex characters |
| `answer_key` | one sentence, the fault the review found; `fail` cases only |

A control case must not carry an `answer_key`; a `fail` case must carry a non-empty one. An `id` may
appear on one line only. Unknown extra fields are ignored.

## The result files

`<dir>` holds one file per case and stage, named `<id>.<stage>`, for the stages `report`, `record`
and `fixround` in that order — every case, `fail` and `control` alike, needs all three. Each file is
one JSON object of string fields:

| field | shape |
| --- | --- |
| `verdict` | `pass` or `fail` |
| `matched` | `yes` or `no`; `fail` cases only, at every stage |

`matched` is the grader's mark of whether the stage's finding describes the answer key. A result
file of a control case must not carry `matched`. Every file in `<dir>` must be one of the expected
`<id>.<stage>` files: a file that names no case is exit 2.

## The hit rule per stage

A `fail` case is a hit at a stage only when that stage and every earlier stage has `verdict` `fail`
and `matched` `yes`. So a report miss hides every later mark, and a finding that never reaches the
fix round (an earlier stage passed, or an earlier mark was `no`) is a miss at stage `fixround`.

## The false-block rule

A control is a false block when its report stage has `verdict` `fail`. The record and fixround
verdicts of a control are read and checked, but not scored.

## Set rules

Half of the FAIL heads tune the review brief; the other half stay a fixed test set. The tuning half
cannot grade itself, so:

- the tune set must hold at least one `fail` case;
- the test set must hold at least one `fail` case and at least one `control`.

The tune set may hold controls; they are scored (their false-block line is printed) but are not
required.

## Output

Exit 0, stderr empty, and on stdout one line per set and stage, then one line per set, then the case
count:

```
set=tune stage=report fail_cases=6 hits=4 recall=4/6
set=tune stage=record fail_cases=6 hits=3 recall=3/6
set=tune stage=fixround fail_cases=6 hits=2 recall=2/6
set=test stage=report fail_cases=6 hits=5 recall=5/6
set=test stage=record fail_cases=6 hits=4 recall=4/6
set=test stage=fixround fail_cases=6 hits=4 recall=4/6
set=tune controls=2 false_blocks=0
set=test controls=2 false_blocks=1
cases=16 tune=8 test=8
```

The lines hold counts and the fixed words only — never an answer key and never a head.

## Exit codes

- 0: scored, the lines above on stdout.
- 2: any refusal, with one stderr line and stdout empty. No case is skipped: the score is buffered
  until every case is graded, so a refusal prints nothing on stdout. The refusals:
  - usage: a relative path, a `..` part, an unknown flag or subcommand;
  - the cases file is missing, a folder, unreadable, or not UTF-8 text;
  - a case line is not a JSON object of string fields;
  - an `id`, `kind`, `set` or `head` that is missing or not of its shape; a missing or empty `repo`;
  - a control case with an `answer_key`; a `fail` case with an empty `answer_key`;
  - a duplicate `id`;
  - the tune set has no `fail` case, the test set has no `fail` case, or the test set has no
    control;
  - the results dir is missing, not a folder, or holds a file that names no case;
  - a result file for any case and stage is missing or unreadable;
  - a result file is not a JSON object of string fields, or its `verdict` is not `pass` or `fail`;
  - a `fail`-case result has no `matched` field, or a `matched` value that is not `yes` or `no`;
  - a control result carries a `matched` field.

The error line names the case by its `id` (`harness: error: case <id>: <cause>`), or by line number
when the `id` itself is unreadable (`harness: error: case line <n>: <cause>`), and states the cause;
it never holds a field value, so no answer key and no head reaches stderr.

## Read only

The verb writes no file, creates no folder and runs no git call: it reads the cases file, the
results dir listing and the result files, and prints the score. Nothing is logged to the event log.
