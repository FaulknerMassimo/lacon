# Lacon primer

Lacon is a statically typed, memory-safe language. Files end in `.lc`.
`lacon run f.lc [args]` runs `main`; `lacon test f.lc` runs tests and prints only
failures; `lacon check f.lc` reports errors; `lacon sig f.lc` lists signatures.

```
# comment
type User {name str, age u32, city str}

fn parse_line(line str) User! =
  p = line.split(",").map(it.trim())
  if p.len != 3: fail "bad line: {line}"
  User{p[0], p[1].parse()?, p[2]}

fn main()! =
  var counts = {}
  for line in fs.read("users.csv")?.lines.skip(1):
    u = parse_line(line)?
    if u.age >= 18: counts[u.city] += 1
  for city, n in counts.sort_by(-it.1):
    print("{city}: {n}")

test "parses": parse_line("ann, 30, Oslo")? == User{"ann", 30, "Oslo"}
```

## Layout and bindings

- Indented blocks. `:` opens a block, `=` opens a function body. A block may be
  the rest of the line: `if x < 0: return 0`.
- No `let`, braces, semicolons or imports; the standard library is always in
  scope. No top-level statements: `fn main()` runs.
- `x = 1` is immutable, `var x = 0` is mutable. Assigning, `+=`, setting fields
  and `push` all need `var`. Rebinding in the same block is fine
  (`line = line.trim()`).
- A name bound inside a block ends with it. Use the block's value instead:
  `y = if c: 1 else: 2`.
- `a, b = pair` destructures; `a, b = b, a` swaps `var`s. Top-level `MAX = 100`
  is a constant.

## Types

`int` (64-bit), `i8`..`i64`, `u8`..`u64`, `f64`, `f32`, `bool`, `str` (UTF-8;
one-character strings stand in for chars). `[T]` list, `{K:V}` map
(insertion-ordered), `{T}` set, `(A, B)` tuple, `T?` optional (a value or
`none`), `T!` result (a value or an error). Literals: `[1, 2]`, `{"a": 1}`, `{}`
(empty map), `{1, 2}`, `set()`, `(1, "a")`, `none`.

Ints and floats mix (the result is `f64`); int `/` truncates; overflow traps.
Convert with `int(x)`, `f64(x)`, `str(x)` or `x as f64`.

## Functions

```
fn add(a int, b int) int = a + b
fn clamp(x int, lo int = 0, hi int = 9) int =
  if x < lo: return lo
  x.min(hi)                       # the last expression is the result
fn log(msg str) = print(msg)      # no return type: returns nothing
fn biggest[T](a T, b T) T = if a > b: a else: b
fn bump(mut n int) = n += 1       # `mut`: changes the caller's variable
```

- Every parameter is typed. A function is also a method on its first
  parameter's type: `fn area(s Shape) f64` is called `s.area()` or `area(s)`.
  Builtin methods also work as functions: `len(xs)`, `abs(x)`.
- Methods with no arguments can drop the parentheses: `xs.len`, `s.lines`.
- Lambdas: `it` is the implicit parameter (`xs.filter(it.age > 18)`), or write
  `|a, b| a + b`. Function types are `fn(int) int`.
- No classes, `impl`, `self`, exceptions, macros or `async`.

## Structs and enums

```
type Point {x int, y int = 0}     # field default
p = Point{1, 2}                   # or Point{x: 1, y: 2}, or Point{x, y}
var q = p                         # a copy: values, not references
q.x = 5                           # p is unchanged

enum Shape = Circle(f64) | Rect(f64, f64) | Empty

fn area(s Shape) f64 = match s
  Circle(r): 3.14 * r * r
  Rect(w, h): w * h
  Empty: 0.0
```

A match must cover every variant or end with `_:`. Patterns: literals, `1 | 2`,
`1..=9`, `_`, names, `(a, _)`, `[first, ..rest]`, `Shape.Circle(r)`, `none`,
`err(e)`, and guards `n if n > 0:`. Arms can be indented blocks.

## Control flow

