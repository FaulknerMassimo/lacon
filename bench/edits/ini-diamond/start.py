import sys


def unquote(v):
    """A value in double quotes keeps the spaces inside them."""
    if len(v) >= 2 and v[0] == '"' and v[-1] == '"':
        return v[1:-1]
    return v


def parse_doc(lines):
    """The document's sections, {section: {key: value}} with "" for entries
    before the first header, and the sections in the order they first
    appear. Prints an error for each line that is none of the forms."""
    secs = {"": {}}
    order = []
    section = ""
    for n, raw in enumerate(lines, 1):
        line = raw.strip()
        if not line or line[0] in ";#":
            continue
        if line.startswith("[") and line.endswith("]"):
            section = line[1:-1].strip()
            if section not in secs:
                secs[section] = {}
                order.append(section)
            continue
        key, eq, value = line.partition("=")
        if not eq or not key.strip():
            print(f"line {n}: bad line")
            continue
        secs[section][key.strip()] = unquote(value.strip())
    return secs, order


def split_ref(section, ref):
    """The (section, key) a reference names: `key` is in section, `s.key` in
    s, and `.key` outside any section."""
    sec, dot, key = ref.partition(".")
    if not dot:
        return section, ref
    return sec, key


def expand(secs, section, value, seen):
    """value with each @{...} reference replaced by what it names, and @@
    by @. seen holds the entries being expanded, to catch a cycle."""
    out = []
    i = 0
    while i < len(value):
        if value.startswith("@@", i):
            out.append("@")
            i += 2
            continue
        if value.startswith("@{", i):
            end = value.find("}", i)
            if end >= 0:
                out.append(lookup(secs, section, value[i + 2 : end], seen))
                i = end + 1
                continue
        out.append(value[i])
        i += 1
    return "".join(out)


def lookup(secs, section, ref, seen):
    """What a reference names, expanded in turn. A reference to a missing
    entry stays as written, and one that leads back to an entry being
    expanded is <cycle>."""
    sec, key = split_ref(section, ref)
    if sec not in secs or key not in secs[sec]:
        return "@{" + ref + "}"
    if (sec, key) in seen:
        return "<cycle>"
    seen.add((sec, key))
    return expand(secs, sec, secs[sec][key], seen)


def split_name(name):
    """The (section, key) of a query's SECTION.KEY or KEY."""
    sec, dot, key = name.partition(".")
    return (sec, key) if dot else ("", name)


def run_query(secs, order, query):
    """The answer to one query."""
    parts = query.split()
    if parts == ["sections"]:
        return " ".join(order)
    if parts and parts[0] == "keys" and len(parts) <= 2:
        sec = parts[1] if len(parts) == 2 else ""
        if sec not in secs:
            return "missing"
        return ", ".join(sorted(secs[sec]))
    if len(parts) != 2:
        return "bad query"
    sec, key = split_name(parts[1])
    found = sec in secs and key in secs[sec]
    if parts[0] == "has":
        return "yes" if found else "no"
    if parts[0] == "raw":
        return secs[sec][key] if found else "missing"
    if parts[0] == "get":
        if not found:
            return "missing"
        return expand(secs, sec, secs[sec][key], {(sec, key)})
    return "bad query"


def main():
    lines = sys.stdin.read().splitlines()
    split = lines.index("---") if "---" in lines else len(lines)
    secs, order = parse_doc(lines[:split])
    for query in lines[split + 1 :]:
        if query.strip():
            print(run_query(secs, order, query))


main()
