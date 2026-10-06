import sys

groups = {}
for w in set(sys.stdin.read().lower().split()):
    groups.setdefault("".join(sorted(w)), []).append(w)
out = [sorted(g) for g in groups.values() if len(g) >= 2]
for g in sorted(out, key=lambda g: (-len(g), g[0])):
    print(" ".join(g))
