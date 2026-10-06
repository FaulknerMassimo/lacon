Standard input holds one JSON value, possibly spread over several lines. Print
it reformatted: objects with their keys sorted (by the decoded key, in byte
order), nested values indented by two spaces per level, `"key": value` inside
objects, items separated by a comma at the end of the line, and empty objects
and arrays printed as `{}` and `[]`. This is the format of Python's
`json.dumps(value, indent=2, sort_keys=True)`.

The input is restricted JSON: values are objects, arrays, strings, integers
(`-?(0|[1-9][0-9]*)`), `true`, `false` and `null`; strings contain printable
ASCII characters, with `\"` and `\\` as the only escapes, and are printed with
the same escapes; keys within an object are distinct; spaces, tabs and
newlines may appear between tokens. If the input is not such a value (for
example a trailing comma, a missing colon, an unknown escape, a number with a
leading zero or anything after the value), print `invalid`.
