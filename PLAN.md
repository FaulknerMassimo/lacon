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
- `lacon fix` applies the suggested fixes.

**Edits by name.** `lacon put users.parse_line` replaces a function body read
from stdin, and `lacon rename` and `lacon add-field` cover common refactors.
The agent never has to quote the old code.

**Queries.** `lacon q callers|type|impls <name>` with short output.

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
  the type checker, then either the interpreter or the C backend. Mode
  checking, RC insertion and reuse analysis are not built yet.
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
and 67% of its first attempts built. A smoke run against Rust and Go and a
primer experiment follow it in §13. Not done yet: the full Rust and Go runs
and several trials. See §12 for the
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
is 2-15x faster than the interpreter but well short of Rust, because every
value is still a tagged, reference-counted cell; typed representations belong
with Phase 2's memory model.

### Phase 2 — Memory model and agent tooling

Mode checking, Perceus reference counting with reuse, `sig`, `fix`, `put`,
`q`, the diagnostics format, and primer v1.

**Done when** runtime is within 1.5× of Rust on the benchmarks and an agent
finishes tasks using only Lacon's tools.

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
tests/           golden tests: run/ (stdout), check/ (diagnostics), unit/ (`lacon test`)
bench/tokens/    the same program in Rust, Go, Python and Lacon, plus a token counter
bench/tasks/     Phase 0 tasks (prompt, hidden tests, reference) and a checker
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
  and Rust no-ops like `.iter()`, `.collect()` and `.clone()`. It rejects
  `let`, `//`, `del`, braces, `::`, `:=`, `++`, `if let`, comprehensions and
  macros, each with a one-line hint. This breaks §8's "no second way to do anything" on purpose: accepting
  costs no retry, and a formatter can rewrite aliases to the canonical form.
  Whether the aliases cost accuracy later is a Phase 0 question.
- **No truthiness.** Conditions must be `bool`.
- **Numbers.** Ints and floats mix and give `f64`; int `/` and `%` truncate
  like Rust; integers are 64-bit at run time, with declared sized types
  (`u8`, `u32`) range-checked at function, struct and return boundaries.
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
The checker has to record a type for every expression for that, which it
doesn't yet.

Build time is the other gap: a 109-line program becomes 77 KB of C and takes
1.2 s (0.3 s for a small one), against §6's 1 s for 10,000 lines. Programs
compile at `-O1`, which runs as fast as `-O2` on this code in 60% of the time;
the runtime is compiled once at `-O2` and cached. The generated C is verbose;
a less wordy generator or Cranelift (Phase 3) would cut the rest.
