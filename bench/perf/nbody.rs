#[derive(Clone, Copy)]
struct Body { x: f64, y: f64, z: f64, vx: f64, vy: f64, vz: f64, m: f64 }

fn energy(bs: &[Body]) -> f64 {
    let mut e = 0.0;
    for i in 0..bs.len() {
        let b = bs[i];
        e += 0.5 * b.m * (b.vx * b.vx + b.vy * b.vy + b.vz * b.vz);
        for j in i + 1..bs.len() {
            let c = bs[j];
            let (dx, dy, dz) = (b.x - c.x, b.y - c.y, b.z - c.z);
            e -= b.m * c.m / (dx * dx + dy * dy + dz * dz).sqrt();
        }
    }
    e
}

fn advance(bs: &mut Vec<Body>, dt: f64) {
    let n = bs.len();
    for i in 0..n {
        for j in i + 1..n {
            let dx = bs[i].x - bs[j].x;
            let dy = bs[i].y - bs[j].y;
            let dz = bs[i].z - bs[j].z;
            let d2 = dx * dx + dy * dy + dz * dz;
            let mag = dt / (d2 * d2.sqrt());
            let mj = bs[j].m * mag;
            let mi = bs[i].m * mag;
            bs[i].vx -= dx * mj; bs[i].vy -= dy * mj; bs[i].vz -= dz * mj;
            bs[j].vx += dx * mi; bs[j].vy += dy * mi; bs[j].vz += dz * mi;
        }
    }
    for i in 0..n {
        bs[i].x += dt * bs[i].vx; bs[i].y += dt * bs[i].vy; bs[i].z += dt * bs[i].vz;
    }
}

fn main() {
    let n: i64 = 6_000_000;
    let pi = 3.141592653589793; let sm = 4.0 * pi * pi; let dp = 365.24;
    let mut bs = vec![
        Body{x:0.0,y:0.0,z:0.0,vx:0.0,vy:0.0,vz:0.0,m:sm},
        Body{x:4.84143144246472090e+00,y:-1.16032004402742839e+00,z:-1.03622044471123109e-01,vx:1.66007664274403694e-03*dp,vy:7.69901118419740425e-03*dp,vz:-6.90460016972063023e-05*dp,m:9.54791938424326609e-04*sm},
        Body{x:8.34336671824457987e+00,y:4.12479856412430479e+00,z:-4.03523417114321381e-01,vx:-2.76742510726862411e-03*dp,vy:4.99852801234917238e-03*dp,vz:2.30417297573763929e-05*dp,m:2.85885980666130812e-04*sm},
        Body{x:1.28943695621391310e+01,y:-1.51111514016986312e+01,z:-2.23307578892655734e-01,vx:2.96460137564761618e-03*dp,vy:2.37847173959480950e-03*dp,vz:-2.96589568540237556e-05*dp,m:4.36624404335156298e-05*sm},
        Body{x:1.53796971148509165e+01,y:-2.59193146099879641e+01,z:1.79258772950371181e-01,vx:2.68067772490389322e-03*dp,vy:1.62824170038242295e-03*dp,vz:-9.51592254519715870e-05*dp,m:5.15138902046611451e-05*sm},
    ];
    let (mut px, mut py, mut pz) = (0.0, 0.0, 0.0);
    for b in &bs { px += b.vx * b.m; py += b.vy * b.m; pz += b.vz * b.m; }
    bs[0].vx = -px / sm; bs[0].vy = -py / sm; bs[0].vz = -pz / sm;
    println!("{:.9}", energy(&bs));
    for _ in 0..n { advance(&mut bs, 0.01); }
    println!("{:.9}", energy(&bs));
}
