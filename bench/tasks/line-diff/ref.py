import sys

lines = sys.stdin.read().split("\n")
if lines and lines[-1] == "":
    lines.pop()
sep = lines.index("---")
a, b = lines[:sep], lines[sep + 1:]
n, m = len(a), len(b)
L = [[0] * (m + 1) for _ in range(n + 1)]
for i in range(n - 1, -1, -1):
    for j in range(m - 1, -1, -1):
        L[i][j] = L[i + 1][j + 1] + 1 if a[i] == b[j] else max(L[i + 1][j], L[i][j + 1])
i = j = 0
while i < n and j < m:
    if a[i] == b[j]:
        print("  " + a[i])
        i += 1
        j += 1
    elif L[i + 1][j] >= L[i][j + 1]:
        print("- " + a[i])
        i += 1
    else:
        print("+ " + b[j])
        j += 1
for x in a[i:]:
    print("- " + x)
for x in b[j:]:
    print("+ " + x)
