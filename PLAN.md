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
(`claude -p`, no API key needed). Not done yet: a full run, and a Go
toolchain on the benchmark machine. See §12 for the semantics the
interpreter settled on and §13 for what writing the tasks taught.

### Phase 1 — MVP

Type checker, C backend, core standard library (`str`, collections, `fs`, formatting),
`lacon run`, `lacon build`, `lacon test`.

**Done when** every benchmark task compiles natively and passes.

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
  interpolation is literal.*
- **Blocks inside brackets.** Layout is off inside `( )`, so
  `push(match x ...)` with indented arms couldn't parse, and writing the tasks
  hit it at once (§13). *A `match` inside brackets finds its arms by column:
  each starts a line at the first arm's column and is one expression. Arms
  that need statements get a hint to bind the match first.*

---

## 11. Repository layout

Now:

```
PLAN.md          this document
docs/primer.md   the language primer an agent gets (≤ 3,000 tokens)
crates/syntax    lexer, parser, AST, diagnostics
crates/interp    Phase 0 resolver and tree-walking interpreter
crates/cli       the `lacon` binary
tests/           golden tests: run/ (stdout), check/ (diagnostics), unit/ (`lacon test`)
bench/tokens/    the same program in Rust, Go, Python and Lacon, plus a token counter
bench/tasks/     Phase 0 tasks (prompt, hidden tests, reference) and a checker
bench/harness/   has Claude solve the tasks in each language and reports tokens-to-green
bench/results/   harness runs (created by the harness)
```

Planned: `check`, `ir` and `cgen` crates for the Phase 1 compiler.

---

## 12. Phase 0 semantics

Decisions the interpreter made beyond §3, for the benchmark to confirm or
overturn.

- **Methods are functions.** No `impl` blocks or `self`: `fn area(s Shape)` is
  called `s.area()` or `area(s)`, overloaded on the first parameter's type.
  Builtin methods work as functions too (`len(xs)`). Traits aren't
  implemented; generics are checked at run time.
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
