# /// script
# requires-python = ">=3.10"
# dependencies = ["anthropic", "tiktoken"]
# ///
"""Count tokens for the same program written in Rust, Go, Python and Lacon,
and for the Lacon primer.

Run with: uv run bench/tokens/count.py [--model claude-opus-5]

Counts come from Claude's token-counting endpoint when API credentials are
available. Without them the script falls back to `o200k_base`, which is not
Claude's tokenizer, so read those numbers as relative between languages only.
"""
import argparse
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


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--model", default="claude-opus-5")
    args = ap.parse_args()
    try:
        count = claude_counter(args.model)
        source = f"Claude tokenizer ({args.model})"
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
