fn main() {
    let mut seed: i64 = 7;
    let mut total: i64 = 0;
    for r in 0..300i64 {
        let mut xs = Vec::new();
        for _ in 0..100000 {
            seed = (seed * 1103515245 + 12345) % 2147483648;
            xs.push(seed % 100000);
        }
        let mut ys: Vec<i64> = xs.into_iter().map(|x| x * 3 + r).filter(|x| x % 7 != 0).collect();
        ys.sort_by_key(|&x| -x);
        total += ys[0] + ys[ys.len() - 1] + ys.len() as i64;
    }
    println!("{}", total);
}
