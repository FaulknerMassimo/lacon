"""A small SQL database. Reads statements separated by semicolons from
standard input and prints each one's result."""
import copy
import sys
from dataclasses import dataclass, field

KEYWORDS = {
    "add", "alter", "and", "as", "asc", "begin", "between", "by", "column",
    "commit", "create", "delete", "desc", "describe", "drop", "from", "in",
    "insert", "int", "into", "is", "key", "like", "limit", "not", "null", "or",
    "order", "primary", "rollback", "select", "set", "show", "table", "tables",
    "text", "update", "values", "where",
}
AGGREGATES = {"count", "sum", "min", "max", "avg"}
# Scalar functions and how many arguments each takes (None: one or more).
FUNCTIONS = {"upper": 1, "lower": 1, "length": 1, "abs": 1, "coalesce": None}
COMPARISONS = ("=", "!=", "<", "<=", ">", ">=")


class SqlError(Exception):
    pass


# ----- reading statements -----


def split_statements(src):
    """The statements in src: split at semicolons outside strings, with --
    comments removed. Text after the last semicolon is a statement too."""
    stmts = []
    cur = []
    quoted = False
    i = 0
    while i < len(src):
        c = src[i]
        if quoted:
            cur.append(c)
            if c == "'":
                quoted = False
        elif c == "'":
            quoted = True
            cur.append(c)
        elif src.startswith("--", i):
            while i < len(src) and src[i] != "\n":
                i += 1
            continue
        elif c == ";":
            stmts.append("".join(cur))
            cur = []
        else:
            cur.append(c)
        i += 1
    stmts.append("".join(cur))
    return [s for s in stmts if s.strip()]


def tokenize(src):
    """A statement's tokens as (kind, text) pairs, kind being num, str, name,
    kw or op. Names and keywords are lowercased."""
    toks = []
    i = 0
    while i < len(src):
        c = src[i]
        if c.isspace():
            i += 1
        elif c.isdigit():
            j = i
            while j < len(src) and src[j].isdigit():
                j += 1
            toks.append(("num", src[i:j]))
            i = j
        elif c.isalpha() or c == "_":
            j = i
            while j < len(src) and (src[j].isalnum() or src[j] == "_"):
                j += 1
            word = src[i:j].lower()
            toks.append(("kw" if word in KEYWORDS else "name", word))
            i = j
        elif c == "'":
            chars = []
            j = i + 1
            while True:
                if j >= len(src):
                    raise SqlError("unterminated string")
                if src[j] == "'":
                    if src.startswith("''", j):
                        chars.append("'")
                        j += 2
                        continue
                    break
                chars.append(src[j])
                j += 1
            toks.append(("str", "".join(chars)))
            i = j + 1
        elif src[i : i + 2] in ("<=", ">=", "!=", "<>", "||"):
            toks.append(("op", src[i : i + 2]))
            i += 2
        elif c in "(),*+-/%=<>":
            toks.append(("op", c))
            i += 1
        else:
            raise SqlError(f"unexpected character '{c}'")
    return toks


# ----- syntax -----

# Expressions are tuples:
#   ("lit", value)             a number, a string or None for NULL
#   ("col", name)
#   ("neg", e), ("not", e)
#   ("bin", op, left, right)   arithmetic, comparison, ||, and, or
#   ("isnull", e, negated)     e IS NULL, or e IS NOT NULL when negated
#   ("like", e, pattern)
#   ("in", e, [items])
#   ("between", e, low, high)
#   ("call", name, [args])     a scalar function
#   ("agg", name, arg)         an aggregate; arg is None for count(*)


@dataclass
class Column:
    name: str
    type: str  # "INT" or "TEXT"
    not_null: bool = False
    primary: bool = False


@dataclass
class CreateTable:
    name: str
    columns: list


@dataclass
class DropTable:
    name: str


@dataclass
class AlterAdd:
    table: str
    column: Column


@dataclass
class Insert:
    table: str
    columns: list | None  # None: every column, in order
    rows: list  # a list of expressions per row


@dataclass
class Select:
    items: list  # (expression, alias or None); the expression is None for *
    table: str | None
    where: tuple | None
    order: list  # (expression, descending)
    limit: int | None


