# /// script
# requires-python = ">=3.10"
# dependencies = ["anthropic"]
# ///
"""Phase 0 harness: Claude solves every task in each language, and we log the
tokens it spends getting to passing tests (tokens-to-green).

    uv run bench/harness/run.py                                # every task and language
    uv run bench/harness/run.py --langs lacon,python --tasks rpn,calc --trials 3
    uv run bench/harness/run.py --agent claude-code            # through Claude Code: no API key
    uv run bench/harness/run.py --agent replay                 # no model: submit the reference solutions
    uv run bench/harness/run.py --langs lacon --primer docs/primer-short.md   # try another primer
    uv run bench/harness/run.py --agent claude-code --langs lacon,lacon-write,lacon-tools   # how the program is written
    uv run bench/harness/report.py bench/results/<run>         # summarize a run

In each episode the model gets the task prompt, one example, and two tools:
`run` builds a program and runs it on input of its choice, and `submit` runs
the hidden tests and returns the first failing one. The episode is green when a
submission passes. Lacon episodes also get the primer in the system prompt;
nothing else differs between languages.

Through Claude Code, the model writes the program with Claude Code's Read,
Write and Edit tools. Two variants take those away, leaving only Bash:
`<lang>-write` (`python-write`, `lacon-write`, ...) saves the whole program
with `./write`, and `lacon-tools` writes and changes it with `./lacon put`
and `./lacon fix` and reads it with `./lacon q` and `./lacon sig`. Against
each other, they measure whether Lacon's own tools cut tokens-to-green, apart
from what dropping the file tools' definitions saves on every call.

Agents:
- `claude` calls the Messages API (needs ANTHROPIC_API_KEY or similar).
- `claude-code` runs `claude -p` in the episode's directory on your Claude
  Code login. The tools are the scripts `./run` and `./submit`; Bash may run
  nothing else, file tools are confined to the directory, and the session
  starts with this harness's system prompt and no CLAUDE.md, skills, plugins
  or settings. Tokens are summed from the session's API calls.
- `replay` submits each task's reference solution, to test the harness.

Results go to bench/results/<run>/: config.json, episodes.jsonl (one line per
episode) and transcripts/. Passing an existing run to --out resumes it,
skipping episodes already recorded. Episodes cut off by an API error (a usage
limit, an outage) aren't recorded, so a resume retries them.

Refusal fallbacks are deliberately off: a fallback model would solve some
episodes and blur the comparison, so a refusal is recorded as an outcome.
"""
from __future__ import annotations

import argparse
import json
import sys
import tempfile
import threading
import time
import traceback
from concurrent.futures import ThreadPoolExecutor
from dataclasses import asdict, dataclass, field
from datetime import datetime, timezone
from pathlib import Path

import langs as L

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
TASKS = ROOT / "bench" / "tasks"
PRIMER = ROOT / "docs" / "primer.md"
RESULTS = ROOT / "bench" / "results"

DEFAULT_MODEL = "claude-opus-5-5"
# $ per million tokens: (input, output, cache read). Cache writes with the
# default 5-minute lifetime cost 1.25x input. Used for an estimated cost only.
PRICES = {
    "claude-fable-5-1": (10.0, 50.0, 0.25),
    "claude-opus-5-5": (4.0, 20.0, 0.20),
    "claude-opus-5": (5.0, 25.0, 0.50),
    "claude-opus-4-8": (5.0, 25.0, 0.50),
    "claude-sonnet-5": (2.0, 10.0, 0.20),
    "claude-haiku-4-5": (1.0, 5.0, 0.10),
}

TOOLS = [
    {
        "name": "run",
        "description": "Build the program and run it on the given standard input. Returns the build errors, or the exit code, standard output and standard error.",
        "strict": True,
        "input_schema": {
            "type": "object",
            "properties": {
                "code": {"type": "string", "description": "The complete program source."},
                "stdin": {"type": "string", "description": "Standard input for this run."},
            },
            "required": ["code", "stdin"],
            "additionalProperties": False,
        },
    },
    {
        "name": "submit",
        "description": "Submit the program: it is built and run against the hidden tests. If they all pass the task is done; otherwise the first failing test comes back.",
        "strict": True,
        "input_schema": {
            "type": "object",
            "properties": {"code": {"type": "string", "description": "The complete program source."}},
            "required": ["code"],
            "additionalProperties": False,
        },
    },
]

