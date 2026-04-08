---
name: bincast-add-channel
description: "Bincast: Add a distribution channel to an existing project."
metadata:
  version: 0.1.1
  openclaw:
    category: "recipe"
    domain: "devtools"
    requires:
      bins:
        - bincast
      skills:
        - bincast-shared
---

# Add a Distribution Channel

> **PREREQUISITE:** Project must already have `bincast.toml` (run `bincast init` first).

## Available Channels

| Channel | Config section | What it needs |
|---------|---------------|---------------|
| PyPI | `[distribute.pypi]` | `package_name`, `PYPI_TOKEN` secret |
| npm | `[distribute.npm]` | `scope`, `NPM_TOKEN` secret |
| Homebrew | `[distribute.homebrew]` | `tap` repo, `TAP_GITHUB_TOKEN` secret |
| crates.io | `[distribute.cargo]` | `crate_name`, `CARGO_REGISTRY_TOKEN` secret |
| Install scripts | `[distribute.install_script]` | Nothing extra |

## Steps

1. Ask the user which channel to add

2. Edit `bincast.toml` — add the appropriate section:

```toml
# Example: adding PyPI
[distribute.pypi]
package_name = "my-tool"
```

3. Regenerate CI and distribution files:
```bash
bincast generate
```

```bash
gh repo create owner/homebrew-my-tool --private
```

5. Set the required secret:
```bash
gh secret set PYPI_TOKEN --repo owner/repo
```

6. Commit the changes:
```bash
git add bincast.toml .github/workflows/release.yml
git commit -m "add PyPI distribution"
```

7. Verify: `bincast check`

## Tips

- Always run `bincast generate` after editing `bincast.toml` — it regenerates the CI workflow.
- For private repos, Homebrew taps need `HOMEBREW_GITHUB_API_TOKEN` on the client side.
- crates.io requires a verified email at https://crates.io/settings/profile.

## See Also

- `../bincast-shared/SKILL.md` — full config reference
- `../bincast-release/SKILL.md` — release after adding channel
