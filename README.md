# Lacon

Lacon is a compiled, memory-safe language designed for AI agents to write.
The aim is Rust-level speed with fewer tokens than Python, and a toolchain that
cuts the total tokens an agent spends getting from a task to passing tests.
Human readability is not a goal; agent accuracy is.

The name comes from *laconic*: saying a lot in few words.

## Status

Phase 0: testing whether agents actually spend fewer tokens in Lacon before
any compiler work starts. There is a [primer](docs/primer.md) (the whole
language in under 3,000 tokens), a throwaway tree-walking interpreter that
runs Lacon programs and their tests, 30 benchmark tasks with hidden tests, and
a harness that has Claude solve them in Lacon, Python, Rust and Go.

Phase 1 adds a static type checker and a C backend. `run`, `test` and `check`
report type errors before anything runs, each on one line with a fix where
there is one. `build` compiles a program to a native executable through C;
its output matches the interpreter's byte for byte on every golden program
and task. Phase 2 has started: native code holds the ints, bools and floats
the checker proves as C scalars, and on seven benchmark programs takes
1.3-7.6x Rust's time (geometric mean 3.0x, down from 9.7x). Typed lists and
cheaper allocation come next. The design and build plan is in
[PLAN.md](PLAN.md).

```
cargo build --release
./target/release/lacon run bench/tasks/rpn/solution.lc < bench/tasks/rpn/tests/1.in
./target/release/lacon test tests/unit/primer.lc     # run inline tests
./target/release/lacon check file.lc                 # syntax, name and type errors, one per line
./target/release/lacon build file.lc -o prog         # native executable, through cc
./target/release/lacon sig bench/tokens/users.lc     # signatures and effects
./target/release/lacon explain E0202                 # long form of a diagnostic
cargo test                                           # golden tests in tests/
uv run bench/tasks/check.py                          # task solutions vs hidden tests
uv run bench/tasks/check.py --native                 # the same, compiled with `lacon build`
uv run bench/perf/run.py                             # native speed against the same programs in Rust
uv run bench/harness/run.py --agent claude-code      # Claude solves the tasks, through Claude Code (Opus 5.5)
uv run bench/harness/run.py --langs lacon,python     # the same through the API (needs an API key)
uv run bench/harness/report.py bench/results/<run>   # tokens-to-green per language
```

The harness runs model-written programs under `bwrap` when it's installed
(read-only filesystem, no network).

## What it looks like

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

The same program in four languages, in [`bench/tokens/`](bench/tokens):

| Language | Tokens | Lacon saves |
|---|---|---|
| Go | 553 | 65% |
| Rust | 471 | 59% |
| Python | 239 | 20% |
| **Lacon** | **192** | — |

Counted in Claude's tokenizer (Opus 5.5). Reproduce with
`uv run bench/tokens/count.py --claude-code`, or without `--claude-code` given
an API key.

## Design at a glance

- **Tokens-to-green is the metric:** total tokens from task to passing tests,
  not characters per line.
- **Plain-word syntax** that suits BPE tokenizers, with indentation blocks, no
  `let`, an implicit `it` lambda parameter and interpolation in every string.
- **No lifetimes, no borrow checker fights:** mutable value semantics plus
  compile-time reference counting with in-place reuse (Perceus). No GC.
- **One string type**, `T?` for optionals, `T!` for results, `?` to pass errors
  up.
- **No `async`/`await`:** lightweight threads and structured `spawn`.
- **Toolchain built for agents:** signature-only module views, one-line
  diagnostics with machine-applicable fixes, edits addressed by function name,
  and short query and test output.
- **Compiles to C first**, then Cranelift for fast debug builds.

## License

[MIT](LICENSE)
