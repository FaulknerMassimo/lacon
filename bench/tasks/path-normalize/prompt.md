Each line of standard input is a current directory (an absolute path) and a
path, separated by one space. Print the absolute, normalized form of the path:

- `~` alone or at the start (`~/x`) stands for `/home/user`
- a path that does not start with `/` (after expanding `~`) is relative to
  the current directory
- repeated slashes count as one, `.` components are dropped, and `..`
  removes the previous component (`..` at the root stays at the root)
- the result starts with `/` and has no trailing `/`, except the root itself,
  which is `/`

No symbolic links are involved and nothing needs to exist. Skip blank lines.