SYSTEM = """You are solving a programming task. Write one complete program in {lang} that reads standard input and writes standard output.

Environment: {environment}

Use the `run` tool to try the program on inputs you choose and the `submit` tool to run it against the hidden tests. The task is finished when a submission passes."""

NUDGE = "Submit your program with the `submit` tool when it is ready."

SYSTEM_CC = """You are solving a programming task. Write one complete program in {lang} that reads standard input and writes standard output, in the file `main{ext}` in the current directory.

Environment: {environment}

`./run input.txt` (or `printf '1 2\\n' | ./run`) builds the program and runs it on that input, printing the build errors or the exit code and output. `./submit` runs it against the hidden tests and prints the first failing one. The task is finished when a submission passes.

Bash can run only `./run`, `./submit`, `ls`, `printf` and `echo`; piping into `./run` works, but redirection and other commands are blocked. Create and change files with the Write and Edit tools."""

SYSTEM_CC_TOOLS = """You are solving a programming task. Write one complete program in {lang} that reads standard input and writes standard output, in the file `main{ext}` in the current directory.

Environment: {environment}

There are no file tools; `./lacon` writes and reads the program. `./lacon put main.lc` reads top-level items (`fn`, `type`, `enum`, constants) from stdin and replaces each item of the same name in main.lc, or adds it, creating the file if needed. It then checks the file and prints each error, or `ok`. Write the program with a heredoc, and change a function by putting just that function again:

```
./lacon put main.lc <<'EOF'
fn main() =
  print("hi")
EOF
```

`./lacon fix main.lc` applies the fixes the errors suggest and prints what is left. `./lacon q def main.lc NAME` prints an item's source, `./lacon q type main.lc:LINE:COL` the type of the expression there, and `./lacon sig main.lc` every signature.

`printf '1 2\\n' | ./run` builds the program and runs it on that input, printing the build errors or the exit code and output. `./submit` runs it against the hidden tests and prints the first failing one. The task is finished when a submission passes.

Bash can run only `./lacon`, `./run`, `./submit`, `ls`, `printf` and `echo`; piping into `./run` works, but other commands are blocked."""

SYSTEM_CC_WRITE = """You are solving a programming task. Write one complete program in {lang} that reads standard input and writes standard output, in the file `main{ext}` in the current directory.

Environment: {environment}

There are no file tools. `./write` saves its stdin as `main{ext}`, replacing the whole file. Write the program with a heredoc:

```
./write <<'EOF'
...the whole program...
EOF
```

`printf '1 2\\n' | ./run` builds the program and runs it on that input, printing the build errors or the exit code and output. `./submit` runs it against the hidden tests and prints the first failing one. The task is finished when a submission passes.

Bash can run only `./write`, `./run`, `./submit`, `ls`, `printf` and `echo`; piping into `./run` works, but other commands are blocked."""

# Claude Code's Bash may run only the episode's own tools, `ls`, and
# printf/echo to pipe input into `./run`.
CC_ALLOWED = ["Read", "Write", "Edit", "Bash(./run)", "Bash(./run *)", "Bash(./submit)", "Bash(ls)", "Bash(ls *)", "Bash(printf *)", "Bash(echo *)"]
# Without file tools, `./write` or `./lacon` takes their place.
CC_BASH = [t for t in CC_ALLOWED if t.startswith("Bash(")]
CC_ALLOWED_EDIT = {
    "files": CC_ALLOWED,
    "write": ["Bash(./write)", "Bash(./write *)", *CC_BASH],
    "tools": ["Bash(./lacon *)", *CC_BASH],
}


# ----- tasks -----


