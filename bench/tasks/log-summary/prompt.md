Standard input is a web server access log in Common Log Format, one request per
line:

```
127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /a/b.gif?x=1 HTTP/1.0" 200 2326
```

The fields are the client address, two fields that are not used, a timestamp in
square brackets, the request line in double quotes (method, target, protocol,
separated by single spaces), a three-digit status code, and the response size
in bytes, which is `-` when nothing was sent. A line is malformed if it does
not have this shape; skip it. The path of a request is its target up to but
not including the first `?`.

Print a summary in exactly this form:

```
requests: <number of well-formed lines>
bytes: <total response size; `-` counts as 0>
status 2xx: <count>
status 4xx: <count>
top paths:
  /a/b.gif 2
  / 1
malformed: <number of malformed lines>
```

Print one `status Nxx` line for each status class (the first digit) that
occurs, in increasing order. Under `top paths:` list up to three paths, by
request count descending and then by path. Ignore blank lines.