@dataclass
class Update:
    table: str
    assignments: list  # (column, expression)
    where: tuple | None


@dataclass
class Delete:
    table: str
    where: tuple | None


@dataclass
class Simple:
    """A statement that is only its keywords: SHOW TABLES, BEGIN, COMMIT or
    ROLLBACK."""

    what: str


@dataclass
class Describe:
    table: str


class Parser:
    def __init__(self, toks):
        self.toks = toks
        self.pos = 0

    def peek(self, ahead=0):
        i = self.pos + ahead
        return self.toks[i] if i < len(self.toks) else ("end", "")

    def next(self):
        tok = self.peek()
        if tok[0] == "end":
            raise SqlError("unexpected end of statement")
        self.pos += 1
        return tok

    def at(self, text, ahead=0):
        """Whether a token is the keyword or operator text."""
        kind, t = self.peek(ahead)
        return kind in ("kw", "op") and t == text

    def accept(self, text):
        if self.at(text):
            self.pos += 1
            return True
        return False

    def expect(self, text):
        if not self.accept(text):
            self.fail()

    def fail(self):
        kind, t = self.peek()
        if kind == "end":
            raise SqlError("unexpected end of statement")
        raise SqlError(f"syntax error at '{t}'")

    def name(self):
        if self.peek()[0] != "name":
            self.fail()
        return self.next()[1]

    def names(self):
        """Names separated by commas, through a closing parenthesis."""
        names = [self.name()]
        while self.accept(","):
            names.append(self.name())
        self.expect(")")
        return names

    def done(self):
        if self.pos < len(self.toks):
            self.fail()

    # Expressions, loosest-binding first.

    def expr(self):
        e = self.conjunction()
        while self.accept("or"):
            e = ("bin", "or", e, self.conjunction())
        return e

    def conjunction(self):
        e = self.negation()
        while self.accept("and"):
            e = ("bin", "and", e, self.negation())
        return e

    def negation(self):
        if self.accept("not"):
            return ("not", self.negation())
        return self.comparison()

    def comparison(self):
        e = self.additive()
        if self.accept("is"):
            negated = self.accept("not")
            self.expect("null")
            return ("isnull", e, negated)
        negated = self.at("not") and any(self.at(k, 1) for k in ("like", "in", "between"))
        if negated:
            self.next()
        if self.accept("like"):
            e = ("like", e, self.additive())
        elif self.accept("in"):
            self.expect("(")
            items = [self.expr()]
            while self.accept(","):
                items.append(self.expr())
            self.expect(")")
            e = ("in", e, items)
        elif self.accept("between"):
            low = self.additive()
            self.expect("and")
            e = ("between", e, low, self.additive())
        else:
            kind, t = self.peek()
            if kind == "op" and t in COMPARISONS + ("<>",):
                self.next()
                return ("bin", "!=" if t == "<>" else t, e, self.additive())
            return e
        return ("not", e) if negated else e

    def additive(self):
        e = self.multiplicative()
        while self.peek()[0] == "op" and self.peek()[1] in ("+", "-", "||"):
            op = self.next()[1]
            e = ("bin", op, e, self.multiplicative())
        return e

    def multiplicative(self):
        e = self.unary()
        while self.peek()[0] == "op" and self.peek()[1] in ("*", "/", "%"):
            op = self.next()[1]
            e = ("bin", op, e, self.unary())
        return e

    def unary(self):
        if self.accept("-"):
            return ("neg", self.unary())
        return self.primary()

    def primary(self):
        kind, t = self.peek()
        if kind == "num":
            self.next()
            return ("lit", int(t))
        if kind == "str":
            self.next()
            return ("lit", t)
        if self.accept("null"):
            return ("lit", None)
        if self.accept("("):
            e = self.expr()
            self.expect(")")
            return e
        if kind == "name":
            self.next()
            if self.accept("("):
                return self.call(t)
            return ("col", t)
        self.fail()

    def call(self, name):
        """A function call, after its opening parenthesis."""
        if name in AGGREGATES:
            if name == "count" and self.accept("*"):
                self.expect(")")
                return ("agg", name, None)
            arg = self.expr()
            self.expect(")")
            return ("agg", name, arg)
        if name not in FUNCTIONS:
            raise SqlError(f"no such function {name}")
        args = []
        if not self.accept(")"):
            args.append(self.expr())
            while self.accept(","):
                args.append(self.expr())
            self.expect(")")
        want = FUNCTIONS[name]
        if want is None and not args:
            raise SqlError(f"{name}() needs at least one argument")
        if want is not None and len(args) != want:
            raise SqlError(f"{name}() takes {want} argument{'' if want == 1 else 's'}")
        return ("call", name, args)


