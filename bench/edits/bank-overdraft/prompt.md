The program processes banking commands from standard input. Add overdraft
limits to it:

- A new command `limit NAME AMOUNT` sets an account's overdraft limit: how far
  below zero its balance may go. The amount has the same form as other amounts,
  and zero is allowed. A new account's limit is zero.
- Its errors are those of the other commands, in the same order: `bad command`
  for the wrong number of fields, then `bad amount`, then `no account NAME` or
  `account closed NAME`.
- A successful `limit` is recorded in the account's history as `limit AMOUNT`.
- A withdrawal or transfer may now take a balance below zero, down to minus the
  account's limit; beyond that it is still `insufficient funds`. Lowering a
  limit never changes a balance.
- A negative amount prints with a minus sign, as in `-12.50` and `-0.05`,
  wherever a balance or total is printed.

Everything else stays as it is: `interest` still skips balances that are not
positive, and `close` still needs a balance of exactly zero.
