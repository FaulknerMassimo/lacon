import sys

lines = sys.stdin.read().split("\n")
sep = lines.index("---")
data, section = {}, ""
for raw in lines[:sep]:
    line = raw.strip()
    if not line or line[0] in ";#":
        continue
    if line.startswith("[") and line.endswith("]"):
        section = line[1:-1].strip()
        continue
    key, _, value = line.partition("=")
    data[(section, key.strip())] = value.strip()
for q in lines[sep + 1:]:
    q = q.strip()
    if not q:
        continue
    sec, _, key = q.rpartition(".")
    print(data.get((sec, key), "missing"))
