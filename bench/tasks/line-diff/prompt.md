Standard input holds two texts separated by a line `---`: the old text, then
the new text. Print a line diff from old to new: each line prefixed with two
characters, `  ` for a line in both, `- ` for a line only in the old text, and
`+ ` for a line only in the new text.

Use a longest common subsequence, chosen this way: let `L(i, j)` be the length
of the longest common subsequence of old lines `i..` and new lines `j..`
(0-based, to the end). Start at `i = j = 0`. While both texts have lines left:
if `old[i] == new[j]`, print it as common and advance both; otherwise, if
`L(i + 1, j) >= L(i, j + 1)`, print `- old[i]` and advance `i`; otherwise print
`+ new[j]` and advance `j`. Then print the remaining old lines with `- ` and
the remaining new lines with `+ `.
