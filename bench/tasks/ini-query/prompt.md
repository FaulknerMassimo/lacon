Standard input is an INI document, then a line `---`, then one query per line.

The document has section headers `[name]`, entries `key = value`, comment lines
starting with `;` or `#`, and blank lines. Leading and trailing spaces on a
line, around a section name, and around a key or a value are ignored. A value
is everything after the first `=` and may contain `=` or be empty. Entries
before the first header belong to no section. A section may appear more than
once, and its entries merge; a later entry for the same key replaces the
earlier one. Section names and keys never contain `.`, `=`, `[` or `]`.

A query `section.key` asks for an entry in a section, and a query `key` asks for
an entry outside any section. For each query, print the value, or `missing` if
there is no such entry.
