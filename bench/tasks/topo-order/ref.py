import heapq
import sys

deps = {}
for line in sys.stdin.read().split("\n"):
    if not line.strip():
        continue
    item, _, rest = line.partition(":")
    item = item.strip()
    deps.setdefault(item, set()).update(rest.split())
    for d in rest.split():
        deps.setdefault(d, set())
users = {k: [] for k in deps}
for k, ds in deps.items():
    for d in ds:
        users[d].append(k)
left = {k: len(ds) for k, ds in deps.items()}
ready = [k for k, n in left.items() if n == 0]
heapq.heapify(ready)
order = []
while ready:
    k = heapq.heappop(ready)
    order.append(k)
    for u in users[k]:
        left[u] -= 1
        if left[u] == 0:
            heapq.heappush(ready, u)
print(" ".join(order) if len(order) == len(deps) else "cycle")
