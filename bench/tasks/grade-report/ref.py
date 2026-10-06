import sys

students = []
for line in sys.stdin.read().split("\n"):
    parts = line.split()
    if not parts:
        continue
    scores = [int(x) for x in parts[1:]]
    students.append((parts[0], sum(scores) / len(scores)))
if not students:
    print("no students")
else:
    for name, avg in sorted(students, key=lambda s: (-s[1], s[0])):
        grade = "A" if avg >= 90 else "B" if avg >= 80 else "C" if avg >= 70 else "D" if avg >= 60 else "F"
        print(f"{name} {avg:.1f} {grade}")
    print(f"class average: {sum(a for _, a in students) / len(students):.1f}")
