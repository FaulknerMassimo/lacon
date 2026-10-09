The program is a small SQL database: it reads statements separated by
semicolons from standard input and prints each one's result. Add `GROUP BY`
and `HAVING` to its `SELECT`:

- The clauses come after `WHERE` and before `ORDER BY`:
  `SELECT ... FROM t WHERE ... GROUP BY c1, c2 HAVING ... ORDER BY ... LIMIT n`.
  `GROUP BY` names one or more columns of the table; a column the table
  doesn't have is `no such column NAME`, as elsewhere. `group` and `having`
  become keywords.
- Rows that agree on every `GROUP BY` column form a group, and a NULL agrees
  with a NULL. Each group gives one result row, and the groups come in the
  order of their first rows unless `ORDER BY` says otherwise. When no rows
  are left after `WHERE`, there are no groups and no result rows.
- In a grouped query, aggregates range over the group's rows. Outside an
  aggregate, the select list and `HAVING` may use only the `GROUP BY`
  columns, and `ORDER BY` those and the result's column names. Any other
  column is the error `column NAME must be in GROUP BY or used in an
  aggregate`.
- `HAVING` keeps the groups whose condition is true, as `WHERE` does for
  rows, and may use aggregates that aren't in the select list. `HAVING`
  without `GROUP BY` is the error `HAVING needs GROUP BY`.

Everything else stays as it is.
