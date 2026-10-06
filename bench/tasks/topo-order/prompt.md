Each line of standard input is `item: dep1 dep2 ...`, meaning the item depends
on the listed items (the list may be empty). Items are words; an item that only
appears as a dependency has no dependencies of its own. Skip blank lines.

Print every item on one line, separated by spaces, in an order where each
item comes after all of its dependencies. When several items could come next,
take the alphabetically smallest. If the dependencies form a cycle, print
`cycle` instead.
