package main

import (
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
)

type User struct {
	Name string
	Age  int
	City string
}

func parseLine(line string) (User, error) {
	parts := strings.Split(line, ",")
	if len(parts) != 3 {
		return User{}, fmt.Errorf("bad line: %s", line)
	}
	age, err := strconv.Atoi(strings.TrimSpace(parts[1]))
	if err != nil {
		return User{}, err
	}
	return User{strings.TrimSpace(parts[0]), age, strings.TrimSpace(parts[2])}, nil
}

func main() {
	data, err := os.ReadFile("users.csv")
	if err != nil {
		fmt.Println(err)
		os.Exit(1)
	}
	counts := map[string]int{}
	for _, line := range strings.Split(strings.TrimSpace(string(data)), "\n")[1:] {
		u, err := parseLine(line)
		if err != nil {
			fmt.Println(err)
			os.Exit(1)
		}
		if u.Age >= 18 {
			counts[u.City]++
		}
	}
	type kv struct {
		City string
		N    int
	}
	var sorted []kv
	for c, n := range counts {
		sorted = append(sorted, kv{c, n})
	}
	sort.Slice(sorted, func(i, j int) bool { return sorted[i].N > sorted[j].N })
	for _, e := range sorted {
		fmt.Printf("%s: %d\n", e.City, e.N)
	}
}