def parse_statement(toks):
    p = Parser(toks)
    if p.accept("select"):
        stmt = parse_select(p)
    elif p.accept("insert"):
        stmt = parse_insert(p)
    elif p.accept("update"):
        stmt = parse_update(p)
    elif p.accept("delete"):
        p.expect("from")
        table = p.name()
        stmt = Delete(table, parse_where(p))
    elif p.accept("create"):
        p.expect("table")
        stmt = parse_create(p)
    elif p.accept("drop"):
        p.expect("table")
        stmt = DropTable(p.name())
    elif p.accept("alter"):
        p.expect("table")
        table = p.name()
        p.expect("add")
        p.accept("column")
        stmt = AlterAdd(table, parse_column(p))
    elif p.accept("show"):
        p.expect("tables")
        stmt = Simple("show")
    elif p.accept("describe"):
        stmt = Describe(p.name())
    elif p.at("begin") or p.at("commit") or p.at("rollback"):
        stmt = Simple(p.next()[1])
    else:
        p.fail()
    p.done()
    return stmt


def parse_where(p):
    return p.expr() if p.accept("where") else None


def parse_select(p):
    items = []
    while True:
        if p.accept("*"):
            items.append((None, None))
        else:
            e = p.expr()
            items.append((e, p.name() if p.accept("as") else None))
        if not p.accept(","):
            break
    table = p.name() if p.accept("from") else None
    where = parse_where(p)
    order = []
    if p.accept("order"):
        p.expect("by")
        while True:
            e = p.expr()
            desc = p.accept("desc")
            if not desc:
                p.accept("asc")
            order.append((e, desc))
            if not p.accept(","):
                break
    limit = None
    if p.accept("limit"):
        if p.peek()[0] != "num":
            p.fail()
        limit = int(p.next()[1])
    return Select(items, table, where, order, limit)


def parse_insert(p):
    p.expect("into")
    table = p.name()
    columns = p.names() if p.accept("(") else None
    p.expect("values")
    rows = []
    while True:
        p.expect("(")
        values = [p.expr()]
        while p.accept(","):
            values.append(p.expr())
        p.expect(")")
        rows.append(values)
        if not p.accept(","):
            break
    return Insert(table, columns, rows)


def parse_update(p):
    table = p.name()
    p.expect("set")
    assignments = []
    while True:
        name = p.name()
        p.expect("=")
        assignments.append((name, p.expr()))
        if not p.accept(","):
            break
    return Update(table, assignments, parse_where(p))


def parse_column(p):
    """A column definition: a name, a type and constraints."""
    name = p.name()
    if p.accept("int"):
        column = Column(name, "INT")
    elif p.accept("text"):
        column = Column(name, "TEXT")
    else:
        p.fail()
    while True:
        if p.accept("not"):
            p.expect("null")
            column.not_null = True
        elif p.accept("primary"):
            p.expect("key")
            column.primary = column.not_null = True
        else:
            return column


def parse_create(p):
    name = p.name()
    p.expect("(")
    columns = [parse_column(p)]
    while p.accept(","):
        columns.append(parse_column(p))
    p.expect(")")
    return CreateTable(name, columns)


def children(e):
    """The subexpressions of e."""
    kind = e[0]
    if kind in ("lit", "col"):
        return []
    if kind in ("neg", "not", "isnull"):
        return [e[1]]
    if kind == "bin":
        return [e[2], e[3]]
    if kind == "like":
        return [e[1], e[2]]
    if kind == "in":
        return [e[1], *e[2]]
    if kind == "between":
        return [e[1], e[2], e[3]]
    if kind == "call":
        return e[2]
    return [] if e[2] is None else [e[2]]


