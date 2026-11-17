# session limit-check

`harness session limit-check` reads one seat transcript and says whether the last error of the seat was a usage limit, of
which provider, and when it resets. It reads one file, writes nothing and calls no provider. Later tickets of plan row A25
record and act on the verdict; this verb only reports it.

## Synopsis

```
harness session limit-check --transcript <file> --provider <claude|zai> [--utc-offset <+HH:MM|-HH:MM>] [--now-ms <n>]
```

- `--transcript <file>`: required. The seat transcript, a JSON-lines file.
- `--provider <claude|zai>`: required. The provider of the seat, taken from the launch record.
- `--utc-offset <+HH:MM|-HH:MM>`: optional. A sign, hours 00 to 14, a colon, minutes 00 to 59. The zone of the Claude reset text.
- `--now-ms <n>`: optional. The clock in milliseconds since the epoch, digits only. Without it the system clock is used.

Each flag is given at most once. No environment variable is read, no file is written, no event is logged.

## The row that is read

The whole file is read line by line. Each line must be one JSON object (a CR LF line end is read; an empty line or a byte
order mark on line 1 is not valid JSON). An error row is a line whose top-level key `isApiErrorMessage` is the JSON value
`true`; words inside a string do not count. Only the last error row is classified.

Recovered: a line whose top-level `type` is `assistant` and whose `isApiErrorMessage` is absent or false comes after the last
error row. The seat got a real answer after its error. Rows of any other type after the error row do not count.

The fields read from the error row: `apiErrorStatus` (a number), `error` (a string), `apiErrorIsTransient` (a bool) and the
text of the first item of `message.content` whose `type` is `text`.

## The five limit shapes

A limit needs `apiErrorStatus` 429, `error` `rate_limit` and `apiErrorIsTransient` not true, and a text of one of these shapes
for the given `--provider`. The apostrophe is the plain ASCII one and the separator is the middle dot with one space each side.

| provider | text | code | scope |
|---|---|---|---|
| claude | starts `You've hit your weekly limit · resets ` | weekly | provider |
| claude | starts `You've hit your session limit · resets ` | session | provider |
| claude | starts `You've hit your <name> limit` or `You've reached your <name> limit` | model | fable or provider |
| zai | holds `[1310]` or `Weekly/Monthly Limit Exhausted` | 1310 | provider |
| zai | holds `[1113]` | 1113 | provider (resets unknown) |

`<name>` is 1 to 32 characters of ASCII letters, digits, spaces and dots, and is not weekly or session. A text with no name
(`You've hit your limit · resets 3pm`) matches no shape. The Fable split: when the first word of `<name>` is `Fable` the scope
is `fable`; a cap on any other Claude model marks the whole of Claude, scope `provider`. A z.ai text holding `[1308]` (a 5
hour window) is not a limit shape and is class unknown.

A Claude shape under `--provider zai`, or a z.ai code under `--provider claude`, is class unknown.

## Other and unknown

Other (exit 4): the row has `apiErrorIsTransient` true, or it has a text item, its status is not 429, its error is not
`rate_limit` and its text holds no limit marker.

Unknown (exit 5): any other last error row: a 429 or `rate_limit` row that matches no shape, a `[1308]` row, a provider
mismatch, a row with a marker whose status is not 429 or whose error is not `rate_limit`, a row that is not transient and has no text item.

The limit markers are the starts `You've hit your ` and `You've reached your `, and the words `[1310]`, `[1113]`, `[1308]` and
`Limit Exhausted`. The plain word limit is not a marker. Exit 0 means limit and nothing else.

## Reset time

`resets=` is `YYYY-MM-DDTHH:MM:SSZ` in UTC, or `unknown`.

- z.ai 1310: the text time `YYYY-MM-DD HH:MM:SS` is read at +08:00, the provider zone.
- Claude: the words after `resets ` and before the opening bracket of the zone name are `Mon D at <clock>` or `<clock>` alone.
  `<clock>` is `Ham`, `Hpm`, `H:MMam` or `H:MMpm` (lowercase, no space; H 1 to 12, MM two digits 00 to 59). The time is local at `--utc-offset`; the zone name
  in the text is not checked. Without `--utc-offset` the Claude reset is unknown.
- A date with no year takes the year of now at that offset, and the next year when that moment is not after now. A clock with
  no date takes today at that offset, or tomorrow when that moment is not after now. Feb 29 is read only in a leap year.
- Bound: a reset not after now, or more than 8 days (691200000 ms) after now, is unknown. Exactly 8 days is kept.
- A text with no reset words, or words that do not parse, gives `resets=unknown` and the class stays limit. 1113 gives unknown.

## Output line

One stdout line on exit 0, 4 and 5, fields in this order separated by one space:
`class provider scope code resets text`. `text=` is last and holds the first 80 characters of the error text, each character
below U+0020, and U+007F, replaced by one space. On exit 4 and 5, `provider`, `scope` are `-` and `code` is `apiErrorStatus` when it is a whole number of up to 6 digits, else `-`.

Run with `--utc-offset +08:00` and `--now-ms` at 2026-11-17T00:00:00Z:

```
class=limit provider=claude scope=provider code=weekly resets=2026-11-19T03:00:00Z text=You've hit your weekly limit · resets Nov 19 at 11am (Etc/UTC)
class=other provider=- scope=- code=403 resets=unknown text=Please run /login
class=unknown provider=- scope=- code=429 resets=unknown text=Too many requests
```

## Exit codes

| exit | meaning | stdout | stderr |
|---|---|---|---|
| 0 | the last error row is a limit | one line | empty |
| 2 | usage: missing, repeated or unknown flag, bad value, stray word; or the system clock is before the epoch | empty | the usage text, or `harness: error: clock is before the epoch` |
| 3 | the transcript is unreadable: missing, not a regular file, empty, not UTF-8, a line that is not a JSON object | empty | `harness: limit-check refused: reason=unreadable` and the cause, with the 1-based line number for a line problem |
| 4 | other | one line | empty |
| 5 | unknown | one line | empty |
| 6 | no error row, or the seat recovered | empty | `harness: limit-check: reason=no-error-row` or `reason=recovered` |

The verb reads one file, writes nothing and calls no provider.
