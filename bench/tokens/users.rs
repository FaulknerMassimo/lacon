use std::collections::HashMap;
use std::error::Error;
use std::fs;

#[derive(Debug, Clone)]
struct User {
    name: String,
    age: u32,
    city: String,
}

fn parse_line(line: &str) -> Result<User, Box<dyn Error>> {
    let parts: Vec<&str> = line.split(',').map(|s| s.trim()).collect();
    if parts.len() != 3 {
        return Err(format!("bad line: {}", line).into());
    }
    Ok(User {
        name: parts[0].to_string(),
        age: parts[1].parse()?,
        city: parts[2].to_string(),
    })
}

fn main() -> Result<(), Box<dyn Error>> {
    let text = fs::read_to_string("users.csv")?;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in text.lines().skip(1) {
        let user = parse_line(line)?;
        if user.age >= 18 {
            *counts.entry(user.city.clone()).or_insert(0) += 1;
        }
    }
    let mut sorted: Vec<(String, usize)> = counts.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (city, n) in sorted {
        println!("{}: {}", city, n);
    }
    Ok(())
}
