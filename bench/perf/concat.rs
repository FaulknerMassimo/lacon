fn main() {
    let mut seed: i64 = 7;
    let mut rows: Vec<String> = Vec::new();
    for _ in 0..400_000 {
        let mut line = String::from(" ");
        for _ in 0..6 {
            seed = (seed * 1103515245 + 12345) % 2147483648;
            line.push_str(&(seed % 100).to_string());
            line.push(',');
        }
        rows.push(line.trim().to_string());
    }
    rows.sort();
    let mut out = String::new();
    for r in &rows {
        out.push_str(&r[..2]);
    }
    println!("{} {} {} {}", rows.len(), out.len(), rows[0], rows[rows.len() - 1]);
}
