fn main() {
    let size = 400;
    let mut inside = 0;
    for py in 0..size {
        for px in 0..size {
            let cx = px as f64 / size as f64 * 3.0 - 2.0;
            let cy = py as f64 / size as f64 * 2.0 - 1.0;
            let (mut x, mut y) = (0.0f64, 0.0f64);
            let mut k = 0;
            while k < 100 && x * x + y * y <= 4.0 {
                let t = x * x - y * y + cx;
                y = 2.0 * x * y + cy;
                x = t;
                k += 1;
            }
            if k == 100 {
                inside += 1;
            }
        }
    }
    println!("{inside}");
}
