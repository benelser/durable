---
name: bincast-init
description: "Bincast: Set up a new project for multi-platform distribution."
metadata:
  version: 0.1.3
  openclaw:
    category: "recipe"
    domain: "devtools"
    requires:
      bins:
        - bincast
      skills:
        - bincast-shared
---

# Initialize a Project

> **PREREQUISITE:** Read `../bincast-shared/SKILL.md` — verify bincast is installed.

## Pre-checks

Use your file reading tools (Read, Glob) for file checks — do NOT use shell `test -f` commands as security hooks may block them.

1. Read `Cargo.toml` — verify it exists and has `[package]` section
2. Check that `bincast.toml` does NOT exist (use Glob or ls)
3. Check git remote: `git remote -v`
4. Check bincast binary: `bincast version` (if command fails, install it first — see `../bincast-shared/SKILL.md`)

If bincast is not installed, install it:
```bash
brew install benelser/bincast/bincast
```
Do NOT build from apm_modules source — always install from a package manager.

## Agent Flow (non-interactive)

Ask the user how they want people to install their tool. Map their answer to channels:

| User says | Channels flag |
|-----------|--------------|
| "pip install" | `pypi` |
| "npm install" | `npm` (also need `--npm-scope`) |
| "brew install" | `homebrew` |
| "scoop install" | `scoop` |
| "cargo install" | `cargo` |
| "curl script" | `install-scripts` |
| "everything" / "all" | `github,pypi,npm,homebrew,scoop,cargo,install-scripts` |
| "just GitHub" | `github,install-scripts` |
| "rust developers" | `github,cargo,install-scripts` |

Then run:

```bash
# Example: user wants pip + brew + cargo + curl
bincast init --channels github,pypi,homebrew,cargo,install-scripts --yes

# Example: user wants everything including npm
bincast init --channels github,pypi,npm,homebrew,scoop,cargo,install-scripts --npm-scope @their-org --yes

# Example: minimal — just GitHub Releases
bincast init --channels github,install-scripts --yes
```

### Channel-specific flags

- `--npm-scope @org` — required when npm channel is enabled
- `--tap owner/homebrew-name` — optional, defaults to `owner/homebrew-{project}`
- `--bucket owner/scoop-name` — optional, defaults to `owner/scoop-{project}`

### Confirmation

Always confirm with the user BEFORE running. Show them what will be set up:

```
"I'll set up bincast with these channels:
  - GitHub Releases (archives + checksums)
  - PyPI (pip install my-tool)
  - Homebrew (brew install owner/tap/my-tool)
  - crates.io (cargo install my-tool)
  - Install scripts (curl | sh)

This will create bincast.toml, a CI workflow, install scripts, and a Homebrew tap repo.

Proceed?"
```

## Interactive Flow (human at terminal)

If the user prefers to run it interactively:

```bash
! bincast init
```

The `!` prefix runs it in the current session. The wizard will ask for profile and channel-specific config.

## After Init

1. Run `bincast check` to validate
2. Review generated files: `cat bincast.toml`
3. **Set up secrets** — invoke `../bincast-setup-secrets/SKILL.md` to create tokens and set GitHub Actions secrets. This is the critical next step before the first release.

## See Also

- `../bincast-setup-secrets/SKILL.md` — create tokens and set secrets (browser-assisted)
- `../bincast-shared/SKILL.md` — installation and config reference
- `../bincast-release/SKILL.md` — releasing after setup

> [!CAUTION]
> `bincast init` creates files and may create GitHub repositories (Homebrew tap, Scoop bucket) if `gh` CLI is available. Always confirm with the user before running.
