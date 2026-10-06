import sys

counts = {}
for line in sys.stdin.buffer.read().decode("utf-8").split("\n"):
    if line:
        counts[line] = counts.get(line, 0) + 1
if counts:
    w = len(str(max(counts.values())))
    for line, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0].encode())):
        print(f"{n:>{w}} {line}")
