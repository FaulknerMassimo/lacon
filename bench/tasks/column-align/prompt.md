Standard input is a table: one row per line, cells separated by one or more
spaces. Skip blank lines. Print the table with its columns aligned: each
column is as wide as its widest cell, cells are separated by two spaces, and
lines have no trailing spaces. A column whose cells below the first row are
all numbers (an optional `-` followed by digits, optionally with a `.` and
more digits) is right-aligned; other columns are left-aligned. Rows may have
fewer cells than others; missing cells are empty (and do not stop a column
from being numeric).
