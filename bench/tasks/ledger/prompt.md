Standard input is a list of banking commands, one per line:

- `open NAME AMOUNT` opens an account with an initial balance
- `deposit NAME AMOUNT`
- `withdraw NAME AMOUNT`
- `transfer FROM TO AMOUNT`

An amount is one or more digits, optionally followed by a point and one or two
digits (`12`, `0.5`, `03.75`); compute exactly, without floating-point
rounding. A command that cannot be carried out changes nothing and prints
`line N: REASON`, where N is the 1-based line number and REASON is the first
that applies of:

- `bad amount` if the amount is not of that form or is zero (`open` accepts
  zero)
- `account exists` when opening an account that is already open
- `no account NAME` for the first named account that is not open
- `insufficient funds` when a withdrawal or transfer exceeds the balance
- `same account` when transferring from an account to itself

A line that is not one of the four commands with the right number of fields
prints `line N: bad command`. Skip blank lines (they still count for N). At
the end, print `NAME BALANCE` for each account, by name, with the balance to
two decimal places.
