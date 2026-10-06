# /// script
# requires-python = ">=3.9"
# dependencies = ["tiktoken"]
# ///
"""Count tokens for the same program written in Rust, Go, Python and Lacon.

Run with: uv run bench/tokens/count.py

o200k_base is a stand-in. Claude's tokenizer is not published as a library,
so read the numbers as relative between languages, not as absolute counts.
"""
from pathlib import Path

import tiktoken

HERE = Path(__file__).parent
enc = tiktoken.get_encoding("o200k_base")

counts = {p.name: len(enc.encode(p.read_text())) for p in HERE.glob("users.*")}
lacon = counts["users.lc"]

for name, n in sorted(counts.items(), key=lambda kv: -kv[1]):
    saving = "" if name == "users.lc" else f"  lacon saves {1 - lacon / n:.0%}"
    print(f"{name:10} {n:4}{saving}")
