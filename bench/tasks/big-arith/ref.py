import sys

for line in sys.stdin.read().split("\n"):
    if not line:
        continue
    a, op, b = line.split(" ")
    a, b = int(a), int(b)
    print(a + b if op == "+" else a - b if op == "-" else a * b)
