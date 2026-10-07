fn steps(start: i64) -> i64 {
    let mut n = start;
    let mut k = 0;
    while n != 1 {
        n = if n % 2 == 0 { n / 2 } else { 3 * n + 1 };
        k += 1;
    }
    k
}

fn main() {
    let (mut best, mut arg) = (0, 0);
    for i in 1..300_000 {
        let s = steps(i);
        if s > best {
            best = s;
            arg = i;
        }
    }
    println!("{arg} {best}");
}
