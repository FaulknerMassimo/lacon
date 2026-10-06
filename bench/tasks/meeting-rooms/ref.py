import sys

lines = sys.stdin.read().split("\n")
n = int(lines[0])


def mins(t):
    h, m = t.split(":")
    return int(h) * 60 + int(m)


events = []
for line in lines[1:1 + n]:
    a, b = line.split()
    events.append((mins(a), 1))
    events.append((mins(b), -1))
events.sort()
cur = best = 0
peak = None
for t, d in events:
    cur += d
    if cur > best:
        best, peak = cur, t
print(f"rooms: {best}")
if n:
    print(f"peak: {peak // 60:02d}:{peak % 60:02d}")
