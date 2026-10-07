# Lacon primer

Lacon is statically typed and indented like Python. Most Python and Rust
guesses work, and errors say what to write instead.

```
type Rec {name str, score int}

fn parse_rec(line str) Rec! =
  p = line.split(",").map(it.trim())
  if p.len != 2: fail "bad line: {line}"
  Rec{p[0], p[1].parse()?}

fn main()! =
  var totals = {}
  for line in io.lines():
    r = parse_rec(line)?
    totals[r.name] += r.score
  for name, n in totals.sort_by(-it.1):
    print("{name:<8}{n:>5}")
```

- `fn name(a int, b [str]) Ret =`, then an indented body or one expression;
  the last expression is the result. No braces, `let`, imports or classes.
- `x = 1` is immutable. `+=`, `push`, setting a field and assigning from an
  inner block need `var x = 0`. A name bound in a block ends with it:
  `y = if c: 1 else: 2`.
- Types: `int`, `f64`, `bool`, `str` (a char is a `str`), `[T]`, `{K:V}`,
  `{T}`, `(A, B)`, `T?` (a value or `none`), `T!` (a value or an error). `{}`
  is an empty map, `set()` an empty set. `enum Op = Add | Num(int)`, then
  `match op` with indented arms `Num(n): ...` and `_: ...`.
- A `T!` function fails with `fail "msg"`; `f()?` passes the error up; `x ?? d`
  gives a default. Using a `T?` or `T!` as a `T` is a compile error. To handle
  an error, `match r` with arms `err(e): ...` then `v: ...`.
- Int `/` and `%` truncate as in Rust: `7 / 2` is `3`. Conditions must be
  `bool`: `!xs.is_empty()`. Assignment copies: after `row = grid[i]`, changing
  `row` leaves `grid` alone; write `grid[i][j] = x`.
- Methods return new values: `xs = xs.sort()`, `xs.sort_by(-it.age)`,
  `xs.rev()`. These change the list and need `var`: `push`, `pop()` (T?),
  `insert(i, x)`, `remove(i)` (by index), `extend`. Lambdas are
  `xs.filter(it > 0)` or `|a, b| a + b`.
- Maps: `m[k]` on a missing key is an error, `m.get(k)` is `V?`, and `m[k] += 1`
  or `m[k].push(x)` creates the entry. `for k, v in m:`; `sort_by`, `filter`
  and `map` on a map see `(k, v)` pairs.
- `heap()` is a min-heap with `push(x)` and `pop()` (T?); push `(-p, x)` for
  a max-heap.
- Strings interpolate: `"{x} {y:.2f} {s:>{w}}"`. `{{` is a literal brace;
  inside `{...}` quote with `'`. Strings index and slice by char. `trim`,
  `split(sep)`, `split()`, `lines`, `chars`, `starts_with`, `ends_with`,
  `contains`, `find` (int?), `replace`, `upper`, `lower`, `is_digit`,
  `is_alpha`, `is_space`, `ord`, `n.chr()`, `parse()`, `pad_left(n, "0")`,
  `split_once(sep)`, `xs.join(", ")`.
- `io.read()` (all of stdin), `io.lines()`, `io.read_line()` (str?),
  `print(a, b)`, `io.write(s)` (no newline), `os.exit(code)`.
