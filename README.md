# Lacon

Lacon is a compiled, memory-safe language for AI agents to write. It aims to
run as fast as Rust while an agent spends far fewer tokens getting a task from
prompt to passing tests. Human readability is not a goal; agent accuracy is.

The name comes from *laconic*: saying a lot in few words.

## Goals

| | Goal | Now |
|---|---|---|
| Native speed | at or under 1.0x Rust | 1.16x (geometric mean of 11 programs); 4 of them already beat Rust |
| Tokens-to-green, changing a large program | 0.4x Rust | 0.36x with Lacon's own tools, reading by signature (two episodes); 0.90x with the same file tools Rust gets |
| Tokens-to-green, writing a small program | below Rust | 1.30x Rust's tokens, 0.85x its cost |
| First attempts that build | 90% or more | 97% |
| The primer an agent reads | 3,000 tokens or fewer | 1,356 |

**Tokens-to-green** is everything an agent spends from receiving a task to
passing its hidden tests: input, output and cached reads, over every call.
That is the metric, rather than how short the code looks.

## Benchmarks

Agent results are from Opus 5.5 through Claude Code, October 2026. The
details are in [PLAN.md](PLAN.md) §26, §30, §31, §32 and §33.

### Speed

`lacon build` compiles through C. Each program is the same in Lacon and
Rust (`rustc -O`); the times are the best of 7 runs pinned to one core.

| Program | Lacon | Rust | Lacon / Rust | What the gap is |
|---|---|---|---|---|
| trees | 0.238 s | 0.283 s | **0.84x** | |
| records | 0.216 s | 0.242 s | **0.89x** | |
| pipeline | 0.379 s | 0.427 s | **0.89x** | |
| wordfreq | 0.238 s | 0.258 s | **0.92x** | |
| mandel | 0.217 s | 0.206 s | 1.05x | |
| sieve | 0.331 s | 0.292 s | 1.13x | |
| knapsack | 0.228 s | 0.176 s | 1.30x | GCC: the same C by hand takes 1.25x with GCC and 1.0x with clang |
| collatz | 0.327 s | 0.247 s | 1.32x | GCC's loop layout; a check on the step counter |
| concat | 0.161 s | 0.108 s | 1.49x | building and sorting 400,000 strings (1.37x–1.49x run to run) |
| fib | 0.429 s | 0.269 s | 1.59x | call records kept for error traces; the overflow check on `+` |
| nbody | 0.260 s | 0.156 s | 1.67x | LLVM vectorizes Rust's inner loop; GCC doesn't vectorize Lacon's C |
| **geometric mean** | | | **1.16x** | |

Lacon wins on programs that allocate many small values or rebuild lists:
it takes small values from free lists, and updates a list or string in place
when nothing else holds it. It loses on tight arithmetic, which pays for
overflow checks that Rust's release builds skip, except where a loop's
ranges prove them unnecessary.

### Tokens: changing a large program

`sql-groups` starts the agent from a SQL database (a tokenizer, a parser,
expressions with NULL logic, aggregates, transactions, table output) and asks
for `GROUP BY` and `HAVING`. The program is 1,060 lines of Python, 870 of Lacon
and 1,290 of Rust. Two trials each; every episode passed:

| | Tokens-to-green | vs Rust | Cost | API calls |
|---|---|---|---|---|
| Rust (Read, Edit) | 137,633 | 1.00x | $0.343 | 5.5 |
| Lacon (Read, Edit) | 123,386 | 0.90x | $0.277 | 6 |
| Python (Read, Edit) | 90,039 | 0.65x | $0.238 | 5 |
| Lacon's own tools | 68,449 | 0.50x | $0.231 | 4 |
| **Lacon's own tools, reading by signature** | **49,650** | **0.36x** | **$0.185** | **4.5** |

On four smaller programs to change (about 100–250 lines), Lacon's tools take 0.71x
Rust's tokens and 0.81x its cost; with Rust's file tools Lacon takes 1.15x,
and Python 0.98x.

- **The tools are where Lacon leads.** `put` replaces top-level items by
  name, so the agent writes every change into one file and puts, runs and
  submits in a single command. It also needs no Read or Edit tool
  definitions in its context.
- **With the same tools, Lacon trails Python.** Each call also reads the
  primer, and agents still run and submit in separate calls in Lacon.
