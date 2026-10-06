import sys

VALS = [(1000, "M"), (900, "CM"), (500, "D"), (400, "CD"), (100, "C"), (90, "XC"),
        (50, "L"), (40, "XL"), (10, "X"), (9, "IX"), (5, "V"), (4, "IV"), (1, "I")]
DIG = {"I": 1, "V": 5, "X": 10, "L": 50, "C": 100, "D": 500, "M": 1000}


def to_roman(n):
    out = []
    for v, s in VALS:
        while n >= v:
            out.append(s)
            n -= v
    return "".join(out)


def from_roman(s):
    total = 0
    for i, c in enumerate(s):
        v = DIG[c]
        if i + 1 < len(s) and DIG[s[i + 1]] > v:
            total -= v
        else:
            total += v
    return total


for line in sys.stdin.read().splitlines():
    if line and all("0" <= c <= "9" for c in line):
        n = int(line)
        print(to_roman(n) if 1 <= n <= 3999 else "invalid")
    elif line and all(c in DIG for c in line):
        n = from_roman(line)
        print(n if 1 <= n <= 3999 and to_roman(n) == line else "invalid")
    else:
        print("invalid")
