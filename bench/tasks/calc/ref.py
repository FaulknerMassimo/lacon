import sys


def tokenize(s):
    out, i = [], 0
    while i < len(s):
        c = s[i]
        if c == " ":
            i += 1
        elif c.isdigit() and c.isascii():
            j = i
            while j < len(s) and s[j].isdigit() and s[j].isascii():
                j += 1
            out.append(int(s[i:j]))
            i = j
        elif c in "+-*/()":
            out.append(c)
            i += 1
        else:
            raise ValueError
    return out


def evaluate(s):
    toks = tokenize(s)
    pos = 0

    def peek():
        return toks[pos] if pos < len(toks) else None

    def take():
        nonlocal pos
        t = peek()
        if t is None:
            raise ValueError
        pos += 1
        return t

    def expr():
        v = term()
        while peek() in ("+", "-"):
            if take() == "+":
                v += term()
            else:
                v -= term()
        return v

    def term():
        v = unary()
        while peek() in ("*", "/"):
            if take() == "*":
                v *= unary()
            else:
                d = unary()
                if d == 0:
                    raise ValueError
                q = abs(v) // abs(d)
                v = q if (v >= 0) == (d >= 0) else -q
        return v

    def unary():
        if peek() == "-":
            take()
            return -unary()
        return primary()

    def primary():
        t = take()
        if isinstance(t, int):
            return t
        if t == "(":
            v = expr()
            if take() != ")":
                raise ValueError
            return v
        raise ValueError

    v = expr()
    if pos != len(toks):
        raise ValueError
    return v


for line in sys.stdin.read().splitlines():
    try:
        print(evaluate(line))
    except ValueError:
        print("error")
