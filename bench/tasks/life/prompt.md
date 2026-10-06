The first line of standard input is a number of generations G. The rest is a
grid of `.` (dead) and `#` (alive) cells; all rows have the same length.
Run Conway's Game of Life for G generations on this grid, where cells outside
the grid are always dead: a live cell with two or three live neighbours (of
its eight) stays alive, a dead cell with exactly three live neighbours comes
alive, and every other cell is dead in the next generation. Print the final
grid in the same format.
