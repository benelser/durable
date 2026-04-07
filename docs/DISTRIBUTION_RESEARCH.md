# Distribution Research

Observations from studying how other projects solve binary + SDK distribution,
and the novel patterns Durable should adopt.

## Case Study: Microsoft APM

APM is a Python CLI tool that distributes across 5 channels from a single
tag push. Their release workflow is the most complete open-source example
of multi-channel distribution we've found.

### What they do well

**1. Fully chained release pipeline.**
One `git tag v*` push triggers: build → test → GitHub Release → PyPI →
Homebrew → Scoop. Zero manual steps after tagging. Each downstream channel
is triggered via `peter-evans/repository-dispatch` which sends the version
and SHA256 checksums to the tap/bucket repo, where a second workflow
auto-updates the formula/manifest and commits.

**2. SHA256 checksums as release assets.**
Every binary archive has a companion `.sha256` file uploaded as a release
asset. Downstream channels (Homebrew, Scoop, install scripts) pull the
checksum from the release rather than computing it themselves. This
eliminates the "build it again to get the hash" problem.

**3. Separate repos for each distribution channel.**
- `microsoft/apm` — source code + CI
- `microsoft/homebrew-apm` — Homebrew tap (auto-updated by CI)
- `microsoft/scoop-apm` — Scoop bucket (auto-updated by CI)

Each channel repo is simple and single-purpose. The main repo's CI
dispatches events to them. No manual formula/manifest editing.

**4. Install scripts as the universal fallback.**
`curl -sSL https://aka.ms/apm-unix | sh` works everywhere. It detects
OS and architecture, downloads the right binary, installs it. No package
manager required. This is the first thing they show in the README.

