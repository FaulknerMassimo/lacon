fn main() {
    let (n, cap) = (8000usize, 50000usize);
    let (mut w, mut v) = (Vec::new(), Vec::new());
    let mut seed: i64 = 42;
    for _ in 0..n {
        seed = (seed * 1103515245 + 12345) % 2147483648;
        w.push((seed % 100 + 1) as usize);
        seed = (seed * 1103515245 + 12345) % 2147483648;
        v.push(seed % 1000);
    }
    let mut best = vec![0i64; cap + 1];
    for i in 0..n {
        let (wi, vi) = (w[i], v[i]);
        let mut c = cap;
        while c >= wi {
            let cand = best[c - wi] + vi;
            if cand > best[c] {
                best[c] = cand;
            }
            c -= 1;
        }
    }
    println!("{}", best[cap]);
}
