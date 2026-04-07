//! Durable Runtime binary entrypoint.
//!
//! Usage:
//!   durable-runtime --sdk-mode                    # SDK mode, no auth
//!   durable-runtime --sdk-mode --auth-token abc   # SDK mode with auth
//!   durable-runtime init <name> [--lang python]   # Scaffold a new project

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 && args[1] == "init" {
        cmd_init(&args[2..]);
        return;
    }

    if !args.contains(&"--sdk-mode".to_string()) {
        eprintln!("durable-runtime v{}", env!("CARGO_PKG_VERSION"));
        eprintln!();
        eprintln!("Usage:");
        eprintln!("  durable-runtime init <name> [--lang python|typescript]");
        eprintln!("  durable-runtime --sdk-mode                    SDK mode (stdin/stdout protocol)");
        eprintln!("  durable-runtime --sdk-mode --auth-token TOK   SDK mode with authentication");
        std::process::exit(1);
    }

    let auth_token = args
        .iter()
        .position(|a| a == "--auth-token")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    let env_token = std::env::var("DURABLE_AUTH_TOKEN").ok();
    let effective_token = auth_token.or(env_token.as_deref());

    durable_runtime::protocol::sdk_mode::run_sdk_mode_with_auth(effective_token);
}

fn cmd_init(args: &[String]) {
    let name = args.first().map(|s| s.as_str()).unwrap_or("my-agent");

    let lang = args
        .iter()
        .position(|a| a == "--lang")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
        .unwrap_or("python");

    let dir = std::path::Path::new(name);
    if dir.exists() && std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        eprintln!("Error: '{}' already exists and is not empty.", name);
        std::process::exit(1);
    }

    std::fs::create_dir_all(dir).unwrap_or_else(|e| {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    });

    match lang {
        "python" | "py" => scaffold_python(dir, name),
        "typescript" | "ts" => scaffold_typescript(dir, name),
        other => {
            eprintln!("Unknown language: {}. Supported: python, typescript", other);
            std::process::exit(1);
        }
    }
}

fn scaffold_python(dir: &std::path::Path, name: &str) {
    write_file(dir, "agent.py", r#"#!/usr/bin/env python3
"""Your durable agent — crash-recoverable with exactly-once tool execution.

Run:     python agent.py
Resume:  python agent.py --resume <execution-id>
Inspect: durable status --data-dir ./data
"""

import sys
from durable import Agent, tool
from durable.providers import OpenAI


@tool("search", description="Search for information on a topic")
def search(query: str) -> dict:
    """Replace with a real API call."""
    print(f"  [search] {query}")
    return {"results": [{"title": f"Result for '{query}'", "snippet": "Information found."}]}


@tool("save_note", description="Save a research note")
def save_note(title: str, content: str) -> dict:
    """Replace with a real database write."""
    print(f"  [save_note] {title}")
    return {"saved": True, "title": title}


def main():
    agent = Agent(
        "./data",
        system_prompt=(
            "You are a research assistant. When asked to research a topic:\n"
            "1. Search for information using the search tool\n"
            "2. Save your findings using the save_note tool\n"
            "3. Summarize what you found"
        ),
    )
    agent.add_tool(search)
    agent.add_tool(save_note)
    agent.set_llm(OpenAI())

    execution_id = None
    if "--resume" in sys.argv:
        idx = sys.argv.index("--resume")
        if idx + 1 < len(sys.argv):
            execution_id = sys.argv[idx + 1]

    prompt = " ".join(
        a for a in sys.argv[1:] if a != "--resume" and not a.startswith("-")
    ) or "Research the benefits of durable execution for AI agents"

    print(f"\nAgent: thinking...\n")
    response = agent.run(prompt, execution_id=execution_id)

    print(f"\nAgent: {response.text}")
    print(f"\nExecution ID: {response.execution_id}")
    print(f"Status: {response.status.value}")
    agent.close()


if __name__ == "__main__":
    main()
"#);

    write_file(dir, ".env.example", "# Add your API key\nOPENAI_API_KEY=sk-...\n");

    write_file(dir, ".gitignore", "/data/\n.env\n__pycache__/\n");

    println!();
    println!("  \x1b[32m\x1b[1mCreated {}/\x1b[0m", name);
    println!();
    println!("  {}/agent.py       Your agent (ready to run)", name);
    println!("  {}/.env.example   API key template", name);
    println!("  {}/.gitignore", name);
    println!();
    println!("  \x1b[1mGet started:\x1b[0m");
    println!("    cd {}", name);
    println!("    cp .env.example .env");
    println!("    python agent.py");
    println!();
}

fn scaffold_typescript(dir: &std::path::Path, name: &str) {
    write_file(dir, "agent.ts", r#"/**
 * Your durable agent — crash-recoverable with exactly-once tool execution.
 *
 * Run: bun run agent.ts
 *
 * TypeScript SDK coming soon. Use Python today:
 *   durable-runtime init my-agent --lang python
 */

console.log("Durable TypeScript SDK — coming soon.");
"#);

    write_file(dir, ".env.example", "# Add your API key\nOPENAI_API_KEY=sk-...\n");
    write_file(dir, ".gitignore", "/data/\n.env\nnode_modules/\n");

    println!();
    println!("  \x1b[32m\x1b[1mCreated {}/\x1b[0m (TypeScript)", name);
    println!();
    println!("  {}/agent.ts       Placeholder (SDK coming soon)", name);
    println!();
    println!("  \x1b[33mTypeScript SDK is in development.");
    println!("  Use Python today: durable-runtime init {} --lang python\x1b[0m", name);
    println!();
}

fn write_file(dir: &std::path::Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap_or_else(|e| {
        eprintln!("Error writing {}: {}", name, e);
        std::process::exit(1);
    });
}
