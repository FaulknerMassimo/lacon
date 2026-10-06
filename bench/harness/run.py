# /// script
# requires-python = ">=3.10"
# dependencies = ["anthropic"]
# ///
"""Phase 0 harness: Claude solves every task in each language, and we log the
tokens it spends getting to passing tests (tokens-to-green).

    uv run bench/harness/run.py                                # every task and language
    uv run bench/harness/run.py --langs lacon,python --tasks rpn,calc --trials 3
    uv run bench/harness/run.py --agent replay                 # no API: submit the reference solutions
    uv run bench/harness/report.py bench/results/<run>         # summarize a run

In each episode the model gets the task prompt, one example, and two tools:
`run` builds a program and runs it on input of its choice, and `submit` runs
the hidden tests and returns the first failing one. The episode is green when a
submission passes. Lacon episodes also get the primer in the system prompt;
nothing else differs between languages.

Results go to bench/results/<run>/: config.json, episodes.jsonl (one line per
episode) and transcripts/. Passing an existing run to --out resumes it,
skipping episodes already recorded.

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

DEFAULT_MODEL = "claude-opus-5"
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


def system_prompt(lang: L.Lang) -> str:
    s = SYSTEM.format(lang=lang.name, environment=lang.environment)
    if lang.key == "lacon":
        s += "\n\n" + PRIMER.read_text()
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

    system = system_prompt(ep.lang)
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
    if not ref.exists() and ep.lang.key == "python":
        ref = d / "ref.py"
    if not ref.exists():
        return "no_reference", []
    code = ref.read_text()
    log = []
    for name, inp in [("run", {"code": code, "stdin": ep.task.tests[0][0]}), ("submit", {"code": code})]:
        log.append({"tool": name, "result": ep.tool(name, inp)})
    return ("pass" if ep.passed else "fail"), log


def run_episode(task_name: str, lang: L.Lang, trial: int, args, client, out: Path) -> dict:
    task = Task.load(task_name)
    start = time.monotonic()
    with tempfile.TemporaryDirectory(prefix=f"lacon-bench-{task_name}-") as tmp:
        ep = Episode(task, lang, trial, Path(tmp))
        error = None
        try:
            if args.agent == "replay":
                outcome, transcript = replay_agent(ep)
            else:
                outcome, transcript = claude_agent(ep, client, args)
        except Exception:
            outcome, transcript, error = "harness_error", [], traceback.format_exc()
    record = {
        "task": task_name,
        "lang": lang.key,
        "trial": trial,
        "agent": args.agent,
        "model": args.model if args.agent == "claude" else None,
        "effort": args.effort,
        "outcome": outcome,
        "passed": ep.passed,
        "api_calls": ep.api_calls,
        "runs": ep.runs,
        "submits": ep.submits,
        "first_build_ok": ep.first_build_ok,
        "tokens": {**asdict(ep.usage), "total": ep.usage.total},
        "cost_usd": ep.usage.cost(args.model) if args.agent == "claude" else 0.0,
        "wall_s": round(time.monotonic() - start, 2),
        "final_code": ep.final_code,
        "error": error,
    }
    name = f"{task_name}.{lang.key}.{trial}.json"
    (out / "transcripts" / name).write_text(json.dumps({"system": system_prompt(lang), "messages": jsonable(transcript)}, indent=1))
    return record


# ----- driver -----


def prompt_tokens(client, model: str, lang: L.Lang, task: Task) -> int | None:
    """Tokens in an episode's fixed starting context: tools, system prompt
    (with the primer for Lacon) and the first user message."""
    try:
        r = client.messages.count_tokens(model=model, system=system_prompt(lang), tools=TOOLS, messages=[{"role": "user", "content": task.user_message()}])
        return r.input_tokens
    except Exception:
        return None


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--agent", choices=["claude", "replay"], default="claude")
    ap.add_argument("--model", default=DEFAULT_MODEL)
    ap.add_argument("--effort", choices=["low", "medium", "high", "xhigh", "max"], help="output_config.effort (default: the model's)")
    ap.add_argument("--langs", default="lacon,python,rust,go")
    ap.add_argument("--tasks", help="comma-separated task names (default: all)")
    ap.add_argument("--trials", type=int, default=1)
    ap.add_argument("--jobs", type=int, default=4, help="episodes run in parallel")
    ap.add_argument("--max-turns", type=int, default=25, help="API calls per episode")
    ap.add_argument("--max-tokens", type=int, default=16000)
    ap.add_argument("--out", type=Path, help="run directory (default: a new one in bench/results)")
    args = ap.parse_args()

    tasks = args.tasks.split(",") if args.tasks else all_tasks()
    for t in tasks:
        if not (TASKS / t / "prompt.md").exists():
            ap.error(f"no task {t!r}")
    langs: list[L.Lang] = []
    for key in args.langs.split(","):
        if key not in L.LANGS:
            ap.error(f"unknown language {key!r}; choose from {', '.join(L.LANGS)}")
        lang = L.LANGS[key]()
        if why := lang.unavailable():
            print(f"skipping {key}: {why}", file=sys.stderr)
            continue
        langs.append(lang)
    if not langs:
        ap.error("no languages available")
    if not L.SANDBOX:
        print("warning: running model-written programs without a sandbox (bwrap not available or disabled)", file=sys.stderr)

    client = None
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
        "max_turns": args.max_turns,
        "max_tokens": args.max_tokens,
        "sandbox": L.SANDBOX,
        "languages": {lang.key: lang.environment for lang in langs},
    }
    if client is not None:
        sample = Task.load(tasks[0])
        config["prompt_tokens"] = {lang.key: prompt_tokens(client, args.model, lang, sample) for lang in langs}
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
            with log_path.open("a") as f:
                f.write(json.dumps(rec) + "\n")
            cost = f" ${rec['cost_usd']:.3f}" if rec["cost_usd"] else ""
            print(f"{t:18} {lang.key:7} #{trial} {rec['outcome']:12} {rec['tokens']['total']:>8} tok {rec['api_calls']:>2} calls{cost}", file=sys.stderr)
        return rec

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        records = list(pool.map(work, jobs))
    failed = [r for r in records if r["outcome"] == "harness_error"]
    for r in failed:
        print(f"harness error in {r['task']}/{r['lang']}:\n{r['error']}", file=sys.stderr)
    print(f"done: uv run bench/harness/report.py {out}", file=sys.stderr)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
