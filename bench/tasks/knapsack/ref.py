import sys

data = sys.stdin.read().split()
n, cap = int(data[0]), int(data[1])
best = [0] * (cap + 1)
for k in range(n):
    w, v = int(data[2 + 2 * k]), int(data[3 + 2 * k])
    for c in range(cap, w - 1, -1):
        if best[c - w] + v > best[c]:
            best[c] = best[c - w] + v
print(best[cap])
