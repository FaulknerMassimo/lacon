import heapq
import sys

data = sys.stdin.read().split()
n, m, s = int(data[0]), int(data[1]), int(data[2])
adj = [[] for _ in range(n + 1)]
for k in range(m):
    u, v, w = (int(x) for x in data[3 + 3 * k: 6 + 3 * k])
    adj[u].append((v, w))
dist = [-1] * (n + 1)
heap = [(0, s)]
while heap:
    d, u = heapq.heappop(heap)
    if dist[u] != -1:
        continue
    dist[u] = d
    for v, w in adj[u]:
        if dist[v] == -1:
            heapq.heappush(heap, (d + w, v))
print("\n".join(str(d) for d in dist[1:]))
