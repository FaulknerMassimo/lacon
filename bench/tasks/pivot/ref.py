import sys

cells, regions, products = {}, set(), set()
for line in sys.stdin.read().split("\n")[1:]:
    if not line.strip():
        continue
    r, p, a = line.split(",")
    regions.add(r)
    products.add(p)
    cells[(r, p)] = cells.get((r, p), 0) + int(a)
ps = sorted(products)
print(",".join(["region", *ps, "total"]))
for r in sorted(regions):
    row = [cells.get((r, p), 0) for p in ps]
    print(",".join([r, *map(str, row), str(sum(row))]))
cols = [sum(cells.get((r, p), 0) for r in regions) for p in ps]
print(",".join(["total", *map(str, cols), str(sum(cols))]))
