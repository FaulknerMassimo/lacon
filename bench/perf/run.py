"""Time Lacon programs compiled with `lacon build` against the same programs in Rust.

    uv run bench/perf/run.py                # every benchmark, best of 3 runs
    uv run bench/perf/run.py fib sieve      # selected benchmarks
    uv run bench/perf/run.py --interp       # also time the interpreter (one run)
    uv run bench/perf/run.py --runs 5

Each benchmark is `X.lc` and `X.rs`, which must print the same thing; a
mismatch is reported instead of a time. Rust is compiled with `rustc -O`.
"""
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent


def lacon() -> str:
    subprocess.run(["cargo", "build", "-q", "--release", "-p", "lacon-cli"], cwd=ROOT, check=True)
    return str(ROOT / "target" / "release" / "lacon")


def timed(cmd: list[str], runs: int) -> tuple[float, str]:
    best, out = float("inf"), ""
    for _ in range(runs):
        start = time.perf_counter()
        r = subprocess.run(cmd, capture_output=True, text=True, check=True)
        best = min(best, time.perf_counter() - start)
        out = r.stdout
    return best, out


def main() -> None:
    args = sys.argv[1:]
    interp = "--interp" in args
    runs = 3
    if "--runs" in args:
        runs = int(args[args.index("--runs") + 1])
        args = [a for a in args if a != str(runs)]
    names = [a for a in args if not a.startswith("--")] or sorted(p.stem for p in HERE.glob("*.lc"))
    lc = lacon()
    have_rust = shutil.which("rustc") is not None
    cols = ["benchmark", "native", "rust", "native / rust"] + (["interpreter"] if interp else [])
    print("| " + " | ".join(cols) + " |")
    print("|" + "---|" * len(cols))
    with tempfile.TemporaryDirectory() as tmp:
        for name in names:
            src = HERE / f"{name}.lc"
            exe = Path(tmp) / name
            subprocess.run([lc, "build", str(src), "-o", str(exe)], check=True)
            t_native, out = timed([str(exe)], runs)
            row = [name, f"{t_native:.3f} s"]
            rs = HERE / f"{name}.rs"
            if have_rust and rs.exists():
                rexe = Path(tmp) / f"{name}-rs"
                subprocess.run(["rustc", "-O", "-o", str(rexe), str(rs)], check=True)
                t_rust, rout = timed([str(rexe)], runs)
                if rout != out:
                    row += ["output differs", "-"]
                else:
                    row += [f"{t_rust:.3f} s", f"{t_native / t_rust:.1f}x"]
            else:
                row += ["-", "-"]
            if interp:
                t_interp, iout = timed([lc, "run", str(src)], 1)
                row.append(f"{t_interp:.3f} s" if iout == out else "output differs")
            print("| " + " | ".join(row) + " |", flush=True)


if __name__ == "__main__":
    main()
