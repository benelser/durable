# Distribution

How to get `durable` into developers' hands across every platform and package manager.

## Current State

The binary is built. The CI pipeline is ready. What's needed for each channel:

## Tier 1: Ship Today

### PyPI (pip, uv, poetry)

**Status:** CI workflow ready. Needs one secret.

**Steps:**
1. Create account at [pypi.org](https://pypi.org/account/register/)
2. Generate API token at [pypi.org/manage/account/token](https://pypi.org/manage/account/#api-tokens)
3. Add `PYPI_API_TOKEN` as a secret in GitHub repo settings → Secrets → Actions
4. Tag and push:
   ```bash
   git tag v0.1.0
   git push --tags
   ```
5. CI cross-compiles for 5 platforms, builds wheels, publishes to PyPI

**Result:**
```bash
pip install durable
uv add durable
poetry add durable
```

**Friction:** None once the secret is set. Fully automated.

**Note:** The package name `durable` may already be taken on PyPI. Check [pypi.org/project/durable](https://pypi.org/project/durable/). If taken, use `durable-runtime` and alias the binary. If available, claim it immediately — even before the first real release.

---

### cargo install (Rust developers)

**Status:** Works today. Zero setup.

```bash
cargo install --git https://github.com/benelser/durable
```

Once published to crates.io:
```bash
cargo install durable-runtime
```

**Steps to publish to crates.io:**
1. `cargo login` with crates.io API token
2. `cargo publish`

**Friction:** Need to verify crate name availability on crates.io.

---

### Homebrew Tap (macOS)

**Status:** Need to create the tap repo and formula.

**Steps:**
1. Create repo `benelser/homebrew-durable` on GitHub
2. Add the formula file (see below)
3. After tagging a release, update the formula with the new SHA256

**Usage:**
```bash
brew tap benelser/durable
brew install durable
```

**Formula** (`Formula/durable.rb`):
```ruby
class Durable < Formula
  desc "The SQLite of durable agent execution"
  homepage "https://github.com/benelser/durable"
  version "0.1.0"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "https://github.com/benelser/durable/releases/download/v0.1.0/durable-aarch64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER"
    else
      url "https://github.com/benelser/durable/releases/download/v0.1.0/durable-x86_64-apple-darwin.tar.gz"
      sha256 "PLACEHOLDER"
    end
  end

  on_linux do
    if Hardware::CPU.arm?
      url "https://github.com/benelser/durable/releases/download/v0.1.0/durable-aarch64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER"
    else
      url "https://github.com/benelser/durable/releases/download/v0.1.0/durable-x86_64-unknown-linux-gnu.tar.gz"
      sha256 "PLACEHOLDER"
    end
  end

  def install
    bin.install "durable"
  end

  test do
    assert_match "durable", shell_output("#{bin}/durable version")
  end
end
```

**Friction:** Need to add a CI step that produces tarballs (not just wheels) for the Homebrew formula to reference. Currently the release workflow only produces Python wheels.

**Fix needed:** Add a step to the release workflow that creates `durable-{target}.tar.gz` archives containing just the binary, and uploads them as GitHub Release assets.

---

### Scoop (Windows, developer-friendly)

**Status:** Need to create the bucket repo and manifest.

**Steps:**
1. Create repo `benelser/scoop-durable` on GitHub
2. Add the manifest file (see below)

**Usage:**
```
scoop bucket add durable https://github.com/benelser/scoop-durable
scoop install durable
```

**Manifest** (`bucket/durable.json`):
```json
{
  "version": "0.1.0",
  "description": "The SQLite of durable agent execution",
  "homepage": "https://github.com/benelser/durable",
  "license": "MIT",
  "architecture": {
    "64bit": {
      "url": "https://github.com/benelser/durable/releases/download/v0.1.0/durable-x86_64-pc-windows-msvc.zip",
      "hash": "PLACEHOLDER"
    }
  },
  "bin": "durable.exe",
  "checkver": "github",
  "autoupdate": {
    "architecture": {
      "64bit": {
        "url": "https://github.com/benelser/durable/releases/download/v$version/durable-x86_64-pc-windows-msvc.zip"
      }
    }
  }
}
```

**Friction:** Same as Homebrew — need tarball/zip release assets.

---

## Tier 2: Ship This Week

### apt (Debian/Ubuntu — own repo)

**Steps:**
1. Add `cargo-deb` to the CI:
   ```bash
   cargo install cargo-deb
   cargo deb --target x86_64-unknown-linux-gnu
   cargo deb --target aarch64-unknown-linux-gnu
   ```
2. Host the `.deb` files on GitHub Releases
3. Create a simple APT repo (GitHub Pages or Cloudflare R2)
4. Generate GPG key for signing
5. Document the install:
   ```bash
   curl -fsSL https://durable.dev/install.sh | sh
   ```

**Friction:**
- `cargo-deb` needs a `[package.metadata.deb]` section in Cargo.toml
- APT repos need GPG signing (generate a key, host the public key)
- Need a domain or GitHub Pages for the repo URL

**Cargo.toml addition:**
```toml
[package.metadata.deb]
maintainer = "Ben Elser <ben@durable.dev>"
copyright = "2025, Ben Elser"
depends = ""
section = "utils"
priority = "optional"
assets = [
  ["target/release/durable", "usr/bin/", "755"],
]
```

---

### winget (Windows, official)

**Steps:**
1. Create a manifest YAML:
   ```yaml
   PackageIdentifier: benelser.durable
   PackageVersion: 0.1.0
   PackageName: durable
   Publisher: Ben Elser
   License: MIT
   ShortDescription: The SQLite of durable agent execution
   Installers:
     - Architecture: x64
       InstallerUrl: https://github.com/benelser/durable/releases/download/v0.1.0/durable-x86_64-pc-windows-msvc.zip
       InstallerSha256: PLACEHOLDER
       InstallerType: zip
   ```
2. Submit PR to [microsoft/winget-pkgs](https://github.com/microsoft/winget-pkgs)

**Friction:** Review takes 2-7 days. Requires a stable release URL.

---

## Tier 3: After Adoption

### Homebrew Core (mass macOS)

**Requirements:**
- 30+ GitHub stars
- Stable release history (2+ releases)
- Passing CI on macOS
- Formula follows Homebrew conventions

**Steps:** Submit PR to [Homebrew/homebrew-core](https://github.com/Homebrew/homebrew-core)

**Friction:** Review takes weeks. Strict requirements on formula quality.

---

### Official Linux repos (Debian, Ubuntu, Fedora)

**Requirements:**
- Active maintainer willing to sponsor
- Stable release history
- Passes distribution packaging guidelines

**Friction:** Takes months to years. Not practical until significant adoption.

---

### npm/bun (TypeScript SDK)

**When:** After TypeScript SDK is built.

**Approach:** The esbuild pattern — platform-specific binary packages as optional dependencies.

**Package structure:**
```
@durable/durable-linux-x64          # contains linux x86_64 binary
@durable/durable-linux-arm64        # contains linux arm64 binary
@durable/durable-darwin-x64         # contains macos x86_64 binary
@durable/durable-darwin-arm64       # contains macos arm64 (Apple Silicon) binary
@durable/durable-win32-x64          # contains windows x86_64 binary
durable                             # meta-package + TypeScript SDK
```

**How it works:**
1. The main `durable` package lists all platform packages as `optionalDependencies`
2. npm/bun/pnpm only installs the one matching the current platform
3. The SDK's binary discovery checks `node_modules/@durable/durable-{platform}/bin/durable`
4. Fallback: `$PATH` (for Homebrew/apt users)

**package.json** (main package):
```json
{
  "name": "durable",
  "version": "0.1.0",
  "optionalDependencies": {
    "@durable/durable-linux-x64": "0.1.0",
    "@durable/durable-linux-arm64": "0.1.0",
    "@durable/durable-darwin-x64": "0.1.0",
    "@durable/durable-darwin-arm64": "0.1.0",
    "@durable/durable-win32-x64": "0.1.0"
  }
}
```

**package.json** (platform package, e.g., `@durable/durable-darwin-arm64`):
```json
{
  "name": "@durable/durable-darwin-arm64",
  "version": "0.1.0",
  "os": ["darwin"],
  "cpu": ["arm64"],
  "bin": {
    "durable": "bin/durable"
  }
}
```

**CI addition:** The release workflow adds steps to:
1. Copy the binary into `sdks/typescript/platforms/{target}/bin/durable`
2. `npm publish` each platform package
3. `npm publish` the main package

**Friction:**
- Need an npm account and `NPM_TOKEN` GitHub secret
- Need to create the `@durable` org on npm (or use `@benelser/durable-*`)
- Each platform package is published separately (5 publishes per release)
- bun and pnpm handle `optionalDependencies` natively. npm does too since v7.

**Bun-specific:** Bun fully supports this pattern. `bun add durable` downloads the right platform binary. `bun run agent.ts` finds it in `node_modules/.bin/durable`.

**Result:**
```bash
npm install durable        # npm
bun add durable            # bun
pnpm add durable           # pnpm
yarn add durable           # yarn
```

All give you the TypeScript SDK + the Rust binary for the current platform.

---

## CI Changes Needed

The current release workflow produces Python wheels. For Homebrew, Scoop, and apt, we also need standalone binary archives.

**Add to `.github/workflows/release.yml`:**

```yaml
- name: Create binary archive
  shell: bash
  run: |
    cd target/${{ matrix.target }}/release
    if [[ "${{ matrix.os }}" == "windows-latest" ]]; then
      7z a ../../../durable-${{ matrix.target }}.zip durable.exe
    else
      tar czf ../../../durable-${{ matrix.target }}.tar.gz durable
    fi

- name: Upload binary archive
  uses: actions/upload-artifact@v4
  with:
    name: binary-${{ matrix.target }}
    path: durable-${{ matrix.target }}.*
```

And include them in the GitHub Release step.

---

## Install Script (universal)

For quick installs without a package manager:

```bash
curl -fsSL https://durable.dev/install.sh | sh
```

The script:
1. Detects OS and architecture
2. Downloads the right binary from GitHub Releases
3. Installs to `/usr/local/bin/durable` (or `~/.local/bin/durable`)
4. Verifies checksum

This is how many tools bootstrap: `rustup`, `nvm`, `deno`, `bun`.

**Friction:** Need a domain (durable.dev) or use the GitHub raw URL:
```bash
curl -fsSL https://raw.githubusercontent.com/benelser/durable/main/install.sh | sh
```

---

## Priority Checklist

```
[ ] Claim "durable" on PyPI (check availability, register if free)
[ ] Add PYPI_API_TOKEN to GitHub repo secrets
[ ] Add binary archive step to release workflow
[ ] Tag v0.1.0 and push (triggers first release)
[ ] Create benelser/homebrew-durable repo + formula
[ ] Create benelser/scoop-durable repo + manifest
[ ] Write install.sh script
[ ] Add cargo-deb metadata to Cargo.toml
[ ] Claim "durable-runtime" on crates.io
```
