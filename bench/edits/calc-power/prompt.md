The program is a calculator over integers: each line of standard input is an
expression to print, a `let` statement, `vars`, or blank, and `#` starts a
comment. Add an exponent operator `^` to it:

- `a ^ b` is `a` raised to the power `b`. It binds tighter than `*`, `/`, `%`
  and unary minus, so `-2 ^ 2` is -4 and `2 * 3 ^ 2` is 18, and it is
  right-associative, so `2 ^ 3 ^ 2` is 512.
- The exponent may itself start with a minus: `2 ^ -1` parses, but a negative
  exponent is an error, printed as `line N: negative exponent` like the
  program's other errors. `0 ^ 0` is 1.
- `^` is an operator everywhere else an operator is: `2 ^` is an unexpected
  end, and `^ 2` an unexpected `^`, as the program reports those now.

Every value, including intermediate ones, fits in a signed 64-bit integer.
Everything else stays as it is.
