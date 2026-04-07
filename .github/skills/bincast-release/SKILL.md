---
name: bincast-release
description: "Bincast: Release a new version of your project."
metadata:
  version: 0.1.1
  openclaw:
    category: "recipe"
    domain: "devtools"
    requires:
      bins:
        - bincast
        - git
      skills:
        - bincast-shared
---

# Release a New Version

> **PREREQUISITE:** Read `../bincast-shared/SKILL.md` for conventions.

Guide the user through releasing a new version. Two composable commands:
- `bincast version` — bumps the version
- `bincast release` — tags and pushes

## Pre-checks

1. Must be on `main` or `master` branch
2. Working tree must be clean (all changes committed)
3. `bincast check` should pass

## Solo Developer Flow

```bash
# 1. Validate
bincast check

# 2. Ask the user: what kind of release?
#    patch (bug fixes), minor (new features), major (breaking changes)
bincast version patch    # or minor, or major

# 3. Tag and push
bincast release
```

That's it. CI handles building, packaging, and publishing to all channels.

## Team Flow (branch protection on main)

```bash
# 1. On a feature or release branch:
bincast version patch

# 2. Open a PR with the version bump
#    PR title: "release v0.X.Y"
#    Get review, merge to main

# 3. After merge, on main:
git checkout main && git pull
bincast release
```

## What `bincast version` Does

- Reads current version from `Cargo.toml`
- Bumps according to semver (patch/minor/major)
- Updates `Cargo.toml`
- For workspaces: updates `workspace.package.version`
- Commits: `release v{new_version}`

## What `bincast release` Does

- Reads version from `Cargo.toml`
- Pre-flight checks (on main, clean tree, tag doesn't exist)
- Creates git tag `v{version}`
- Pushes commit + tag to origin
- Prints CI link

## If Something Goes Wrong

- Tag already exists: `bincast version patch` to bump, then retry
- CI fails: see `../bincast-troubleshoot/SKILL.md`
- Wrong version tagged: `git tag -d v0.X.Y && git push origin :refs/tags/v0.X.Y`

> [!CAUTION]
> `bincast release` creates a git tag and pushes it. This triggers CI which publishes to registries. Confirm the version with the user before running.

## See Also

- `../bincast-troubleshoot/SKILL.md` — diagnose CI failures
- `../bincast-add-channel/SKILL.md` — add distribution channels
