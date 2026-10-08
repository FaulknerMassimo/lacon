# Lacon — Design and Build Plan

Lacon is a compiled, memory-safe language that AI agents write. It aims for
Rust-level runtime speed with fewer tokens than Python, and a toolchain that
minimises the total tokens an agent spends getting a task from prompt to
passing tests. Human readability is not a goal; agent accuracy is.

The name comes from *laconic*: saying a lot in few words, after the Laconians
(Spartans), who were known for terse replies. Source files use `.lc` and the
CLI is `lacon`.

---

## 1. The core thesis

### Optimise tokens-to-green, not characters per line

Most of an agent's tokens don't go into writing code. They go into reading
existing code, reading compiler output, and retrying after failures. The metric
Lacon optimises is **tokens-to-green**: the total input and output tokens an
agent spends from receiving a task to having its tests pass.

There are four levers:

1. **Syntax that suits the tokenizer.** Short code isn't always fewer tokens.
   Common English words (`fn`, `match`, `return`) are usually one token each.
   APL-style Unicode symbols often take two or three tokens and are guessed
   wrong more often. Lacon uses plain words and very little punctuation.
2. **Fewer failed attempts.** Rust's lifetimes, its two string types (`&str`
   and `String`) and fights with the borrow checker are a large source of
   agent retry loops. Lacon is designed so those errors cannot occur.
3. **Read less.** The compiler can show a module's signatures without its
   function bodies, so an agent doesn't read whole files to learn an API.
4. **Write less when editing.** An agent can replace a function by name instead
   of quoting the old text for a find-and-replace edit.

### The cold-start problem

No model has been trained on Lacon. Every token saved per line can be lost to a
single retry. Three rules follow from that:

- **Guessability.** Each construct should be what a model would guess from
  Python, Rust, Kotlin, Go or Swift. New semantics appear only where they pay
  for themselves.
- **Primer budget.** The whole language must be teachable in a primer of at
  most 3,000 tokens that sits in the agent's context.
- **Errors teach.** When an agent writes a construct from another language
  (`let x =`, `&str`, `String::new()`, `async fn`, `try:`), the compiler names
  the Lacon form in a one-line hint.

---

## 2. What it looks like

Read a CSV, count adults per city, print the counts sorted:

```
type User {name str, age u32, city str}

fn parse_line(line str) User! =
  p = line.split(',').map(it.trim())
  if p.len != 3: fail "bad line: {line}"
  User{p[0], p[1].parse()?, p[2]}

fn main()! =
  var counts = {}
  for line in fs.read("users.csv")?.lines.skip(1):
    u = parse_line(line)?
    if u.age >= 18: counts[u.city] += 1
  for city, n in counts.sort_by(-it.1):
    print("{city}: {n}")
```

The same program in each language, counted in Claude's tokenizer
(`uv run bench/tokens/count.py --claude-code`):

| Language | Tokens | Lacon saves |
|---|---|---|
| Go | 553 | 65% |
| Rust | 471 | 59% |
| Python | 239 | 20% |
| **Lacon** | **192** | — |

Claude's tokenizer isn't published as a library. `count.py` uses the
token-counting endpoint when API credentials are set, measures through
`claude -p` with `--claude-code`, and otherwise falls back to `o200k_base`,
which gives smaller counts and smaller savings (17% against Python, 54%
against Rust).

Against Python the syntax alone saves only about 20%, because Python is already
terse. What Lacon adds over Python is native speed and static safety; further
token savings have to come from fewer retries and from the toolchain (§4).

---

## 3. Language design

### 3.1 Syntax

| Area | Decision | Why |
|---|---|---|
| Blocks | Indentation; `:` for one-line bodies, `=` for function bodies | No closing-brace tokens, and models know the style well from Python |
| Bindings | `x = 1` is immutable, `var x = 0` is mutable | No `let`, and mutation stays visible |
| Lambdas | `it` is the implicit parameter (from Kotlin) | `.filter(it.age > 18)` |
| Strings | Every string interpolates `{expr}`; `{{` escapes | No `format!`, no `f` prefix |
| Standard library | Always in scope (`fs.read` needs no import) | Imports cost tokens and are a common error source |
| Struct literals | Positional `User{a, b, c}` or named `User{name: a}` | Positional saves field-name tokens (see §10) |

### 3.2 Types

- Primitives: `i8`–`i64`, `u8`–`u64`, `int` (i64), `f32`, `f64`, `bool`, and
  `str`, the single owned UTF-8 string type with value semantics.
- Composites: `[T]` list, `{K:V}` map, `(A, B)` tuple, `T?` optional, and
  `T!` / `T!E` for results.
- `type Name {field T, ...}` for structs and `enum` for sum types, with
  exhaustive `match`:

  ```
  enum Shape = Circle(f64) | Rect(f64, f64)

  fn area(s Shape) f64 = match s
    Circle(r): 3.14159 * r * r
    Rect(w, h): w * h
  ```

- Generics `fn max[T: Ord](a T, b T) T` and traits. No inheritance.
- **Inference:** signatures are written out; bodies are fully inferred.
  Signatures are the part agents read (§4), so they must be explicit. Bodies are
  where tokens add up, so nothing inside them needs an annotation.

### 3.3 Memory model: Rust-level speed without a borrow checker

- **Mutable value semantics** (the model used by Hylo and Swift).
  - Parameters are borrowed by default. `mut` lets a function change one in
    place and `own` moves it into the function.
  - References are second-class: they can be passed to functions but never
    stored in structs or returned. That means **lifetime annotations never
    exist**.
- **Compile-time reference counting** (Perceus, from Koka and Lean 4). The
  compiler inserts the counting and reuses memory in place when a value has a
  single owner, so pipelines like `xs.map(...).filter(...)` update in place
  when they can. There is no garbage collector and there are no pauses. Counts
  only become atomic for values that are shared between threads.
- **Escape hatches:** arenas, and `unsafe` raw pointers for hot paths.

### 3.4 Errors

- `T!` means the function can fail with an inferred error type; `T!E` names it.
- `?` passes an error up and `fail "msg"` raises one.
- No exceptions and no `Box<dyn Error>`-style boilerplate.

### 3.5 Concurrency

- No `async`/`await`, so functions never split into sync and async versions.
- Lightweight threads with structured `spawn` (children finish before their
  scope ends) and channels.
- Value semantics rules out data races without `Send`/`Sync` annotations.

### 3.6 Effects

The compiler infers each function's effects (`io`, `alloc`, `panic`, `spawn`)
and shows them in `lacon sig`, so an agent can tell what a function does without
reading its body. Nothing is written by hand. A package may optionally declare
an effect it forbids, such as `no io`.

### 3.7 Tests

Tests sit inline next to the code they test:

```
test "parses a line": parse_line("ann, 30, Oslo")? == User{"ann", 30, "Oslo"}
```

---

## 4. Agent-native toolchain

The toolchain matters as much as the syntax.

**`lacon sig <module>`** shows signatures, one-line docs and inferred effects,
without function bodies. Reading an API this way costs roughly a tenth of
reading the source.

```
$ lacon sig users
type User {name str, age u32, city str}
fn parse_line(line str) User!        # pure
fn main()!                           # io
```

**Diagnostics** are one line each, in a fixed format:

```
<code> <file>:<line>:<col> <message> [| fix: <replacement>]
E0301 users.lc:5:22 want u32 got str | fix: p[1].parse()?
```

- Errors caused by an earlier error are suppressed.
- Output is capped (default 20) with a count of the rest.
- Codes are stable; `lacon explain E0301` gives the long form on demand.
- `lacon fix` applies the suggested fixes. A fix that is only a shape to
  follow, which `lacon fix` leaves alone, reads `| e.g. fn name[T](...)`.

**Edits by name.** `lacon put users.lc` reads top-level items from stdin and
replaces each item of the same name, or adds it, and `lacon rename` and
`lacon add-field` cover common refactors. The agent never has to quote the
old code.

**Queries.** `lacon q def|callers|type` with short output: an item's source,
where a function is called, and the type of the expression at a position.
Lacon has no traits, so there is no `impls`.

**Tests.** `lacon test` prints only failures, each as a minimal diff.

**Also:** one formatter with no options, so diffs stay small; an MCP server and
an LSP; and the primer shipped as an agent skill and `CLAUDE.md` snippet.

---

## 5. Compiler architecture

Written in Rust.

```
source ─▶ parser (continues past errors) ─▶ name resolution ─▶ type inference
       ─▶ mode check (borrow / mut / own) ─▶ Lacon IR
       ─▶ RC insertion + reuse analysis ─▶ backend
```

- **Now:** the parser, the resolver (to an IR of slots and explicit lambdas),
  the type checker, then either the interpreter or the C backend, which
  unboxes the ints, bools and floats the checker proves (§16), packs
  lists of them (§17), takes small values from free lists (§18), reads
  a list once before a loop that only assigns its elements (§20) or their
  fields, and reads struct fields in place (§23). Mode checking, RC
  insertion and reuse analysis are not built yet.
- **Backends, in order:**
  1. **C** first. That's how Koka, Nim and Lean work, and it's the fastest way
     to native speed and portability.
  2. **Cranelift** for fast debug builds.
  3. **LLVM** directly, only if the C backend leaves performance on the table.
- **Incremental builds:** query-based compilation (the `salsa` crate) at
  function granularity, with caching by content hash.
- **Generics:** compiled per type in release builds and passed through
  dictionaries in debug builds, which avoids Rust's compile-time blowup.
- **Interop:** C ABI in both directions, so existing C libraries fill gaps in
  the ecosystem.

---

## 6. Targets

| Measure | Target |
|---|---|
| `lacon check` after a change | < 100 ms |
| Debug build, 10,000 lines | < 1 s |
| Release runtime | ≤ 1.2× Rust on the benchmark suite |
| Peak memory | ≤ 1.2× Rust, no GC |
| Tokens-to-green | ≥ 30% below Rust and Go, at or below Python |

---

## 7. Roadmap

### Phase 0 — Test the idea before building a compiler (3–4 weeks)

- Write `docs/primer.md`, at most 3,000 tokens.
- Write a parser and a throwaway tree-walking interpreter in Rust: no type
  checker, no performance work, just enough to run tests on agent output.
- Build a suite of 30 tasks with hidden tests, each solvable in under 150
  lines: parsing, data processing, CLI tools, algorithms.
- Build a harness that has Claude solve every task in Rust, Go, Python and
  Lacon (Lacon from the primer only), logging input and output tokens, turns,
  wall time and pass/fail.

**Done when** Lacon's tokens-to-green beats Rust and Go, and at least 90% of
first attempts parse. If not, change the syntax and rerun. This phase is the
cheapest place to find out the idea doesn't work.

