use lacon_syntax::{parse, Source};

fn diags(src: &str) -> Vec<String> {
    let (_, d) = parse(src);
    let s = Source::new("t.lc", src);
    d.iter().map(|d| d.render(&s)).collect()
}

#[test]
fn benchmark_program_parses() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../bench/tokens/users.lc")).unwrap();
    assert_eq!(diags(&src), Vec::<String>::new());
}

#[test]
fn layout_forms_parse() {
    let src = r#"
enum Shape = Circle(f64) | Rect(f64, f64)

enum Tok =
  | Num(int)
  | Plus

fn area(s Shape) f64 = match s
  Circle(r): 3.14159 * r * r
  Rect(w, h): w * h

fn f(x int) int =
  y = if x > 0: 1 else: -1
  z = if x > 2:
    10
  elif x > 1: 5
  else:
    0
  total = xs
    .filter(it > 0)
    .map(it * 2)
  y + z + total.len

test "area": area(Rect(2.0, 3.0)) == 6.0
"#;
    assert_eq!(diags(src), Vec::<String>::new());
}

#[test]
fn foreign_syntax_hints() {
    let d = diags("fn main() =\n  let x = 5\n  print(x)\n");
    assert!(d[0].starts_with("E0120 t.lc:2:3"), "{d:?}");
    assert!(d[0].ends_with("| fix: x = 5"), "{d:?}");
    let d = diags("fn main() {\n  print(1)\n}\n");
    assert!(d[0].starts_with("E0123"), "{d:?}");
    let d = diags("fn main() =\n  x = 1 // two\n");
    assert!(d[0].starts_with("E0124"), "{d:?}");
}