def columns_in(e):
    """The columns e refers to, in order."""
    if e[0] == "col":
        return [e[1]]
    return [n for c in children(e) for n in columns_in(c)]


def aggregates_in(e):
    """The aggregates in e, outermost first."""
    if e[0] == "agg":
        return [e]
    return [a for c in children(e) for a in aggregates_in(c)]


def bare_columns(e):
    """The columns e refers to outside any aggregate."""
    if e[0] == "agg":
        return []
    if e[0] == "col":
        return [e[1]]
    return [n for c in children(e) for n in bare_columns(c)]


def render(e):
    """e as SQL text, to head a result column that has no alias."""
    kind = e[0]
    if kind == "lit":
        v = e[1]
        if v is None:
            return "null"
        if isinstance(v, int):
            return str(v)
        return "'" + v.replace("'", "''") + "'"
    if kind == "col":
        return e[1]
    if kind == "neg":
        return "-" + operand(e[1])
    if kind == "not":
        return "not " + operand(e[1])
    if kind == "isnull":
        return operand(e[1]) + (" is not null" if e[2] else " is null")
    if kind == "bin":
        return f"{operand(e[2])} {e[1]} {operand(e[3])}"
    if kind == "like":
        return f"{operand(e[1])} like {operand(e[2])}"
    if kind == "in":
        return f"{operand(e[1])} in ({', '.join(render(x) for x in e[2])})"
    if kind == "between":
        return f"{operand(e[1])} between {operand(e[2])} and {operand(e[3])}"
    if kind == "call":
        return f"{e[1]}({', '.join(render(a) for a in e[2])})"
    return f"{e[1]}({'*' if e[2] is None else render(e[2])})"


def operand(e):
    """e rendered as part of a larger expression: in parentheses if it has
    operators of its own."""
    if e[0] in ("lit", "col", "call", "agg"):
        return render(e)
    return f"({render(e)})"


# ----- values -----

# A value is None (NULL), an int or a str.


def type_name(v):
    if v is None:
        return "NULL"
    return "INT" if isinstance(v, int) else "TEXT"


def show(v):
    return "NULL" if v is None else str(v)


def truth(v):
    """A condition's value: None for NULL, else whether it is nonzero."""
    if v is None:
        return None
    if not isinstance(v, int):
        raise SqlError("a condition must be INT, not TEXT")
    return v != 0


def compare(op, a, b):
    if type(a) is not type(b):
        raise SqlError(f"cannot compare {type_name(a)} with {type_name(b)}")
    if op == "=":
        return a == b
    if op == "!=":
        return a != b
    if op == "<":
        return a < b
    if op == "<=":
        return a <= b
    if op == ">":
        return a > b
    return a >= b


def binary(op, a, b):
    if op == "and":
        x, y = truth(a), truth(b)
        if x is False or y is False:
            return 0
        return None if x is None or y is None else 1
    if op == "or":
        x, y = truth(a), truth(b)
        if x or y:
            return 1
        return None if x is None or y is None else 0
    if a is None or b is None:
        return None
    if op == "||":
        return show(a) + show(b)
    if op in COMPARISONS:
        return int(compare(op, a, b))
    if not isinstance(a, int) or not isinstance(b, int):
        raise SqlError(f"cannot apply {op} to TEXT")
    if op == "+":
        return a + b
    if op == "-":
        return a - b
    if op == "*":
        return a * b
    if b == 0:
        raise SqlError("division by zero")
    # `/` and `%` truncate toward zero.
    q = abs(a) // abs(b)
    if (a < 0) != (b < 0):
        q = -q
    return q if op == "/" else a - b * q


def like(s, pattern):
    """Whether s matches pattern, in which % is any run of characters and _
    any one character."""
    # matched[i]: whether the pattern so far matches s[:i]
    matched = [True] + [False] * len(s)
    for c in pattern:
        if c == "%":
            for i in range(1, len(s) + 1):
                matched[i] = matched[i] or matched[i - 1]
        else:
            for i in range(len(s), 0, -1):
                matched[i] = matched[i - 1] and (c == "_" or c == s[i - 1])
            matched[0] = False
    return matched[len(s)]


