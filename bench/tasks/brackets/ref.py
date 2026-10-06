import sys
pairs = {")": "(", "]": "[", "}": "{"}
for line in sys.stdin.read().splitlines():
    stack, result = [], "ok"
    for i, c in enumerate(line, 1):
        if c in "([{":
            stack.append(c)
        elif c in pairs:
            if not stack or stack.pop() != pairs[c]:
                result = f"error at {i}"
                break
    else:
        if stack:
            result = "error at end"
    print(result)