`if`/`elif`/`else`, `while c:`, `break`, `continue`, `return x`. `for x in xs:`,
`for i in 0..n:` (`0..=n` includes n), `for i, x in xs.enumerate():`,
`for k, v in m:` (`for k in m:` gives keys). `if` and `match` are expressions;
`a if c else b` works too. Inside brackets lines do not matter, so a `match`
whose arms need their own lines cannot go there: bind it to a name first.

Operators: `+ - * / % **`, `and or not`, `== != < <= > >=` (chains allowed),
`in`, `not in`, `??`, `& | ^ << >>`. Conditions must be `bool`; there is no
truthiness, so write `!xs.is_empty()` or `n != 0`.

## Errors and optionals

- A `T!` function fails with `fail "msg"`. `f()?` passes an error up, also from
  inside a lambda: `lines.map(parse(it)?)`.
- `x ?? d` replaces `none` or an error with `d`.
- Handle an error with `match r` and arms `err(e): ...` then `v: ...`.
- `m.get(k)`, `xs.get(i)`, `xs.first`, `xs.find(pred)`, `s.find(sub)` and
  `io.read_line()` return `T?`. In a `T?` function, `x?` returns `none`.
- Ignoring an error value is a runtime error.

## Strings

Every string interpolates: `"{name} is {age}"`. Specs: `{x:.2}`, `{n:>5}`,
`{n:05}`, `{n:x}`. `{{` is a literal brace (a `{` with no closing `}` is literal
too). `'single'` works; `"""` strings span lines and drop their common
indentation.

`s.len` (characters), `s[i]`, `s[a..b]`, `s[-1]`, `+`, `s * 3`, `split(",")`,
`split()` (whitespace), `lines`, `trim`, `upper`, `lower`, `replace(a, b)`,
`starts_with`, `ends_with`, `contains`, `find` (int?), `parse()` (a number, or
an error), `chars`, `rev`, `repeat(n)`, `is_digit`, `is_alpha`, `is_space`,
`ord`, `n.chr()`, `split_once(sep)` ((str, str)?), `pad_left(n, "0")`,
`xs.join(", ")`.

## Collections

Methods return new values and leave the receiver alone, so `xs.sort()` on its
own line is an error: write `xs = xs.sort()`.

Lists: `len is_empty first last get(i) contains index(x) map filter find
position any all count sum product min max min_by max_by sort sort_by rev unique
enumerate zip flat_map flatten take skip take_while skip_while chunks windows
group_by partition fold(init, f) reduce join to_set to_map`. Slices `xs[1..3]`,
`xs[2:]`; `xs[-1]` is the last item; `xs + ys` concatenates. Sort descending
with `sort_by(-it.score)`, by two keys with `sort_by((it.city, -it.age))`.

Mutators (need `var`): `push(x)`, `pop()` (T?), `insert(i, x)`, `remove(i)`,
`extend(ys)`, `clear()`, `swap(i, j)`, `retain(pred)`.

Maps: `m[k]` (a missing key is an error), `m.get(k)`, `m.get(k, d)`,
`m[k] = v`, `k in m`, `m.remove(k)`, `keys`, `values`, `items`, `len`.
`m[k] += 1` and `m[k].push(x)` create a missing entry first. Other methods see
`(k, v)` pairs: `m.sort_by(-it.1)`, `m.filter(it.1 > 2).to_map()`.

Sets: `s.add(x)`, `s.remove(x)`, `x in s`, `a | b`, `a & b`, `a - b`. Ranges:
`(0..n).rev()`, `.step_by(2)`, `.to_list()`.

## Standard library

`print(a, b)` (space-separated), `eprint`, `io.read()` (all stdin),
`io.lines()`, `io.read_line()` (str?), `io.write(s)` (no newline),
`fs.read(path)` (str!), `fs.write(path, s)`, `fs.lines(path)`, `fs.exists(p)`,
`os.args` ([str], without the program name), `os.env(k)` (str?),
`os.exit(code)`, `math.pi`, `math.inf`, `int.max`, `range(a, b, step)`,
`min(a, b)`, `max(xs)`, `panic(msg)`. Numbers: `abs min max pow sqrt floor
ceil round clamp`.

## Tests

`test "name": expr` passes when `expr` is true; a test can also be a block.
`assert cond, "msg"` checks inside it. When a test's last line is `a == b`, a
failure shows both sides and where they first differ.
