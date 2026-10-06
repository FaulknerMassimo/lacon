"""Toolchains for the benchmark languages: build a single-file program, then
run it on some input, inside a sandbox when one is available.

Programs are written by a model, so they run under bubblewrap (`bwrap`) when it
is installed: the root filesystem is read-only, only the program's own
directory is writable, and there is no network. Set LACON_BENCH_NO_SANDBOX=1 to
run them directly.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import sys
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent.parent
RUN_TIMEOUT = 10
BUILD_TIMEOUT = 120
OUTPUT_LIMIT = 1 << 20


@dataclass
class Result:
    exit_code: int | None  # None: timed out
    stdout: str
    stderr: str


@dataclass
class Build:
    ok: bool
    errors: str = ""
    cmd: list[str] = field(default_factory=list)


def _works(cmd: list[str]) -> bool:
    try:
        return subprocess.run(cmd, capture_output=True, timeout=30).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def find_tool(name: str, *fallbacks: Path) -> str | None:
    """A working executable: on PATH (version managers' shims can be present
    but unconfigured, so it must answer `--version`), else a fallback."""
    for cand in [shutil.which(name), *map(str, fallbacks)]:
        if cand and Path(cand).exists() and _works([cand, "--version"] if name != "go" else [cand, "version"]):
            return cand
    return None


CARGO_BIN = Path.home() / ".cargo" / "bin"


def _sandbox_ok() -> bool:
    if os.environ.get("LACON_BENCH_NO_SANDBOX") or not shutil.which("bwrap"):
        return False
    return _works(["bwrap", "--ro-bind", "/", "/", "--dev", "/dev", "--unshare-all", "true"])


SANDBOX = _sandbox_ok()


def sandboxed(cmd: list[str], workdir: Path, stdin: str = "", timeout: int = RUN_TIMEOUT, env: dict | None = None) -> Result:
    """Runs `cmd` in `workdir`, which is the only writable place."""
    if SANDBOX:
        cmd = [
            "bwrap", "--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp",
            "--bind", str(workdir), str(workdir), "--unshare-all", "--die-with-parent", "--chdir", str(workdir), *cmd,
        ]
    try:
        r = subprocess.run(cmd, input=stdin.encode(), capture_output=True, timeout=timeout, cwd=workdir, env=env)
    except subprocess.TimeoutExpired as e:
        return Result(None, (e.stdout or b"")[:OUTPUT_LIMIT].decode(errors="replace"), (e.stderr or b"")[:OUTPUT_LIMIT].decode(errors="replace"))
    return Result(r.returncode, r.stdout[:OUTPUT_LIMIT].decode(errors="replace"), r.stderr[:OUTPUT_LIMIT].decode(errors="replace"))


class Lang:
    key = ""
    name = ""
    ext = ""
    # One line telling the model what it is writing for.
    environment = ""

    def unavailable(self) -> str | None:
        """Why this language can't run here, or None."""
        return None

    def build(self, src: Path) -> Build:
        raise NotImplementedError

    def run(self, build: Build, workdir: Path, stdin: str) -> Result:
        return sandboxed(build.cmd, workdir, stdin)


class Lacon(Lang):
    key, name, ext = "lacon", "Lacon", ".lc"
    environment = "Lacon. The language is described below; no other documentation is available."

    def __init__(self, build: bool = True) -> None:
        self.exe = os.environ.get("LACON") or str(ROOT / "target" / "release" / "lacon")
        self.error: str | None = None
        cargo = find_tool("cargo", CARGO_BIN / "cargo") if build else None
        if cargo:
            r = subprocess.run([cargo, "build", "-q", "--release", "-p", "lacon-cli"], cwd=ROOT, capture_output=True, text=True)
            if r.returncode != 0 and not Path(self.exe).exists():
                self.error = "cargo build failed: " + r.stderr[-500:]
        if not Path(self.exe).exists():
            self.error = self.error or f"no lacon binary at {self.exe}"

    def unavailable(self) -> str | None:
        return self.error

    def build(self, src: Path) -> Build:
        r = sandboxed([self.exe, "check", src.name], src.parent, timeout=BUILD_TIMEOUT)
        if r.exit_code != 0:
            return Build(False, (r.stdout + r.stderr).strip())
        return Build(True, cmd=[self.exe, "run", src.name])


class Python(Lang):
    key, name, ext = "python", "Python", ".py"

    def __init__(self) -> None:
        self.exe = sys.executable
        v = sys.version_info
        self.environment = f"Python {v.major}.{v.minor}, standard library only."

    def build(self, src: Path) -> Build:
        r = sandboxed([self.exe, "-m", "py_compile", src.name], src.parent, timeout=BUILD_TIMEOUT)
        if r.exit_code != 0:
            return Build(False, (r.stdout + r.stderr).strip())
        return Build(True, cmd=[self.exe, src.name])


class Rust(Lang):
    key, name, ext = "rust", "Rust", ".rs"

    def __init__(self) -> None:
        self.exe = find_tool("rustc", CARGO_BIN / "rustc")
        version = ""
        if self.exe:
            version = subprocess.run([self.exe, "--version"], capture_output=True, text=True).stdout.split()[1]
        self.environment = f"Rust {version}, standard library only (no crates), one file built with `rustc -O --edition 2021 main.rs`."

    def unavailable(self) -> str | None:
        return None if self.exe else "rustc not found"

    def build(self, src: Path) -> Build:
        r = sandboxed([self.exe, "-O", "--edition", "2021", "-o", "prog", src.name], src.parent, timeout=BUILD_TIMEOUT)
        if r.exit_code != 0:
            return Build(False, (r.stdout + r.stderr).strip())
        return Build(True, cmd=["./prog"])


class Go(Lang):
    key, name, ext = "go", "Go", ".go"

    def __init__(self) -> None:
        self.exe = find_tool("go")
        version = ""
        if self.exe:
            version = subprocess.run([self.exe, "version"], capture_output=True, text=True).stdout.split()[2].removeprefix("go")
        self.environment = f"Go {version}, standard library only, one file `main.go` (package main) built with `go build`."

    def unavailable(self) -> str | None:
        return None if self.exe else "go not found"

    def build(self, src: Path) -> Build:
        env = {**os.environ, "GOCACHE": str(src.parent / ".gocache"), "GOPATH": str(src.parent / ".gopath"), "GOTELEMETRY": "off"}
        r = sandboxed([self.exe, "build", "-o", "prog", src.name], src.parent, timeout=BUILD_TIMEOUT, env=env)
        if r.exit_code != 0:
            return Build(False, (r.stdout + r.stderr).strip())
        return Build(True, cmd=["./prog"])


LANGS: dict[str, type[Lang]] = {"lacon": Lacon, "python": Python, "rust": Rust, "go": Go}


def norm(s: str) -> str:
    """Output comparison ignores trailing spaces on lines and trailing blank lines."""
    return "\n".join(line.rstrip() for line in s.rstrip().split("\n"))
