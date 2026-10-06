import json
import sys

WS = " \t\n\r"


class Bad(Exception):
    pass


def parse(s):
    pos = 0

    def ws():
        nonlocal pos
        while pos < len(s) and s[pos] in WS:
            pos += 1

    def expect(c):
        nonlocal pos
        ws()
        if pos >= len(s) or s[pos] != c:
            raise Bad
        pos += 1

    def string():
        nonlocal pos
        expect('"')
        out = []
        while True:
            if pos >= len(s):
                raise Bad
            c = s[pos]
            if c == '"':
                pos += 1
                return "".join(out)
            if c == "\\":
                if pos + 1 < len(s) and s[pos + 1] in '"\\':
                    out.append(s[pos + 1])
                    pos += 2
                    continue
                raise Bad
            if not (" " <= c <= "~"):
                raise Bad
            out.append(c)
            pos += 1

    def value():
        nonlocal pos
        ws()
        if pos >= len(s):
            raise Bad
        c = s[pos]
        if c == "{":
            pos += 1
            obj = {}
            ws()
            if pos < len(s) and s[pos] == "}":
                pos += 1
                return obj
            while True:
                k = string()
                expect(":")
                obj[k] = value()
                ws()
                if pos < len(s) and s[pos] == ",":
                    pos += 1
                    continue
                expect("}")
                return obj
        if c == "[":
            pos += 1
            arr = []
            ws()
            if pos < len(s) and s[pos] == "]":
                pos += 1
                return arr
            while True:
                arr.append(value())
                ws()
                if pos < len(s) and s[pos] == ",":
                    pos += 1
                    continue
                expect("]")
                return arr
        if c == '"':
            return string()
        for word, v in (("true", True), ("false", False), ("null", None)):
            if s.startswith(word, pos):
                pos += len(word)
                return v
        j = pos
        if j < len(s) and s[j] == "-":
            j += 1
        if j < len(s) and s[j] == "0":
            j += 1
        elif j < len(s) and "1" <= s[j] <= "9":
            while j < len(s) and "0" <= s[j] <= "9":
                j += 1
        else:
            raise Bad
        if j < len(s) and "0" <= s[j] <= "9":
            raise Bad
        n = int(s[pos:j])
        pos = j
        return n

    v = value()
    ws()
    if pos != len(s):
        raise Bad
    return v


try:
    print(json.dumps(parse(sys.stdin.read()), indent=2, sort_keys=True))
except Bad:
    print("invalid")
