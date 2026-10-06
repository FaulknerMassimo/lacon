import sys

for line in sys.stdin.read().split("\n"):
    if not line.strip():
        continue
    cwd, path = line.split(" ", 1)
    if path == "~" or path.startswith("~/"):
        path = "/home/user" + path[1:]
    if not path.startswith("/"):
        path = cwd + "/" + path
    out = []
    for part in path.split("/"):
        if part in ("", "."):
            continue
        if part == "..":
            if out:
                out.pop()
        else:
            out.append(part)
    print("/" + "/".join(out))
