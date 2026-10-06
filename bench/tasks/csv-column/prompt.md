The first line of standard input is a column name. The rest is CSV: a header
line of column names, then one record per line. Fields are separated by
commas. A field may be enclosed in double quotes, and then it may contain
commas, and `""` inside it stands for one `"`. Fields never span lines. Print
the value of the named column for each record, one per line, without the
enclosing quotes. A record with fewer fields than the header has empty values
for the missing ones. If no header field has the name, print `no column NAME`
(with the name). Skip blank lines.
