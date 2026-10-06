import re, sys
lines = sys.stdin.read().split("\n")
n = int(lines[0])
counts = {}
for w in re.findall(r"[a-z]+", "\n".join(lines[1:]).lower()):
    counts[w] = counts.get(w, 0) + 1
for w, c in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:n]:
    print(w, c)
