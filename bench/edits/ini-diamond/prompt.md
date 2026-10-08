The program reads an INI document and answers queries about it. A value can
refer to other entries with `@{key}`, `@{section.key}` or `@{.key}`, and `get`
expands those references. Users report a bug: a value that uses the same entry
twice, such as `path = @{.dir}/@{.dir}`, prints `<cycle>` for the second use,
and so does any entry reached a second time while expanding one value, even
when nothing refers back to itself.

Fix it. Only a reference that leads back to an entry whose expansion is still
in progress is a cycle and expands to `<cycle>`, as `self = @{self}` does now;
any other reference expands in full, however many times it is used. Everything
else stays as it is.
