use std::collections::HashMap;
use std::io::{self, Read};

struct Req {
    client: String,
    hour: usize,
    method: String,
    path: String,
    status: u32,
    size: u64,
    ms: Option<u64>, // how long the request took, or None if the line didn't say
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// The hour of a timestamp like 10/Oct/2000:13:55:36 -0700, or None.
fn parse_hour(stamp: &str) -> Option<usize> {
    let (date, clock) = stamp.split_once(':').unwrap_or((stamp, ""));
    let parts: Vec<&str> = clock.split(':').collect();
    if date.split('/').count() != 3 || parts.len() != 3 {
        return None;
    }
    let hh = parts[0];
    if hh.len() != 2 || !is_digits(hh) {
        return None;
    }
    let h: usize = hh.parse().ok()?;
    if h > 23 {
        return None;
    }
    Some(h)
}

/// (method, path) from a request line like GET /a?x=1 HTTP/1.0, or None.
fn parse_request(text: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = text.split(' ').collect();
    if parts.len() != 3 || parts.contains(&"") {
        return None;
    }
    let path = parts[1].split('?').next().unwrap();
    Some((parts[0].to_string(), path.to_string()))
}

/// The request on one log line, or None if the line is malformed:
///
/// 127.0.0.1 - frank [10/Oct/2000:13:55:36 -0700] "GET /a.gif HTTP/1.0" 200 2326 85
///
/// The last field, the time taken in milliseconds, is optional.
fn parse_line(line: &str) -> Option<Req> {
    let start = line.find('[')?;
    let end = line.find(']')?;
    if end < start {
        return None;
    }
    // the client, two unused fields, then the space before '['
    let head: Vec<&str> = line[..start].split(' ').collect();
    if head.len() != 4 || !head[3].is_empty() || head[..3].contains(&"") {
        return None;
    }
    let hour = parse_hour(&line[start + 1..end])?;
    let rest = &line[end + 1..];
    if !rest.starts_with(" \"") {
        return None;
    }
    let q = rest[2..].find('"')? + 2;
    let (method, path) = parse_request(&rest[2..q])?;
    let tail: Vec<&str> = rest[q + 1..].split(' ').collect();
    if !(tail.len() == 3 || tail.len() == 4) || !tail[0].is_empty() {
        return None;
    }
    let mut ms = None;
    if tail.len() == 4 {
        if !is_digits(tail[3]) {
            return None;
        }
        ms = Some(tail[3].parse().ok()?);
    }
    let (status, size) = (tail[1], tail[2]);
    if status.len() != 3 || !is_digits(status) {
        return None;
    }
    if size != "-" && !is_digits(size) {
        return None;
    }
    Some(Req {
        client: head[0].to_string(),
        hour,
        method,
        path,
        status: status.parse().ok()?,
        size: if size == "-" { 0 } else { size.parse().ok()? },
        ms,
    })
}

/// Up to n (key, count) pairs, by count descending and then by key.
fn top(counts: &HashMap<String, u64>, n: usize) -> Vec<(String, u64)> {
    let mut pairs: Vec<(String, u64)> = counts.iter().map(|(k, v)| (k.clone(), *v)).collect();
    pairs.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    pairs.truncate(n);
    pairs
}

fn report_totals(reqs: &[Req]) {
    println!("requests: {}", reqs.len());
    println!("bytes: {}", reqs.iter().map(|r| r.size).sum::<u64>());
}

fn report_status(reqs: &[Req]) {
    let mut classes = std::collections::BTreeMap::new();
    for r in reqs {
        *classes.entry(r.status / 100).or_insert(0) += 1;
    }
    for (c, n) in classes {
        println!("status {}xx: {}", c, n);
    }
}

fn report_methods(reqs: &[Req]) {
    let mut counts = HashMap::new();
    for r in reqs {
        *counts.entry(r.method.clone()).or_insert(0) += 1;
    }
    let parts: Vec<String> = top(&counts, counts.len()).iter().map(|(m, n)| format!("{} {}", m, n)).collect();
    println!("methods: {}", parts.join(", "));
}

fn report_paths(reqs: &[Req]) {
    let mut counts = HashMap::new();
    for r in reqs {
        *counts.entry(r.path.clone()).or_insert(0) += 1;
    }
    println!("top paths:");
    for (path, n) in top(&counts, 3) {
        println!("  {} {}", path, n);
    }
}

/// Paths by their average time, over the requests that have one.
fn report_slowest(reqs: &[Req]) {
    let mut total: HashMap<String, u64> = HashMap::new();
    let mut timed: HashMap<String, u64> = HashMap::new();
    for r in reqs {
        let Some(ms) = r.ms else { continue };
        *total.entry(r.path.clone()).or_insert(0) += ms;
        *timed.entry(r.path.clone()).or_insert(0) += 1;
    }
    let average: HashMap<String, u64> = total.iter().map(|(path, t)| (path.clone(), t / timed[path])).collect();
    println!("slowest paths:");
    for (path, ms) in top(&average, 3) {
        println!("  {} {}ms", path, ms);
    }
}

fn report_errors(reqs: &[Req]) {
    let mut counts = HashMap::new();
    for r in reqs {
        if r.status >= 400 {
            *counts.entry(r.path.clone()).or_insert(0) += 1;
        }
    }
    println!("error paths:");
    for (path, n) in top(&counts, 3) {
        println!("  {} {}", path, n);
    }
}

fn report_clients(reqs: &[Req]) {
    let mut counts = HashMap::new();
    let mut sizes: HashMap<String, u64> = HashMap::new();
    for r in reqs {
        *counts.entry(r.client.clone()).or_insert(0) += 1;
        *sizes.entry(r.client.clone()).or_insert(0) += r.size;
    }
    println!("top clients:");
    for (client, n) in top(&counts, 3) {
        println!("  {} {} requests, {} bytes", client, n, sizes[&client]);
    }
}

fn report_hours(reqs: &[Req]) {
    if reqs.is_empty() {
        println!("busiest hour: none");
        return;
    }
    let mut counts = [0u64; 24];
    for r in reqs {
        counts[r.hour] += 1;
    }
    let mut best = 0;
    for h in 0..24 {
        if counts[h] > counts[best] {
            best = h;
        }
    }
    println!("busiest hour: {:02} ({} requests)", best, counts[best]);
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let mut reqs = Vec::new();
    let mut malformed = 0;
    for line in input.lines() {
        if line.trim().is_empty() {
            continue;
        }
        match parse_line(line) {
            Some(r) => reqs.push(r),
            None => malformed += 1,
        }
    }
    report_totals(&reqs);
    report_status(&reqs);
    report_methods(&reqs);
    report_paths(&reqs);
    report_slowest(&reqs);
    report_errors(&reqs);
    report_clients(&reqs);
    report_hours(&reqs);
    println!("malformed: {}", malformed);
}
