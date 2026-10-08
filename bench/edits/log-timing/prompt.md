The program summarizes a web server's access log from standard input. The log
now records how long each request took. Change the program to report it:

- A line may end with one more field after the response size, separated by one
  space: the time the request took in milliseconds, one or more digits, as in
  `"GET / HTTP/1.1" 200 512 85`. A line without it is still well-formed, and a
  line with anything else there, or with more fields, is malformed.
- Add a section right after `top paths:`, headed `slowest paths:`, that lists up
  to three paths by their average time, highest first and then by path, as
  `  /login 310ms`. A path's average is over its requests that have a time,
  rounded down to a whole millisecond. A path with no timed request is not
  listed; with no timed request at all, the section is just its header.

Everything else stays as it is.
