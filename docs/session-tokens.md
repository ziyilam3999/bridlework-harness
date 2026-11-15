# `harness session tokens`

```
harness session tokens --transcript <file>
```

Reads a session transcript (JSON lines, one object per line) and prints exactly one stdout line:

```
tokens messages=<n> max_context=<n> output=<n> compactions=<n>
```

The file is read line by line and never held in memory whole, so a multi-gigabyte transcript is fine. Only the set of
distinct message ids is kept.

## Definitions

- **messages**: the number of distinct `message.id` over lines with `"type":"assistant"`. A repeated id counts once (the
  first line carrying it supplies its usage).
- **max_context**: the largest `input_tokens + cache_creation_input_tokens + cache_read_input_tokens` of any one message.
- **output**: the sum of `output_tokens` over distinct messages.
- **compactions**: the number of lines with `"type":"system"` and `"subtype":"compact_boundary"`.

## Exit codes

- `0`: the line was printed.
- `2`: anything else, with nothing on stdout:
  - a usage error (no `--transcript`, or extra arguments);
  - the file is missing, unreadable, or empty;
  - a line is not valid JSON (stderr names its 1-based line number);
  - an assistant line has no `message.id`, no `message.usage`, or a missing or non-integer (or negative, fractional,
    out-of-range) value in any of the four usage fields.

`harness session` with no verb prints a usage text that includes `session tokens --transcript <file>` and
`session cost --transcript <file> --model <id> [--prices <file>] [--label <text>] [--out <file>]`.

# `harness session cost`

```
harness session cost --transcript <file> --model <id> [--prices <file>] [--label <text>] [--out <file>]
```

Reads the transcript with the same reader as `session tokens` (same fields and no others, a repeated message id counts
once with its first line, same errors, exit 2 for the same bad inputs) and prints exactly one stdout line:

```
cost label=<text> model=<id> messages=<n> input=<n> cache_write=<n> cache_read=<n> output=<n> max_context=<n> compactions=<n> usd=<amount or unpriced>
```

- **label**: the `--label` value, or `-` when it is not given.
- **model**: the `--model` value. It is required and comes from the caller; the verb does not read a model name from the
  transcript.
- **messages**, **max_context**, **compactions**: as defined for `session tokens` above.
- **input**, **cache_write**, **cache_read**, **output**: the sums over distinct messages of `input_tokens`,
  `cache_creation_input_tokens`, `cache_read_input_tokens` and `output_tokens`. A sum that does not fit in an unsigned
  64-bit integer exits 2.
- **usd**: the amount (below), or `unpriced`.

`--model` and `--label` accept only ASCII letters, digits, `.`, `_`, `-` and `:`, 1 to 128 characters. Anything else exits
2 with nothing on stdout.

## The price file (`--prices`)

A text file of at most 65536 bytes that is a regular file (a symbolic link or a directory is refused). One line per model:
the model id and four prices in US dollars per one million tokens, separated by spaces or tabs, in the order input, cache
write, cache read, output. A price is a plain decimal: digits, then optionally a point and 1 to 6 digits; at most 9 digits
before the point; no sign, no exponent. The model id follows the same character rule as `--model`. Empty lines (blank or
only spaces) and lines whose first character is `#` are skipped. Example (the numbers are an example, not real prices):

```
# model            input  cache_write  cache_read  output
example-model-a    3      3.75         0.3         15
example-model-b    0.25   0.3125       0.025       1.25
```

A line that does not have exactly five fields, a bad price, a model id listed twice, a file that is not a regular file or is
larger than the limit, or a file that is not UTF-8 exits 2 with a stderr line that names the problem (and the 1-based line
number for a line problem) and nothing on stdout. A path where nothing exists exits 2.

## The amount and the rounding rule

Whole-number arithmetic only; no floating point is used to compute or to print the amount. Each price is held in millionths
of a dollar per million tokens. The amount in millionths of a dollar is
`(input*p_input + cache_write*p_cache_write + cache_read*p_cache_read + output*p_output) / 1000000`, rounded to the nearest
whole millionth, with an exact half rounded up. It is printed as a plain decimal with exactly 6 digits after the point, for
example `usd=0.012345`. The prices used are those of the line whose model id equals `--model` exactly (case included).

`usd=unpriced` is printed, with exit 0, when `--prices` is not given, or when the price file is valid and has no line for the
model. A missing price never invents a number and never fails the token count. A refused price file is not unpriced: it
exits 2.

## `--out <file>`

After the line was printed, the same line plus a newline is appended to the file. The file is created when it does not
exist; an existing file keeps every earlier byte. The path must be absolute, and an existing path must be a regular file: a
relative path, a symbolic link, a directory or any other kind exits 2 with nothing on stdout and the path untouched. Nothing
is written when the transcript, the price file, `--model`, `--label` or the `--out` path was refused. When the append itself
fails (for example the folder does not exist) the line has already been printed on stdout, the exit code is 2 and stderr says
the write failed.

## What the verb does not know

- Whether the prices in the price file are right: it uses the numbers it is given.
- Which model really produced a message: the caller says so with `--model`.
- Anything a provider bills that is not in the four usage numbers.
