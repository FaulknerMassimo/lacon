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

The same program in each language, counted with `o200k_base`
(`uv run bench/tokens/count.py`):

| Language | Tokens | Lacon saves |
|---|---|---|
| Go | 338 | 59% |
| Rust | 302 | 54% |
| Python | 168 | 17% |
| **Lacon** | **139** | — |

`o200k_base` stands in for Claude's tokenizer, which isn't published as a
library, so the numbers are relative. Phase 0 switches to the Claude API's
token-counting endpoint.

Against Python the syntax alone saves only about 17%, because Python is already
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

To settle with Phase 0 data rather than by taste:

- **Indentation or braces.** Measure both on the task suite.
- **Parentheses on zero-argument methods.** The sample uses `p.len` and
  `.lines` but `it.trim()` and `.parse()`. That's inconsistent; pick one rule
  (likely: parentheses optional, the formatter removes them).
- **Positional struct literals.** They save tokens but allow swapped fields of
  the same type. Possibly allow them only when every field type is distinct.
- **Missing map keys.** `counts[k] += 1` relies on Go-style zero values. Decide
  whether a plain read of a missing key returns the zero value or `V?`.
- **`?` inside `it` lambdas.** Does it return from the lambda or from the
  enclosing function?
- **Integer overflow.** Trap always, or trap in debug and wrap in release like
  Rust.
- **Effects.** Inferred only, or declarable per package as in §3.6.

---

## 11. Repository layout

Now:

```
PLAN.md          this document
bench/tokens/    the same program in Rust, Go, Python and Lacon, plus a token counter
```

Planned:

```
docs/primer.md   the ≤ 3,000-token language primer
bench/tasks/     the Phase 0 task suite and harness
crates/          Rust workspace: syntax, check, ir, cgen, cli
```
