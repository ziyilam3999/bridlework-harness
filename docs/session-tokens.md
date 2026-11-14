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

`harness session` with no verb prints a usage text that includes `session tokens --transcript <file>`.
