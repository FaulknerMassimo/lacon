import sys

groups = {}
for line in sys.stdin.read().split("\n"):
    if not line.strip():
        continue
    cat, val = line.split()
    groups.setdefault(cat, []).append(float(val))
for cat in sorted(groups):
    xs = groups[cat]
    print(f"{cat} {len(xs)} {min(xs):.2f} {max(xs):.2f} {sum(xs) / len(xs):.2f}")
