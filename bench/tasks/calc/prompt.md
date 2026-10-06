Each line of standard input is an arithmetic expression over integers using
`+ - * /`, unary minus, parentheses and any number of spaces between tokens. A
number is one or more digits. `*` and `/` bind tighter than `+` and `-`,
operators of equal precedence are left-associative, and `/` is integer division
truncating toward zero. Print the value of each expression on its own line, or
`error` if the line is not a valid expression (including a blank line) or
divides by zero. All values fit in a signed 64-bit integer.
