#!/usr/bin/env python3
"""Builds the large-scale mode's client library (rust/halo-client) as a 32-bit
static library, for the Linux and Windows builds to link:

    python tools/rust_client.py --target i686-unknown-linux-gnu --output build/linux/libhalo_client.a

The output is only rewritten when the library changed, so that ninja (restat)
does not relink the game when cargo found nothing to do. The Rust toolchain
needs the target (``rustup target add i686-unknown-linux-gnu``, or
``i686-pc-windows-msvc`` on Windows), and on Linux a C compiler for 32-bit x86
and perl, which build the OpenSSL that rust/halo-client/Cargo.toml vendors.
"""

import argparse
import filecmp
import os
import shutil
import subprocess
import sys
from pathlib import Path
from typing import List

ROOT = Path(__file__).resolve().parent.parent
CRATE = Path("rust/halo-client")
# the crates it builds from, for ninja to know when to run cargo again
SOURCE_FOLDERS = [CRATE, Path("rust/halo-wire"), Path("rust/halo-sim"), Path("rust/halo-map"),
                  Path("rust/halo-match-driver")]

LINUX_TARGET = "i686-unknown-linux-gnu"
WINDOWS_TARGET = "i686-pc-windows-msvc"

# what the library needs of the system, which the game's own link does not
# name: from `cargo rustc --lib -- --print native-static-libs` for each target
# (Windows' Schannel, for the SpacetimeDB SDK's TLS, adds secur32 and crypt32)
LINUX_LIBRARIES = ["atomic", "gcc_s", "util", "rt", "pthread", "dl", "m"]
WINDOWS_LIBRARIES = ["ntdll", "userenv", "dbghelp", "ws2_32", "secur32", "crypt32", "bcrypt", "advapi32"]


def library_name(target: str) -> str:
    """the static library cargo writes for the target"""
    return "halo_client.lib" if "windows-msvc" in target else "libhalo_client.a"


def library_path(target: str, root: Path = ROOT) -> Path:
    """where cargo leaves it, for a release build"""
    return root / CRATE / "target" / target / "release" / library_name(target)


def system_libraries(target: str) -> List[str]:
    return WINDOWS_LIBRARIES if "windows-msvc" in target else LINUX_LIBRARIES


def cargo_command(target: str) -> List[str]:
    """always a release build (the debug library is ten times the size and the
    game's debug builds do not need it) and always against the lock file"""
    return [os.environ.get("CARGO", "cargo"), "build", "--release", "--locked", "--target", target,
            "--manifest-path", str(CRATE / "Cargo.toml")]


def available() -> bool:
    """whether there is a Rust toolchain to build it with"""
    return shutil.which(os.environ.get("CARGO", "cargo")) is not None


def source_files() -> List[Path]:
    """the files its build reads (relative to the repository root)"""
    files: List[Path] = []
    for folder in SOURCE_FOLDERS:
        for path in sorted((ROOT / folder).rglob("*")):
            if "target" in path.relative_to(ROOT / folder).parts or not path.is_file():
                continue
            if path.suffix in (".rs", ".toml", ".lock"):
                files.append(path.relative_to(ROOT))
    return files


def build(target: str, output: Path) -> None:
    subprocess.run(cargo_command(target), cwd=ROOT, check=True)
    built = library_path(target)
    if not built.is_file():
        sys.exit(f"cargo built no {built}")
    output.parent.mkdir(parents=True, exist_ok=True)
    if not output.is_file() or not filecmp.cmp(built, output, shallow=False):
        shutil.copy2(built, output)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--target", default=WINDOWS_TARGET if os.name == "nt" else LINUX_TARGET)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    build(args.target, args.output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
