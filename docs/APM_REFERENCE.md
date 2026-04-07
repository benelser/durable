# APM (Agent Package Manager) — Reference for Durable

Source: [github.com/microsoft/apm](https://github.com/microsoft/apm) |
[microsoft.github.io/apm](https://microsoft.github.io/apm/)

This document distills what APM is, what problems it solves, and how
another agent or developer can evaluate where to leverage or build on it.

---

## What APM Is

APM is a dependency manager for AI agent configuration. It solves the
problem of **configuration fragmentation** — developers manually copy
prompt files, instructions, and tool configs between projects and team
members. APM makes agent configuration portable, versioned, and shareable.

The mental model: **npm for AI agent setup.** You declare what your
agents need in `apm.yml`, run `apm install`, and every developer on the
team gets the same configured agent environment.

Source: [What is APM](https://microsoft.github.io/apm/) |
[Key Concepts](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/introduction/key-concepts.md)

---

## The Core Concept: Primitives

APM manages six types of agent configuration artifacts ("primitives"):

### 1. Instructions (`.instructions.md`)
Targeted guidance by file type. Apply automatically based on glob patterns.

```yaml
---
description: Python coding standards
applyTo: "**/*.py"
---
Follow PEP 8. Use type hints. Include docstrings.
```

**What this solves:** Every developer's Copilot/Claude follows the same
coding standards for Python files, without copy-pasting a CLAUDE.md.

### 2. Agents (`.agent.md`)
Specialized AI assistant personalities with defined expertise and tool access.

```yaml
---
description: Senior backend developer focused on API design
tools: ["terminal", "file-manager"]
expertise: ["security", "performance"]
---
You are a senior backend engineer. Focus on security and maintainability.
```

**What this solves:** Team members get consistent AI personas. The
"code reviewer" agent behaves the same for everyone.

### 3. Skills (`SKILL.md`)
Package meta-guides that help AI agents understand what a package does.
A concise summary an AI can read to know the package's capabilities.

**What this solves:** When an agent encounters a dependency, the SKILL.md
tells it what the package does and how to use it — no manual explanation.

### 4. Hooks (`.json` in `hooks/`)
Lifecycle event handlers that run scripts at specific points during
agent operations (before/after tool use, on stop, etc.).

```json
{
  "hooks": {
    "PostToolUse": [{
      "matcher": { "tool_name": "write_file" },
      "hooks": [{ "type": "command", "command": "./scripts/lint.sh $TOOL_INPUT_path" }]
    }]
  }
}
```

Supported events: `PreToolUse`, `PostToolUse`, `Stop`, `Notification`,
`SubagentStop`.

**What this solves:** Automated quality gates. Every file write triggers
a linter. Every API call triggers a security check. Enforced by the
tool, not by human discipline.

### 5. Prompts (`.prompt.md`)
Reusable, parameterized agent workflows.

```yaml
---
description: Implement secure authentication
mode: backend-dev
input: [auth_method, session_duration]
---
Use ${input:auth_method} with ${input:session_duration} sessions.
```

**What this solves:** Repeatable workflows. "Code review" is a prompt
you install, not instructions you retype every time.

### 6. Plugins (`plugin.json`)
Pre-packaged agent bundles that normalize into APM packages. Projects
can use `apm.yml`, `plugin.json`, or both.

Source: [Key Concepts](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/introduction/key-concepts.md) |
[Primitive Types](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/reference/primitive-types.md)

---

## The Manifest: `apm.yml`

```yaml
name: my-project
version: 1.0.0
scripts:
  review: "copilot -p 'code-review.prompt.md'"
dependencies:
  apm:
    - anthropics/skills/skills/frontend-design
    - github/awesome-copilot/plugins/context-engineering
    - microsoft/apm-sample-package#v1.0.0
  mcp:
    - ghcr.io/github/github-mcp-server
```

Key features:
- **Git-based packages.** Install from GitHub, GitLab, Bitbucket, Azure
  DevOps, any git host. Format: `owner/repo/path#version`.
- **Version pinning.** `#v1.0.0` pins to a git tag. Lockfile
  (`apm.lock.yaml`) pins exact commit hashes.
- **MCP servers.** Declare Model Context Protocol servers as dependencies.
- **Scripts.** Named commands that invoke agents with specific prompts.
- **Transitive dependencies.** Resolved automatically.

Source: [Manifest Schema](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/reference/manifest-schema.md)

---

## Multi-Tool Compilation

The killer feature: **one manifest, every AI tool.**

`apm install` deploys primitives to tool-native directories:

```
.github/instructions/    → GitHub Copilot
.github/agents/          → GitHub Copilot
.claude/commands/        → Claude Code
.claude/agents/          → Claude Code
.cursor/rules/           → Cursor
.opencode/agents/        → OpenCode
```

For tools that don't watch directories (Codex, Gemini), `apm compile`
generates optimized `AGENTS.md` files using mathematical optimization.

**What this solves:** You configure your agents once. Every team member
using Copilot, Claude, or Cursor gets the same configuration. No manual
file copying per tool.

Source: [Quick Start](https://microsoft.github.io/apm/getting-started/quick-start/) |
[Compilation Guide](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/guides/compilation.md)

---

## Context Linking

Primitives can reference each other via markdown links, creating
composable knowledge graphs:

```markdown
<!-- .apm/instructions/api.instructions.md -->
Follow `our API standards` and ensure `GDPR compliance` for all endpoints.
```

APM resolves these links during install/compile, rewriting them to point
to actual source locations in `apm_modules/`. This means linked knowledge
works in the IDE, in GitHub rendering, and in AI tool context windows.

Source: [Key Concepts — Context Linking](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/introduction/key-concepts.md)

---

## Supply Chain Security

`apm audit` scans packages for hidden Unicode and security threats before
deployment. This prevents malicious skills, prompts, or hooks from
reaching your agent environment.

Source: [README](https://github.com/microsoft/apm)

---

## Distribution Model

APM itself is distributed via:
- Install scripts: `curl -sSL https://aka.ms/apm-unix | sh` (macOS/Linux),
  `irm https://aka.ms/apm-windows | iex` (Windows)
- Homebrew: `brew install microsoft/apm/apm`
- PyPI: `pip install apm-cli`
- Scoop: planned via `microsoft/scoop-apm`
- GitHub Releases: platform binaries (linux x64, linux arm64, macos x64,
  macos arm64, windows x64) with SHA256 checksums

APM packages (the things you install with `apm install`) are distributed
via git repositories. No centralized registry — any git host works.

Marketplace support exists for curated registries:
`apm marketplace add <registry>`.

Source: [Build/Release Workflow](https://github.com/microsoft/apm/blob/main/.github/workflows/build-release.yml) |
[Pack & Distribute Guide](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/guides/pack-distribute.md)

---

## CLI Commands

| Command | Purpose |
|---------|---------|
| `apm init [name]` | Create new project with apm.yml and .apm/ structure |
| `apm install [pkg]` | Install dependencies, deploy primitives to tool dirs |
| `apm compile` | Generate optimized AGENTS.md for tools that need it |
| `apm pack` | Bundle configuration as a distributable package |
| `apm audit` | Security scan for hidden Unicode and threats |
| `apm marketplace add` | Add curated plugin registry |
| `apm update` | Update APM itself |

Source: [CLI Commands Reference](https://github.com/microsoft/apm/blob/main/docs/src/content/docs/reference/cli-commands.md)

---

## What Makes APM Novel

1. **Configuration as packages.** Agent configuration (prompts,
   instructions, agents) is treated as installable, versioned, shareable
   dependencies — the same way code libraries are.

2. **Multi-tool from one source.** One manifest compiles to Copilot,
   Claude, Cursor, OpenCode, and Codex native formats. No manual
   duplication.

3. **Git-native distribution.** No centralized registry. Any git repo
   is a package source. Version pinning via tags. Lockfile via commit hashes.

4. **Context linking.** Primitives reference each other via markdown
   links, creating composable knowledge graphs that AI tools can traverse.

5. **Lifecycle hooks.** Automated quality gates that fire on agent
   actions (file writes, tool calls). Enforced by tooling, not discipline.

6. **Supply chain security for prompts.** `apm audit` treats prompts
   and instructions as attack surface — because they are.

---

## Where Durable Could Leverage APM

APM manages **what agents know** (instructions, prompts, skills).
Durable manages **how agents execute** (crash recovery, exactly-once,
confirmation gates).

They are complementary:

| Concern | APM | Durable |
|---------|-----|---------|
| Agent configuration | Yes (primitives) | No |
| Agent execution | No | Yes (durable runtime) |
| Crash recovery | No | Yes (event log replay) |
| Tool definitions | No (tools are in the AI tool) | Yes (SDK-level tools) |
| Team sharing | Yes (git packages) | Not yet |
| Multi-tool support | Yes (compile to each tool) | Not yet |

**Potential integration points:**

1. **APM package that installs Durable tools.** An APM package could
   include instructions for how an AI agent should use Durable — e.g.,
   "always use confirmation gates for payment tools."

2. **Durable contracts as APM hooks.** APM hooks fire on tool use.
   Durable contracts validate tool arguments. These could be unified —
   an APM hook that invokes Durable's contract system.

3. **apm.yml declaring Durable as a dependency.** A project's apm.yml
   could declare that it requires the Durable runtime, and `apm install`
   could ensure it's available.

4. **Shared primitive format.** Durable's `@tool` decorator and APM's
   `.agent.md` format could be interoperable — a Durable tool definition
   exported as an APM skill.
