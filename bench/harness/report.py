"""Summarize harness runs.

    uv run bench/harness/report.py bench/results/<run> [more runs...] [--md]

Tokens-to-green for an episode is every token of every API call until a
submission passed: uncached input, cache writes, cache reads and output
(thinking included). Languages are compared on the tasks both solved, as the
geometric mean of the per-task ratios, so a language isn't rewarded for
skipping hard tasks.
"""
from __future__ import annotations

import argparse
import json
import math
import statistics
import sys
from collections import defaultdict
from pathlib import Path

ORDER = ["lacon", "python", "rust", "go"]


def load(paths: list[Path]) -> list[dict]:
    recs = []
    for p in paths:
        f = p / "episodes.jsonl" if p.is_dir() else p
        recs += [json.loads(line) for line in f.read_text().splitlines() if line.strip()]
    return recs


def table(rows: list[list[str]], md: bool) -> str:
    if md:
        out = ["| " + " | ".join(rows[0]) + " |", "|" + "|".join("---" for _ in rows[0]) + "|"]
        out += ["| " + " | ".join(r) + " |" for r in rows[1:]]
        return "\n".join(out)
    widths = [max(len(r[i]) for r in rows) for i in range(len(rows[0]))]
    return "\n".join("  ".join(c.rjust(w) if i else c.ljust(w) for i, (c, w) in enumerate(zip(r, widths))) for r in rows)


def pct(n: int, d: int) -> str:
    return f"{100 * n / d:.0f}%" if d else "-"


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("runs", nargs="+", type=Path)
    ap.add_argument("--md", action="store_true", help="Markdown tables")
    args = ap.parse_args()
    recs = [r for r in load(args.runs) if r["outcome"] not in ("no_reference", "harness_error")]
    if not recs:
        print("no episodes")
        return 1
    by_lang: dict[str, list[dict]] = defaultdict(list)
    for r in recs:
        by_lang[r["lang"]].append(r)
    langs = sorted(by_lang, key=lambda k: ORDER.index(k) if k in ORDER else 99)

    # Runs from before `served_by` was recorded fall back to the requested model.
    served = sorted({m for r in recs for m in (r.get("served_by") or [r.get("model")]) if m})
    if served:
        print(f"models: {', '.join(served)}\n")

    rows = [["language", "episodes", "passed", "1st build ok", "median tokens-to-green", "mean output tok", "mean calls", "mean $/episode"]]
    for lang in langs:
        rs = by_lang[lang]
        green = [r["tokens"]["total"] for r in rs if r["passed"]]
        builds = [r["first_build_ok"] for r in rs if r["first_build_ok"] is not None]
        costs = [r["cost_usd"] for r in rs if r.get("cost_usd") is not None]
        rows.append([
            lang,
            str(len(rs)),
            pct(sum(r["passed"] for r in rs), len(rs)),
            pct(sum(builds), len(builds)),
            f"{statistics.median(green):,.0f}" if green else "-",
            f"{statistics.mean(r['tokens']['output'] for r in rs):,.0f}",
            f"{statistics.mean(r['api_calls'] for r in rs):.1f}",
            f"{statistics.mean(costs):.3f}" if costs else "-",
        ])
    print(table(rows, args.md))

    # Mean tokens-to-green and cost per (task, language) over passing trials.
    # Cost weighs cached reads at their price, a tenth of uncached input.
    green: dict[tuple[str, str], float] = {}
    green_cost: dict[tuple[str, str], float] = {}
    tasks = sorted({r["task"] for r in recs})
    for t in tasks:
        for lang in langs:
            passed = [r for r in by_lang[lang] if r["task"] == t and r["passed"]]
            if passed:
                green[(t, lang)] = statistics.mean(r["tokens"]["total"] for r in passed)
            if passed and all(r.get("cost_usd") for r in passed):
                green_cost[(t, lang)] = statistics.mean(r["cost_usd"] for r in passed)

    if "lacon" in langs and len(langs) > 1:
        print()
        rows = [["Lacon vs", "tasks both solved", "Lacon tokens relative", "Lacon saves", "Lacon cost relative"]]
        for other in langs:
            if other == "lacon":
                continue
            both = [t for t in tasks if (t, "lacon") in green and (t, other) in green and green[(t, other)] > 0]
            if not both:
                rows.append([other, "0", "-", "-", "-"])
                continue
            ratio = math.exp(statistics.mean(math.log(green[(t, "lacon")] / green[(t, other)]) for t in both))
            priced = [t for t in both if (t, "lacon") in green_cost and (t, other) in green_cost]
            cost = math.exp(statistics.mean(math.log(green_cost[(t, "lacon")] / green_cost[(t, other)]) for t in priced)) if priced else None
            rows.append([other, str(len(both)), f"{ratio:.2f}x", f"{1 - ratio:.0%}", f"{cost:.2f}x" if cost else "-"])
        print(table(rows, args.md))
        lacon_builds = [r["first_build_ok"] for r in by_lang["lacon"] if r["first_build_ok"] is not None]
        if lacon_builds:
            print(f"\nLacon first attempts that build: {pct(sum(lacon_builds), len(lacon_builds))} (Phase 0 target: 90%)")

    print()
    rows = [["task", *langs]]
    for t in tasks:
        row = [t]
        for lang in langs:
            rs = [r for r in by_lang[lang] if r["task"] == t]
            if not rs:
                row.append("")
            elif (t, lang) in green:
                fails = sum(not r["passed"] for r in rs)
                row.append(f"{green[(t, lang)]:,.0f}" + (f" ({fails} failed)" if fails else ""))
            else:
                row.append("failed: " + ",".join(sorted({r["outcome"] for r in rs})))
        rows.append(row)
    print(table(rows, args.md))
    return 0


if __name__ == "__main__":
    sys.exit(main())
