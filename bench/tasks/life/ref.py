import sys

lines = sys.stdin.read().split("\n")
g = int(lines[0])
grid = [list(r) for r in lines[1:] if r]
h, w = len(grid), len(grid[0])
for _ in range(g):
    nxt = []
    for r in range(h):
        row = []
        for c in range(w):
            n = sum(grid[r + dr][c + dc] == "#" for dr in (-1, 0, 1) for dc in (-1, 0, 1)
                    if (dr or dc) and 0 <= r + dr < h and 0 <= c + dc < w)
            row.append("#" if n == 3 or (n == 2 and grid[r][c] == "#") else ".")
        nxt.append(row)
    grid = nxt
print("\n".join("".join(r) for r in grid))