@dataclass
class Task:
    name: str
    prompt: str
    tests: list[tuple[str, str]]  # (input, expected output)

    @staticmethod
    def load(name: str) -> Task:
        d = TASKS / name
        ins = sorted(d.glob("tests/*.in"), key=lambda p: int(p.stem))
        tests = [(p.read_text(), p.with_suffix(".out").read_text()) for p in ins]
        return Task(name, (d / "prompt.md").read_text().strip(), tests)

    def user_message(self) -> str:
        example_in, example_out = self.tests[0]
        return f"{self.prompt}\n\nExample input:\n```\n{example_in}```\n\nExample output:\n```\n{example_out}```"


def all_tasks() -> list[str]:
    return sorted(p.name for p in TASKS.iterdir() if (p / "prompt.md").exists())


# --chain-hint: told to every language, to see whether it saves the separate
# `run` call a model makes before submitting.
CHAIN_HINT = {
    "claude": "A failed submission has no penalty and `submit` reports build errors too, so you can submit without running first.",
    "claude-code": "A failed submission has no penalty and `./submit` reports build errors too, so you can write `main{ext}` and run `./run input.txt && ./submit` in the same turn.",
    "bash": "A failed submission has no penalty and `./submit` reports build errors too, so one command can {verb} the program, run it and submit: the heredoc, then `printf '...' | ./run && ./submit` on the line after `EOF`.",
}


def system_prompt(lang: L.Lang, agent: str = "claude", primer: Path = PRIMER, chain_hint: bool = False) -> str:
    template = {"write": SYSTEM_CC_WRITE, "tools": SYSTEM_CC_TOOLS}.get(lang.edit, SYSTEM_CC if agent == "claude-code" else SYSTEM)
    s = template.format(lang=lang.name, ext=lang.ext, environment=lang.environment)
    if chain_hint:
        hint = "bash" if lang.edit != "files" else "claude-code" if agent == "claude-code" else "claude"
        s += "\n\n" + CHAIN_HINT[hint].format(ext=lang.ext, verb="put" if lang.edit == "tools" else "write")
    if isinstance(lang, L.Lacon):
        s += "\n\n" + primer.read_text()
    return s


# ----- one episode -----


def clip(s: str, n: int) -> str:
    return s if len(s) <= n else s[:n] + f"\n... ({len(s) - n} more characters)"


@dataclass
class Usage:
    input: int = 0
    cache_write: int = 0
    cache_read: int = 0
    output: int = 0

    @property
    def total(self) -> int:
        return self.input + self.cache_write + self.cache_read + self.output

    def add(self, u) -> None:
        self.input += u.input_tokens or 0
        self.cache_write += u.cache_creation_input_tokens or 0
        self.cache_read += u.cache_read_input_tokens or 0
        self.output += u.output_tokens or 0

    def cost(self, model: str) -> float | None:
        if model not in PRICES:
            return None
        i, o, r = PRICES[model]
        return (self.input * i + self.cache_write * i * 1.25 + self.cache_read * r + self.output * o) / 1e6


