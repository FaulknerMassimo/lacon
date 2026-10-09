//! Compiling generated C: the runtime is compiled once per version and
//! cached; each program is compiled and linked against it.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

fn cc() -> String {
    std::env::var("CC").unwrap_or_else(|_| "cc".into())
}

/// No fused multiply-add: unboxed float arithmetic must round as the
/// interpreter's does.
const CFLAGS: &[&str] = &["-std=gnu11", "-D_GNU_SOURCE", "-w", "-ffp-contract=off"];
/// Both get `-O3`. `-O1` builds programs in about 60% of `-O2`'s time, but
/// on unboxed code it runs mandel at twice `-O2`'s time and collatz at
/// 1.2x. `-O3` builds 60 agent programs in 1.03x `-O2`'s median time, and
/// runs fib in 0.6x: it inlines a recursive call into itself.
const RUNTIME_OPT: &str = "-O3";
const PROGRAM_OPT: &str = "-O3";

/// Extra C compiler flags from `LACON_CFLAGS` (e.g. `-fsanitize=address`).
fn extra() -> Vec<String> {
    std::env::var("LACON_CFLAGS").map(|s| s.split_whitespace().map(String::from).collect()).unwrap_or_default()
}

fn cache_dir() -> PathBuf {
    if let Ok(d) = std::env::var("LACON_CACHE") {
        return PathBuf::from(d);
    }
    let base = std::env::var("XDG_CACHE_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        std::env::var("HOME").map(|h| PathBuf::from(h).join(".cache")).unwrap_or_else(|_| std::env::temp_dir())
    });
    base.join("lacon")
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("cannot run {:?}: {e}", cmd.get_program()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let lines: Vec<&str> = err.lines().take(20).collect();
        return Err(format!("{:?} failed:\n{}", cmd.get_program(), lines.join("\n")));
    }
    Ok(())
}

/// The runtime's object files, compiled if the cache doesn't have them.
fn runtime(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    lacon_cgen::HEADER.hash(&mut h);
    for (name, text) in lacon_cgen::RUNTIME {
        name.hash(&mut h);
        text.hash(&mut h);
    }
    cc().hash(&mut h);
    CFLAGS.hash(&mut h);
    RUNTIME_OPT.hash(&mut h);
    extra().hash(&mut h);
    let rt = dir.join(format!("rt-{:016x}", h.finish()));
    let objs: Vec<PathBuf> = lacon_cgen::RUNTIME.iter().map(|(n, _)| rt.join(n.replace(".c", ".o"))).collect();
    if objs.iter().all(|o| o.exists()) {
        return Ok(objs);
    }
    // Build in a fresh directory, then move it into place, so concurrent
    // builds never see half-written objects.
    let tmp = dir.join(format!("tmp-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create {}: {e}", tmp.display()))?;
    std::fs::write(tmp.join("lacon.h"), lacon_cgen::HEADER).map_err(|e| e.to_string())?;
    for (name, text) in lacon_cgen::RUNTIME {
        let c = tmp.join(name);
        std::fs::write(&c, text).map_err(|e| e.to_string())?;
        run(Command::new(cc()).args(CFLAGS).arg(RUNTIME_OPT).args(extra()).arg("-c").arg(&c).arg("-o").arg(tmp.join(name.replace(".c", ".o"))))?;
    }
    if std::fs::rename(&tmp, &rt).is_err() {
        let _ = std::fs::remove_dir_all(&tmp);
    }
    Ok(objs)
}

pub fn compile(c: &str, out: &str, keep_c: bool) -> Result<(), String> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let objs = runtime(&dir)?;
    let work = std::env::temp_dir().join(format!("lacon-build-{}", std::process::id()));
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    std::fs::write(work.join("lacon.h"), lacon_cgen::HEADER).map_err(|e| e.to_string())?;
    let prog_c = work.join("prog.c");
    std::fs::write(&prog_c, c).map_err(|e| e.to_string())?;
    if keep_c {
        std::fs::write(format!("{out}.c"), c).map_err(|e| e.to_string())?;
    }
    let r = run(Command::new(cc()).args(CFLAGS).arg(PROGRAM_OPT).args(extra()).arg(&prog_c).args(&objs).arg("-lm").arg("-lpthread").arg("-o").arg(out));
    let _ = std::fs::remove_dir_all(&work);
    r
}
