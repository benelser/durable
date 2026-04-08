---
name: bincast-setup-secrets
description: "Bincast: Set up registry tokens and GitHub secrets for release publishing."
metadata:
  version: 0.1.5
  openclaw:
    category: "recipe"
    domain: "devtools"
    requires:
      bins:
        - bincast
        - gh
      skills:
        - bincast-shared
      mcp:
        - microsoft/playwright-mcp
---

# Set Up Secrets

> **PREREQUISITE:** Project must have `bincast.toml` (run bincast init first).

> [!CAUTION]
> **Never read token values from the browser into the agent context.** Tokens are sensitive credentials. The agent navigates the user to the right page and tells them what to create. The user copies the token and pastes it directly into `gh secret set` in their terminal. The agent never sees the token.

## Determine which secrets are needed

Read `bincast.toml` and check which channels are enabled:

| Channel in config | Secret needed | Create at |
|---|---|---|
| `[distribute.cargo]` | `CARGO_REGISTRY_TOKEN` | https://crates.io/settings/tokens |
| `[distribute.pypi]` | `PYPI_TOKEN` | https://pypi.org/manage/account/token/ |
| `[distribute.npm]` | `NPM_TOKEN` | https://www.npmjs.com/settings/~/tokens |
| `[distribute.homebrew]` | `TAP_GITHUB_TOKEN` | https://github.com/settings/personal-access-tokens/new |
| `[distribute.scoop]` | `BUCKET_GITHUB_TOKEN` | https://github.com/settings/personal-access-tokens/new |
| `[distribute.github]` | `GITHUB_TOKEN` | **Automatic** — no action needed |

Check which are already set:
```bash
gh secret list --repo owner/repo
```

Only set up missing secrets.

## Flow: Browser-assisted (with Playwright MCP)

> **IMPORTANT:** Use `@playwright/mcp` tools (prefixed with `browser_`), NOT chrome-devtools MCP. Check if Playwright MCP is available by looking for `browser_navigate` in your available tools. If only chrome-devtools tools are available (like `navigate_page`), fall back to the manual flow below.

The agent navigates the user to the correct page and provides exact instructions. The agent DOES NOT read token values from the page.

### For each missing secret:

**Step 1: Navigate to the token creation page**
```
browser_navigate: <URL from table above>
```

**Step 2: Wait for authentication**
The browser will show a login page. Tell the user:
```
"I've opened [service] in the browser. Please log in if needed."
```

**Step 3: Guide through token creation**
Use `browser_snapshot` to verify the page loaded, then tell the user exactly what to fill in:

For CARGO_REGISTRY_TOKEN:
```
"You should see the crates.io API Tokens page. Please:
  1. Click 'New Token'
  2. Name: bincast-release
  3. Scopes: publish-new, publish-update
  4. Click 'Create'
  5. Copy the token that appears"
```

For PYPI_TOKEN:
```
"You should see the PyPI token creation page. Please:
  1. Token name: bincast-release
  2. Scope: Entire account (or project-scoped to your package)
  3. Click 'Create token'
  4. Copy the token (starts with pypi-)"
```

For NPM_TOKEN:
```
"You should see the npm tokens page. Please:
  1. Click 'Generate New Token' → 'Classic Token'
  2. Name: bincast-release
  3. Type: Automation
  4. Click 'Generate Token'
  5. Copy the token"
```

For TAP_GITHUB_TOKEN / BUCKET_GITHUB_TOKEN:
```
"You should see the GitHub PAT creation page. Please:
  1. Token name: bincast-tap (or bincast-bucket)
  2. Expiration: 90 days
  3. Repository access: Only select repositories → [tap/bucket repo name]
  4. Permissions:
     - Contents: Read and write
     - Metadata: Read-only (auto-selected)
  5. Click 'Generate token'
  6. Copy the token"
```

**Step 4: User sets the secret**
Tell the user to run this command and paste the token when prompted:
```bash
gh secret set SECRET_NAME --repo owner/repo
```
The `gh secret set` command reads from stdin with masking — the token never appears in terminal history or agent context.

**Step 5: Verify**
```bash
gh secret list --repo owner/repo
```

## Flow: Manual (no Playwright MCP)

Same as above but skip the browser_navigate step. Just tell the user:
```
"Please open [URL] in your browser, then follow these steps..."
```

## After all secrets are set

Run `bincast check` to validate everything is ready, then proceed to first release:
```bash
bincast version patch
bincast release
```

## See Also

- `../bincast-init/SKILL.md` — project initialization
- `../bincast-release/SKILL.md` — releasing
- `../bincast-troubleshoot/SKILL.md` — CI failure diagnosis