@dataclass
class Episode:
    """A workspace for one attempt at one task, and the tools acting on it."""

    task: Task
    lang: L.Lang
    trial: int
    workdir: Path
    runs: int = 0
    submits: int = 0
    first_build_ok: bool | None = None
    passed: bool = False
    final_code: str | None = None
    usage: Usage = field(default_factory=Usage)
    api_calls: int = 0
    # Claude Code's own cost figure (list price; notional on a subscription).
    reported_cost: float | None = None
    # The models that actually answered, to catch a run on the wrong one.
    served: set[str] = field(default_factory=set)
    # Claude Code's tool calls by name, and the episode's own commands other
    # than run and submit (`write`, or `put`, `fix`... for lacon-tools).
    tool_uses: dict[str, int] = field(default_factory=dict)
    commands: dict[str, int] = field(default_factory=dict)

    def _build(self, code: str) -> L.Build:
        src = self.workdir / f"main{self.lang.ext}"
        src.write_text(code)
        b = self.lang.build(src)
        if self.first_build_ok is None:
            self.first_build_ok = b.ok
        return b

    def tool(self, name: str, inp: dict) -> str:
        if name == "run":
            self.runs += 1
            b = self._build(inp["code"])
            if not b.ok:
                return "build failed:\n" + clip(b.errors, 4000)
            r = self.lang.run(b, self.workdir, inp["stdin"])
            status = f"timed out after {L.RUN_TIMEOUT}s" if r.exit_code is None else f"exit code {r.exit_code}"
            return f"{status}\nstdout:\n{clip(r.stdout, 4000)}\nstderr:\n{clip(r.stderr, 2000)}"
        if name == "submit":
            self.submits += 1
            self.final_code = inp["code"]
            b = self._build(inp["code"])
            if not b.ok:
                return "build failed:\n" + clip(b.errors, 4000)
            for i, (stdin, want) in enumerate(self.task.tests, 1):
                r = self.lang.run(b, self.workdir, stdin)
                if r.exit_code is None or L.norm(r.stdout) != L.norm(want):
                    status = f"timed out after {L.RUN_TIMEOUT}s" if r.exit_code is None else f"exit code {r.exit_code}"
                    return (
                        f"failed test {i} of {len(self.task.tests)} ({status}).\n"
                        f"input:\n{clip(stdin, 1500)}\nexpected output:\n{clip(want, 1500)}\n"
                        f"actual output:\n{clip(r.stdout, 1500)}\nstderr:\n{clip(r.stderr, 1500)}"
                    )
            self.passed = True
            return f"all {len(self.task.tests)} tests passed"
        return f"unknown tool {name!r}"

    def write(self, text: str) -> str:
        """`./write` in a `write` episode."""
        self.commands["write"] = self.commands.get("write", 0) + 1
        src = self.workdir / f"main{self.lang.ext}"
        src.write_text(text)
        return f"wrote {src.name}, {len(text.splitlines())} lines"

    def lacon(self, args: list[str], stdin: str) -> L.Result:
        """A `./lacon` command in a lacon-tools episode. The commands that
        check the program count as its first build if nothing built it
        before, since the model sees their errors as it would a build's."""
        sub = args[0] if args else ""
        if sub not in L.Lacon.COMMANDS:
            return L.Result(2, "", f"./lacon runs only {', '.join(L.Lacon.COMMANDS)}; run the program with ./run and ./submit\n")
        self.commands[sub] = self.commands.get(sub, 0) + 1
        r = self.lang.command(args, self.workdir, stdin)
        if self.first_build_ok is None and r.exit_code is not None:
            if sub in ("put", "check") and r.exit_code in (0, 1):
                self.first_build_ok = r.exit_code == 0
            elif sub == "fix":
                # A fix made means the program had errors.
                self.first_build_ok = r.exit_code == 0 and not any(line.startswith("fixed ") for line in r.stdout.splitlines())
        return r


def valid_input(name: str, inp) -> bool:
    if not isinstance(inp, dict) or not isinstance(inp.get("code"), str):
        return False
    return name != "run" or isinstance(inp.get("stdin"), str)


def jsonable(x):
    if hasattr(x, "model_dump"):
        return x.model_dump(mode="json", exclude_none=True)
    if isinstance(x, dict):
        return {k: jsonable(v) for k, v in x.items()}
    if isinstance(x, list):
        return [jsonable(v) for v in x]
    return x


def claude_agent(ep: Episode, client, args) -> tuple[str, list]:
    """The model loop. Returns the outcome and the transcript."""
    import anthropic

    system = system_prompt(ep.lang, "claude", args.primer, args.chain_hint)
    messages: list = [{"role": "user", "content": ep.task.user_message()}]
    extra = {"output_config": {"effort": args.effort}} if args.effort else {}
    nudged = False
    for _ in range(args.max_turns):
        try:
            resp = client.messages.create(
                model=args.model,
                max_tokens=args.max_tokens,
                system=system,
                tools=TOOLS,
                messages=messages,
                cache_control={"type": "ephemeral"},
                **extra,
            )
        except anthropic.APIError as e:
            messages.append({"api_error": str(e)})
            return "api_error", messages
        ep.api_calls += 1
        ep.usage.add(resp.usage)
        ep.served.add(resp.model)
        messages.append({"role": "assistant", "content": resp.content})
        if resp.stop_reason == "refusal":
            return "refusal", messages
        uses = [b for b in resp.content if b.type == "tool_use"]
        if not uses:
            if nudged:
                return "gave_up", messages
            nudged = True
            messages.append({"role": "user", "content": NUDGE})
            continue
        results = []
        for u in uses:
            if not valid_input(u.name, u.input):
                text = "the tool input was incomplete (possibly cut off); send the call again"
                results.append({"type": "tool_result", "tool_use_id": u.id, "content": text, "is_error": True})
                continue
            results.append({"type": "tool_result", "tool_use_id": u.id, "content": ep.tool(u.name, u.input)})
            if ep.passed:
                messages.append({"role": "user", "content": results})
                return "pass", messages
        messages.append({"role": "user", "content": results})
    return "turn_limit", messages


