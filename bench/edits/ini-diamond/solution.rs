use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Read};

type Sections = HashMap<String, BTreeMap<String, String>>;
type Seen = HashSet<(String, String)>;

/// A value in double quotes keeps the spaces inside them.
fn unquote(v: &str) -> String {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        return v[1..v.len() - 1].to_string();
    }
    v.to_string()
}

/// The document's sections, {section: {key: value}} with "" for entries
/// before the first header, and the sections in the order they first
/// appear. Prints an error for each line that is none of the forms.
fn parse_doc(lines: &[&str]) -> (Sections, Vec<String>) {
    let mut secs = Sections::new();
    secs.insert(String::new(), BTreeMap::new());
    let mut order = Vec::new();
    let mut section = String::new();
    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') && line.len() >= 2 {
            section = line[1..line.len() - 1].trim().to_string();
            if !secs.contains_key(&section) {
                secs.insert(section.clone(), BTreeMap::new());
                order.push(section.clone());
            }
            continue;
        }
        match line.split_once('=') {
            Some((key, value)) if !key.trim().is_empty() => {
                secs.get_mut(&section).unwrap().insert(key.trim().to_string(), unquote(value.trim()));
            }
            _ => println!("line {}: bad line", i + 1),
        }
    }
    (secs, order)
}

/// The (section, key) a reference names: `key` is in section, `s.key` in s,
/// and `.key` outside any section.
fn split_ref(section: &str, r: &str) -> (String, String) {
    match r.split_once('.') {
        Some((sec, key)) => (sec.to_string(), key.to_string()),
        None => (section.to_string(), r.to_string()),
    }
}

/// value with each @{...} reference replaced by what it names, and @@ by @.
/// seen holds the entries being expanded, to catch a cycle.
fn expand(secs: &Sections, section: &str, value: &str, seen: &mut Seen) -> String {
    let cs: Vec<char> = value.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < cs.len() {
        if cs[i] == '@' && cs.get(i + 1) == Some(&'@') {
            out.push('@');
            i += 2;
            continue;
        }
        if cs[i] == '@' && cs.get(i + 1) == Some(&'{') {
            if let Some(end) = (i..cs.len()).find(|&j| cs[j] == '}') {
                let r: String = cs[i + 2..end].iter().collect();
                out += &lookup(secs, section, &r, seen);
                i = end + 1;
                continue;
            }
        }
        out.push(cs[i]);
        i += 1;
    }
    out
}

/// What a reference names, expanded in turn. A reference to a missing entry
/// stays as written, and one that leads back to an entry being expanded is
/// <cycle>.
fn lookup(secs: &Sections, section: &str, r: &str, seen: &mut Seen) -> String {
    let (sec, key) = split_ref(section, r);
    let Some(value) = secs.get(&sec).and_then(|s| s.get(&key)) else {
        return format!("@{{{}}}", r);
    };
    if seen.contains(&(sec.clone(), key.clone())) {
        return "<cycle>".to_string();
    }
    seen.insert((sec.clone(), key.clone()));
    let out = expand(secs, &sec, value, seen);
    seen.remove(&(sec, key));
    out
}

/// The (section, key) of a query's SECTION.KEY or KEY.
fn split_name(name: &str) -> (String, String) {
    match name.split_once('.') {
        Some((sec, key)) => (sec.to_string(), key.to_string()),
        None => (String::new(), name.to_string()),
    }
}

/// The answer to one query.
fn run_query(secs: &Sections, order: &[String], query: &str) -> String {
    let parts: Vec<&str> = query.split_whitespace().collect();
    if parts == ["sections"] {
        return order.join(" ");
    }
    if parts.first() == Some(&"keys") && parts.len() <= 2 {
        let sec = parts.get(1).copied().unwrap_or("");
        return match secs.get(sec) {
            Some(keys) => keys.keys().cloned().collect::<Vec<_>>().join(", "),
            None => "missing".to_string(),
        };
    }
    if parts.len() != 2 {
        return "bad query".to_string();
    }
    let (sec, key) = split_name(parts[1]);
    let found = secs.get(&sec).and_then(|s| s.get(&key));
    match parts[0] {
        "has" => if found.is_some() { "yes" } else { "no" }.to_string(),
        "raw" => found.cloned().unwrap_or_else(|| "missing".to_string()),
        "get" => match found {
            None => "missing".to_string(),
            Some(value) => {
                let mut seen = Seen::new();
                seen.insert((sec.clone(), key.clone()));
                expand(secs, &sec, value, &mut seen)
            }
        },
        _ => "bad query".to_string(),
    }
}

fn main() {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input).unwrap();
    let lines: Vec<&str> = input.lines().collect();
    let split = lines.iter().position(|l| *l == "---").unwrap_or(lines.len());
    let (secs, order) = parse_doc(&lines[..split]);
    for query in lines.iter().skip(split + 1) {
        if !query.trim().is_empty() {
            println!("{}", run_query(&secs, &order, query));
        }
    }
}
