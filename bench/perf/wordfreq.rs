use std::collections::HashMap;

fn main() {
    let words = ["alpha", "beta", "gamma", "delta", "epsilon", "zeta", "eta", "theta"];
    // Insertion-ordered counts, as Lacon's maps are.
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut counts: Vec<(String, i64)> = Vec::new();
    let mut seed: i64 = 1;
    for _ in 0..2_000_000 {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        let w = format!("{}{}", words[(seed % 8) as usize], seed / 8 % 50);
        match index.get(&w) {
            Some(&i) => counts[i].1 += 1,
            None => {
                index.insert(w.clone(), counts.len());
                counts.push((w, 1));
            }
        }
    }
    let n = counts.len();
    counts.sort_by_key(|c| -c.1);
    for (w, c) in counts.iter().take(3) {
        println!("{w} {c}");
    }
    println!("{n}");
}