def replay_agent(ep: Episode) -> tuple[str, list]:
    """Submits the task's reference solution, if it has one, to test the
    harness without the API."""
    d = TASKS / ep.task.name
    ref = d / f"solution{ep.lang.ext}"
    if not ref.exists() and isinstance(ep.lang, L.Python):
        ref = d / "ref.py"
    if not ref.exists():
        return "no_reference", []
    code = ref.read_text()
    log = []
    if ep.lang.edit == "tools":
        # Put the whole solution, as a model would, and submit what put wrote.
        r = ep.lacon(["put", f"main{ep.lang.ext}"], code)
        log.append({"tool": "lacon put", "result": r.stdout + r.stderr})
        code = (ep.workdir / f"main{ep.lang.ext}").read_text()
    elif ep.lang.edit == "write":
        log.append({"tool": "write", "result": ep.write(code)})
    for name, inp in [("run", {"code": code, "stdin": ep.task.tests[0][0]}), ("submit", {"code": code})]:
        log.append({"tool": name, "result": ep.tool(name, inp)})
    return ("pass" if ep.passed else "fail"), log


TOOL_SCRIPT = """#!{python}
import sys
sys.path.insert(0, {harness!r})
from run import tool_main
sys.exit(tool_main({tool!r}, {task!r}, {lang!r}, {events!r}))
"""


def tool_main(tool: str, task_name: str, lang_key: str, events: str) -> int:
    """`./run [input-file]`, `./submit`, `./write` and lacon-tools' `./lacon
    <command>` inside a Claude Code episode."""
    lang = L.make(lang_key, build=False)
    src = Path.cwd() / f"main{lang.ext}"
    ep = Episode(Task.load(task_name), lang, 0, Path.cwd())
    if tool == "write":
        text = ep.write(sys.stdin.read() if not sys.stdin.isatty() else "")
        with open(events, "a") as f:
            f.write(json.dumps({"tool": "write", "command": "write", "build_ok": None, "passed": False}) + "\n")
        print(text)
        return 0
    if tool == "lacon":
        args = sys.argv[1:]
        # Only `put` reads stdin, which may be a terminal or left open.
        stdin = sys.stdin.read() if args[:1] == ["put"] and not sys.stdin.isatty() else ""
        r = ep.lacon(args, stdin)
        with open(events, "a") as f:
            f.write(json.dumps({"tool": "lacon", "command": args[0] if args else "", "build_ok": ep.first_build_ok, "passed": False}) + "\n")
        status = f"timed out after {L.BUILD_TIMEOUT}s\n" if r.exit_code is None else ""
        sys.stdout.write(clip(r.stdout, 8000))
        sys.stdout.flush()
        sys.stderr.write(status + clip(r.stderr, 4000))
        return 1 if r.exit_code is None else r.exit_code
    if not src.exists():
        print(f"there is no {src.name} yet; write the program first")
        return 1
    if tool == "run":
        stdin = Path(sys.argv[1]).read_text() if len(sys.argv) > 1 else sys.stdin.read()
        text = ep.tool("run", {"code": src.read_text(), "stdin": stdin})
    else:
        text = ep.tool("submit", {"code": src.read_text()})
    with open(events, "a") as f:
        f.write(json.dumps({"tool": tool, "build_ok": ep.first_build_ok, "passed": ep.passed, "code": src.read_text()}) + "\n")
    print(text)
    return 0


