import sys


class Req:
    def __init__(self, client, hour, method, path, status, size, ms):
        self.client = client
        self.hour = hour
        self.method = method
        self.path = path
        self.status = status
        self.size = size
        self.ms = ms  # how long the request took, or None if the line didn't say


def parse_hour(stamp):
    """The hour of a timestamp like 10/Oct/2000:13:55:36 -0700, or None."""
    date, _, clock = stamp.partition(":")
    parts = clock.split(":")
    if len(date.split("/")) != 3 or len(parts) != 3:
        return None
    hh = parts[0]
    if len(hh) != 2 or not hh.isdigit() or int(hh) > 23:
        return None
    return int(hh)


def parse_request(text):
    """(method, path) from a request line like GET /a?x=1 HTTP/1.0, or None."""
    parts = text.split(" ")
    if len(parts) != 3 or "" in parts:
        return None
    return parts[0], parts[1].split("?")[0]


def parse_line(line):
    """The request on one log line, or None if the line is malformed:

    127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /a.gif HTTP/1.0" 200 2326 85

    The last field, the time taken in milliseconds, is optional.
    """
    start = line.find("[")
    end = line.find("]")
    if start < 0 or end < start:
        return None
    # the client, two unused fields, then the space before '['
    head = line[:start].split(" ")
    if len(head) != 4 or head[3] != "" or "" in head[:3]:
        return None
    hour = parse_hour(line[start + 1 : end])
    if hour is None:
        return None
    rest = line[end + 1 :]
    if not rest.startswith(' "'):
        return None
    q = rest.find('"', 2)
    if q < 0:
        return None
    req = parse_request(rest[2:q])
    if req is None:
        return None
    tail = rest[q + 1 :].split(" ")
    if len(tail) not in (3, 4) or tail[0] != "":
        return None
    ms = None
    if len(tail) == 4:
        if not tail[3].isdigit():
            return None
        ms = int(tail[3])
    status, size = tail[1], tail[2]
    if len(status) != 3 or not status.isdigit():
        return None
    if size != "-" and not size.isdigit():
        return None
    return Req(head[0], hour, req[0], req[1], int(status), 0 if size == "-" else int(size), ms)


def top(counts, n):
    """Up to n (key, count) pairs, by count descending and then by key."""
    return sorted(counts.items(), key=lambda kv: (-kv[1], kv[0]))[:n]


def report_totals(reqs):
    print(f"requests: {len(reqs)}")
    print(f"bytes: {sum(r.size for r in reqs)}")


def report_status(reqs):
    classes = {}
    for r in reqs:
        c = r.status // 100
        classes[c] = classes.get(c, 0) + 1
    for c in sorted(classes):
        print(f"status {c}xx: {classes[c]}")


def report_methods(reqs):
    counts = {}
    for r in reqs:
        counts[r.method] = counts.get(r.method, 0) + 1
    print("methods: " + ", ".join(f"{m} {n}" for m, n in top(counts, len(counts))))


def report_paths(reqs):
    counts = {}
    for r in reqs:
        counts[r.path] = counts.get(r.path, 0) + 1
    print("top paths:")
    for path, n in top(counts, 3):
        print(f"  {path} {n}")


def report_slowest(reqs):
    """Paths by their average time, over the requests that have one."""
    total = {}
    timed = {}
    for r in reqs:
        if r.ms is None:
            continue
        total[r.path] = total.get(r.path, 0) + r.ms
        timed[r.path] = timed.get(r.path, 0) + 1
    average = {path: total[path] // timed[path] for path in total}
    print("slowest paths:")
    for path, ms in top(average, 3):
        print(f"  {path} {ms}ms")


def report_errors(reqs):
    counts = {}
    for r in reqs:
        if r.status >= 400:
            counts[r.path] = counts.get(r.path, 0) + 1
    print("error paths:")
    for path, n in top(counts, 3):
        print(f"  {path} {n}")


def report_clients(reqs):
    counts = {}
    sizes = {}
    for r in reqs:
        counts[r.client] = counts.get(r.client, 0) + 1
        sizes[r.client] = sizes.get(r.client, 0) + r.size
    print("top clients:")
    for client, n in top(counts, 3):
        print(f"  {client} {n} requests, {sizes[client]} bytes")


def report_hours(reqs):
    if not reqs:
        print("busiest hour: none")
        return
    counts = [0] * 24
    for r in reqs:
        counts[r.hour] += 1
    best = 0
    for h in range(24):
        if counts[h] > counts[best]:
            best = h
    print(f"busiest hour: {best:02d} ({counts[best]} requests)")


def main():
    reqs = []
    malformed = 0
    for line in sys.stdin.read().splitlines():
        if not line.strip():
            continue
        r = parse_line(line)
        if r is None:
            malformed += 1
        else:
            reqs.append(r)
    report_totals(reqs)
    report_status(reqs)
    report_methods(reqs)
    report_paths(reqs)
    report_slowest(reqs)
    report_errors(reqs)
    report_clients(reqs)
    report_hours(reqs)
    print(f"malformed: {malformed}")


main()