**Status.** Built: the primer (2,988 tokens in Claude's tokenizer), the parser, the resolver and
interpreter (`crates/`), the `lacon` CLI with `run`, `test`, `check`, `sig` and
`explain`, golden tests over `tests/`, all 30 tasks with hidden tests, a Python
reference and a Lacon solution each (`bench/tasks/`), and the harness that has
Claude solve them (`bench/harness/`), through the API or through Claude Code
(`claude -p`, no API key needed). A pilot run, all 30 tasks in Lacon and
Python, is in §13: every episode passed, but Lacon took 2.92x Python's tokens
and 67% of its first attempts built. After fixes and a short primer
(`docs/primer-short.md`, 1,356 tokens), a run against Python, Rust and Go
(§13) had 97% of Lacon's first attempts build, and Lacon took 1.37-1.49x
their tokens but 0.76-0.93x their cost. Not done yet: more trials of Python,
Rust and Go, and deciding how cached tokens count. See §12 for the
semantics the interpreter settled on and §13 for what writing the tasks and
the runs taught.

### Phase 1 — MVP

Type checker, C backend, core standard library (`str`, collections, `fs`, formatting),
`lacon run`, `lacon build`, `lacon test`.

**Done when** every benchmark task compiles natively and passes.

**Status.** Done: every benchmark task compiles natively and passes. The type
checker (`crates/check`, §14) runs before `run`, `test`, `check` and `build`.
The C backend (`crates/cgen`, §15) compiles the resolved IR to C against a
runtime that mirrors the interpreter, so native output matches the
interpreter's byte for byte on every golden program and every task input.
`lacon run` still interprets; `lacon build` makes an executable. Native code
was 2-15x faster than the interpreter but well short of Rust, because every
value was a tagged, reference-counted cell; Phase 2 starts there (§16).

### Phase 2 — Memory model and agent tooling

Mode checking, Perceus reference counting with reuse, `sig`, `fix`, `put`,
`q`, the diagnostics format, and primer v1.

**Done when** runtime is within 1.5× of Rust on the benchmarks and an agent
finishes tasks using only Lacon's tools.

**Status.** Started with representations. In native code, the ints, bools
and floats the checker proves are C scalars, and functions get typed entries
(§16); on the seven programs in `bench/perf/`, native code took 1.3-7.6x
Rust's time (geometric mean 3.0x, from 9.7x). Lists of ints, floats and bools
are now packed (§17): sieve went from 4.5x Rust to 1.8x and knapsack from
3.9x to 2.5x, a geometric mean of 2.1x from 2.5x on the machine §17 used.
wordfreq is within the 1.5x bar.
Strings, lists, structs and the rest now come from a small-object
allocator (§18): records went from 2.5x Rust to 1.7x and wordfreq from
1.09x to 1.03x. The benchmarks now run long enough for their ratios to
hold between runs, and a lambda given to `map`, `filter` or `sort_by` runs
inlined over a list, with int keys sorted by radix (§19): records went
from 1.8x Rust to 1.0x, and the geometric mean is 1.9x. records, wordfreq
and sieve are within the 1.5x bar. Programs now compile at `-O2`, a direct
call pushes its own call record, and a loop reads a list once when it only
assigns its elements (§20): fib went from 5.6x Rust to 2.6x, knapsack from
3.2x to 2.3x, mandel from 2.1x to 1.05x, and the geometric mean is 1.4x,
within the bar. mandel joins the four within it; fib, knapsack and collatz
(1.6x) are left, mostly paying for overflow checks and negative indexes.
An index in range now costs one compare, as in Rust, where it cost a sign
test and a compare (§21): knapsack went from 2.3x Rust to 1.85x, and the
geometric mean is 1.40x. Range facts reach little of what the three have
left (§21), so Perceus reuse is next for speed.
`fix`, `put` and `q` are built (§22), but no harness run has used them
yet. Mode checking isn't started.
Two benchmarks were added, nbody and trees (§23). nbody ran at 56x Rust,
which found a semantic bug: an int assigned to an `f64` field or typed
variable stayed an int, so `/` truncated. Stores now take the declared
type, and native code reads and updates struct fields in place: nbody
went to 4.5x, and the nine programs' geometric mean is 1.51x, at the
bar rather than under it.
An agent has now finished every task with no file tools, writing the
program only through `lacon put` (§24), which meets the tool criterion
in its narrow sense. On these small tasks the tools saved nothing over
writing the whole file with a plain script: none of 60 episodes used
`fix`, `q` or `sig`. Taking away Claude Code's file tools saved 35% of
Lacon's tokens, which says more about the harness than about Lacon.

### Phase 3 — Speed and scale

Cranelift backend, incremental builds, the concurrency runtime, a package
manager, C interop, the LSP and MCP server.

**Done when** the build-time targets in §6 are met.

### Phase 4 — Ecosystem

Translate Rosetta Code and Advent of Code solutions into a test-checked Lacon
corpus, usable as training data for models. Publish the benchmark results.

---

## 8. Not in v1

Macros, inheritance, exceptions, `async`/`await`, operator overloading beyond
the standard traits, a REPL, self-hosting, and a second way to do anything.

---

## 9. Risks

- **No training data.** Models will make more mistakes in Lacon than in Python
  at first, and the retries could cancel out the token savings. Phase 0 exists
  to find this out early and cheaply.
- **No libraries.** C interop and a large standard library fill the gap.
- **Scope.** Languages take years to build. Keep v1 small and let the
  benchmark decide what gets built.

---

## 10. Open questions

To settle with Phase 0 data rather than by taste. The interpreter needed an
answer for each, so each has a provisional one (in italics) that the benchmark
can overturn.

- **Indentation or braces.** Measure both on the task suite. *Indentation;
  braces get a hint.*
- **Parentheses on zero-argument methods.** The sample uses `p.len` and
  `.lines` but `it.trim()` and `.parse()`. That's inconsistent; pick one rule
  (likely: parentheses optional, the formatter removes them). *Optional.*
- **Positional struct literals.** They save tokens but allow swapped fields of
  the same type. Possibly allow them only when every field type is distinct.
  *Allowed; a bare name that matches a field is shorthand for `field: field`,
  so `User{city, name, age}` can't swap.*
- **Missing map keys.** `counts[k] += 1` relies on Go-style zero values. Decide
  whether a plain read of a missing key returns the zero value or `V?`. *A
  plain read is an error, `m.get(k)` is `V?`, and compound assignment or a
  mutating method (`m[k].push(x)`) creates the zero value first.*
- **`?` inside `it` lambdas.** Does it return from the lambda or from the
  enclosing function? *The enclosing function, so `lines.map(parse(it)?)`
  works.*
- **Integer overflow.** Trap always, or trap in debug and wrap in release like
  Rust. *Trap always.*
- **Effects.** Inferred only, or declarable per package as in §3.6. *Inferred
  only; `lacon sig` shows `io` or `pure`.*
- **Braces in strings.** With interpolation everywhere, `"{"` was an error, and
  bracket and JSON code hit it at once. *A `{` that can't start an
  interpolation is literal, and so is a string that is only `{}`.*
- **Blocks inside brackets.** Layout is off inside `( )`, so
  `push(match x ...)` with indented arms couldn't parse, and writing the tasks
  hit it at once (§13). *A `match` inside brackets finds its arms by column:
  each starts a line at the first arm's column and is one expression. Arms
  that need statements get a hint to bind the match first.*
- **Branches of different types.** `print(eval(line) ?? "error")` (`calc`,
  `rpn`) and `[region] + counts` (`pivot`) join an int with a str. *Allowed:
  the value can be printed or interpolated, and anything else is E0303.*
- **Unhandled optionals and results.** Using a `T?` or `T!` as a `T` was a
  runtime error. *A compile error with a fix (`?? 0` or `?`); after
  `if x != none:` (or a `none:` arm), `x` is a `T`.*
- **`?` on an error in a `T?` function.** *Returns `none`, which is what
  `ledger`'s `cents(s) int?` assumed of `int(s)? * 100`.*
- **Ints that meet floats.** `var total = 0` then `total += x` with a float
  `x`. *The variable is a float.* A declared `int` stays an error.

---

## 11. Repository layout

Now:

```
PLAN.md          this document
docs/            the language primer an agent gets (≤ 3,000 tokens), and a
                 shorter one under test (§13)
crates/syntax    lexer, parser, AST, diagnostics
crates/interp    Phase 0 resolver and tree-walking interpreter
crates/check     type checker over the resolved IR
crates/cgen      C backend: IR to C, and the C runtime in runtime/
crates/cli       the `lacon` binary
tests/           golden tests: run/ (stdout), check/ (diagnostics), unit/ (`lacon test`),
                 fix/, put/ and q/ (the editing and query commands)
bench/tokens/    the same program in Rust, Go, Python and Lacon, plus a token counter
bench/tasks/     Phase 0 tasks (prompt, hidden tests, reference) and a checker
bench/perf/      native speed: the same programs in Lacon and Rust, and a timer
bench/harness/   has Claude solve the tasks in each language and reports tokens-to-green
bench/results/   harness runs (created by the harness)
```

Planned: an `ir` crate, once the resolver and IR move out of `crates/interp`.

---

## 12. Phase 0 semantics

Decisions the interpreter made beyond §3, for the benchmark to confirm or
overturn.

- **Methods are functions.** No `impl` blocks or `self`: `fn area(s Shape)` is
  called `s.area()` or `area(s)`, overloaded on the first parameter's type.
  Builtin methods work as functions too (`len(xs)`). Traits aren't
  implemented: the checker instantiates generics at each call but checks a
  generic function's own body loosely, since nothing says what a `T` can do.
- **Methods don't mutate**, except `push`, `pop`, `insert`, `remove`,
  `extend`, `clear`, `add`, `swap`, `truncate` and `retain`. `xs.sort()`
  returns the sorted list, and calling it as a statement is a static error
  with the fix `xs = xs.sort()`.
- **`mut` parameters** copy in and copy out, which matches in-place
  modification under value semantics. Arguments need no marker at the call
  site.
- **Accept foreign spellings with the same meaning; hint the rest.** The
  parser accepts `->`, `&&`, `||`, `!`, `else if`, `name: Type`, `f"..."`,
  `case` and `=>` in match arms, `loop:`, `User(a, b)`, `var`/`mut`/`pub`
  before a field, `p mut P` for `mut p P`, `f(mut x)` at a call site, `pass`,
  Rust's `Some(x)`, `Ok(x)`, `Err(e)` and `None` in patterns (and `some(x)`),
  `x ?? fail "msg"`, `?? return` and `?? continue`, an `elif` or `else`
  indented under a one-line `if`, a literal given to `map` as every
  element's value (`(0..n).map([])`), and Rust no-ops like `.iter()`,
  `.collect()` and `.clone()`. It rejects
  `let`, `//`, `del`, braces, `::`, `:=`, `++`, `if let`, comprehensions and
  macros, each with a one-line hint. This breaks §8's "no second way to do anything" on purpose: accepting
  costs no retry, and a formatter can rewrite aliases to the canonical form.
  Whether the aliases cost accuracy later is a Phase 0 question.
- **No truthiness.** Conditions must be `bool`.
- **Numbers.** Ints and floats mix and give `f64`; int `/` and `%` truncate
  like Rust; integers are 64-bit at run time, with declared sized types
  (`u8`, `u32`) range-checked at function, struct and return boundaries.
  A value stored where a type is declared takes it: a parameter, a
  result, a struct field, a typed binding (`x f64 = 7` is `7.0`) and an
  assignment to any of these (§23). List and map elements don't convert.
- **Determinism.** Maps and sets keep insertion order, so output never
  depends on hashing.
- **Strings** index and slice by character. One-character strings stand in
  for `char`.
- **Iteration.** `for k, v in m` gives pairs and `for k in m` gives keys.
  Other methods on a map see `(k, v)` tuples and return lists.
- **Heaps.** `heap()` / `heap(xs)` is a min-heap ordered by `<`, with type
  `heap[T]`: `push`, `pop` (the smallest, `T?`), `first`, `len`, `extend`, and
  iteration in ascending order. A max-heap pushes `(-priority, x)`, as in
  Python. Python, Rust and Go all ship one, and shortest paths, scheduling and
  top-k need one; without it the Dijkstra task took 30 more lines.
- **Format specs** take computed widths and precisions, as in Python:
  `{name:<{w}}`, `{x:.{digits}}`.
- **`map(f, xs)`** and `filter(f, xs)` mean `xs.map(f)` and `xs.filter(f)` when
  `f` is clearly a function (a lambda, or the name of a function, conversion
  or variant). Python habit; same meaning, so accepted.
- **Declared types on bindings.** `n int = v` (or `n: int = v`) binds an
  immutable `n` of a declared type, as `var n int = v` does a mutable one.
  It is how Python and Rust programmers tell `parse` what to read.
- **`parse()` reads the declared type.** Where a declared type is expected
  (a binding, return, parameter or field type, through one `?` or `??`),
  `s.parse()` reads that type or fails, like Rust's `s.parse::<T>()`:
  `n int = "1.5".parse()?` is an error, and so is `"300"` for a `u8`.
  Elsewhere it reads whichever number the text is, so `t += s.parse()?`
  still sums floats. Types inferred from use don't count: `s.parse() ?? 0`
  would otherwise turn `"2.5"` into `0` without a word.
- **String methods win in method syntax.** A program's own
  `fn parse(line str) Rec!` is called `parse(line)`; `s.parse()` stays the
  builtin, and so does every other string method name. Other functions are
  methods of their first parameter's type as before.

---

## 13. Findings from writing the tasks

Writing Lacon solutions for the 24 new tasks found these. (The solutions were
written by the interpreter's author, so they are a lower bound on what a model
working from the primer alone will hit; the harness measures that.) Each is
fixed and has a golden test.

- **A `match` inside a call** (`print(match op ...)`) was written by reflex
  even with the primer rule in mind. Now parsed (§10).
- **No priority queue** cost 30 lines in Dijkstra. Added `heap` (§12).
- **Computed widths** (`{n:>{w}}`) are the natural way to align columns and
  failed with an unrelated parse error. Now supported (§12).
- **Misleading or missing hints:** `def f():` suggested `fn f():` instead of
  `fn f() =`; `print(x, end="")`, `sort(reverse=true)` and `key=` got a generic
  "no named arguments" (now they name `io.write`, `sort_by(-it)` and
  `sort_by(key)`); a nested `fn` and `|x| total += x` got lambda-syntax hints
  that didn't apply; `console.log(x)` reported an unused result instead of an
  unknown name; `"%d" % x`, `ljust`, `rjust`, `zfill` and `heapq` had no hint.
- **`"{}"`** for an empty JSON object is an error (an empty interpolation). It
  stays an error, because silently printing `{}` for a Rust-style
  `print("{} items", n)` costs more than one retry; the hint names `{{}}`.
  (Later narrowed: a string that is only `{}` is literal; see "Closing the
  gaps" below.)

### First harness run

Three tasks (`calc`, `dijkstra`, `ledger`) in Lacon and Python through
Claude Code, `claude-opus-5`, one trial each. All six passed. Lacon took 1.76x
Python's tokens (geometric mean), and two of three first attempts built.

- **The primer is about 3,400 tokens in Claude's tokenizer**, not the 2,411
  `o200k_base` counts, so it is over the 3,000-token budget. Every API call
  re-reads it: on `dijkstra` both languages took four calls, and the primer
  accounts for about 13.7k of Lacon's 15.7k extra tokens. Shrinking it is the
  biggest lever this run found.
- **The failed first build** in `calc` came from three guesses with obvious
  meanings: `var` on a struct field, `p mut P`, and `f(mut p)`. All three are
  now accepted, and the model's first program runs unchanged. Its three
  mistakes produced eight errors, because the failed `type` made every use of
  it an error too; errors that follow from a failed declaration are now
  suppressed.
- **The harness itself** caused denied commands in four of six episodes (`ls`,
  `sed`, shell redirection), in both languages. `ls` is now allowed and the
  system prompt says what Bash can run.
- **The run used the wrong model.** The harness defaulted to `claude-opus-5`.
  It now defaults to `claude-opus-5-5`, records the model that answered each
  episode (`served_by`), and flags a mismatch as it runs.

### Cutting the primer

The primer went from 3,412 to 2,988 tokens in Claude's tokenizer, under the
3,000 budget. Rather than guess what was safe to cut, the cuts were checked
against probes: short programs written the way a model would guess without the
primer (Python and Rust habits), run through the interpreter. A line stayed in
the primer only if its guess fails.

- **Cut:** things the guess gets right: Python slicing, `range(n)`,
  `min(a, b)`, `", ".join(xs)`, `x == none`, `m.contains_key(k)`, `.iter()`
  chains, `s.insert(x)`, rarely used `fs`/`os` functions, and the operator
  list (now "Python's, except `//` and `~`, plus `??`").
- **Kept:** what fails or means something different: `var`, `=` bodies, `it`,
  `T!`/`?`/`fail`, methods returning new values, missing map keys, no
  truthiness, and the list and string method names.
- **Format specs** now take all of Python's mini-language (`,` and `_`
  grouping, `%`, `g`, `#`, `=`, a space sign, `e` with a signed two-digit
  exponent), so the primer can say "Python's". The one difference is
  deliberate: `{x:.2}` is two decimals, as in Rust, not two significant
  digits.
- **Fixes the probes found:** `chain` got the machine-applicable fix `chars`
  and `scan` got `sign`, because names five letters or shorter allowed two
  edits; they now get one, and a swapped pair of letters counts as one edit
  (`fitler` suggested `iter`, now `filter`). `sys.stdin.read()` had no hint;
  `del m[k]` was a confusing parse error and now gets `E0148` with the fix
  `m.remove(k)`; Python's `pass` is accepted. `chain`, `filter_map`,
  `sort_unstable`, `char_indices` and `scan` got hints.

Not measured yet: whether the cut primer costs first-build accuracy. The next
run answers that.

The tasks themselves split into parsing (`calc`, `json-format`, `ini-query`,
`csv-column`, `roman`, `log-summary`, `rpn`, `brackets`), data processing
(`group-stats`, `grade-report`, `ledger`, `anagrams`, `meeting-rooms`, `pivot`,
`adults-per-city`, `word-freq`), CLI tools (`wc`, `uniq-count`,
`column-align`, `line-diff`, `kv-store`, `path-normalize`) and algorithms
(`dijkstra`, `topo-order`, `edit-distance`, `knapsack`, `life`, `big-arith`,
`grid-path`, `merge-intervals`). Lacon solutions average 26 lines.

### Pilot run

All 30 tasks in Lacon and Python through Claude Code, `claude-opus-5-5`, one
trial each, with the cut primer and the type checker
(`bench/results/20261006-234906-claude-opus-5-5`, not committed).

| | Lacon | Python |
|---|---|---|
| Passed | 30/30 | 30/30 |
| First attempt builds | 67% | 100% |
| Median tokens-to-green | 31,692 | 9,597 |
| Mean API calls | 4.1 | 2.3 |
| Mean output tokens | 1,382 | 1,087 |
| Mean cost | $0.055 | $0.040 |

Lacon took 2.92x Python's tokens (geometric mean of the per-task ratios).
Most of the gap is not failed builds: the 20 Lacon episodes whose first
attempt built still had a median of 29.4k.

- **The primer is paid on every call.** Each Lacon call reads about 3.5k more
  cached tokens than a Python call, which is the primer. A Python episode
  writes about 1.1k output tokens in all, so on tasks this small no syntax
  can win back the primer: Lacon at zero output tokens would still be behind.
  §6's target of tokens-to-green at or below Python's needs larger tasks, or
  a model that knows Lacon (Phase 4).
- **Lacon takes more calls when nothing goes wrong.** In Python the model
  runs and submits in one command (`./run && ./submit`); in Lacon it runs,
  reads the output, then submits. Caution in an unknown language costs a
  call per episode.
- **The programs aren't shorter.** Lacon solutions averaged 39 lines against
  Python's 37, and output tokens were 27% higher.

The ten first attempts that didn't build:

- **A declared type on a binding**, so that `parse` knows what to read, in six:
  `n int = s.parse()?`, `v: int = ...`, `v f64 = ...`. The parser said
  "expected end of line, found `int`". Now accepted (§12).
- **`parse` ignored the declared type.** `fn num(t str) int! = t.parse()`
  returned a float for `"1.5"` and failed the return check instead of
  returning an error; `rpn` wrote its own digit parser around it. `parse`
  now reads the type expected of it (§12).
- **A program's `fn parse(line str) Rec!` took over `s.parse()`**, even inside
  its own body, and the error said only "mixes Rec and int". `log-summary`
  spent seven calls and 71k tokens on it, the worst episode of the run.
  String methods now win in method syntax (§12).
- **`[0].repeat(n)`** in two: the primer lists `repeat(n)` among the string
  methods, and it is valid Rust. Lists have `repeat` now.
- **`trim_end("\r")`** in one. `trim`, `trim_start` and `trim_end` now take the
  characters to remove, as Python's `strip` does.
- **`"{}"`** for an empty JSON object in one, which stays an error (above); the
  hint fixed it in one step.

With these changes 29 of the 30 first attempts build and match every hidden
test unchanged, `rpn` and `log-summary` included; `json-format` is the
exception. That would have taken first-build success from 67% to 97%, but it
removes the smaller part of the gap: the primer and the extra calls remain.
Next: Rust and Go, which is the comparison Phase 0's done-when names.

### Smoke run against Rust and Go

`calc` and `dijkstra` in Lacon, Rust and Go, one trial each
(`bench/results/20261007-002953-claude-opus-5-5`). All six passed and every
first attempt built. Lacon took 2.53x Rust's tokens and 2.90x Go's, but it
was the cheapest of the three in dollars ($0.046 an episode against $0.051
and $0.059): almost all of its extra tokens are cached reads of the primer,
which cost a tenth of input. Its output was the smallest or close to it.

The gap is the pilot's: Go and Rust wrote the file and ran
`./run && ./submit` in one call, while Lacon wrote, ran, submitted and
summarised in four, each reading about 7k tokens of context against 3-4k.
Whether tokens-to-green counts cached reads at full weight decides whether
Lacon passes Phase 0 at this task size.

### Primer experiment

Two ways to cut those tokens, each tried on all 30 tasks in Lacon, one trial
(`bench/results/20261007-005436-primer-*`):

- **A short primer** (`docs/primer-short.md`, about 1,290 tokens against
  2,988). It keeps the traps that are common or silent (`var`, `T!` and `?`,
  methods returning new values, missing map keys, int `/` truncating,
  assignment copying, the string method names) and drops what the guess gets
  right or a compiler hint now fixes. Probes of about 120 Python and Rust
  guesses chose the cuts.
- **A chaining hint** (`--chain-hint`), given to every language: a failed
  submission has no penalty and `./submit` reports build errors too.

| | Primer | Short primer | Primer + chaining hint |
|---|---|---|---|
| Passed | 30/30 | 30/30 | 30/30 |
| First attempt builds | 90% | 70% | 97% |
| Median tokens-to-green | 24,340 | 22,032 | 22,396 |
| Tokens against the primer (geometric mean) | 1.00 | 0.85 | 0.80 |
| Mean API calls | 3.5 | 3.8 | 2.8 |
| Mean cost | $0.047 | $0.051 | $0.046 |

- **The short primer saves 1.7k tokens a call** but fails more first builds.
  Episodes whose first build worked had a median of 17.7k against 24.2k;
  each failure cost about 15k, mostly in output and cache writes, so it
  saved tokens but not dollars. The misses were specific:
  `trim_end_matches("\r")`, `to_f64`, `to_str`, `remove_key` on a map,
  comparator lambdas in `sort_by`, and `split_once` used unhandled because
  the short primer dropped its `?`.
- **The chaining hint saves 0.7 calls an episode** and was cheapest in
  tokens and dollars. Rust and Go already chain, so it should help Lacon
  most; that isn't measured yet.
- **The pilot's fixes held.** With the same primer, the median fell from
  31.7k to 24.3k and first builds rose from 67% to 90%.
- **`json-format` failed its first build in every run**, the pilot's
  included: `"{}"` (by design, §13) and `"{\n"`, whose `{` is read as an
  interpolation.

The probes also found guesses with no hint or a misleading one, now fixed:
`HashMap::new()`, `Vec::new()`, `HashSet::new()` and `String::new()` name
their literal; `Counter`, `defaultdict`, `deque`, `float`, `divmod`,
`.copy()`, `.cmp()` and the `trim_*_matches` methods get hints; `not xs`
says there is no truthiness; and `any(x for x in xs)` gets the comprehension
hint instead of a parse error.

Next: add the short primer's misses back (about 60 tokens), read `"{\n"`
as a literal brace, and rerun the short primer with the chaining hint.

### Closing the gaps

- **`json-format`'s first builds.** Each run's first attempt failed on
  `"{}"` first, and two had a second error. `"{\n" + body + "\n}"` read the
  `{` as an interpolation that ran past the closing quote to the `}` of a
  later string; a `{` before a `\` is now literal, since no expression holds
  a `\`. `(if c: "0" else: num, i)` took `num, i` as the else branch, a
  tuple, where Python reads a pair; inside brackets a one-line branch now
  ends at a comma.
- **`"{}"` is literal when it is the whole string.** It had failed
  `json-format`'s first build in all four runs, and the Rust-style mistake
  it guards against (§13) hadn't appeared once. `"{} items"` stays an error,
  and so does `print("{}", x)`, with the fix `print("{x}")`. With these
  three changes two of the three first attempts from the primer experiment
  build and pass every hidden test unchanged; the third has a bug of its own.
- **The short primer's misses** are back: `int(x)`, `f64(x)`, `str(x)`;
  `sort_by` takes a key, not a comparator; `m.remove(k)`; `trim_end("\r")`;
  `split_once` returns `(str, str)?`. It is 1,356 tokens, up from 1,286.
  `remove_key` and a two-parameter lambda in `sort_by`, `min_by` or `max_by`
  now get hints too; the lambda had been told it "takes 2 arguments, but gets
  one int".

### Against Python, Rust and Go

With those fixes, three runs, all with the chaining hint
(`bench/results/20261007-113217-*`, not committed): Lacon with each primer,
three trials of all 30 tasks; then Python, Rust and Go, one trial of all 30
plus a second of 11, where the run was stopped to save credits.

**The short primer wins.** Both primers reached 97% first builds; the short
one took 0.76x the full one's tokens (median 17.5k against 22.3k) and 0.97x
its cost. It is the one to use from here.

| | Lacon | Python | Rust | Go |
|---|---|---|---|---|
| Passed | 90/90 | 34/34 | 34/34 | 33/33 |
| First attempt builds | 97% | 100% | 97% | 97% |
| Median tokens-to-green | 17,470 | 9,330 | 10,154 | 10,290 |
| Mean output tokens | 1,102 | 1,107 | 1,380 | 1,430 |
| Mean API calls | 2.6 | 2.1 | 2.1 | 2.2 |
| Mean cost | $0.038 | $0.040 | $0.048 | $0.049 |

| Lacon against | Tokens | Cost |
|---|---|---|
| Python | 1.49x | 0.93x |
| Rust | 1.40x | 0.78x |
| Go | 1.37x | 0.76x |

(Geometric means of the per-task ratios. Lacon's first trial alone gives
1.33-1.41x the tokens.)

- **Phase 0's first-build bar (90%) is met.** Its token bar, beating Rust and
  Go, is met in cost but not in raw tokens. The smoke run's gap was
  2.5-2.9x.
- **Lacon writes the least.** Its output is 20-23% below Rust's and Go's and
  level with Python's.
- **The gap is cached reads:** 13.5k an episode against 7.6-8.3k. Each Lacon
  call reads 5.2k against about 3.5k, which is the primer, and Lacon takes
  more calls. 29 of the 90 Lacon episodes built first time, ran once and
  submitted once, but did the run and the submission in separate calls
  despite the chaining hint; Python, Rust and Go did that in a handful.
- **Cached reads cost a tenth of input**, so they are most of Lacon's tokens
  and little of its cost. Whether tokens-to-green weighs them at full price
  decides whether Lacon passes Phase 0 at this task size. `report.py` now
  prints both ratios.

The six first builds that failed, three in each Lacon run, were natural
guesses that nothing catches yet:

- `st.pop() ?? fail "underflow"`, Kotlin's `?: throw`, in three: `fail`
  isn't an expression.
- `some((a, b)):` as an arm of a match on an optional, in one.
- `(0..n).map([])` and `.map(-1)` for a list of copies, in one. The error
  reads "the function given to `map` must return fn(int) _".
- An `elif` on an indented line continuing a one-line `if` expression, in
  one.
- `int(s)`, an `int!`, inside a returned tuple, in one. The error names the
  whole tuple's type rather than the element, and has no fix.

All five are fixed, each with a golden test. `fail`, `return`, `break` and
`continue` are expressions that never yield a value, so `??` takes them.
`some(p)` matches anything but `none`, and `Some`, `Ok`, `Err` and `None`
resolve to Lacon's patterns unless the program has a variant of that name. A
literal given to `map` becomes a lambda, but only for `map`, since
`s.find("?")` and `s.count("a")` share their names with list methods that
take a function. An `elif` or `else` line indented past its block
continues the line above. A tuple that fails a declared type only because
an element is unhandled is reported on that element, with its fix. Five of
the six first attempts now build and pass every hidden test unchanged; the
sixth has the real bug the clearer error names, and its fix passes.

Next: the separate run and submit calls, which cost about a call in a third
of Lacon's episodes.

---

## 14. Phase 1: the type checker

`crates/check` checks the resolver's IR, so names are already slots, `it`
arguments are lambdas, and assignment targets are places; the IR now keeps
full declared types and a span on every expression. Signatures are declared
and bodies inferred:

- **Bidirectional where it helps, unification for the rest.** An expected type
  flows into lambdas (`xs.map(it.trim())` knows `it` is a `str`), empty
  literals and `none`. Everything else is unified.
- **Deferred operations.** `var stack = []` says nothing about the elements,
  and code often uses them before saying: `kv-store` calls
  `stack[-1].push(...)` before `stack.push([])`, and `merge-intervals` reads
  `merged[-1].1` before its first push. A method, field, index, loop or
  operator on a value of unknown type waits until the type is known. At the
  end of a function, the method names the type (`m[k].push(x)` makes the
  values lists) and anything still unknown is left unchecked rather than
  reported.
- **Coercions** mirror the interpreter's: an int where a float is wanted, a
  value where an optional or result is wanted, a range where a list is.
- **Narrowing.** `x == none` and `x != none` in `if`, `while`, `and` and `or`,
  `none:` and `err(_):` arms, and assignments of a plain value narrow an
  optional variable. Loops forget what they assign, since the body runs again.
- **Leniency.** A wrong type error costs a retry, so the checker reports only
  what it is sure of: unknown types are never errors, generic bodies are
  checked loosely, and a no-op `?` on a plain value passes.

Writing it against the existing programs found:

- **Three solutions mix types in a printed value**: `calc` and `rpn` print
  `eval(line) ?? "error"`, an int or a str, and `pivot` prints
  `([region] + counts).join(",")`, a list of strs and ints. Both are natural,
  so a join of unrelated types is allowed as a value that can only be
  displayed (§10).
- **`ledger` relied on `?` in a `T?` function**, with an error the interpreter
  would have reported only at run time; the checker made the rule explicit
  (§10).
- **Two golden tests were wrong under static types**: `sum(map(int, strs))`
  sums `int!` values, and a `{str: int}` map got a list pushed into one key.
  Both now fail with E0406 and E0203; the tests changed.
- **`tests/run/basics.lc` never ran.** Its expected output was a parse error
  since it was written: `"[{"ab":<4}]"` reads `{"` as a literal brace. The test
  now quotes with `'`, and that parse error gets a hint saying so.
- **`fn f() {str: int} =` didn't parse**; the map type was taken for a braced
  body.

The new codes: E0302 (`?` or `fail` in a function that can't fail), E0303 (a
mixed value used other than for display), E0304 (a path ends without a value),
E0305 (a function without a return type used for its value). Type mismatches
are E0301, and optionals and results used unhandled reuse the runtime codes
E0407 and E0406, so `lacon explain` gives the same advice either way.

The primer now says that an unhandled `T?` or `T!` is a compile error, that
`?` in a `T?` function also turns errors into `none`, that `fs.write` returns
`()!`, and to quote with `'` inside `{...}`. Two lines whose guesses work
(`a if c else b`, the list of number methods) were cut to pay for it, so it
is 35 bytes shorter than the measured 2,988-token version. It hasn't been
re-measured in Claude's tokenizer.

Not checked yet: trait bounds (nothing says what a generic `T` supports),
`mut`/`own` modes beyond what the resolver checks, exhaustiveness for matches
that aren't over an enum, and error types (`T!E`). Whether the checker costs
or saves tokens is a harness question: it moves errors from `submit` to `run`
and catches them on paths the example input doesn't reach.

---

## 15. Phase 1: the C backend

`lacon build` compiles a program to C, runs `cc` and links against a runtime
(`crates/cgen/runtime/`) that is compiled once and cached. The runtime is a C
port of the interpreter's values and builtins, so the two agree by
construction and the golden outputs check both:

- **Values** are a tagged 16-byte `lc_v`. Strings, lists, maps, sets, heaps,
  structs and variants are reference counted and copied on write, the
  interpreter's `Rc::make_mut`; a unique string or list is appended to in
  place, so `s += x` in a loop stays linear.
- **Expressions** become GCC statement expressions that yield owned values;
  runtime functions borrow their arguments. A function's slots are a C array,
  or a heap frame when the function contains a lambda: closures share that
  frame, as the interpreter's do, so a lambda sees later assignments and can
  push to a captured list (`xs.each(acc.push(it))`).
- **`?` and `return` inside a lambda** leave the enclosing named function. The
  lambda sets a flag and returns, every runtime function that calls functions
  stops at once, and the first named function on the way up takes the value.
- **Errors** carry the same codes, messages, spans and call traces, down to
  which of two incomparable keys a sort names first.

Verified by running every golden program and every task input both ways and
comparing stdout, stderr and the exit code byte for byte (118 runs), also
under AddressSanitizer and UndefinedBehaviorSanitizer; `cargo test` builds the
golden programs natively, and `bench/tasks/check.py --native` checks the
tasks compiled.

Where it stands against §6:

| | Interpreter | Native |
|---|---|---|
| `fib(27)` | 0.14 s | 0.0095 s |
| sieve of 2,000,000 | 1.17 s | 0.18 s |
| 300,000 map updates | 0.10 s | 0.05 s |
| 200,000 structs filtered, mapped, sorted | 0.24 s | 0.14 s |

(Native with integer arithmetic, list indexing, calls and boundary checks on
values that already have the declared type done inline.)

That is a tree-walker's overhead removed, not Rust's speed: every value is
still a tagged cell, every operation a runtime call with a type check, and
every read of a variable a reference count. The next step is the one §3.3 and
Phase 2 describe: representations from the checker's types (unboxed ints and
floats, typed lists and structs), then Perceus-style RC insertion and reuse.
§16 takes the first of those steps.

Build time is the other gap: a 109-line program becomes 77 KB of C and takes
1.2 s (0.3 s for a small one), against §6's 1 s for 10,000 lines. Programs
compile at `-O1`, which runs as fast as `-O2` on this code in 60% of the time
(§20 moves them to `-O2`, which unboxed code needs); the runtime is compiled
once at `-O2` and cached. The generated C is verbose;
a less wordy generator or Cranelift (Phase 3) would cut the rest.

---

## 16. Phase 2: unboxed values

Phase 1's native code held every value as a tagged, reference-counted
`lc_v`. Now the ints, bools and floats the checker proves are C scalars.

**What the checker records.** The type of every expression and of every
slot of every function, lambda, constant and default, once inference is
done, and whether those types are *sound*: whether a value typed `int` is
always an int at run time. The checker is lenient (§14), so they aren't
always: an `any` value flowing into an `int` place, an operation that never
learned its receiver's type, and the guess that an unknown operand of `+`
has the other's type all let a wrong type through. Each now counts as a
leak, and a program with any leak outside its tests compiles as before,
fully boxed. 58 of the 60 golden, task and benchmark programs are sound;
the other two use `"12".parse() ?? 0` (below).

**Representations.** In a sound program:

- An int or bool expression is an `int64_t` or a C `bool`, and so is a local
  typed int or bool, unless a lambda captures it (it lives in the heap frame)
  or something reaches it by pointer: an assignment to a field or element
  through it, a mutating method, a `mut` argument.
- Arithmetic on unboxed ints checks overflow, division by zero and shift
  ranges with the runtime's messages. Comparisons, `and`, `or`, `not`,
  conditions, `if`, `match` and blocks stay unboxed, and a range written in a
  `for` loop is a C loop.
- A function without generics, `mut` parameters, defaults or lambdas gets a
  typed entry `FT{id}` that takes and returns unboxed values. Direct calls use
  it, checking arguments as the runtime does and in the same order, a sized
  int's range included. `F{id}` stays, as a wrapper, for calls through values,
  overloads and method syntax.
- A boxed value going into an unboxed place (a list element, a field, the
  result of a call through a value) is checked: a value of another kind is a
  checker bug, reported as E0000 rather than computed with.

**Floats** can't be unboxed by the checker's types, because an `f64` can hold
an int at run time: `if c: 1 else: 2.5`, `var t = 0` widened by `t += x`
(which prints `0` if the loop never runs), `z = n` into a float variable, an
int assigned to an `f64` field, and `x f64 = 1`, since a declared binding
doesn't convert. Each prints as an int. So a float is unboxed only where it
is one for certain: a float literal, `f64(n)`, `sqrt` and the other float
functions, a declared `f64` parameter or result (the runtime converts those),
and arithmetic between such a float and any number. An `f64` local is a
`double` when everything written to it is; starting from all of them, those
with a write that may be an int are dropped until none is left. Ordering NaN
is still the runtime's error, and programs compile with `-ffp-contract=off`
so no multiply-add is fused where the interpreter rounds twice.

Writing it found:

- **`"12".parse() ?? 0` is typed int by its use,** but without a declared type
  `parse` reads whichever number the text is (§12), so `"2.5".parse() ?? 0`
  is a float typed int. The unboxing check caught it in
  `declared_types.lc`. Such a `parse` now counts as a leak.
- **`n ** k` and `n.pow(k)` on ints** are typed int but give a float for a
  negative `k`; unless `k` is a literal, they count as a leak.
- **`n.round(d)` on an int** is typed `f64` but gives the int. Harmless here,
  since floats aren't taken from the checker's types.
- **A runtime bug:** a lambda call counted toward the depth limit without
  writing a frame, so an error trace could read an uninitialized or stale
  frame (an ASan crash in `lambda_error.lc`). Lambdas now write a frame
  without a site, which traces skip without breaking a run of identical
  frames, as the interpreter's do.

Also: a list read with an unboxed index borrows the variable's list instead
of taking a reference; `xs.len` on a list or a string skips the method call;
a known struct's field is read by position; an `if` used as a statement is C
control flow; ints format without `snprintf`; sorting by int keys compares
ints directly; string `+` allocates once; and a struct field whose value the
checker proves has its declared type isn't checked again.

Verified as before (§15): every golden program, task input and benchmark
program gives the same stdout, stderr and exit code from the interpreter and
from native code (137 runs), also under AddressSanitizer and
UndefinedBehaviorSanitizer.

**Results.** `bench/perf/` holds seven programs in Lacon and Rust, and
`run.py` times them (best of seven runs on a shared four-core VM; a ratio
moves by up to a fifth between runs):

| Program | Work | Before | After |
|---|---|---|---|
| collatz | int loops | 13.6x | 2.1x |
| fib | recursive calls | 5.9x | 3.7x |
| knapsack | DP over a list of ints | 17.0x | 4.3x |
| mandel | float loops | 32.9x | 1.7x |
| records | 600,000 structs filtered, sorted, summed | 4.6x | 3.9x |
| sieve | 5,000,000 bools in a list | 12.6x | 7.6x |
| wordfreq | strings counted in a map | 3.1x | 1.3x |
| geometric mean | | 9.7x | 3.0x |

(Native time as a multiple of Rust's, `rustc -O`. The interpreter takes
1.5-10 s on each, 2-95x native's time.)

What's left:

- **sieve** is memory-bound: a list of 5,000,000 bools takes 16 bytes an
  element against Rust's one. Typed lists, from the checker's element types
  (`[int]` as `int64_t`, `[bool]` as bytes), are next; §17 does this.
- **records** and **wordfreq** allocate: each string, struct and list is its
  own `malloc`. A small-object allocator and Perceus-style reuse (§3.3) would
  remove most of it.
- **fib** records each call's function and site for error traces, two stores
  and a bound check that Rust doesn't pay.

Typed C is also smaller and builds faster: `json-format`'s C went from 81.5
KB to 75.7 KB and its build from 1.5 s to 1.3 s.

---

## 17. Phase 2: packed lists

§16 left every list element a 16-byte `lc_v`. Now a list holds its elements
packed, as raw `int64_t`s, `double`s or `bool`s, while every element is one
of that kind.

**The kind is the list's, at run time.** Each list carries a kind: boxed,
int, float or bool. An empty list takes the kind of the first element pushed,
and a list that gets a value of another kind is boxed in place, once. The
checker's element types aren't trusted for this, because an `[f64]` can hold
ints (§16): `[0.5] * 3` is packed as floats, and `f[0] = 7` boxes it, since
the 7 must still print as `7`. Only a list nothing else holds is ever boxed in
place, so no reader sees its storage change. Tuples and heaps stay boxed.

**The runtime reads every kind.** Its 120-odd uses of a list's storage go
through `lc_vget`, which boxes a packed element on the way out; copies,
slices and `[x] * n` copy packed storage a block at a time; `insert`, `pop`,
`remove` and `swap` move raw elements. A method called on a list no longer
copies it first (`lc_items` did, for every `filter`, `map` and `sort_by`),
`sum` adds packed ints in a loop, and `sort` sorts packed ints directly.

**Generated code uses the checker's types to pick fast paths**, which check
the list's kind and fall back to the general case:

- `xs[i]` typed int or bool reads the packed element, and one typed `f64` is
  read as a `double` where a number is wanted.
- `xs[i] = v` and `xs[i] op= v` with an unboxed index and value write the
  packed element in place, if nothing else holds the list. An int added to a
  list of floats is done in place too, since the result is a float.
- `xs.push(v)` with an unboxed value appends in place when there's room.
- `for x in xs` with an unboxed int or bool `x` reads packed elements straight
  into it.

The rest of a place is walked as before, but the walk now stops at the last
index, which stores into the container (`lc_store_index`,
`lc_update_index`). `grid[r][c] = v` and `bag.xs[0] += 1` keep the inner list
packed, and a list a shared row starts from is copied on write
(`[[0] * 3] * 2`).

Verified as before: every golden program, task input and benchmark program
gives the same stdout, stderr and exit code from the interpreter and from
native code (139 runs), typed and with `LACON_BOXED=1`, each also under
AddressSanitizer and UndefinedBehaviorSanitizer. `packed_lists.lc` covers
each operation and change of kind, and `packed_update.lc` an overflow in an
element update, reported at the statement as the interpreter reports it.
Generated C is the same size as before, and builds as fast.

**Results**, on a different machine from §16's: an Intel Core Ultra 7 155H,
best of eleven runs, each pinned to one performance core with `taskset`.
Unpinned, a run moved by up to 40% as the scheduler picked the kind of core.
Before is §16's compiler on the same machine:

| Program | Before | After |
|---|---|---|
| collatz | 1.9x | 1.9x |
| fib | 3.9x | 3.9x |
| knapsack | 3.9x | 2.5x |
| mandel | 1.9x | 1.9x |
| records | 2.7x | 2.6x |
| sieve | 4.5x | 1.8x |
| wordfreq | 1.1x | 1.1x |
| geometric mean | 2.5x | 2.1x |

(Native time as a multiple of Rust's, `rustc -O`.) A program that builds,
sorts, sums, filters and maps 2,000,000 ints takes 0.18 s, from 0.29 s.

What's left:

- **sieve** and **knapsack** pay, on every element they touch, the checks
  that make the fast paths safe: the list's tag and kind, that nothing else
  holds it, the bounds, and overflow on the arithmetic around it. The C
  compiler can't move them out of the loop. Checking a list once before a
  loop that can't change its length would remove most of them.
- **records** and **wordfreq** allocate, and **fib** keeps call records, as
  §16 says. Allocation is next.

---

## 18. Phase 2: a small-object allocator

§17 left every string, list, map, struct, closure and heap frame its own
`malloc` and `free`. Programs run on a second thread, for its 1 GB stack, so
glibc serves them from a secondary arena; records took 19% less time with
glibc's trimming turned off, and 47% less with jemalloc preloaded.

**Free lists by size.** A block of up to 512 bytes is popped from the free
list of its size class, a multiple of 16 bytes, and pushed back when freed.
An empty list is refilled from a chunk, 64 KB at first and doubling to
4 MB, and what is left of the old chunk goes on the list of its size.
Blocks have no header: freeing takes the size, which every object knows (a
string from its capacity, a list from its kind and capacity, a struct from
its field count). Larger blocks, such as a long list's elements, are
malloc's, and a block that grows past 512 bytes moves there. Freed blocks
wait for the next block of their class and are never given back to malloc.
The lists are plain globals, since the runtime runs on one thread; threads
(§3.5) will need a heap each.

**Checking sizes.** A block freed with too large a size would later be
handed out over its neighbour, so under AddressSanitizer every block comes
from malloc instead, with its size in a header, and freeing it with any
other size aborts with both sizes. A size planted wrong in the struct case of
`lc_free` fails `tests/run/alloc.lc` at once. That test grows strings,
lists and maps past 512 bytes, empties packed lists and gives them an
element of another size, boxes packed lists in place, and frees many values
and makes others of different sizes.

**Not moved:** `lc_buf`, the buffer behind interpolation and formatting,
still uses malloc. Moving it saved nothing measurable, since glibc's
per-thread cache hands the same block back each time.

Verified as before: every golden program, task input and benchmark program
gives the same stdout, stderr and exit code from the interpreter and from
native code (140 runs), typed and with `LACON_BOXED=1`, each also under
AddressSanitizer and UndefinedBehaviorSanitizer.

**Results**, on §17's machine but on battery, where a run's time moved by
up to half with the CPU's clock. Old and new builds were run back to back
40 times, and these are the medians:

| Program | New time / old | Against Rust, before | After |
|---|---|---|---|
| records | 0.68 (quartiles 0.64-0.72) | 2.5x | 1.7x |
| wordfreq | 0.94 | 1.09x | 1.03x |

The other five allocate nothing in their loops and didn't change. records
also takes 38% fewer page faults (14,600 from 23,800), and its peak memory
is unchanged: 123 MB, against Rust's 88 MB.

What's left in records is mostly not allocation: `sort_by` takes about half
its time and `map(it.score).sum()` a quarter, each a closure call per
element.

---

## 19. Phase 2: longer benchmarks, inlined lambdas and a radix sort

**Longer benchmarks.** Rust ran fib in 4 ms, knapsack in 5 ms and mandel in
9 ms, short enough that a millisecond moved a ratio by a fifth: fib was
3.9x Rust in §17 and 5.1x on a later run. Each input is now large enough
that Rust takes 0.17-0.29 s: collatz to 2,000,000, fib(41), knapsack of
8,000 items into 50,000, mandel at 2,000 pixels a side, 1,500,000 records,
a sieve to 50,000,000 and 6,000,000 words. Two runs of the set agree to
0.1x. The longer runs moved two ratios on their own: sieve is 1.4x, not
1.9x, and knapsack 3.2x, not 2.8x. `run.py --interp` takes minutes on them.

**Where records' time went.** §18 put it on a closure call per element.
Timing records in stages said otherwise for `sort_by`: its 1,200,000 key
calls took 0.017 s, and the merge sort of the pairs 0.086 s. For `map` it
said yes, for a reason particular to the sorted list: `map(it.score)` reads
the structs in sorted order, so each read misses the cache, and a closure
call, which retains and releases its argument between misses, took 0.08 s
where the same loop written out in C took 0.023 s.

**A radix sort for int keys.** When every key is an int, `sort_by` and
`sort` of a boxed list now sort their pairs with a stable LSD radix sort,
11 bits a pass on each key's offset from the smallest, so keys within 2,048
of each other take one pass. It is used from 4,096 pairs, or from 256 when
the keys span less than 2^22; below that, merge sort was faster in a
microbenchmark at every range of keys. Values are also retained in list
order as the keys are made, rather than in sorted order as the result is
built, so the sort no longer writes to each struct at random.

**Inlined lambdas.** A lambda written as the argument of `map`, `filter` or
`sort_by` is also compiled as `I{n}(P1, x)`, a C function the compiler must
inline, and when the receiver is a list at run time, generated code loops
over it and calls that instead of handing a closure to the runtime. A
range, map, set, string or tuple goes through the closure as before, so
the lambda is compiled twice. The loop:

- borrows each element: a parameter the body only reads, which every
  lambda parameter is since they are immutable (`only_read` checks), takes
  the element as the list holds it, without a retain and a release;
- counts toward the depth limit as a lambda call does: one lambda frame for
  the whole loop, pushed only when the list isn't empty, so traces and the
  stack-overflow error are unchanged;
- passes on `?` and `return` as the closure would, releasing what it holds.

Lambdas of two parameters, which take tuples apart, and lambdas holding a
lambda aren't inlined; nor are the other methods that take one (`any`,
`count`, `min_by` and the rest).

Verified as before: every golden program, task input and benchmark program
gives the same stdout, stderr and exit code from the interpreter and from
native code (141 programs; the benchmarks at their old sizes, which the
interpreter can run), typed and with `LACON_BOXED=1`, each also under
AddressSanitizer and UndefinedBehaviorSanitizer. LeakSanitizer reports
leaks in 18 of the programs, on error paths; it reported 20 before this
change. `inline_lambdas.lc` covers each method on a list and on the other
receivers, captures through an enclosing lambda, `?` and `return`, keys of
other types, each of the four ways int keys are sorted with the order
checked for stability, and an error inside an inlined lambda.

Generated C for the 30 tasks is 8% larger (529 KB from 488 KB) and takes
8% longer to build (5.9 s from 5.5 s for all 30).

**Results**, on §17's machine, pinned, best of seven runs, both compilers
on the longer benchmarks:

| Program | Before | After |
|---|---|---|
| collatz | 2.0x | 2.0x |
| fib | 5.6x | 5.7x |
| knapsack | 3.2x | 3.2x |
| mandel | 2.1x | 2.0x |
| records | 1.8x | 1.0x |
| sieve | 1.4x | 1.4x |
| wordfreq | 1.0x | 1.0x |
| geometric mean | 2.1x | 1.9x |

Only records uses the changed paths; the others moved within the 0.1x two
runs differ by. records takes 0.23 s from 0.39 s: its filter and sort
0.064 s from 0.136 s, and its `map` and `sum` 0.027 s from 0.08-0.10 s.
What it has left is building its 1,500,000 structs and their strings,
0.086 s, and freeing them all as `main` returns, 0.056 s.

---

## 20. Phase 2: `-O2`, call records at the call site, and loop views

§19 left fib at 5.6x Rust and knapsack at 3.2x. Three changes, and a bug
found on the way.

**`-O2`.** §15 compiled programs at `-O1`, which ran boxed code as fast as
`-O2`. Unboxed code it doesn't: at `-O2`, mandel goes from 2.1x Rust to
1.05x and collatz from 2.0x to 1.6x, and the rest move by 0.05x or less.
The 30 tasks take 47% longer to build (8.6 s from 5.9 s for all 30, with
the runtime cached), all of it from `-O2`. `lacon run`, `lacon test` and
the harness interpret, so only `lacon build` pays; a debug build at `-O1`
can come back with Phase 3's build modes.

**Call records at the call site.** For traces, each call of a typed entry
(§16) stored the function and call site: the caller wrote the global
`lc_callsite`, and the callee pushed `{fn, lc_callsite}` onto a growable
array and popped it on return. fib(41) makes 330 million calls, and this
took two thirds of its time. Now:

- A direct call to a typed entry pushes the record itself, the function
  and site being constants there (`lc_push`), and sets the depth back when
  the call returns; the callee does nothing. `F{id}`, the boxed wrapper,
  pushes for calls through values, and functions without a typed entry
  still push on entry from `lc_callsite`.
- The records are two fixed arrays of 20,000 entries, the depth limit: one
  of functions and one of sites. A fixed array's address is a constant,
  where the growable one's pointer had to be loaded again after every
  store. One array of `{fn, site}` structs ran fib in 0.86 s, two arrays
  in 0.71 s.

Traces and the stack-overflow error are unchanged. No golden test reached
the depth limit; `stack_overflow.lc` reaches it through direct calls after
calls that returned, unwound with `?` or ran inside a lambda, so a record
left behind would change the count of repeated frames, and
`stack_overflow_value.lc` through a function value and a function with a
default. fib went from 5.5x Rust to 2.6x at `-O2`. Without records at all
it would be 1.9x, and the rest is the overflow check on `+`: fib without
any checks runs in 0.16 s, faster than Rust's 0.27 s.

**Loop views.** knapsack's inner loop reads `best[c - wi]` and `best[c]`
and assigns `best[c]`. For each, generated code checked the list's tag and
kind, and for the assignment that nothing else held it, and loaded its
length and elements. It loaded them again for every element, since the C
compiler had to assume that a store to an `int64_t` element might change
the length or the variable. Now:

- A loop that uses a list variable only to read elements, assign elements
  and read `len` (in its condition and body: it never binds or assigns the
  variable, passes it anywhere, calls another method on it or reaches into
  an element) reads it once before it, as an `lc_view`: its elements, a
  length to read them by and a length to write them by.
- The view is for the kind of the checker's element type. Both lengths
  are 0 if the list is packed as another kind, and the write length is 0
  if anything else holds it, so each access is one bounds check, and a
  miss takes the old path. A view's three values stay in registers.
- An assignment that misses may copy the list or change its kind, so it
  reads the view again. Nothing else in the loop can change the list but a
  lambda that captured the variable, through a call, so a variable a
  lambda uses takes no view. An inner loop uses its outer loop's view.

`loop_views.lc` covers ints, bools and floats, negative indexes, `len` in
a condition, a list shared with another variable (copied by the first
assignment), ints assigned into a list of floats (boxed, then read), an
empty list packed by its first push, nested loops, the loops that take no
view, and an assignment out of range. knapsack went from 3.2x Rust to 2.3x
and sieve from 1.33x to 1.12x. What knapsack has left on each element is
the overflow checks on `c - wi`, `+ vi` and `c -= 1` and the test for a
negative index on each access, none of which Rust makes. Some of them
can't fail: the loop's condition, `c >= wi`, means `c - wi` is never a
negative index. Range facts like that are the next step.

**A bug.** `sum` of a packed list of ints whose total overflows printed a
wrong number in native code instead of the overflow error: the fast path
§17 added kept the wrapped total when it stopped, and the general case
went on from there. It now stops before the add that overflows.
`packed_sum.lc` covers it.

Verified as before: every golden program, task input and benchmark program
gives the same stdout, stderr and exit code from the interpreter and from
native code (145 programs; the benchmarks at their old sizes), typed and
with `LACON_BOXED=1`, each also under AddressSanitizer and
UndefinedBehaviorSanitizer. LeakSanitizer reports leaks in the same 18
programs as before, and `bench/tasks/check.py --native` passes every task.

**Results**, on §17's machine, pinned, best of seven runs. Before is §19's
compiler, and `-O2` is §19's compiler at `-O2`:

| Program | Before | `-O2` | After |
|---|---|---|---|
| collatz | 2.0x | 1.6x | 1.6x |
| fib | 5.6x | 5.5x | 2.6x |
| knapsack | 3.2x | 3.2x | 2.3x |
| mandel | 2.1x | 1.05x | 1.05x |
| records | 1.04x | 1.02x | 1.03x |
| sieve | 1.38x | 1.33x | 1.12x |
| wordfreq | 1.03x | 1.00x | 1.00x |
| geometric mean | 1.95x | 1.69x | 1.42x |

(Native time as a multiple of Rust's, `rustc -O`.) Generated C for the 30
tasks is 1% larger (535 KB from 531 KB).

What's left: the geometric mean is within Phase 2's 1.5x bar, and four of
the seven are within it on their own. fib, knapsack and collatz are not.
They pay for checks that Lacon's semantics need and Rust's don't make:
overflow traps (§10) and negative indexes. collatz without the checks on
`3 * n + 1` runs at 1.18x, and fib without its check on `+` faster than
Rust. Removing checks where they can't fire needs range facts, which the C
compiler doesn't derive through the generated code.

---

## 21. Phase 2: one compare for an index

§20 named range facts as the next step for fib, knapsack and collatz.
Reading the three programs first showed how little of their cost range
facts reach:

- **knapsack:** `while c >= wi` proves `c - wi >= 0`, which drops the test
  for a negative index on `best[c - wi]`: one of six checks per element.
  The rest need bounds on `wi`, `vi` or the list's elements, and no
  condition gives them.
- **fib:** `n < 2` proves `n - 1` and `n - 2` safe in the else branch, but
  most of what's left is the check on the sum of the two calls (§20).
- **collatz:** nothing bounds `3 * n + 1` or `k += 1`.

The overflow checks are already `__builtin_*_overflow`, one instruction
and a branch each. The test for a negative index could be cheaper without
any analysis, so this section does that instead.

**One compare for an index in range.** Each inline access computed
`k = i < 0 ? len + i : i`, then tested `0 <= k < len`. gcc compiled the
choice as a sign test and a branch before the bounds compare, so an
access in range took two branches where Rust's takes one. `lc_pos` now
gives `i` when `(uint64_t)i < len`, else `i + len` in unsigned arithmetic,
and callers test the result `< len` unsigned. That is the compare that
chose `i`, so gcc folds the two: an index in range costs one branch, a
negative index goes the other way, and any other index lands at or past
`len`. Every inline access uses it: views, `lc_get_int` and the rest,
`lc_int_at` and the rest, and the boxed `lc_index_int`, `lc_index_fast`
and `lc_place_index_fast`.

**A store through a view tests its bounds itself.** `lc_view_int_at` gave
a pointer to the element or NULL, and the store tested the pointer. gcc
couldn't prove `data + k` non-null, so each store took a second test.
Generated code now tests `k < wlen` and stores to `data + k`.

`index_edges.lc` covers 0, the last, -1 and -len through each path for
ints, bools, floats and strs, in loops and out, and a store at -len - 1
through a view. `index_far.lc` reads through a view at the most negative
int, which `len + i` must not bring into range. No earlier test reached a
negative index before the start of a list.

Verified as before: every golden program and task input gives the same
stdout, stderr and exit code from the interpreter and from native code
(140 runs), typed and with `LACON_BOXED=1`, each also under
AddressSanitizer and UndefinedBehaviorSanitizer. LeakSanitizer reports
leaks in the same programs as before, and `bench/tasks/check.py --native`
passes every task. Generated C for the 30 tasks is the same size.

**Results**, on §17's machine, pinned, best of seven runs. Before is §20's
compiler:

| Program | Before | After |
|---|---|---|
| collatz | 1.6x | 1.6x |
| fib | 2.6x | 2.6x |
| knapsack | 2.3x | 1.85x |
| mandel | 1.05x | 1.05x |
| records | 1.06x | 1.07x |
| sieve | 1.14x | 1.14x |
| wordfreq | 1.02x | 1.02x |
| geometric mean | 1.44x | 1.40x |

(Native time as a multiple of Rust's, `rustc -O`.) sieve gains nothing:
gcc had already proved its index non-negative, and its loop waits on
memory. knapsack without its overflow checks on `+` and `-` would run at
1.4x, so they are about a fifth of its time. Some of the rest is loads
from the stack: gcc keeps the list's data pointer, `vi` and the write
length there, and reads the pointer three times an element.

---

## 22. Phase 2: `fix`, `put` and `q`

Phase 2 is done when an agent can finish tasks with Lacon's own tools.
This section builds three of them. The harness doesn't offer them yet.

**Fixes say what they replace.** A diagnostic's fix was a bare string.
Most replaced the flagged code; the parser's replaced the whole line; four
rewrote the function's signature (`fn half(s str) int! =`); and four were
shapes with `...` (`fn name[T](...)`, `Circle(...)`, `User{...}`) or a
declaration to add somewhere earlier (`var total = 0`). A fix now carries
the range it replaces: the flagged span, the line without its
indentation, or the signature from `fn` to `=`, which the parser now
records. The shapes have no range and print as `| e.g. ...`, so `| fix:`
always means text that replaces the code. One fix had the wrong range:
`return 5` in `main` replaced only the keyword and left
`os.exit(5) 5`. It now covers the value.

**`lacon fix f.lc`** applies the fixes `check` would print (one per line),
skipping any that overlap, then checks again, for up to five rounds.
Fixing a syntax error lets the type checker run and find more:
`let n = count(s)` becomes `n = count(s)`, and then `n * 2` becomes
`n? * 2`. A fix for the same error at the place an earlier fix wrote is
not applied again. It prints each fix as the line it left, then what
still fails as `check` prints it, or `ok`:

```
fixed E0409 left.lc:7:3 xs = xs.sort()
E0201 left.lc:5:3 `total` is not defined; declare it first | e.g. var total = 0
E0201 left.lc:8:9 `totl` is not defined
```

On copies of the check tests it makes all 28 distinct fixes they show, and
each is what its message proposed.

**A misspelled field.** `User{nme: "ann", age: 3}` reported only
`missing field(s) name`. Only one error shows per line, and that one came
first, so the unknown field `nme` and its fix `name` were hidden. A
literal with an unknown field no longer also reports missing ones.

**`lacon put f.lc`** reads top-level items from stdin (functions, types,
enums, tests and constants) and replaces each item of the same kind and
name. Among functions of one name, it replaces the one with as many
parameters. An item it doesn't find is added at the end. A replacement
that has a doc comment replaces the old doc comment; without one, the old
one stays. It prints `replaced fn parse` or `added fn shout` for each
item, so a misspelled name shows up as an addition, then checks the
file. Stdin is dedented first. Items that don't parse are put all the
same, since `lacon fix` or another put can mend them in place; stdin that
doesn't start with an item is refused. The first version refused stdin
with any syntax error. Trying it on `rpn`, it refused a whole function
whose one error was a `let`, which `lacon fix` mends in one call.

`put` finds items from tokens rather than the parse tree, so it works on
a file that doesn't parse (`crates/syntax/src/items.rs`). An item starts
at an unindented token that begins a line, not one that continues the
line before it, so a `}` at column 0 that closes a type stays in the
type. It runs to its last line before the next item that isn't blank or
an unindented comment, and a section comment between items stays put.

**`lacon q`** answers in a few lines:

- `q def f.lc name...` prints items' source with their doc comments: the
  counterpart of `put`, so an agent reads one function, not the file.
- `q callers f.lc name` prints each call of a function and each use of it
  as a value, with the function it's in and the line.
- `q type f.lc:line:col`, in the format diagnostics use, prints the type
  of the innermost expression there, or of the variable a binding or
  `for` there names. `q type f.lc name` prints a function's signature, a
  type's declaration or a constant's type. It works on a file with type
  errors, which is when it's needed.

```
$ lacon q callers users.lc parse_line
users.lc:15:9 in adults: u = parse_line(line)?
users.lc:21:34 in main: names = ["ann,30", "bo,4"].map(parse_line)
$ lacon q type users.lc:21:3
`names` [User!]
```

There is no `q impls`, since Lacon has no traits. cgen's walk over the IR
moved next to the IR, in `lacon_interp::ir`, for `q` to use; the C it
generates is byte for byte the same.

`tests/fix/`, `tests/put/` and `tests/q/` are golden tests of each
command, and `crates/syntax/tests/items.rs` tests item ranges. No other
golden output changed.

**A checker gap.** Trying the tools on `rpn` turned up a program the
checker should reject. A stack `var st = []` filled by
`st.push(int(tok))`, with no `?`, is a `[int!]`, and `a + b` on two of its
elements passed, because the element type was still unknown at `a + b`,
which comes before the `push`. `q type` showed it: `st` is `[int!]` and
`b` is `int!`. §23 fixes it.

Not done: neither the harness nor either primer offers these commands,
so no run has measured them, and `rename`, `add-field` and mode checking
aren't built. Next: a harness mode in which Lacon episodes edit with
`lacon put` and `lacon fix` and read with `lacon q` and `lacon sig`, in
place of Write, Edit and Read, to see whether they cut tokens-to-green.
§24 runs it.

---

## 23. Phase 2: declared types on stores, and struct fields in native code

The seven benchmarks had no structs updated in place and no recursive
data, so two more cover them: `nbody` (float fields of five structs in a
list, updated in place, 6,000,000 steps) and `trees` (a recursive enum
built and walked, depth 16). trees ran at 1.02x Rust from the start;
nbody at 56x. Finding why turned up a semantic bug.

**An `f64` field could hold an int.** By design an `f64` can hold an int
at run time, so it prints as Python would (§16). Construction, arguments
and returns convert an int to a float, but three stores didn't: assigning
a field (`p.x = 7`), a typed binding (`x f64 = 7`) and assigning a typed
variable (`var g f64 = 1.5`, then `g = 7`). Int `/` truncates, so `p.x / 2`
gave 3 for a field declared `f64`, with no error. Now a value stored in a
struct field, or in a variable declared with a type (a parameter or
`var x T = ...`), takes that type: an int becomes a float, a list of ints
stored as an `[f64]` becomes floats, and a sized int is range-checked, so
`c.r += 10` on a `u8` field holding 250 is an error, where it used to store
260. List and map elements and a tuple's elements keep what they are
given, as before; `push` and the other methods would need the same rule.
Two golden outputs changed: `q.x = 3` into an `f64` field prints `3.0`,
and `var g [f64] = [1, 2]` holds floats, as a `[f64]` argument already did.

The resolver gives each place the declared type of its variable, and the
interpreter's walk of a place finds each field's declared type as it goes.
Native code does the same through `lc_store_field` and `lc_update_field`,
which look the field's type up in the struct table (now with each field's
type), and `lc_coerce_var`.

**Fields in native code.** Since every store converts, a field declared
`f64` of a struct the checker knows always holds a float, and one declared
an int type or `bool` holds one of those. Native code now:

- reads such a field as a C scalar in place, without taking a reference.
  Field reads had gone through a boxed value, and their arithmetic through
  the runtime's generic `lc_arith_own`: most of nbody's 56x.
- reads `xs[i].f` from the element of the borrowed list, without retaining
  the element.
- updates `p.f = v` and `p.f op= v` in place when nothing else holds the
  struct, for fields declared `f64` or `int`.
- takes a loop view (§20) of a list of structs when a loop only reads
  elements, reads their fields and assigns their fields. Each access then
  tests the bounds, the element's tag and the struct's type with the list
  in registers. An assignment that misses, because something else holds
  the list or the struct, takes the general path and reads the view again.
- writes the general path of a field read from a list (`xs[i].f`) as
  ending in an error: on a list the checker types as one of these structs
  it can only report an index out of range, or E0000 for a checker bug.
  Knowing nothing else happens there, gcc shares the tests between reads;
  that took nbody from 0.38 s to 0.23 s at 2,000,000 steps.

A hand-written C version with the same layout (a list of tagged pointers
to records of tagged fields) and no tests runs as fast as Rust: 0.077 s
against 0.065 s at 2,000,000 steps. What's left is the tests on each
access: the bounds, the element's tag, the struct's type and the field's
tag, and for a store that nothing else holds the struct. A store's
general path can't be written as ending in an error, since a struct two
elements share must be copied first; written so on bodies nothing shares,
the stores would save another 24%.

**A checker gap closed.** §22's stack of `int!` values: `a + b` on two
elements of a list whose element type was learned later went unchecked,
because both operands had the same unknown type. It is now checked once
the type is known, and reported as E0406 with the fix `a?`
(`tests/check/late_elements.lc`). It changes no generated C: every golden,
task and benchmark program compiles to the same C as before.

`declared_stores.lc` covers each kind of store: fields assigned, updated
and unpacked, nested, through a list and a map and a `mut` parameter;
typed bindings and variables; generic fields, tuples and list elements,
which don't convert; and loops through a view over structs two elements
share and a list another variable holds. `declared_range.lc`,
`declared_binding.lc` and `field_far.lc` cover a sized field out of range
in a loop, a sized binding out of range, and a field read through a view
one past the end.

Verified as before: every golden program and task input gives the same
stdout, stderr and exit code from the interpreter and from native code
(67 programs, 144 runs), typed and with `LACON_BOXED=1`, and typed under
AddressSanitizer and UndefinedBehaviorSanitizer; `bench/tasks/check.py
--native` passes every task. Generated C for the 30 tasks is 2% larger
(548 KB from 535 KB).

**Results**, on §17's machine, pinned, best of seven runs. Before is
§22's compiler:

| Program | Before | After |
|---|---|---|
| collatz | 1.61x | 1.60x |
| fib | 2.62x | 2.62x |
| knapsack | 1.83x | 1.82x |
| mandel | 1.06x | 1.04x |
| nbody | 56.2x | 4.52x |
| records | 1.04x | 1.02x |
| sieve | 1.13x | 1.12x |
| trees | 1.02x | 1.02x |
| wordfreq | 1.00x | 1.00x |
| geometric mean | 2.00x | 1.51x |

(Native time as a multiple of Rust's, `rustc -O`. The seven of §21 alone
have a geometric mean of 1.37x.) The nine are at Phase 2's 1.5x bar,
not under it: nbody is 4.5x, and fib, knapsack and collatz pay for
overflow checks (§20, §21). Holding a struct's fields unboxed, or proving
the tests on a field access once for a whole loop, would close most of
nbody's gap.

---

## 24. Phase 2: Lacon's own tools in the harness

Phase 2 is done when an agent finishes tasks with Lacon's own tools
(§22). A harness mode now tests that, along with a control that the first
episodes showed was needed.

**Three ways to write the program.** Through Claude Code, episodes have
used Claude Code's Read, Write and Edit tools. Two variants take those
away and leave only Bash (`bench/harness/run.py`):

- `lacon-tools`: `./lacon put main.lc`, with the program on stdin in a
  heredoc, writes it, and putting one function again replaces that
  function. `./lacon fix`, `q`, `sig`, `check` and `explain` are
  allowed. Only `./run` and `./submit` run the program.
- `<lang>-write`, for any language: `./write` saves stdin as the whole
  program. This is the control, the same Bash-only setup without Lacon's
  tools.

The control was added after the first lacon-tools episodes read about
930 fewer tokens a call than plain Lacon ones, despite a longer system
prompt. The definitions of Read, Write and Edit are about 1,130 tokens on
every call; without the control, that saving would have been credited
to `put`.

**Results.** 30 tasks, two trials each, the short primer and the
chaining hint, Opus 5.5 through Claude Code, all on the same compiler
(`bench/results/20261007-tools`, not committed). The account's usage
limit cut off 27 episodes mid-run, and they were rerun. The harness now
leaves an episode cut off by an API error unrecorded, so resuming a run
retries it; such episodes had been recorded as `gave_up`.

| | Lacon (file tools) | `lacon-write` | `lacon-tools` |
|---|---|---|---|
| Passed | 60/60 | 60/60 | 60/60 |
| First attempt builds | 100% | 98% | 95% |
| Median tokens-to-green | 17,700 | 9,658 | 10,221 |
| Mean API calls | 2.6 | 2.1 | 2.2 |
| Episodes done in two calls | 26 | 53 | 50 |
| First call's context (median) | 5,159 | 4,026 | 4,225 |
| Mean cost | $0.042 | $0.033 | $0.035 |

- **Dropping the file tools saves 35%**: 0.65x the tokens and 0.79x the
  cost, as a geometric mean of per-task ratios. Every call reads about
  1,130 fewer tokens, and most episodes take two calls: one command
  that writes the program, runs it and submits, then a summary. With
  file tools, the Write and the `./run && ./submit` are separate tool
  calls, and the model often read the run's output before submitting.
  That was §13's last open item, the separate run and submit calls, and
  in a Bash-only setup it goes away for any language.
- **Lacon's tools add nothing on these tasks.** lacon-tools took 1.07x
  lacon-write's tokens; on episodes whose first build worked, 1.05x,
  which is the 200 tokens describing the tools, read on every call.
  Every episode wrote its program with one `put` of the whole program
  (1.08 puts an episode), and none used `fix`, `q` or `sig`. The programs
  are about 30 lines, written whole in one go: there is nothing to look
  up and little to change. Where a change was needed, a `put` of only the
  changed items did what it should (`ledger` put `main` and a new `fmt`
  again, not `parse_amt`), but rewriting 30 lines costs little more.
- **Phase 2's tool criterion is met in its narrow sense:** an agent
  finished all 30 tasks, twice, with no file tools, writing only through
  `lacon put`. Whether the tools pay needs tasks in which an agent changes
  a program it didn't just write: several functions, a few hundred lines
  and a change to make. The current suite can't show that.
- **The cross-language comparison (§13) gave every language file
  tools,** so all four paid the 1,130 tokens a call. With Python, Rust
  and Go in `-write` mode, the remaining gap would be Lacon's own: the
  primer, about 1,400 tokens a call.

**Five of the 180 first attempts failed to build,** each on a guess that
nothing caught:

- `[0; w + 1]`, Rust's repeated list, in `knapsack`. It was a parse
  error, and the variable it declared then failed too: `dp[c] = ...`
  reported "can only assign to a variable, a field or an index". It is
  now E0149 with the fix `[0] * (w + 1)`, and parses as that, so nothing
  follows from it. Separately, an assignment through any name that isn't
  defined (`dq[0] = 1`) had reported the same misleading E0210. It now
  says "`dq` is not defined" with the name's fix, and is dropped after a
  parse error like any other use of the name.
- `p[1].parse::<f64>()?` in `group-stats`. E0125 said "no turbofish"
  without a fix; the fix is now `f64(p[1])`, an `f64!` as `parse` is.
- `fn emit(v Json, ind int, var out [str])` in `json-format`: a `var`
  parameter, the body's own copy to change. It is now accepted, and the
  body starts with `var out = out`, which was already valid.
- `"\u0001"` in `ini-query`, the escape of Python, JavaScript and Java.
  It is now accepted alongside Rust's `\u{1}`.
- In `ledger`, `"{str(b % 100).pad_left(2, \"0\")}"` built and printed the
  interpolation as text, because any `\` inside `{...}` made the `{`
  literal (§13, for `"{\n"`). It cost a call and a rewrite. A string
  quoted `\"...\"` inside an interpolation is now an error with the fix
  `{str(b % 100).pad_left(2, '0')}`. A `{` directly before a `\` stays
  literal, so JSON such as `"{\"a\": {n}}"` prints as before.

Two of the five now build and pass every hidden test unchanged; the
other three pass after one `lacon fix`. `tests/check/assign_targets.lc`,
`tests/fix/targets.lc` and `tests/run/var_params.lc` are new, and
`rust_habits.lc` and `interp_quotes.lc` cover the rest.

**`put` keeps a whole program's layout.** Putting a program into an
empty file added a blank line between two constants on adjacent lines
and dropped the comments between items. Items added in a row now keep
the text between them, and in an empty file the text above the first,
so the file comes out as written (`tests/put/whole`). Each task
solution put into an empty file is now the same, byte for byte.

Verified as before: the golden tests (typed and native) and
`bench/tasks/check.py`, interpreted and `--native`, pass on every task.
No other golden output changed.

Next: tasks that change an existing program, to see whether `put`, `fix`
and `q` pay where they should, and Python, Rust and Go in `-write` mode
to redo §13's comparison without the file tools' overhead.
