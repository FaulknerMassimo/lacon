import re
import sys

NUM = re.compile(r"^-?\d+(\.\d+)?$")
rows = [l.split() for l in sys.stdin.read().split("\n") if l.strip()]
if rows:
    ncol = max(len(r) for r in rows)
    rows = [r + [""] * (ncol - len(r)) for r in rows]
    widths = [max(len(r[c]) for r in rows) for c in range(ncol)]
    numeric = [all(NUM.match(r[c]) or r[c] == "" for r in rows[1:]) for c in range(ncol)]
    for r in rows:
        cells = [r[c].rjust(widths[c]) if numeric[c] else r[c].ljust(widths[c]) for c in range(ncol)]
        print("  ".join(cells).rstrip())