- **Reading by signature takes it under 0.4x.** When `./show` prints the
  outline (`sig`) rather than the whole program, both agents read it, then
  printed the 13 or 14 items they needed by name: 0.46x of the program's
  tokens, and 8,000–15,000 tokens a call where reading all of it took
  17,000–21,000. That is two episodes on one task, and it relies on the
  program's doc comments, which the outline shows.
- **None of this needs Lacon's syntax.** An outline and edits by name for
  Python or Rust would save the same reading.

### Tokens: writing a small program

30 tasks of about 30 lines each (parsers, data processing, CLI tools,
algorithms), one trial each. Every language writes the whole program with the
Write tool and runs it from Bash.

| | Tokens vs Rust | Cost vs Rust | First attempt builds |
|---|---|---|---|
| Lacon | 1.30x | 0.85x | 97% |
| Python | 0.96x | 0.81x | 100% |
| Go | 1.00x | 1.03x | 100% |

On tasks this small, two thirds of an episode is context every call re-reads
whatever the language is (tool definitions, instructions, the task). A
program of zero tokens would still score 0.90x Rust, so the 0.4x goal can only
be met on larger programs. Lacon's extra tokens are
its primer, about 1,340 a call; without it Lacon would be at 0.97x Rust. It
writes 24% less output than Rust, and output costs five times input, which
is why it is cheaper.

### Program size

The program in [`bench/tokens/`](bench/tokens), in Claude's tokenizer:

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

| Language | Tokens | Lacon / other |
|---|---|---|
| Go | 553 | 0.35x |
| Rust | 471 | 0.41x |
| Python | 239 | 0.80x |
| **Lacon** | **192** | |

That is Lacon's best case. Its gains shrink on real programs, counted with
`o200k_base` (an approximate tokenizer, so read these as ratios only):

| Program | Lacon / Python | Lacon / Rust |
|---|---|---|
| The 30 small tasks' reference solutions | | 0.49x |
| The same tasks as agents write them | 1.20x | 0.69x |
| The edit-task programs (93–870 lines) | 0.92–1.08x | 0.70–0.80x |

Typed signatures and the `?` after each fallible call cost about what the
shorter syntax saves over Python. Agents also write loops where the reference
solutions use list methods, so their Lacon is 1.35x the size of the reference.

## Status

- **Phase 0, the idea test: done.** A primer (the whole language in under
  3,000 tokens, and a short one of 1,356), a tree-walking interpreter, 30
  tasks with hidden tests, and a harness that has Claude solve them in
  Lacon, Python, Rust and Go.
- **Phase 1, the checker and the C backend: done.** `check` reports syntax,
  name and type errors on one line each, with a fix where there is one;
  `build` compiles to a native executable whose output matches the
  interpreter's on every golden program, task and agent-written program.
- **Phase 2, the memory model and agent tools: in progress.**
  - Native code keeps proven ints, bools and floats in C variables, packs
    lists of them, and takes small values from free lists.
  - It inlines the lambdas given to `map`, `filter` and `sort_by`, and
    checks a list once before a loop rather than at every index.
  - A variable's last read moves its value, so `s = s + x`,
    `xs = xs.sort()` and `xs.map(...).filter(...)` reuse a string or list
    nothing else holds.
  - A loop tests once, as it starts, what its ranges prove about its
    `+`, `-` and `*` (`c - wi` in `while c >= wi: ...; c -= 1`), and
    runs a copy that trusts the views of the packed lists it keeps packed,
    so that nothing returns into it from a slow path.
  - Agent tools: `fix` applies the fixes errors carry, `put` replaces items
    by name, `q` and `sig` read a program by item.
  - 5 edit tasks that start the agent from an existing program, in Python,
    Lacon and Rust, the largest a 1,060-line SQL database.
- **Phase 3, speed and scale: planned.** A Cranelift backend for fast debug
  builds, a concurrency runtime with structured `spawn`, packages, C interop,
  and an LSP and MCP server.
- **Phase 4, the ecosystem: planned.** A test-checked corpus of Lacon
  programs that models can be trained on, which would let the primer go.

## Next steps

**Speed, toward 1.0x Rust:**

1. Give error traces without runtime call records: frame pointers or unwind
   tables, read only when a program fails, as Rust does. This is for fib.
2. Store a list of structs as one array per field, or add an LLVM backend,
   so nbody's loop vectorizes. LLVM would also close knapsack's gap.
3. Decide whether a counter that starts low and steps by one may go
   unchecked, since overflowing it would take 2^62 iterations. This is
   collatz's `k += 1`; no range bounds it. (Checking overflow once per loop,
   the earlier plan, made knapsack slower: §33.)

