Each line of standard input is an expression in reverse Polish notation:
integers (possibly negative) and the operators `+ - * /`, separated by spaces.
`/` is integer division truncating toward zero. Print the value of each
expression on its own line, or `error` if an operator lacks operands, a number
cannot be parsed, a division by zero occurs, or more than one value remains.
Blank lines print `error`.
