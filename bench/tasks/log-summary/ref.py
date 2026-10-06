import re
import sys

LINE = re.compile(r'^(\S+) (\S+) (\S+) \[([^\]]+)\] "(\S+) (\S+) (\S+)" (\d{3}) (\d+|-)$')
reqs = total = bad = 0
status, paths = {}, {}
for line in sys.stdin.read().split("\n"):
    if not line.strip():
        continue
    m = LINE.match(line)
    if not m:
        bad += 1
        continue
    reqs += 1
    total += 0 if m[9] == "-" else int(m[9])
    cls = m[8][0]
    status[cls] = status.get(cls, 0) + 1
    path = m[6].split("?")[0]
    paths[path] = paths.get(path, 0) + 1
print(f"requests: {reqs}")
print(f"bytes: {total}")
for c in sorted(status):
    print(f"status {c}xx: {status[c]}")
print("top paths:")
for p, n in sorted(paths.items(), key=lambda kv: (-kv[1], kv[0]))[:3]:
    print(f"  {p} {n}")
print(f"malformed: {bad}")
