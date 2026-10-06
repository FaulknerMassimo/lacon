import sys
counts = {}
for line in sys.stdin.read().split("\n")[1:]:
    if not line.strip():
        continue
    name, age, city = [p.strip() for p in line.split(",")]
    if int(age) >= 18:
        counts[city] = counts.get(city, 0) + 1
for city, n in sorted(counts.items(), key=lambda kv: (-kv[1], kv[0])):
    print(f"{city}: {n}")