def child_env() -> dict:
    """The environment for a nested `claude`: without the parent session's
    variables, so the harness also works when run from inside Claude Code."""
    import os

    return {k: v for k, v in os.environ.items() if not (k.startswith("CLAUDE_CODE_") or k in ("CLAUDECODE", "CLAUDE_PID", "CLAUDE_EFFORT"))}


def claude_code_agent(ep: Episode, args) -> tuple[str, list]:
    import subprocess

    with tempfile.TemporaryDirectory(prefix="lacon-bench-events-") as side:
        events = Path(side) / "events.jsonl"
        edit = ep.lang.edit
        for tool in ("run", "submit", *{"write": ["write"], "tools": ["lacon"]}.get(edit, [])):
            script = ep.workdir / tool
            script.write_text(TOOL_SCRIPT.format(python=sys.executable, harness=str(HERE), tool=tool, task=ep.task.name, lang=ep.lang.key, events=str(events)))
            script.chmod(0o755)
        cmd = [
            args.claude, "-p", ep.task.user_message(),
            "--output-format", "stream-json", "--verbose",
            "--model", args.model,
            "--system-prompt", system_prompt(ep.lang, "claude-code", args.primer, args.chain_hint),
            "--tools", "Bash,Read,Write,Edit" if edit == "files" else "Bash",
            "--allowedTools", *CC_ALLOWED_EDIT[edit],
            "--permission-prompts", "none",
            "--restricted", "--safe-mode", "--no-session-persistence",
            "--max-budget-usd", str(args.max_budget),
        ]
        if args.effort:
            cmd += ["--effort", args.effort]
        try:
            r = subprocess.run(cmd, cwd=ep.workdir, env=child_env(), capture_output=True, text=True, timeout=args.session_timeout)
            stdout, stderr, timed_out = r.stdout, r.stderr, False
        except subprocess.TimeoutExpired as e:
            stdout = e.stdout.decode(errors="replace") if isinstance(e.stdout, bytes) else (e.stdout or "")
            stderr, timed_out = "", True
        log = [json.loads(line) for line in stdout.splitlines() if line.startswith("{")]
        if events.exists():
            evs = [json.loads(line) for line in events.read_text().splitlines()]
        else:
            evs = []

    # One API call can appear as several assistant messages sharing an id.
    ep.api_calls = len({m["message"]["id"] for m in log if m.get("type") == "assistant" and (m.get("message") or {}).get("id")})
    # The usage on streamed messages is captured as each starts (partial
    # output counts); the result record has the session's totals, per model.
    result = next((m for m in reversed(log) if m.get("type") == "result"), {})
    per_model = (result.get("modelUsage") or {}).values()
    ep.served.update(u.get("canonicalModel") or name for name, u in (result.get("modelUsage") or {}).items())
    if per_model:
        for u in per_model:
            ep.usage.add(SimpleUsage({
                "input_tokens": u.get("inputTokens"),
                "cache_creation_input_tokens": u.get("cacheCreationInputTokens"),
                "cache_read_input_tokens": u.get("cacheReadInputTokens"),
                "output_tokens": u.get("outputTokens"),
            }))
    elif result.get("usage"):
        ep.usage.add(SimpleUsage(result["usage"]))
    ep.runs = sum(e["tool"] == "run" for e in evs)
    ep.submits = sum(e["tool"] == "submit" for e in evs)
    ep.first_build_ok = next((e["build_ok"] for e in evs if e["build_ok"] is not None), None)
    for e in evs:
        if e.get("command"):
            ep.commands[e["command"]] = ep.commands.get(e["command"], 0) + 1
    uses = {b["id"]: b["name"] for m in log if m.get("type") == "assistant" for b in (m.get("message") or {}).get("content") or [] if isinstance(b, dict) and b.get("type") == "tool_use"}
    for name in uses.values():
        ep.tool_uses[name] = ep.tool_uses.get(name, 0) + 1
    ep.passed = any(e["passed"] for e in evs)
    submitted = [e for e in evs if e["tool"] == "submit"]
    ep.final_code = submitted[-1]["code"] if submitted else None
    ep.reported_cost = result.get("total_cost_usd")
    if ep.passed:
        outcome = "pass"
    elif timed_out:
        outcome = "timeout"
    elif not result:
        outcome = "cc_error"
        log.append({"stderr": stderr[-4000:]})
    elif result.get("is_error") and result.get("api_error_status"):
        # A usage limit or an outage, not the model giving up.
        outcome = "api_error"
    elif "budget" in str(result.get("subtype", "")):
        outcome = "budget_limit"
    elif "turns" in str(result.get("subtype", "")):
        outcome = "turn_limit"
    else:
        outcome = "gave_up"
    return outcome, log