def call(name, args):
    if name == "coalesce":
        return next((a for a in args if a is not None), None)
    v = args[0]
    if v is None:
        return None
    if name == "abs":
        if not isinstance(v, int):
            raise SqlError("abs() needs INT, not TEXT")
        return abs(v)
    if not isinstance(v, str):
        raise SqlError(f"{name}() needs TEXT, not INT")
    if name == "upper":
        return v.upper()
    if name == "lower":
        return v.lower()
    return len(v)


def evaluate(e, row, group=None):
    """The value of e for row, a dict from column name to value. In a query
    with aggregates, group holds the rows they range over."""
    kind = e[0]
    if kind == "lit":
        return e[1]
    if kind == "col":
        if e[1] not in row:
            raise SqlError(f"no such column {e[1]}")
        return row[e[1]]
    if kind == "neg":
        v = evaluate(e[1], row, group)
        if v is None:
            return None
        if not isinstance(v, int):
            raise SqlError("cannot apply - to TEXT")
        return -v
    if kind == "not":
        x = truth(evaluate(e[1], row, group))
        return None if x is None else int(not x)
    if kind == "isnull":
        return int((evaluate(e[1], row, group) is None) != e[2])
    if kind == "bin":
        return binary(e[1], evaluate(e[2], row, group), evaluate(e[3], row, group))
    if kind == "like":
        s, pattern = evaluate(e[1], row, group), evaluate(e[2], row, group)
        if s is None or pattern is None:
            return None
        if not isinstance(s, str) or not isinstance(pattern, str):
            raise SqlError("LIKE needs TEXT, not INT")
        return int(like(s, pattern))
    if kind == "in":
        v = evaluate(e[1], row, group)
        saw_null = False
        for item in e[2]:
            r = binary("=", v, evaluate(item, row, group))
            if r == 1:
                return 1
            saw_null = saw_null or r is None
        return None if saw_null else 0
    if kind == "between":
        v = evaluate(e[1], row, group)
        low = binary(">=", v, evaluate(e[2], row, group))
        return binary("and", low, binary("<=", v, evaluate(e[3], row, group)))
    if kind == "call":
        return call(e[1], [evaluate(a, row, group) for a in e[2]])
    if group is None:
        raise SqlError(f"{e[1]}() is not allowed here")
    return aggregate(e, group)


def aggregate(e, group):
    """An aggregate's value over the rows in group."""
    _, name, arg = e
    if arg is None:
        return len(group)
    values = [v for v in (evaluate(arg, row) for row in group) if v is not None]
    if name == "count":
        return len(values)
    if not values:
        return None
    if name in ("min", "max"):
        best = values[0]
        for v in values[1:]:
            if compare("<" if name == "min" else ">", v, best):
                best = v
        return best
    if any(not isinstance(v, int) for v in values):
        raise SqlError(f"{name}() needs INT, not TEXT")
    total = sum(values)
    if name == "sum":
        return total
    q = abs(total) // len(values)  # avg truncates toward zero
    return -q if total < 0 else q


# ----- tables -----


@dataclass
class Table:
    name: str
    columns: list
    rows: list = field(default_factory=list)

    def names(self):
        return [c.name for c in self.columns]

    def index(self, name):
        for i, c in enumerate(self.columns):
            if c.name == name:
                return i
        raise SqlError(f"no such column {name}")


class Database:
    def __init__(self):
        self.tables = {}
        self.saved = None  # the tables as BEGIN found them, in a transaction

    def table(self, name):
        if name not in self.tables:
            raise SqlError(f"no such table {name}")
        return self.tables[name]


def check_columns(e, names):
    """Raises an error for the first column e refers to that isn't in names."""
    for name in columns_in(e):
        if name not in names:
            raise SqlError(f"no such column {name}")


def forbid_aggregates(e, clause):
    aggs = aggregates_in(e)
    if aggs:
        raise SqlError(f"{aggs[0][1]}() is not allowed in {clause}")


def check_value(column, v):
    if v is None:
        if column.not_null:
            raise SqlError(f"column {column.name} cannot be NULL")
    elif type_name(v) != column.type:
        raise SqlError(f"column {column.name} is {column.type}, not {type_name(v)}")


