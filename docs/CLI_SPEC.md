# Developer CLI Design Philosophy

A spec for building CLI tools that make developers rave.
Not about what commands to build — about how to think about the
developer sitting at the terminal.

---

## The SQLite Shell Standard

SQLite didn't win because it had the best SQL engine. It won because
a developer could type `sqlite3 my.db` and immediately understand
their data. No server. No config. No documentation required. The
shell respected the developer's time and intelligence.

That's the bar. Every CLI interaction should feel like the tool
already knows what you want.

---

## Core Philosophy

### 1. The Tool Should Work Before The Developer Reads Anything

The first command a developer types should produce useful output
with zero arguments, zero flags, zero configuration. If your CLI
requires a `--format` flag or a config file before it shows anything,
you've already lost.

The developer just installed your tool. They type the name. They
should see the state of their world.

### 2. Answer The Question They're Actually Asking

Developers don't come to the CLI because they want to "invoke a
subcommand with parameters." They come because they have a question:

- "What's happening right now?"
- "Why did this fail?"
- "How much did this cost?"
- "Is everything healthy?"
- "What exactly happened in this run?"

Each command is an answer to a human question, not a CRUD operation
on a data model. Name commands after the question, not the noun.

`status` not `list-executions`. `cost` not `get-usage-metrics`.
`health` not `storage-diagnostics`.

### 3. Progressive Disclosure Through Zoom Levels

The developer starts broad and drills down. Each command is a zoom
level. The output of one command tells you which command to run next.

```
"What's the state of things?" → status
"Tell me about this one"     → inspect <id from status>
"Walk me through it"         → steps <id>
"Give me everything"         → export <id>
```

The `status` output contains the IDs you pass to `inspect`. The
`inspect` output shows the data you'd explore with `steps`. Each
level links naturally to the next. The developer never has to guess
what to type.

### 4. Color Is Meaning, Not Decoration

Every color in the output carries information:

- Green means success. The developer can move on.
- Yellow means attention needed. Not broken, but not ignorable.
- Red means action required. Something is wrong.
- Blue means in-progress. It's working.
- Dim means context. Supporting information, not the headline.
- Cyan means identity. The thing you'd copy-paste into the next command.

If you remove all colors and the output loses information, you've
used color correctly. If the output looks the same without color,
you were decorating.

Auto-detect the terminal. Piped output gets no ANSI codes. This
is non-negotiable.

### 5. Numbers Are For Humans, Not Machines

`11,234` tokens means nothing at a glance. `11.2K` is instant.
`$0.0731` is noise. `$0.07` is clear. `1048576 bytes` is hostile.
`1.0MB` is kind.

Every number displayed to a human gets a human format. The machine
format goes in `export`.

### 6. Every Warning Is Actionable

Never show a problem without showing the fix. A warning without
a remediation path is just anxiety.

```
BAD:
  ⚠ Storage is large

GOOD:
  ⚠ Storage exceeds 100MB — run `mytool compact --all`
```

The developer should be able to copy-paste the fix directly from
the warning. No Googling. No documentation lookup. The answer is
right there.

### 7. One Screen, One Answer

If the common case requires scrolling, the output is too verbose.
The developer glanced at the terminal to get an answer. Respect
that glance. The critical information should be visible without
pressing any keys.

Pagination is for edge cases (1000 items). The default output is
the dashboard — everything that matters, nothing that doesn't.

### 8. The Export Escape Hatch

For every view the CLI provides, there should be a way to get the
raw data as JSON. The CLI is for humans. JSON is for scripts,
dashboards, and tools we haven't thought of yet.

`export` is not an afterthought — it's the bridge between the
developer experience and the ecosystem.

---

## Anti-Patterns

### Don't Make Me Configure Before I Explore

The developer should never have to set up a config file, create a
profile, or authenticate before seeing their data. Discovery first,
configuration later.

### Don't Hide Information Behind Flags

If a piece of information is useful, show it by default. Flags
should filter and refine, not unlock. The default output should be
the best output for 80% of use cases.

### Don't Make Me Think About The Data Model

The developer doesn't care about your schema, your storage format,
or your internal abstractions. They care about their work: their
executions, their costs, their failures. Map the CLI to their mental
model, not your implementation model.

### Don't Require A Running Server

The CLI reads from data at rest — files, databases, logs. It works
on a laptop, on a production server, on exported data from another
machine. If the CLI needs a daemon running to function, it's not a
CLI — it's a client.

### Don't Use Argument Parsers For 8 Commands

`argparse`, `click`, `clap` — they're all overkill for a CLI with
fewer than 10 commands. Positional arguments and `sys.argv` are
clearer, faster, and zero-dependency. The developer types
`mytool inspect abc-123`, not `mytool --command inspect --id abc-123`.

---

## The Rave Test

A developer raves about a CLI when:

1. **First run gives them an answer** — not an error, not a help page.
2. **They never read the docs** — the output teaches the tool.
3. **They screenshot the output** — it's clear enough to share.
4. **They reach for it instinctively** — it's faster than the dashboard.
5. **They show it to their team** — "look at this."

If your CLI passes all five, developers will adopt it, defend it,
and evangelize it. If it fails any one, it's just another tool in
a crowded field.

---

## Implementation Notes

### Zero Dependencies

The CLI is a single file. It uses only the language's standard
library: JSON, file I/O, terminal detection, string formatting.
No frameworks. No template engines. No color libraries.

This isn't minimalism for its own sake. It's a deployment property.
The CLI works everywhere the language runtime exists. No install
failures. No version conflicts. No supply chain risk.

### Terminal Detection

```
if stdout is a terminal:
    use colors, use unicode, use box drawing
else:
    plain text, ASCII only, machine-parseable
```

This is a single `isatty()` check at startup. Every output function
respects it. The developer never has to pass `--no-color`.

### Table Alignment

Right-align numbers. Left-align text. Bold the header. Dim the
separators. Summary row at the bottom.

This is how spreadsheets work. Developers already know how to read
this format. Don't invent a new one.

### Entry Point

Register as a console script in your package manager. After install,
the command is globally available. No `python -m`, no path setup,
no activation.

---

*Spec path: `/Users/belser/ventures/durable/docs/CLI_SPEC.md`*
