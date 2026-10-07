enum Tree { Leaf, Node(Box<Tree>, Box<Tree>) }
fn make(d: i64) -> Tree { if d == 0 { Tree::Leaf } else { Tree::Node(Box::new(make(d - 1)), Box::new(make(d - 1))) } }
fn check(t: &Tree) -> i64 { match t { Tree::Leaf => 1, Tree::Node(l, r) => 1 + check(l) + check(r) } }
fn main() {
    let n: i64 = 16;
    let long = make(n);
    let mut d = 4;
    while d <= n {
        let iters = 1i64 << (n - d + 4);
        let mut c = 0;
        for _ in 0..iters { c += check(&make(d)); }
        println!("{} trees of depth {} check: {}", iters, d, c);
        d += 2;
    }
    println!("long lived tree of depth {} check: {}", n, check(&long));
}
