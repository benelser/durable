---
name: bincast-troubleshoot
description: "Bincast: Diagnose and fix CI release failures."
metadata:
  version: 0.1.1
  openclaw:
    category: "recipe"
    domain: "devtools"
    requires:
      bins:
        - gh
      skills:
        - bincast-shared
---

# Troubleshoot CI Failures

> **PREREQUISITE:** A release was tagged but CI failed.

## Step 1: Get the failure details

```bash
gh run view --log-failed
```

## Common Failures

### "Repository not found" on checkout

**Cause:** Private repo + job permissions too restrictive.
**Fix:** Ensure the job has `permissions: contents: write` in the CI workflow. Regenerate: `bincast generate`.

### "shasum: command not found" (Windows)

**Cause:** Windows runners don't have `shasum`.
**Fix:** The CI template should use `sha256sum` with `shasum` fallback. Regenerate: `bincast generate`.

### Cross-compilation fails on ARM Linux

**Cause:** Missing cross-compilation toolchain.
**Fix:** The CI template installs `gcc-aarch64-linux-gnu` automatically. If still failing, check that `rustup target add` succeeded.

### "A verified email address is required" (crates.io)

**Cause:** crates.io account email not verified.
**Fix:** Visit https://crates.io/settings/profile and verify email.

### "File already exists" (PyPI)

**Cause:** Wheel with this version already uploaded.
**Fix:** Bump the version (`bincast version patch`) and release again.

### Homebrew formula SHA mismatch

**Cause:** Formula has wrong SHA-256 checksums.
**Fix:** Re-dispatch the formula update:
```bash
gh api repos/OWNER/homebrew-REPO/dispatches \
  -f event_type=update-formula \
  -f 'client_payload[version]=vX.Y.Z'
```

### Attestation fails on private repo

**Cause:** SLSA attestation requires public repos.
**Fix:** Attestation is disabled by default. If enabled, make the repo public or disable it in the CI workflow.

### Smoke test fails on cross-compiled binary

**Cause:** ARM binary can't run on x86 runner.
**Fix:** The CI template skips smoke tests for cross-arch builds. Regenerate: `bincast generate`.

## Step 2: Fix and re-release

After fixing the issue:

```bash
# If the fix is in bincast.toml or CI:
bincast generate
git add . && git commit -m "fix CI"
git push

# Delete the failed tag and re-release:
git tag -d vX.Y.Z
git push origin :refs/tags/vX.Y.Z
bincast release
```

## See Also

- `../bincast-release/SKILL.md` — normal release flow
- `../bincast-shared/SKILL.md` — config reference
