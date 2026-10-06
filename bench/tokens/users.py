from collections import Counter
from dataclasses import dataclass

@dataclass
class User:
    name: str
    age: int
    city: str

def parse_line(line: str) -> User:
    parts = [p.strip() for p in line.split(",")]
    if len(parts) != 3:
        raise ValueError(f"bad line: {line}")
    return User(parts[0], int(parts[1]), parts[2])

def main():
    counts = Counter()
    with open("users.csv") as f:
        for line in list(f)[1:]:
            user = parse_line(line)
            if user.age >= 18:
                counts[user.city] += 1
    for city, n in counts.most_common():
        print(f"{city}: {n}")

main()
