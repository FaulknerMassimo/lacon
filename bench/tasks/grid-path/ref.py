import sys
from collections import deque
lines = sys.stdin.read().split("\n")
r, c = map(int, lines[0].split())
grid = lines[1:1 + r]
start = next((i, j) for i in range(r) for j in range(c) if grid[i][j] == "S")
dist = {start: 0}
q = deque([start])
ans = -1
while q:
    i, j = q.popleft()
    if grid[i][j] == "E":
        ans = dist[(i, j)]
        break
    for di, dj in ((1, 0), (-1, 0), (0, 1), (0, -1)):
        ni, nj = i + di, j + dj
        if 0 <= ni < r and 0 <= nj < c and grid[ni][nj] != "#" and (ni, nj) not in dist:
            dist[(ni, nj)] = dist[(i, j)] + 1
            q.append((ni, nj))
print(ans)
