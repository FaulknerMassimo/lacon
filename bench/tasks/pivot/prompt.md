Standard input is CSV with the header `region,product,amount`, then one sale
per line (the amount is an integer). Print a pivot table as CSV: a header
`region,<products...>,total` with the products sorted, then one line per region
(sorted) with the sum of amounts for each product (0 if there were none) and the
region's total, then a final line starting with `total` holding the column
sums. Skip blank lines. If there are no sales, print just `region,total` and
`total,0`.
