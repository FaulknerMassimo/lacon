"""Run task solutions against their hidden tests.

    uv run bench/tasks/check.py                 # every task, every solution file
    uv run bench/tasks/check.py rpn grid-path   # selected tasks
    uv run bench/tasks/check.py --bless rpn     # rewrite tests/N.out from ref.py

A task is a directory with `prompt.md`, `tests/N.in` + `tests/N.out`, and any
number of solutions: `solution.lc`, `solution.py`, `solution.rs`,
`solution.go`. `ref.py` is the reference that generated the expected outputs.
Programs read stdin and write stdout; output is compared exactly, ignoring
trailing whitespace on each line and trailing blank lines.
"""
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
TIMEOUT = 10


_lacon: str | None = None


def lacon() -> str:
    global _lacon
    if _lacon is None:
        build = ["build", "-q", "--release", "-p", "lacon-cli"]
        r = subprocess.run(["cargo", *build], cwd=ROOT, capture_output=True)
        # A version manager's `cargo` shim can be present but unconfigured.
        rustup_cargo = Path.home() / ".cargo" / "bin" / "cargo"
        if r.returncode != 0 and rustup_cargo.exists():
            r = subprocess.run([str(rustup_cargo), *build], cwd=ROOT, capture_output=True)
        r.check_returncode()
        _lacon = str(ROOT / "target" / "release" / "lacon")
    return _lacon


def command(sol: Path, build_dir: Path) -> list[str] | None:
    """The command that runs a solution, compiling it first if needed."""
    ext = sol.suffix
    if ext == ".lc":
        return [lacon(), "run", str(sol)]
    if ext == ".py":
        return [sys.executable, str(sol)]
    if ext == ".rs":
        exe = build_dir / "rs"
        subprocess.run(["rustc", "-O", "-o", str(exe), str(sol)], check=True, capture_output=True)
        return [str(exe)]
    if ext == ".go":
        exe = build_dir / "go"
        subprocess.run(["go", "build", "-o", str(exe), str(sol)], check=True, capture_output=True)
        return [str(exe)]
    return None


def norm(s: str) -> str:
    return "\n".join(line.rstrip() for line in s.rstrip().split("\n"))


def tests_of(task: Path) -> list[Path]:
    return sorted(task.glob("tests/*.in"), key=lambda p: int(p.stem))


def bless(task: Path) -> None:
    """Regenerate the expected outputs by running the reference."""
    for t in tests_of(task):
        r = subprocess.run([sys.executable, str(task / "ref.py")], stdin=t.open(), capture_output=True, text=True, timeout=TIMEOUT, check=True)
        t.with_suffix(".out").write_text(r.stdout)
    print(f"{task.name}: blessed {len(tests_of(task))} tests")


def run_task(task: Path) -> bool:
    tests = tests_of(task)
    ok = True
    for sol in sorted(p for p in task.iterdir() if p.stem in ("solution", "ref")):
        with tempfile.TemporaryDirectory() as tmp:
            try:
                cmd = command(sol, Path(tmp))
            except subprocess.CalledProcessError as e:
                print(f"{task.name}/{sol.name}: build failed\n{(e.stderr or b'').decode()[:2000]}")
                ok = False
                continue
            if cmd is None:
                continue
            failed = []
            for t in tests:
                try:
                    r = subprocess.run(cmd, stdin=t.open(), capture_output=True, text=True, timeout=TIMEOUT)
                    got = r.stdout
                except subprocess.TimeoutExpired:
                    got, r = "(timeout)", None
                want = t.with_suffix(".out").read_text()
                if norm(got) != norm(want):
                    detail = r.stderr.strip().splitlines()[:3] if r else []
                    failed.append(f"  test {t.stem}: want {norm(want)[:200]!r} got {norm(got)[:200]!r} {' '.join(detail)}")
            status = "ok" if not failed else f"FAIL {len(failed)}/{len(tests)}"
            print(f"{task.name}/{sol.name}: {status}")
            for f in failed:
                print(f)
            ok &= not failed
    return ok


def main() -> int:
    names = [a for a in sys.argv[1:] if a != "--bless"]
    tasks = [HERE / n for n in names] if names else sorted(p for p in HERE.iterdir() if (p / "prompt.md").exists())
    if "--bless" in sys.argv:
        for t in tasks:
            bless(t)
    results = [run_task(t) for t in tasks]
    return 0 if all(results) else 1


if __name__ == "__main__":
    sys.exit(main())
