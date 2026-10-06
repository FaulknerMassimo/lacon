//! Golden tests over `tests/`:
//! - `tests/run/X.lc`: `lacon run` stdout+stderr must equal `X.out`
//! - `tests/check/X.lc`: `lacon check` output must equal `X.out`
//! - `tests/unit/X.lc`: `lacon test` output must equal `X.out`
//! - `tests/run/X.lc` again, built with `lacon build`: the native program's
//!   output must equal the same `X.out` (skipped without a C compiler)
//!
//! Regenerate expectations with `BLESS=1 cargo test`.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests")
}

fn check_dir(dir: &str, cmd: &str) {
    let dir = root().join(dir);
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "lc"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no tests in {}", dir.display());
    let bless = std::env::var("BLESS").is_ok();
    let mut failures = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_str().unwrap().to_string();
        let out = Command::new(env!("CARGO_BIN_EXE_lacon"))
            .arg(cmd)
            .arg(&name)
            .current_dir(&dir)
            .output()
            .expect("run lacon");
        let mut got = String::from_utf8_lossy(&out.stdout).into_owned();
        got.push_str(&String::from_utf8_lossy(&out.stderr));
        got.push_str(&format!("[exit {}]\n", out.status.code().unwrap_or(-1)));
        let expect_path = f.with_extension("out");
        if bless {
            std::fs::write(&expect_path, &got).unwrap();
            continue;
        }
        let want = std::fs::read_to_string(&expect_path).unwrap_or_default();
        if got != want {
            failures.push(format!("--- {name}: want\n{want}--- got\n{got}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn run_programs() {
    check_dir("run", "run");
}

#[test]
fn check_diagnostics() {
    check_dir("check", "check");
}

#[test]
fn inline_tests() {
    check_dir("unit", "test");
}

#[test]
fn native_programs() {
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    if Command::new(&cc).arg("--version").output().is_err() {
        eprintln!("no C compiler; skipping native builds");
        return;
    }
    let dir = root().join("run");
    let out_dir = std::env::temp_dir().join(format!("lacon-native-{}", std::process::id()));
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "lc")).collect();
    files.sort();
    let mut failures = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_str().unwrap().to_string();
        let exe = out_dir.join(f.file_stem().unwrap());
        let b = Command::new(env!("CARGO_BIN_EXE_lacon")).args(["build", &name, "-o"]).arg(&exe).current_dir(&dir).output().expect("run lacon");
        if !b.status.success() {
            failures.push(format!("--- {name}: build failed\n{}", String::from_utf8_lossy(&b.stderr)));
            continue;
        }
        let out = Command::new(&exe).current_dir(&dir).output().expect("run native program");
        let mut got = String::from_utf8_lossy(&out.stdout).into_owned();
        got.push_str(&String::from_utf8_lossy(&out.stderr));
        got.push_str(&format!("[exit {}]\n", out.status.code().unwrap_or(-1)));
        let want = std::fs::read_to_string(f.with_extension("out")).unwrap_or_default();
        if got != want {
            failures.push(format!("--- {name}: want\n{want}--- got\n{got}"));
        }
    }
    let _ = std::fs::remove_dir_all(&out_dir);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
