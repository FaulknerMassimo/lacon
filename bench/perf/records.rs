#[derive(Clone)]
struct Person {
    name: String,
    age: i64,
    score: i64,
}

fn main() {
    let mut people = Vec::new();
    let mut seed: i64 = 7;
    for i in 0..600_000 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        people.push(Person { name: format!("p{i}"), age: seed % 90, score: seed / 7 % 1000 });
    }
    let mut adults: Vec<Person> = people.iter().filter(|p| p.age >= 18).cloned().collect();
    adults.sort_by_key(|p| -p.score);
    let total: i64 = adults.iter().map(|p| p.score).sum();
    println!("{} {} {} {}", adults.len(), total, adults[0].name, adults[adults.len() - 1].name);
}
