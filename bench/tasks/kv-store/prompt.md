Implement an in-memory key-value store with nested transactions. Each line of
standard input is a command (keys and values are single words):

- `SET k v` sets k to v
- `GET k` prints the value of k, or `NULL`
- `DELETE k` removes k (if present)
- `COUNT v` prints how many keys currently have the value v
- `BEGIN` opens a transaction, nested inside any open one
- `ROLLBACK` undoes every change made since the innermost open transaction
  began, and closes it; prints `NO TRANSACTION` if none is open
- `COMMIT` keeps all changes and closes every open transaction; prints
  `NO TRANSACTION` if none is open

Skip blank lines.
