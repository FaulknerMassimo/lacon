# /// script
# requires-python = ">=3.10"
# dependencies = ["anthropic", "tiktoken"]
# ///
"""Count tokens for the same program written in Rust, Go, Python and Lacon,
and for the Lacon primer.

Run with: uv run bench/tokens/count.py [--model claude-opus-5-5] [--claude-code]

Counts come from Claude's token-counting endpoint when API credentials are
available. `--claude-code` counts through `claude -p` on a Claude Code login
instead (one short model call per file). Without either, the script falls back
to `o200k_base`, which is not Claude's tokenizer, so read those numbers as
relative between languages only.
"""
import argparse
import json
import os
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).parent
PRIMER = HERE.parent.parent / "docs" / "primer.md"


def claude_counter(model: str):
    import anthropic

    client = anthropic.Anthropic()

    def count(text: str) -> int:
        return client.messages.count_tokens(model=model, messages=[{"role": "user", "content": text}]).input_tokens

    # The endpoint counts a whole request; subtract the message framing.
    framing = count("x") - 1
    return lambda text: count(text) - framing


def claude_code_counter(model: str):
    """Sends the text as the system prompt of a one-line `claude -p` exchange
    with no tools, and counts the input tokens beyond a near-empty baseline."""
    # Without the parent session's variables, so this also works inside Claude Code.
    env = {k: v for k, v in os.environ.items() if not (k.startswith("CLAUDE_CODE_") or k in ("CLAUDECODE", "CLAUDE_PID", "CLAUDE_EFFORT"))}

    def total(system: str) -> int:
        cmd = ["claude", "-p", "Reply with OK.", "--model", model, "--system-prompt", system, "--tools", "", "--effort", "low",
               "--output-format", "json", "--no-session-persistence", "--safe-mode", "--strict-mcp-config"]
        r = subprocess.run(cmd, capture_output=True, text=True, env=env, cwd=HERE, check=True)
        u = json.loads(r.stdout)["usage"]
        return u["input_tokens"] + u["cache_creation_input_tokens"] + u["cache_read_input_tokens"]

    base = total("x") - 1
    return lambda text: total(text) - base


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="claude-opus-5-5")
    ap.add_argument("--claude-code", action="store_true", help="count through `claude -p` (no API key needed)")
    args = ap.parse_args()
    try:
        count = claude_code_counter(args.model) if args.claude_code else claude_counter(args.model)
        source = f"Claude tokenizer ({args.model}{', via Claude Code' if args.claude_code else ''})"
    except Exception as e:
        import tiktoken

        enc = tiktoken.get_encoding("o200k_base")
        count = lambda text: len(enc.encode(text))
        source = "o200k_base (approximate: no Claude API credentials)"
        print(f"note: falling back to o200k_base: {type(e).__name__}: {str(e)[:120]}", file=sys.stderr)

    counts = {p.name: count(p.read_text()) for p in sorted(HERE.glob("users.*"))}
    lacon = counts["users.lc"]
    print(f"tokenizer: {source}")
    for name, n in sorted(counts.items(), key=lambda kv: -kv[1]):
        saving = "" if name == "users.lc" else f"  lacon saves {1 - lacon / n:.0%}"
        print(f"{name:10} {n:4}{saving}")
    print(f"primer     {count(PRIMER.read_text()):4}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
