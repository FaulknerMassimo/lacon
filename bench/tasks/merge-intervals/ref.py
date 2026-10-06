import sys
lines = sys.stdin.read().split("\n")
n = int(lines[0])
iv = sorted(tuple(map(int, l.split())) for l in lines[1:1 + n])
out = []
for a, b in iv:
    if out and a <= out[-1][1]:
        out[-1][1] = max(out[-1][1], b)
    else:
        out.append([a, b])
for a, b in out:
    print(a, b)
