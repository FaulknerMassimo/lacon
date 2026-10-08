import sys

OPS = "+-*/%(),="


class CalcError(Exception):
    pass


def tokenize(line):
    """Numbers, names and one-character operators, as (kind, text) pairs."""
    toks = []
    i = 0
    while i < len(line):
        c = line[i]
        if c == " ":
            i += 1
        elif c.isdigit():
            j = i
            while j < len(line) and line[j].isdigit():
                j += 1
            toks.append(("num", line[i:j]))
            i = j
        elif c.isalpha() or c == "_":
            j = i
            while j < len(line) and (line[j].isalnum() or line[j] == "_"):
                j += 1
            toks.append(("name", line[i:j]))
            i = j
        elif c in OPS:
            toks.append(("op", c))
            i += 1
        else:
            raise CalcError(f"unexpected character '{c}'")
    return toks


class Parser:
    def __init__(self, toks):
        self.toks = toks
        self.pos = 0

    def peek(self):
        """The next token's text, or "" at the end."""
        return self.toks[self.pos][1] if self.pos < len(self.toks) else ""

    def take(self):
        if self.pos >= len(self.toks):
            raise CalcError("unexpected end")
        self.pos += 1
        return self.toks[self.pos - 1]

    def expect(self, text):
        _, t = self.take()
        if t != text:
            raise CalcError(f"expected '{text}' but found '{t}'")


# Expressions are tuples: ("num", value), ("var", name), ("neg", e),
# ("bin", op, left, right) and ("call", name, args).


def parse_sum(p):
    e = parse_product(p)
    while p.peek() in ("+", "-"):
        op = p.take()[1]
        e = ("bin", op, e, parse_product(p))
    return e


def parse_product(p):
    e = parse_unary(p)
    while p.peek() in ("*", "/", "%"):
        op = p.take()[1]
        e = ("bin", op, e, parse_unary(p))
    return e


def parse_unary(p):
    if p.peek() == "-":
        p.take()
        return ("neg", parse_unary(p))
    return parse_primary(p)


def parse_primary(p):
    kind, t = p.take()
    if kind == "num":
        return ("num", int(t))
    if kind == "name":
        if p.peek() == "(":
            p.take()
            return ("call", t, parse_args(p))
        return ("var", t)
    if t == "(":
        e = parse_sum(p)
        p.expect(")")
        return e
    raise CalcError(f"unexpected '{t}'")


def parse_args(p):
    """A call's arguments, after its '(' and through its ')'."""
    args = []
    if p.peek() == ")":
        p.take()
        return args
    while True:
        args.append(parse_sum(p))
        _, t = p.take()
        if t == ")":
            return args
        if t != ",":
            raise CalcError(f"expected ',' or ')' but found '{t}'")


def parse_whole(p):
    e = parse_sum(p)
    if p.pos < len(p.toks):
        raise CalcError(f"unexpected '{p.peek()}'")
    return e


def evaluate(e, env):
    kind = e[0]
    if kind == "num":
        return e[1]
    if kind == "var":
        if e[1] not in env:
            raise CalcError(f"undefined variable {e[1]}")
        return env[e[1]]
    if kind == "neg":
        return -evaluate(e[1], env)
    if kind == "call":
        return call(e[1], [evaluate(a, env) for a in e[2]])
    return binary(e[1], evaluate(e[2], env), evaluate(e[3], env))


def binary(op, a, b):
    if op == "+":
        return a + b
    if op == "-":
        return a - b
    if op == "*":
        return a * b
    if b == 0:
        raise CalcError("division by zero")
    # `/` and `%` truncate toward zero.
    q = abs(a) // abs(b)
    if (a < 0) != (b < 0):
        q = -q
    if op == "/":
        return q
    return a - b * q


def need(name, args, n):
    if len(args) != n:
        s = "" if n == 1 else "s"
        raise CalcError(f"{name} takes {n} argument{s}")


def call(name, args):
    if name == "abs":
        need(name, args, 1)
        return abs(args[0])
    if name == "min" or name == "max":
        if not args:
            raise CalcError(f"{name} needs at least one argument")
        return min(args) if name == "min" else max(args)
    if name == "gcd":
        need(name, args, 2)
        a, b = abs(args[0]), abs(args[1])
        while b != 0:
            a, b = b, a % b
        return a
    raise CalcError(f"unknown function {name}")


def run_line(line, env):
    """Runs one line; returns the lines to print."""
    toks = tokenize(line)
    if not toks:
        return []
    if toks[0] == ("name", "let"):
        if len(toks) < 3 or toks[1][0] != "name" or toks[2][1] != "=":
            raise CalcError("bad let")
        value = evaluate(parse_whole(Parser(toks[3:])), env)
        env[toks[1][1]] = value
        return []
    if toks == [("name", "vars")]:
        return [f"{k} = {v}" for k, v in sorted(env.items())]
    return [str(evaluate(parse_whole(Parser(toks)), env))]


def main():
    env = {}
    for n, line in enumerate(sys.stdin.read().splitlines(), 1):
        code = line.split("#", 1)[0]
        try:
            out = run_line(code, env)
        except CalcError as e:
            print(f"line {n}: {e}")
            continue
        for s in out:
            print(s)


main()
