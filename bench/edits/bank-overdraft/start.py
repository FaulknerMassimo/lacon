import sys

# Arguments each command takes after its name: (fewest, most).
ARITY = {
    "open": (1, 2),
    "deposit": (2, 2),
    "withdraw": (2, 2),
    "transfer": (3, 3),
    "close": (1, 1),
    "interest": (1, 1),
    "history": (1, 1),
    "balance": (1, 1),
}


class Account:
    def __init__(self, name, balance):
        self.name = name
        self.balance = balance  # in cents
        self.history = []  # (line, what, amount in cents)
        self.closed = False


def parse_cents(s):
    """Digits, optionally followed by a point and one or two digits, in cents."""
    whole, dot, frac = s.partition(".")
    if not whole.isdigit():
        return None
    if dot and (len(frac) not in (1, 2) or not frac.isdigit()):
        return None
    return int(whole) * 100 + int(frac.ljust(2, "0") if dot else "0")


def positive(s):
    """An amount in cents that must not be zero."""
    c = parse_cents(s)
    return c if c != 0 else None


def fmt_cents(c):
    return f"{c // 100}.{c % 100:02d}"


def record(acct, n, what, amount):
    acct.history.append((n, what, amount))


def check_open(accounts, names):
    """Why the first of names is not an open account, or None."""
    for name in names:
        if name not in accounts:
            return f"no account {name}"
        if accounts[name].closed:
            return f"account closed {name}"
    return None


def cmd_open(accounts, n, args):
    name = args[0]
    amount = parse_cents(args[1]) if len(args) == 2 else 0
    if amount is None:
        return "bad amount"
    if name in accounts:
        return "account exists"
    accounts[name] = Account(name, amount)
    record(accounts[name], n, "open", amount)
    return None


def cmd_deposit(accounts, n, args):
    amount = positive(args[1])
    if amount is None:
        return "bad amount"
    why = check_open(accounts, args[:1])
    if why is not None:
        return why
    acct = accounts[args[0]]
    acct.balance += amount
    record(acct, n, "deposit", amount)
    return None


def cmd_withdraw(accounts, n, args):
    amount = positive(args[1])
    if amount is None:
        return "bad amount"
    why = check_open(accounts, args[:1])
    if why is not None:
        return why
    acct = accounts[args[0]]
    if acct.balance < amount:
        return "insufficient funds"
    acct.balance -= amount
    record(acct, n, "withdraw", amount)
    return None


def cmd_transfer(accounts, n, args):
    src, dst = args[0], args[1]
    amount = positive(args[2])
    if amount is None:
        return "bad amount"
    why = check_open(accounts, [src, dst])
    if why is not None:
        return why
    if src == dst:
        return "same account"
    if accounts[src].balance < amount:
        return "insufficient funds"
    accounts[src].balance -= amount
    accounts[dst].balance += amount
    record(accounts[src], n, f"transfer to {dst}", amount)
    record(accounts[dst], n, f"transfer from {src}", amount)
    return None


def cmd_close(accounts, n, args):
    why = check_open(accounts, args)
    if why is not None:
        return why
    acct = accounts[args[0]]
    if acct.balance != 0:
        return "balance not zero"
    acct.closed = True
    record(acct, n, "close", 0)
    return None


def cmd_interest(accounts, n, args):
    """Adds RATE percent to every open account with a positive balance,
    rounded down to the cent."""
    rate = positive(args[0])
    if rate is None:
        return "bad amount"
    for name in sorted(accounts):
        acct = accounts[name]
        if acct.closed or acct.balance <= 0:
            continue
        gain = acct.balance * rate // 10000
        if gain > 0:
            acct.balance += gain
            record(acct, n, "interest", gain)
    return None


def cmd_history(accounts, n, args):
    name = args[0]
    if name not in accounts:
        return f"no account {name}"
    print(f"{name}:")
    for line, what, amount in accounts[name].history:
        print(f"  {line} {what} {fmt_cents(amount)}")
    return None


def cmd_balance(accounts, n, args):
    why = check_open(accounts, args)
    if why is not None:
        return why
    print(f"{args[0]} {fmt_cents(accounts[args[0]].balance)}")
    return None


def run_line(accounts, n, line):
    """Carries out one command; returns why it could not, or None."""
    fields = line.split()
    if not fields:
        return None
    cmd, args = fields[0], fields[1:]
    if cmd not in ARITY:
        return "bad command"
    lo, hi = ARITY[cmd]
    if not lo <= len(args) <= hi:
        return "bad command"
    if cmd == "open":
        return cmd_open(accounts, n, args)
    if cmd == "deposit":
        return cmd_deposit(accounts, n, args)
    if cmd == "withdraw":
        return cmd_withdraw(accounts, n, args)
    if cmd == "transfer":
        return cmd_transfer(accounts, n, args)
    if cmd == "close":
        return cmd_close(accounts, n, args)
    if cmd == "interest":
        return cmd_interest(accounts, n, args)
    if cmd == "history":
        return cmd_history(accounts, n, args)
    return cmd_balance(accounts, n, args)


def print_summary(accounts):
    total = 0
    for name in sorted(accounts):
        acct = accounts[name]
        if acct.closed:
            continue
        print(f"{name} {fmt_cents(acct.balance)}")
        total += acct.balance
    print(f"total {fmt_cents(total)}")


def main():
    accounts = {}
    for n, line in enumerate(sys.stdin.read().splitlines(), 1):
        why = run_line(accounts, n, line)
        if why is not None:
            print(f"line {n}: {why}")
    print_summary(accounts)


main()