class SimpleUsage:
    """A usage dict from Claude Code's output, shaped like the SDK's."""

    def __init__(self, u: dict) -> None:
        self.input_tokens = u.get("input_tokens")
        self.cache_creation_input_tokens = u.get("cache_creation_input_tokens")
        self.cache_read_input_tokens = u.get("cache_read_input_tokens")
        self.output_tokens = u.get("output_tokens")


def run_episode(task_name: str, lang: L.Lang, trial: int, args, client, out: Path) -> dict:
    task = Task.load(task_name)
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix=f"lacon-bench-{task_name}-") as tmp:
        ep = Episode(task, lang, trial, Path(tmp))
        error = None
        try:
            if args.agent == "replay":
                outcome, transcript = replay_agent(ep)
            elif args.agent == "claude-code":
                outcome, transcript = claude_code_agent(ep, args)
            else:
                outcome, transcript = claude_agent(ep, client, args)
        except Exception:
            outcome, transcript, error = "harness_error", [], traceback.format_exc()
    record = {
        "task": task_name,
        "lang": lang.key,
        "trial": trial,
        "agent": args.agent,
        "model": args.model if args.agent != "replay" else None,
        "served_by": sorted(ep.served),
        "effort": args.effort,
        "outcome": outcome,
        "passed": ep.passed,
        "api_calls": ep.api_calls,
        "runs": ep.runs,
        "submits": ep.submits,
        "first_build_ok": ep.first_build_ok,
        "edit": lang.edit if args.agent != "claude" else None,
        "tool_uses": ep.tool_uses,
        "commands": ep.commands,
        "tokens": {**asdict(ep.usage), "total": ep.usage.total},
        # Claude Code's own figure when it gives one (it caches with a 1-hour
        # lifetime, which costs more to write); otherwise an estimate.
        "cost_usd": 0.0 if args.agent == "replay" else ep.reported_cost if ep.reported_cost is not None else ep.usage.cost(args.model),
        "wall_s": round(time.monotonic() - start, 2),
        "final_code": ep.final_code,
        "error": error,
    }
    name = f"{task_name}.{lang.key}.{trial}.json"
    (out / "transcripts" / name).write_text(json.dumps({"system": system_prompt(lang, args.agent, args.primer, args.chain_hint), "messages": jsonable(transcript)}, indent=1))
    return record


# ----- driver -----


