import sys

data, stack = {}, []
for line in sys.stdin.read().split("\n"):
    p = line.split()
    if not p:
        continue
    cmd = p[0]
    if cmd == "SET":
        if stack:
            stack[-1].append((p[1], data.get(p[1])))
        data[p[1]] = p[2]
    elif cmd == "GET":
        print(data.get(p[1], "NULL"))
    elif cmd == "DELETE":
        if p[1] in data:
            if stack:
                stack[-1].append((p[1], data[p[1]]))
            del data[p[1]]
    elif cmd == "COUNT":
        print(sum(1 for v in data.values() if v == p[1]))
    elif cmd == "BEGIN":
        stack.append([])
    elif cmd == "ROLLBACK":
        if not stack:
            print("NO TRANSACTION")
            continue
        for k, old in reversed(stack.pop()):
            if old is None:
                data.pop(k, None)
            else:
                data[k] = old
    elif cmd == "COMMIT":
        if not stack:
            print("NO TRANSACTION")
        stack = []
