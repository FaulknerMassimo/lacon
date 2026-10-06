# Lacon primer

Lacon is a statically typed, memory-safe language. `lacon run f.lc` runs `main`;
`lacon test|check|sig f.lc` runs the tests, reports errors, lists signatures.

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

- Indented blocks: `:` opens a block and `=` a function body; a block can be the
  rest of the line (`if x < 0: return 0`). No `let`, braces, semicolons, imports
  (the standard library is in scope) or top-level statements.
- `x = 1` is immutable, `var x = 0` mutable. Assigning to a name from an outer
  block, `+=`, setting fields and `push` need `var`; rebinding in the same block
  is fine (`line = line.trim()`). Names bound in a block end with it, so write
  `y = if c: 1 else: 2`.
- `a, b = pair` destructures. A top-level `MAX = 100` is a constant.

## Types

`int` (i64), `i8`..`u64`, `f64`, `f32`, `bool`, `str` (UTF-8; a one-character
`str` is a char). `[T]` list, `{K:V}` map (insertion-ordered), `{T}` set,
`(A, B)` tuple, `T?` optional (a value or `none`), `T!` result (a value or an
error). `{}` is an empty map, `set()` an empty set. Ints and floats mix into
`f64`; overflow traps. Convert with `int(x)`, `f64(x)`, `str(x)` or `x as f64`.

## Functions

```
fn clamp(x int, lo int = 0, hi int = 9) int =
  if x < lo: return lo
  x.min(hi)  # the last expression is the result
fn biggest[T](a T, b T) T = if a > b: a else: b
fn bump(mut n int) = n += 1  # `mut` changes the caller's variable
```

- Every parameter is typed. A function is also a method of its first
  parameter's type: `fn area(s Shape)` is called `s.area()` or `area(s)`, and
  builtins work both ways (`len(xs)`). Methods without arguments may drop the
  `()`: `xs.len`, `s.lines`.
- Lambdas use the implicit `it` (`xs.filter(it.age > 18)`) or `|a, b| a + b`.
  Function types are `fn(int) int`.
- No classes, `impl`, `self`, exceptions, macros or `async`.

## Structs and enums

```
type Point {x int, y int = 0}
p = Point{1, 2}  # or Point{x: 1, y: 2}, or Point{x, y}
var q = p  # a copy: values, not references
q.x = 5  # p is unchanged

enum Shape = Circle(f64) | Rect(f64, f64) | Empty

fn area(s Shape) f64 = match s
  Circle(r): 3.14 * r * r
  Rect(w, h): w * h
  Empty: 0.0
```

A match covers every variant or ends with `_:`. Patterns: literals, `1 | 2`,
`1..=9`, `_`, names, `(a, _)`, `[first, ..rest]`, `Shape.Circle(r)`, `none`,
`err(e)`, guards `n if n > 0:`. Arms can be indented blocks.

## Control flow

`if`/`elif`/`else` and `while` as in Python. `for x in xs:`, `for i in 0..n:`
(`0..=n` includes n), `for i, x in xs.enumerate():`, `for k, v in m:`
(`for k in m:` gives keys). `if` and `match` are expressions. Operators are
Python's except `//` and `~`, plus `??`; int `/` and `%` truncate as in Rust.
Conditions must be `bool` (no truthiness): `!xs.is_empty()`, `n != 0`.

## Errors and optionals

- A `T!` function fails with `fail "msg"`. `f()?` passes an error up, also from
  inside a lambda (`lines.map(parse(it)?)`); in a `T?` function, `x?` returns
  `none`, also for an error. Using a `T?` or `T!` as a `T` unhandled is a
  compile error; after `if x != none:`, `x` is a `T`.
- `x ?? d` replaces `none` or an error with `d`. To handle an error, `match r`
  with arms `err(e): ...` then `v: ...`.
- `m.get(k)`, `xs.get(i)`, `xs.first`, `xs.find(pred)`, `s.find(sub)`,
  `xs.pop()`, `h.pop()` and `io.read_line()` return `T?`.

## Strings

Every string interpolates: `"{name} is {age}"`. Format specs are Python's
(`{n:>5}`, `{x:,.2f}`, `{s:<{w}}`), but `{x:.2}` means two decimals. `{{` is a
literal brace, as is a `{` with no closing `}`; inside `{...}`, quote strings
with `'`. `"""` strings span lines and drop common indentation.

Strings index and slice like lists, by character. `len`, `split(",")`,
`split()` (on whitespace), `lines`, `trim`, `upper`, `lower`, `replace(a, b)`,
`starts_with`, `ends_with`, `contains`, `find`, `parse()` (a number or an
error), `chars`, `rev`, `repeat(n)`, `is_digit`, `is_alpha`, `is_space`, `ord`,
`n.chr()`, `split_once(sep)` ((str, str)?), `pad_left(n, "0")`, `xs.join(", ")`.

## Collections

Methods return new values and leave the receiver alone, so `xs.sort()` alone on
a line is an error: write `xs = xs.sort()`.

Lists: `len first last contains index(x) map filter find position any all
count sum product min max min_by max_by sort sort_by rev unique enumerate zip
flat_map flatten take skip take_while skip_while chunks windows group_by
partition fold(init, f) reduce join to_set to_map`. Slices `xs[1..3]` or
`xs[1:3]`; `xs[-1]` and `xs + ys` work as in Python. Sort descending with
`sort_by(-it.score)`, on two keys with `sort_by((it.city, -it.age))`.

Mutators (need `var`): `push(x)`, `pop()`, `insert(i, x)`, `remove(i)`,
`extend(ys)`, `clear()`, `swap(i, j)`, `retain(pred)`.

Maps: `m[k]` (a missing key is an error), `m.get(k)`, `m.get(k, d)`, `k in m`,
`m.remove(k)`, `keys`, `values`, `items`, `len`. `m[k] += 1` and
`m[k].push(x)` create a missing entry first. Other methods see `(k, v)` pairs:
`m.sort_by(-it.1)`, `m.filter(it.1 > 2).to_map()`.

Sets: `s.add(x)`, `s.remove(x)`, `x in s`, `a | b`, `a & b`, `a - b`. Ranges:
`(0..n).rev()`, `.step_by(2)`, `.to_list()`. `heap()` or `heap(xs)` is a
min-heap (type `heap[T]`) with `push(x)`, `pop()` (the smallest), `first`,
`len`; push `(-priority, x)` for a max-heap.

## Standard library

`print(a, b)`, `eprint`, `io.read()` (all of stdin), `io.lines()`,
`io.read_line()`, `io.write(s)` (no newline), `fs.read(path)` (str!),
`fs.write(path, s)` (()!), `os.args` (without the program name),
`os.exit(code)`, `math.pi`, `math.inf`, `int.max`, `panic(msg)`.

## Tests

`test "name": expr` passes when `expr` is true. A test can be a block with
`assert cond, "msg"` inside.