**5. PyInstaller for standalone binaries.**
APM is Python but ships as standalone binaries (no Python needed on the
user's machine). They use PyInstaller + UPX compression. This means their
`pip install apm-cli` gives you a pure Python package, but `brew install`
gives you a frozen binary. Two distribution paths, same codebase.

**6. Trusted PyPI publishing.**
No API token secret needed. They use PyPA's trusted publishing (OIDC) —
GitHub Actions authenticates directly with PyPI. More secure, no token
rotation needed.

### What they don't solve

- No npm/bun distribution (Python-only)
- No Windows package manager beyond Scoop (winget planned but not done)
- No mechanism for SDKs in other languages to find the binary
- Binary is the entire app, not a runtime that SDKs connect to

---

## The Problem Space Durable Is Solving

Durable's distribution challenge is fundamentally different from APM, ruff,
esbuild, or any single-language tool. The problem:

**Durable is a runtime that multiple language SDKs connect to.**

This creates a two-layer distribution problem that nobody has cleanly solved:

### Layer 1: The Runtime Binary

A single Rust binary (`durable`) that:
- Manages the event log, replay, crash recovery
- Handles the NDJSON protocol over stdio
- Provides CLI commands (init, status, inspect, etc.)
- Is language-agnostic — same binary for Python, TypeScript, Go, whatever

This binary needs to be on the user's machine. It can arrive via:
- `brew install durable`
- `apt install durable`
- `curl | sh` install script
- Bundled inside a language SDK package (fallback)

### Layer 2: The Language SDK

A thin, pure-language package that:
- Starts the runtime as a subprocess
- Communicates via NDJSON protocol
- Provides idiomatic API (decorators in Python, types in TypeScript)
- Has zero compiled dependencies — pure stdlib

This SDK is installed via the language's package manager:
- `pip install durable` / `uv add durable` / `poetry add durable`
- `npm install durable` / `bun add durable`
- Future: `go get`, `cargo add`, `gem install`

### The Coupling Problem

Every other project couples these two layers:

| Project | Approach | Problem |
|---------|----------|---------|
| ruff | Binary inside Python wheel | Every new language SDK re-solves binary bundling |
| esbuild | Platform-specific npm packages | Complex, 5 packages to maintain per release |
| Temporal | Server is separate infra | Users must operate a cluster |
| SQLite | System binary + language bindings | Language bindings compile C code (complex) |

None of these work for Durable because:
- ruff's approach means the Python wheel is 15MB and the TypeScript package
  duplicates the binary bundling effort
- esbuild's approach means 5 platform packages × N language SDKs = 5N packages
- Temporal's approach requires infrastructure we explicitly avoid
- SQLite's approach requires compiling native code in each SDK

### The Pattern We Should Use

**System-level binary + pure language SDKs + smart discovery.**

```
┌─────────────────────────────────────────────┐
│ Layer 1: Runtime Binary (installed once)    │
│                                              │
│ brew install durable                         │
│ apt install durable                          │
│ curl -sSL durable.dev/install | sh           │
│                                              │
│ One binary. All platforms. All languages.    │
└──────────────────┬──────────────────────────┘
                   │ found on $PATH
┌──────────────────┴──────────────────────────┐
│ Layer 2: Language SDKs (pure, tiny)          │
│                                              │
│ pip install durable     → 50KB pure Python   │
│ bun add durable         → 30KB pure JS       │
│ go get durable          → pure Go             │
│                                              │
│ Each SDK finds the binary on PATH.           │
│ If not found: helpful error with install cmd.│
└─────────────────────────────────────────────┘
```

**Key insight: the SDKs never bundle the binary.** They are pure language
packages with zero compiled dependencies. They install in milliseconds.
They find the runtime on PATH.

**Convenience fallback:** For users who don't want a separate binary install,
each SDK can optionally bundle it:
```bash
pip install durable[binary]    # includes the Rust binary for this platform
bun add @durable/binary        # includes the binary
```

But this is the fallback, not the primary path.

### Why This Matters

**1. Language SDKs stay pure and tiny.**
A pure Python package with zero deps installs in <1 second. No wheel
compatibility issues. No platform-specific builds for the SDK itself.
Works on any Python version, any OS, any architecture.

**2. One binary install covers all languages.**
A developer who uses both Python and TypeScript installs the binary once.
Both SDKs find it. No duplicate 15MB binaries.

**3. New language SDKs are trivial to add.**
A new Go SDK is just a Go package that speaks NDJSON over stdio. No binary
bundling, no cross-compilation, no platform-specific packages. The entire
SDK is pure Go.

**4. The binary is a first-class citizen.**
`durable help` works without any SDK installed. `durable init` scaffolds
a project. `durable status` inspects executions. The binary is useful
on its own — the SDKs enhance it.

**5. Distribution scales linearly, not quadratically.**
Adding a new platform (e.g., linux arm64) means one new binary build.
Adding a new language SDK means one new pure package. Not N×M combinations.

### The Discovery Protocol

When an SDK starts, it finds the binary via:

1. `DURABLE_PATH` environment variable (explicit override)
2. Bundled in the SDK package at `{pkg}/_bin/durable` (convenience install)
3. `target/release/durable` or `target/debug/durable` (development)
4. `$PATH` lookup for `durable` (system install — the primary path)

If not found, the SDK raises an error with platform-specific install
instructions:

```
RuntimeNotFound: The 'durable' binary was not found.

Install it for your platform:
  macOS:   brew install durable
  Linux:   curl -sSL https://durable.dev/install | sh
  Windows: scoop install durable

Or install with the binary bundled:
  pip install durable[binary]
```

This error message IS the distribution strategy. It tells the user
exactly what to do, for their platform, in one line.

---

## Release Automation Patterns to Adopt from APM

1. **repository-dispatch for downstream channels.** Don't manually update
   the Homebrew formula. Have CI dispatch an event with version + checksums
   to the tap repo, where a workflow auto-updates and commits.

2. **SHA256 checksums as release assets.** Upload `.sha256` files alongside
   every binary. Downstream tooling (Homebrew, Scoop, install scripts)
   references these directly.

3. **Trusted PyPI publishing.** Use OIDC instead of API tokens. More
   secure, no secret rotation.

4. **Install script with `aka.ms` style short URL.** Register a short
   URL (durable.dev/install or similar) that points to the raw install
   script on GitHub. The script detects platform and downloads the right
   binary.

5. **Separate repos for distribution channels.** `benelser/homebrew-durable`,
   `benelser/scoop-durable`. Auto-updated by CI. Single-purpose.

---

## Action Items

```
[ ] Decide: "durable" or "durable-runtime" on PyPI (check availability)
[ ] Register short URL for install script (durable.dev or GitHub raw)
[ ] Add SHA256 checksum generation to release workflow
[ ] Add binary tarball/zip creation to release workflow (for Homebrew/Scoop)
[ ] Switch to trusted PyPI publishing (OIDC, no token)
[ ] Create benelser/homebrew-durable repo
[ ] Create benelser/scoop-durable repo
[ ] Add repository-dispatch steps to release workflow
[ ] Write install.sh that detects OS/arch and downloads from GitHub Releases
[ ] Update RuntimeNotFound error with platform-specific install instructions
[ ] Document the two-layer model (binary + SDK) in README
```
