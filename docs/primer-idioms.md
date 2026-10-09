# Lacon primer

Lacon is statically typed and indented like Python. Most Python and Rust
guesses work, and errors say what to write instead. Write it as the example
does: list methods and destructuring in place of index loops.

```
type Rec {name str, score int}

fn parse_rec(line str) Rec! =
  (name, n) = line.split_once(",") ?? fail "bad line: {line}"
  Rec{name.trim(), int(n.trim())?}

fn main()! =
  recs = io.lines()[1..].filter(!it.is_empty()).map(parse_rec(it)?)
  var by_name = {}
  for r in recs: by_name[r.name].push(r.score)
  for name, xs in by_name.sort_by((-it.1.sum(), it.0)):
    print("{name:<8}{xs.max():>5}{xs.sum() / xs.len:>5}")
  var best = [0] * 3
  for (i, r) in recs.enumerate(): best[i % 3] = best[i % 3].max(r.score)
  print(best, recs.count(it.score > 50), recs.map(it.name).unique().len)
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
- A `T!` function fails with `fail "msg"`; `f()?` passes the error up;
  `x ?? d` gives a default. Using a `T?` or `T!` as a `T` is a compile error.
  To handle an error, `match r` with arms `err(e): ...` then `v: ...`.
- Int `/` and `%` truncate as in Rust: `7 / 2` is `3`. Convert with `int(x)`,
  `f64(x)`, `str(x)`. Conditions must be `bool`: `!xs.is_empty()`. Assignment
  copies: after `row = grid[i]`, changing `row` leaves `grid` alone; write
  `grid[i][j] = x`.
- Methods return new values: `xs = xs.sort()`, `xs.sort_by(-it.age)` (a key,
  not a comparator), `xs.rev()`. These change the list and need `var`: `push`,
  `pop()` (T?), `insert(i, x)`, `remove(i)` (by index), `extend`. Lambdas are
  `xs.filter(it > 0)` or `|a, b| a + b`; `?` in one passes the error up from
  the function: `xs.map(int(it)?)`.
- Lists also have `sum`, `min`, `max`, `count(pred)`, `any`, `all`,
  `index(x)` (int?), `get(i)` (T?), `unique`, `zip`, `take`, `skip`,
  `group_by(key)` (a map of lists) and `windows(n)`. `[0] * n` repeats,
  `xs[1..]` slices, `(a, b) = pair` and `for (i, x) in xs.enumerate():`
  destructure.
- Maps: `m[k]` on a missing key is an error, `m.get(k)` is `V?`, `m.remove(k)`
  deletes, and `m[k] += 1` or `m[k].push(x)` creates the entry.
  `for k, v in m:`; `sort_by`, `filter` and `map` on a map see `(k, v)` pairs.
- `heap()` is a min-heap with `push(x)` and `pop()` (T?); push `(-p, x)` for a
  max-heap.
- Strings interpolate: `"{x} {y:.2f} {s:>{w}}"`. `{{` is a literal brace;
  inside `{...}` quote with `'`. Strings index and slice by char. `trim`,
  `trim_end("\r")`, `split(sep)`, `split()`, `lines`, `chars`, `starts_with`,
  `ends_with`, `contains`, `find` (int?), `replace`, `upper`, `lower`,
  `is_digit`, `is_alpha`, `is_space`, `ord`, `n.chr()`, `parse()`,
  `pad_left(n, "0")`, `split_once(sep)` (`(str, str)?`), `xs.join(", ")`.
- `io.read()` (all of stdin), `io.lines()`, `io.read_line()` (str?),
  `print(a, b)`, `io.write(s)` (no newline), `os.exit(code)`.
