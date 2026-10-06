import sys

data = sys.stdin.buffer.read().decode("utf-8")
print(data.count("\n"), len(data.split()), len(data))