**Tokens, toward 0.4x Rust:**

1. Add a second large edit task of another kind (business rules rather than
   a parser), to check that 0.36x on `sql-groups` isn't a one-off.
2. Run reading by signature on the four mid-size edit tasks, and give
   Python the same outline and edit-by-name tools, to see how much of the
   saving is Lacon's own.
3. Test the example-based primer ([`docs/primer-idioms.md`](docs/primer-idioms.md))
   on the write tasks, to see whether agents' code shrinks toward the
   reference solutions.
4. Save the separate call agents make in Lacon to run before submitting.
5. Weigh the language's own costs on real programs: typed parameters on
   every function, and a `?` on every fallible call.

## Using it

```
cargo build --release
./target/release/lacon run bench/tasks/rpn/solution.lc < bench/tasks/rpn/tests/1.in
./target/release/lacon test tests/unit/primer.lc     # run inline tests
./target/release/lacon check file.lc                 # syntax, name and type errors, one per line
./target/release/lacon fix file.lc                   # apply the errors' fixes, then report what's left
./target/release/lacon build file.lc -o prog         # native executable, through cc
./target/release/lacon put file.lc items.lc          # replace or add top-level items by name (or from stdin)
./target/release/lacon sig file.lc                   # signatures, docs and effects, no bodies
./target/release/lacon q def file.lc parse_line      # items' source (also: q callers, q type)
./target/release/lacon explain E0202                 # long form of a diagnostic
```

Tests and benchmarks:

```
cargo test                                           # golden tests in tests/
uv run bench/tasks/check.py                          # every solution and port against the hidden tests
uv run bench/tasks/check.py --native                 # the same, compiled with `lacon build`
uv run bench/perf/run.py                             # native speed against the same programs in Rust
uv run bench/tokens/count.py --claude-code           # the program-size table above
```

The harness, through Claude Code (or the API with `--agent claude` and a key):

```
uv run bench/harness/run.py --agent claude-code --langs lacon-write,python-write,rust-write
uv run bench/harness/run.py --agent claude-code --suite edits --langs lacon,lacon-tools,python,rust
uv run bench/harness/run.py --agent claude-code --langs lacon-write --primer docs/primer-short.md --chain-hint
uv run bench/harness/report.py bench/results/<run>   # tokens-to-green per language
```

`<lang>` gets Claude Code's Read, Write and Edit; `<lang>-write` only Write,
saving the program with `./write`; `lacon-tools` Write and Lacon's own
`put`, `fix`, `q` and `sig`; `lacon-outline` the same, but in an edit
task its `./show` prints the program's signatures rather than all of it,
and `./show NAME...` those items. `--primer none` runs Lacon with no primer.
Model-written programs run under `bwrap` when it's installed (read-only
filesystem, no network). A run of 30 small tasks costs about $1.20 at API
prices; one `sql-groups` episode about $0.30.

## Design

- **Plain-word syntax** that suits BPE tokenizers: indented blocks, no
  `let`, an implicit `it` lambda parameter, and interpolation in every
  string.
- **Guessable:** each construct is what a model would guess from Python,
  Rust, Kotlin, Go or Swift, and a guess from another language (`let x =`,
  `&str`, `String::new()`) gets an error naming the Lacon form.
- **No lifetimes and no borrow checker:** mutable value semantics, with
  reference counting done at compile time and in-place reuse (Perceus). No
  GC.
- **One string type,** `T?` for optionals, `T!` for results, and `?` to pass
  an error up.
- **A toolchain built for agents:** one-line diagnostics with fixes a tool
  can apply, edits addressed by item name, and signature-only views of a
  program.

## Layout

```
docs/            the primers: full, short, and the example-based one under test
crates/syntax    lexer, parser, AST, diagnostics
crates/interp    resolver and tree-walking interpreter
crates/check     type checker
crates/cgen      C backend, and the C runtime in runtime/
crates/cli       the `lacon` binary
tests/           golden tests for run, check, test, sig, fix, put and q
bench/tasks/     30 write tasks: prompt, hidden tests, reference solutions
bench/edits/     5 edit tasks: the program before and after in Python, Lacon and Rust
bench/perf/      11 speed benchmarks in Lacon and Rust, and a timer
bench/tokens/    one program in four languages, and a token counter
bench/harness/   runs the agent on the tasks and reports tokens-to-green
```

The full design, the build plan and every measurement are in
[PLAN.md](PLAN.md).

## License

[MIT](LICENSE)
