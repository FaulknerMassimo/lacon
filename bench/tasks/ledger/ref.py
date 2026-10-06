import re
import sys

AMOUNT = re.compile(r"^(\d+)(?:\.(\d{1,2}))?$")


def cents(s):
    m = AMOUNT.match(s)
    if not m:
        return None
    return int(m[1]) * 100 + int((m[2] or "0").ljust(2, "0"))


acct = {}
for n, line in enumerate(sys.stdin.read().split("\n"), 1):
    p = line.split()
    if not p:
        continue
    arity = {"open": 3, "deposit": 3, "withdraw": 3, "transfer": 4}
    if p[0] not in arity or len(p) != arity[p[0]]:
        print(f"line {n}: bad command")
        continue
    amt = cents(p[-1])
    if amt is None or (amt == 0 and p[0] != "open"):
        print(f"line {n}: bad amount")
        continue
    cmd, names = p[0], p[1:-1]
    if cmd == "open":
        if names[0] in acct:
            print(f"line {n}: account exists")
        else:
            acct[names[0]] = amt
        continue
    missing = [x for x in names if x not in acct]
    if missing:
        print(f"line {n}: no account {missing[0]}")
        continue
    if cmd == "deposit":
        acct[names[0]] += amt
    elif cmd == "withdraw":
        if acct[names[0]] < amt:
            print(f"line {n}: insufficient funds")
        else:
            acct[names[0]] -= amt
    else:
        a, b = names
        if acct[a] < amt:
            print(f"line {n}: insufficient funds")
        elif a == b:
            print(f"line {n}: same account")
        else:
            acct[a] -= amt
            acct[b] += amt
for name in sorted(acct):
    c = acct[name]
    print(f"{name} {c // 100}.{c % 100:02d}")