def check_keys(table, rows):
    """Raises an error if two rows share a primary key."""
    for i, c in enumerate(table.columns):
        if not c.primary:
            continue
        seen = set()
        for row in rows:
            if row[i] in seen:
                raise SqlError(f"duplicate key {show(row[i])} in column {c.name}")
            seen.add(row[i])


def format_table(headers, rows):
    """A result as lines of aligned columns: numbers to the right, the rest
    to the left."""
    cells = [[show(v) for v in row] for row in rows]
    widths = [max([len(h)] + [len(r[i]) for r in cells]) for i, h in enumerate(headers)]
    lines = [" | ".join(h.ljust(w) for h, w in zip(headers, widths)).rstrip()]
    lines.append("-+-".join("-" * w for w in widths))
    for row, texts in zip(rows, cells):
        parts = [t.rjust(w) if isinstance(v, int) else t.ljust(w) for v, t, w in zip(row, texts, widths)]
        lines.append(" | ".join(parts).rstrip())
    lines.append(f"({len(rows)} {'row' if len(rows) == 1 else 'rows'})")
    return lines


# ----- statements -----


def run_create(db, s):
    if s.name in db.tables:
        raise SqlError(f"table {s.name} already exists")
    seen = set()
    for c in s.columns:
        if c.name in seen:
            raise SqlError(f"duplicate column {c.name}")
        seen.add(c.name)
    if sum(c.primary for c in s.columns) > 1:
        raise SqlError("a table has at most one primary key")
    db.tables[s.name] = Table(s.name, s.columns)
    return ["CREATE TABLE"]


def run_drop(db, s):
    db.table(s.name)
    del db.tables[s.name]
    return ["DROP TABLE"]


def run_alter(db, s):
    table = db.table(s.table)
    c = s.column
    if c.name in table.names():
        raise SqlError(f"duplicate column {c.name}")
    if c.primary:
        raise SqlError("cannot add a primary key")
    if c.not_null and table.rows:
        raise SqlError(f"column {c.name} cannot be NULL")
    table.columns.append(c)
    for row in table.rows:
        row.append(None)
    return ["ALTER TABLE"]


def run_insert(db, s):
    table = db.table(s.table)
    names = s.columns if s.columns is not None else table.names()
    indexes = []
    for name in names:
        i = table.index(name)
        if i in indexes:
            raise SqlError(f"duplicate column {name}")
        indexes.append(i)
    new_rows = []
    for values in s.rows:
        if len(values) != len(indexes):
            raise SqlError(f"expected {len(indexes)} values, got {len(values)}")
        row = [None] * len(table.columns)
        for i, e in zip(indexes, values):
            forbid_aggregates(e, "VALUES")
            row[i] = evaluate(e, {})
        for c, v in zip(table.columns, row):
            check_value(c, v)
        new_rows.append(row)
    check_keys(table, table.rows + new_rows)
    table.rows += new_rows
    return [f"INSERT {len(new_rows)}"]


def run_update(db, s):
    table = db.table(s.table)
    names = table.names()
    targets = []
    for name, e in s.assignments:
        i = table.index(name)
        if i in [t for t, _ in targets]:
            raise SqlError(f"duplicate column {name}")
        check_columns(e, names)
        forbid_aggregates(e, "SET")
        targets.append((i, e))
    if s.where is not None:
        check_columns(s.where, names)
        forbid_aggregates(s.where, "WHERE")
    new_rows = []
    count = 0
    for row in table.rows:
        ctx = dict(zip(names, row))
        if s.where is None or truth(evaluate(s.where, ctx)):
            row = list(row)
            for i, e in targets:
                row[i] = evaluate(e, ctx)
                check_value(table.columns[i], row[i])
            count += 1
        new_rows.append(row)
    check_keys(table, new_rows)
    table.rows = new_rows
    return [f"UPDATE {count}"]


def run_delete(db, s):
    table = db.table(s.table)
    names = table.names()
    if s.where is not None:
        check_columns(s.where, names)
        forbid_aggregates(s.where, "WHERE")
    keep = [row for row in table.rows if s.where is not None and not truth(evaluate(s.where, dict(zip(names, row))))]
    count = len(table.rows) - len(keep)
    table.rows = keep
    return [f"DELETE {count}"]