def prompt_tokens(client, args, lang: L.Lang, task: Task) -> int | None:
    """Tokens in an episode's fixed starting context: tools, system prompt
    (with the primer for Lacon) and the first user message."""
    try:
        r = client.messages.count_tokens(model=args.model, system=system_prompt(lang, "claude", args.primer, args.chain_hint), tools=TOOLS, messages=[{"role": "user", "content": task.user_message()}])
        return r.input_tokens
    except Exception:
        return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--agent", choices=["claude", "claude-code", "replay"], default="claude")
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--effort", choices=["low", "medium", "high", "xhigh", "max"], help="output_config.effort (default: the model's)")
    ap.add_argument("--langs", default="lacon,python,rust,go")
    ap.add_argument("--tasks", help="comma-separated task names (default: all)")
    ap.add_argument("--trials", type=int, default=1)
    ap.add_argument("--jobs", type=int, default=4, help="episodes run in parallel")
    ap.add_argument("--max-turns", type=int, default=25, help="API calls per episode")
    ap.add_argument("--max-tokens", type=int, default=16000)
    ap.add_argument("--out", type=Path, help="run directory (default: a new one in bench/results)")
    ap.add_argument("--claude", default="claude", help="the Claude Code executable (claude-code agent)")
    ap.add_argument("--max-budget", type=float, default=2.0, help="claude-code: --max-budget-usd per episode")
    ap.add_argument("--session-timeout", type=int, default=1800, help="claude-code: seconds per episode")
    ap.add_argument("--primer", type=Path, default=PRIMER, help="the primer Lacon episodes get (default: docs/primer.md)")
    ap.add_argument("--chain-hint", action="store_true", help="tell every language that a failed submission has no penalty")
    args = ap.parse_args()

    tasks = args.tasks.split(",") if args.tasks else all_tasks()
    for t in tasks:
        if not (TASKS / t / "prompt.md").exists():
            ap.error(f"no task {t!r}")
    langs: list[L.Lang] = []
    for key in args.langs.split(","):
        try:
            lang = L.make(key)
        except KeyError:
            ap.error(f"unknown language {key!r}; choose from {', '.join(L.keys())}")
        if why := lang.unavailable():
            print(f"skipping {key}: {why}", file=sys.stderr)
            continue
        langs.append(lang)
    if not langs:
        ap.error("no languages available")
    if not L.SANDBOX:
        print("warning: running model-written programs without a sandbox (bwrap not available or disabled)", file=sys.stderr)

    client = None
    if args.agent == "claude" and any(lang.edit != "files" for lang in langs):
        ap.error("-write and -tools need --agent claude-code or replay")
    if args.agent == "claude":
        import anthropic

        client = anthropic.Anthropic(max_retries=8)

    stamp = datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S")
    out = args.out or RESULTS / f"{stamp}-{args.agent if args.agent == 'replay' else args.model}"
    (out / "transcripts").mkdir(parents=True, exist_ok=True)
    log_path = out / "episodes.jsonl"
    done = set()
    if log_path.exists():
        for line in log_path.read_text().splitlines():
            r = json.loads(line)
            done.add((r["task"], r["lang"], r["trial"]))
    config = {
        "started": stamp,
        "agent": args.agent,
        "model": args.model,
        "effort": args.effort,
        "max_turns": args.max_turns if args.agent == "claude" else None,
        "max_tokens": args.max_tokens,
        "sandbox": L.SANDBOX,
        "languages": {lang.key: lang.environment for lang in langs},
        "primer": {"path": str(args.primer), "chars": len(args.primer.read_text())},
        "chain_hint": args.chain_hint,
    }
    if client is not None:
        sample = Task.load(tasks[0])
        config["prompt_tokens"] = {lang.key: prompt_tokens(client, args, lang, sample) for lang in langs}
        config["prompt_tokens_task"] = sample.name
    if not (out / "config.json").exists():
        (out / "config.json").write_text(json.dumps(config, indent=2) + "\n")

    jobs = [(t, lang, trial) for trial in range(1, args.trials + 1) for t in tasks for lang in langs if (t, lang.key, trial) not in done]
    print(f"{len(jobs)} episodes -> {out}", file=sys.stderr)
    lock = threading.Lock()

    def work(job):
        t, lang, trial = job
        rec = run_episode(t, lang, trial, args, client, out)
        with lock:
            # An episode cut off by the API isn't recorded, so resuming the
            # run retries it.
            if rec["outcome"] != "api_error":
                with log_path.open("a") as f:
                    f.write(json.dumps(rec) + "\n")
            cost = f" ${rec['cost_usd']:.3f}" if rec["cost_usd"] else ""
            other = [m for m in rec["served_by"] if m != args.model]
            warn = f"  (answered by {', '.join(other)}, not {args.model})" if other else ""
            print(f"{t:18} {lang.key:11} #{trial} {rec['outcome']:12} {rec['tokens']['total']:>8} tok {rec['api_calls']:>2} calls{cost}{warn}", file=sys.stderr)
        return rec

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        records = list(pool.map(work, jobs))
    failed = [r for r in records if r["outcome"] == "harness_error"]
    for r in failed:
        print(f"harness error in {r['task']}/{r['lang']}:\n{r['error']}", file=sys.stderr)
    if cut := sum(r["outcome"] == "api_error" for r in records):
        print(f"{cut} episodes hit API errors and weren't recorded; pass --out {out} to retry them", file=sys.stderr)
    print(f"done: uv run bench/harness/report.py {out}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
