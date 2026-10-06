import sys


def fields(line):
    out, cur, i, quoted = [], [], 0, False
    while i < len(line):
        c = line[i]
        if quoted:
            if c == '"':
                if i + 1 < len(line) and line[i + 1] == '"':
                    cur.append('"')
                    i += 1
                else:
                    quoted = False
            else:
                cur.append(c)
        elif c == '"':
            quoted = True
        elif c == ",":
            out.append("".join(cur))
            cur = []
        else:
            cur.append(c)
        i += 1
    out.append("".join(cur))
    return out


lines = [l for l in sys.stdin.read().split("\n")]
name = lines[0]
rows = [fields(l) for l in lines[1:] if l.strip()]
header = rows[0]
if name not in header:
    print(f"no column {name}")
else:
    k = header.index(name)
    for r in rows[1:]:
        print(r[k] if k < len(r) else "")
