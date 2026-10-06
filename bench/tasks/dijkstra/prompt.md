The first line of standard input is `N M S`: the number of nodes (numbered 1
to N), the number of edges, and a start node. Then M lines `u v w` follow,
each a directed edge from u to v with a positive integer weight w (there may be
several edges between the same nodes). Print N lines: for each node in order,
the length of the shortest path from S to it, or `-1` if it cannot be reached.
N is at most 10,000 and M at most 40,000.
