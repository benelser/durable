#!/usr/bin/env python3
"""Bundle the compiled Rust binary into the Python package for distribution.

Usage:
    # Build the Rust binary first
    cd ../../ && cargo build --release

    # Bundle it into the Python package
    python scripts/bundle_binary.py

    # Now 'pip install .' includes the binary
"""

import os
import platform
import shutil
import stat
import sys
from pathlib import Path

SDK_DIR = Path(__file__).parent.parent
RUST_DIR = SDK_DIR.parent.parent
BIN_DIR = SDK_DIR / "durable" / "_bin"


def find_binary() -> Path:
    """Find the compiled Rust binary."""
    # Release build
    release = RUST_DIR / "target" / "release" / "durable-runtime"
    if release.exists():
        return release

    # Debug build
    debug = RUST_DIR / "target" / "debug" / "durable-runtime"
    if debug.exists():
        return debug

    print("ERROR: No compiled binary found.", file=sys.stderr)
    print("Run 'cargo build --release' first.", file=sys.stderr)
    sys.exit(1)


def bundle():
    binary = find_binary()
    BIN_DIR.mkdir(parents=True, exist_ok=True)

    dest = BIN_DIR / "durable-runtime"
    shutil.copy2(binary, dest)

    # Make executable
    dest.chmod(dest.stat().st_mode | stat.S_IEXEC | stat.S_IXGRP | stat.S_IXOTH)

    size_mb = dest.stat().st_size / (1024 * 1024)
    print(f"Bundled: {binary}")
    print(f"    -> {dest} ({size_mb:.1f} MB)")
    print(f"Platform: {platform.system()}-{platform.machine()}")


if __name__ == "__main__":
    bundle()
