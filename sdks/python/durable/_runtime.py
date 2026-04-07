"""RuntimeManager — finds, starts, and manages the Rust binary lifecycle."""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path
from typing import Optional

from .errors import RuntimeNotFound


class RuntimeManager:
    """Manages the durable-runtime binary as an invisible subprocess.

    Search order for the binary:
    1. DURABLE_RUNTIME_PATH environment variable
    2. Bundled with the package (durable/_bin/durable-runtime)
    3. In the project's target/release/ directory (development)
    4. On PATH
    """

    BINARY_NAME = "durable"
    BINARY_NAME_LEGACY = "durable-runtime"  # backward compat

    def __init__(self) -> None:
        self._process: Optional[subprocess.Popen] = None
        self._binary_path: Optional[Path] = None

    def ensure_binary(self) -> Path:
        """Find the binary. Returns the path or raises RuntimeNotFound."""
        if self._binary_path and self._binary_path.exists():
            return self._binary_path

        searched: list[str] = []

        # 1. Environment variable
        env_path = os.environ.get("DURABLE_RUNTIME_PATH")
        if env_path:
            p = Path(env_path)
            searched.append(str(p))
            if p.exists() and p.is_file():
                self._binary_path = p
                return p

        # 2. Bundled with package
        pkg_dir = Path(__file__).parent
        bundled = pkg_dir / "_bin" / self.BINARY_NAME
        searched.append(str(bundled))
        if bundled.exists():
            self._binary_path = bundled
            return bundled

        # 3. Development: target/release/ relative to project root
        # Walk up from the SDK directory to find the Rust project
        for ancestor in pkg_dir.parents:
            candidate = ancestor / "target" / "release" / self.BINARY_NAME
            if candidate.exists():
                searched.append(str(candidate))
                self._binary_path = candidate
                return candidate
            candidate_debug = ancestor / "target" / "debug" / self.BINARY_NAME
            if candidate_debug.exists():
                searched.append(str(candidate_debug))
                self._binary_path = candidate_debug
                return candidate_debug

        # 4. On PATH (try new name, then legacy name)
        import shutil

        for name in (self.BINARY_NAME, self.BINARY_NAME_LEGACY):
            on_path = shutil.which(name)
            searched.append(f"$PATH ({name})")
            if on_path:
                self._binary_path = Path(on_path)
                return self._binary_path

        raise RuntimeNotFound(searched)

    def start(self) -> subprocess.Popen:
        """Start the runtime subprocess in SDK mode."""
        if self._process and self._process.poll() is None:
            return self._process

        binary = self.ensure_binary()
        self._process = subprocess.Popen(
            [str(binary), "runtime"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            bufsize=0,  # Unbuffered for real-time I/O
        )
        return self._process

    def stop(self) -> None:
        """Graceful shutdown: send shutdown command, wait, then kill."""
        if self._process is None:
            return

        if self._process.poll() is not None:
            self._process = None
            return

        try:
            # Send shutdown command
            if self._process.stdin:
                self._process.stdin.write(b'{"type":"shutdown"}\n')
                self._process.stdin.flush()
            # Wait up to 5 seconds
            self._process.wait(timeout=5)
        except (subprocess.TimeoutExpired, BrokenPipeError, OSError):
            self._process.kill()
            self._process.wait()
        finally:
            self._process = None

    @property
    def is_running(self) -> bool:
        return self._process is not None and self._process.poll() is None

    @property
    def process(self) -> Optional[subprocess.Popen]:
        return self._process