def sort_key(v):
    """NULL sorts first, then numbers, then text."""
    if v is None:
        return (0, 0, "")
    if isinstance(v, int):
        return (1, v, "")
    return (2, 0, v)


def sort_rows(rows, keys, descending):
    """rows ordered by their keys: one stable sort per key, from the last."""
    order = list(range(len(rows)))
    for k in reversed(range(len(descending))):
        order.sort(key=lambda i: sort_key(keys[i][k]), reverse=descending[k])
    return [rows[i] for i in order]


def plain_query(items, headers, order, rows):
    """One result row per row. ORDER BY sees the row's columns and the
    result's column names, which win."""
    out = []
    keys = []
    for r in rows:
        values = [evaluate(e, r) for e, _ in items]
        out.append(values)
        if order:
            ctx = {**r, **dict(zip(headers, values))}
            keys.append([evaluate(e, ctx) for e, _ in order])
    return sort_rows(out, keys, [desc for _, desc in order])


def aggregate_query(items, rows):
    """A single result row, the aggregates over every row."""
    for e, _ in items:
        for name in bare_columns(e):
            raise SqlError(f"column {name} must be used in an aggregate")
    return [[evaluate(e, {}, rows) for e, _ in items]]


def run_select(db, s):
    if s.table is None:
        names, rows = [], [{}]
    else:
        table = db.table(s.table)
        names = table.names()
        rows = [dict(zip(names, row)) for row in table.rows]
    items = []
    for e, alias in s.items:
        if e is None:
            if s.table is None:
                raise SqlError("SELECT * needs a table")
            items += [(("col", n), None) for n in names]
        else:
            check_columns(e, names)
            for agg in aggregates_in(e):
                if agg[2] is not None and aggregates_in(agg[2]):
                    raise SqlError("aggregates cannot be nested")
            items.append((e, alias))
    headers = [alias or render(e) for e, alias in items]
    if s.where is not None:
        check_columns(s.where, names)
        forbid_aggregates(s.where, "WHERE")
        rows = [r for r in rows if truth(evaluate(s.where, r))]
    for e, _ in s.order:
        check_columns(e, names + headers)
    if any(aggregates_in(e) for e, _ in items):
        out = aggregate_query(items, rows)
    else:
        out = plain_query(items, headers, s.order, rows)
    if s.limit is not None:
        out = out[: s.limit]
    return format_table(headers, out)


def run_simple(db, s):
    if s.what == "show":
        rows = [[name, len(db.tables[name].rows)] for name in sorted(db.tables)]
        return format_table(["table", "rows"], rows)
    if s.what == "begin":
        if db.saved is not None:
            raise SqlError("already in a transaction")
        db.saved = copy.deepcopy(db.tables)
    elif db.saved is None:
        raise SqlError("no transaction")
    elif s.what == "rollback":
        db.tables = db.saved
        db.saved = None
    else:
        db.saved = None
    return [s.what.upper()]


def run_describe(db, s):
    table = db.table(s.table)
    rows = [[c.name, c.type, "NO" if c.not_null else "YES", "PRI" if c.primary else ""] for c in table.columns]
    return format_table(["column", "type", "null", "key"], rows)


def execute(db, stmt):
    """Carries out one statement; returns the lines to print."""
    if isinstance(stmt, Select):
        return run_select(db, stmt)
    if isinstance(stmt, Insert):
        return run_insert(db, stmt)
    if isinstance(stmt, Update):
        return run_update(db, stmt)
    if isinstance(stmt, Delete):
        return run_delete(db, stmt)
    if isinstance(stmt, CreateTable):
        return run_create(db, stmt)
    if isinstance(stmt, DropTable):
        return run_drop(db, stmt)
    if isinstance(stmt, AlterAdd):
        return run_alter(db, stmt)
    if isinstance(stmt, Describe):
        return run_describe(db, stmt)
    return run_simple(db, stmt)


def main():
    db = Database()
    for text in split_statements(sys.stdin.read()):
        try:
            lines = execute(db, parse_statement(tokenize(text)))
        except SqlError as e:
            lines = [f"error: {e}"]
        for line in lines:
            print(line)


main()
