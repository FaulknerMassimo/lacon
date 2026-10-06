import sys
def ev(line):
    st = []
    for t in line.split():
        if t in "+-*/" and len(t) == 1:
            if len(st) < 2:
                return "error"
            b, a = st.pop(), st.pop()
            if t == "+": st.append(a + b)
            elif t == "-": st.append(a - b)
            elif t == "*": st.append(a * b)
            else:
                if b == 0:
                    return "error"
                q = abs(a) // abs(b)
                st.append(q if (a >= 0) == (b >= 0) else -q)
        else:
            try:
                st.append(int(t))
            except ValueError:
                return "error"
    return str(st[0]) if len(st) == 1 else "error"
for line in sys.stdin.read().splitlines():
    print(ev(line))
