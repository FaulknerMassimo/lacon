//! Item ranges for `lacon put` and `lacon q def`.

use lacon_syntax::items::scan;

fn items(src: &str) -> Vec<(String, String)> {
    scan(src).into_iter().map(|it| (it.label(), src[it.doc..it.end].to_string())).collect()
}

#[test]
fn brackets_closed_at_column_zero_stay_in_their_item() {
    let src = "type Point {\n  x int,\n  y int\n}\n\nNAMES = [\n  \"a\",\n]\n\nfn f() = 1\n";
    let got = items(src);
    assert_eq!(got[0], ("type Point".into(), "type Point {\n  x int,\n  y int\n}".into()));
    assert_eq!(got[1], ("NAMES".into(), "NAMES = [\n  \"a\",\n]".into()));
    assert_eq!(got[2], ("fn f".into(), "fn f() = 1".into()));
}

#[test]
fn doc_comments_go_with_the_item_below() {
    let src = "fn a() = 1\n\n# ---- section ----\n\n# Doc for b.\nfn b() =\n  x = 2\n  # trailing, indented\n  x\n\ntest \"b is 2\": b() == 2\n";
    let got = items(src);
    assert_eq!(got[0], ("fn a".into(), "fn a() = 1".into()));
    assert_eq!(got[1], ("fn b".into(), "# Doc for b.\nfn b() =\n  x = 2\n  # trailing, indented\n  x".into()));
    assert_eq!(got[2], ("test \"b is 2\"".into(), "test \"b is 2\": b() == 2".into()));
}

#[test]
fn a_file_that_does_not_parse_still_has_items() {
    let src = "def foo(x):\n    return x\n\nfn bar() =\n  print(1\n";
    let got = items(src);
    assert_eq!(got[0], ("? def".into(), "def foo(x):\n    return x".into()));
    assert_eq!(got[1], ("fn bar".into(), "fn bar() =\n  print(1".into()));
}
