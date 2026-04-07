---
name: bincast-shared
description: "Bincast: shared reference for installation, configuration, and conventions."
metadata:
  version: 0.1.1
  openclaw:
    category: "reference"
    domain: "devtools"
    requires:
      bins:
        - bincast
---

# Bincast Reference

## Pre-check: Is bincast installed?

Before using any bincast skill, verify the binary is available:

```bash
# macOS/Linux
which bincast && bincast version

# Windows (PowerShell)
Get-Command bincast -ErrorAction SilentlyContinue; bincast version
```

If not found, install using the method appropriate for the user's platform:

**macOS:**
```bash
brew install benelser/bincast/bincast
```

**Windows:**
```powershell
irm https://raw.githubusercontent.com/benelser/bincast/main/install.ps1 | iex
```

**Linux:**
```bash
curl -sSL https://raw.githubusercontent.com/benelser/bincast/main/install.sh | sh
```

**Any platform with Rust toolchain:**
```bash
cargo install bincast
```

> [!IMPORTANT]
> If `bincast` is not found on PATH, you MUST install it before proceeding with any other bincast skill. Guide the user through installation using the commands above.

## Configuration File: `bincast.toml`

```toml
[package]
name = "my-tool"
binary = "my-tool"
description = "What it does"
repository = "https://github.com/owner/repo"
license = "MIT"

[targets]
platforms = [
  "aarch64-apple-darwin",
  "x86_64-apple-darwin",
  "x86_64-unknown-linux-gnu",
  "x86_64-pc-windows-msvc",
]

[distribute.github]
release = true

[distribute.install_script]
enabled = true
```

## Available Commands

| Command | Purpose |
|---------|---------|
| `bincast init` | Interactive project setup — creates bincast.toml + CI + install scripts |
| `bincast generate` | Regenerate CI workflow and distribution files |
| `bincast check` | Validate config, check name availability, verify tokens |
| `bincast version patch\|minor\|major` | Bump version in Cargo.toml and commit |
| `bincast release` | Tag current Cargo.toml version, push, trigger CI |
| `bincast publish` | Build and publish locally (without CI) |

## Distribution Channels

GitHub Releases, PyPI, npm, Homebrew, Scoop, crates.io, cargo-binstall, install scripts (curl|sh + irm|iex).

## Key Conventions

- Version source of truth: `Cargo.toml`
- Tag format: `v{version}` (e.g., `v0.2.0`)
- CI triggers on tag push (`v*`)
- `bincast version` bumps and commits. `bincast release` tags and pushes. They compose.
