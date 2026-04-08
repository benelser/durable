//! delite — the sqlite of durable agent execution.
//!
//! One binary. Every feature. Every language.

use delite_core::json::{self, ToJson};
use delite_core::storage::event::{EventType, FileEventStore, EventStore};
use delite_core::storage::{FileStorage, ExecutionLog};
use delite_core::core::types::ExecutionStatus;
use std::path::Path;

// ---------------------------------------------------------------------------
// ANSI colors (auto-disabled when not a TTY)
// ---------------------------------------------------------------------------

struct C;

impl C {
    fn enabled() -> bool { unsafe { libc_isatty() } }
    fn bold() -> &'static str { if Self::enabled() { "\x1b[1m" } else { "" } }
    fn dim() -> &'static str { if Self::enabled() { "\x1b[2m" } else { "" } }
    fn red() -> &'static str { if Self::enabled() { "\x1b[31m" } else { "" } }
    fn green() -> &'static str { if Self::enabled() { "\x1b[32m" } else { "" } }
    fn yellow() -> &'static str { if Self::enabled() { "\x1b[33m" } else { "" } }
    fn blue() -> &'static str { if Self::enabled() { "\x1b[34m" } else { "" } }
    fn cyan() -> &'static str { if Self::enabled() { "\x1b[36m" } else { "" } }
    fn reset() -> &'static str { if Self::enabled() { "\x1b[0m" } else { "" } }
}

#[cfg(unix)]
unsafe fn libc_isatty() -> bool {
    extern "C" { fn isatty(fd: i32) -> i32; }
    unsafe { isatty(1) != 0 }
}
#[cfg(not(unix))]
unsafe fn libc_isatty() -> bool { true }

// ---------------------------------------------------------------------------
// Formatters
// ---------------------------------------------------------------------------

fn fmt_duration(ms: u64) -> String {
    if ms < 1000 { format!("{}ms", ms) }
    else if ms < 60_000 { format!("{:.1}s", ms as f64 / 1000.0) }
    else if ms < 3_600_000 { format!("{:.1}m", ms as f64 / 60_000.0) }
    else { format!("{:.1}h", ms as f64 / 3_600_000.0) }
}

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 { format!("{}B", bytes) }
    else if bytes < 1024 * 1024 { format!("{:.1}KB", bytes as f64 / 1024.0) }
    else { format!("{:.1}MB", bytes as f64 / (1024.0 * 1024.0)) }
}

fn fmt_time(millis: u64) -> String {
    if millis == 0 { return "—".to_string(); }
    // Simple epoch millis to readable string
    let secs = millis / 1000;
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    format!("{:02}:{:02}:{:02} UTC", h, m, s)
}

fn fmt_status(status: &ExecutionStatus) -> String {
    match status {
        ExecutionStatus::Running => format!("{}RUNNING{}", C::blue(), C::reset()),
        ExecutionStatus::Completed => format!("{}COMPLETED{}", C::green(), C::reset()),
        ExecutionStatus::Failed => format!("{}FAILED{}", C::red(), C::reset()),
        ExecutionStatus::Suspended => format!("{}SUSPENDED{}", C::yellow(), C::reset()),
        _ => format!("{}{:?}{}", C::dim(), status, C::reset()),
    }
}

// ---------------------------------------------------------------------------
// Arg parsing
// ---------------------------------------------------------------------------

fn extract_flag<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(|s| s.as_str())
}

fn has_flag(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn find_data_dir(args: &[String]) -> String {
    if let Some(d) = extract_flag(args, "--data-dir") { return d.to_string(); }
    if let Ok(d) = std::env::var("DELITE_DATA_DIR") { if !d.is_empty() { return d; } }
    for c in &["./data", "./delite-data"] {
        let p = Path::new(c);
        if p.join("events").exists() || p.join("executions").exists() { return c.to_string(); }
    }
    "./data".to_string()
}

fn positional_arg(args: &[String], index: usize) -> Option<&str> {
    let mut positionals = Vec::new();
    let mut skip_next = false;
    for a in args {
        if skip_next { skip_next = false; continue; }
        if a == "--data-dir" || a == "--lang" || a == "-o" || a == "--auth-token" { skip_next = true; continue; }
        if a.starts_with('-') { continue; }
        positionals.push(a.as_str());
    }
    positionals.get(index).copied()
}

fn parse_exec_id(s: &str) -> delite_core::core::types::ExecutionId {
    match delite_core::core::uuid::Uuid::parse(s) {
        Ok(uuid) => delite_core::core::types::ExecutionId::from_uuid(uuid),
        Err(e) => { eprintln!("Invalid execution ID '{}': {}", s, e); std::process::exit(1); }
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");

    // Backward compat: --sdk-mode → runtime
    if cmd == "--sdk-mode" {
        cmd_runtime(&args);
        return;
    }

    let sub_args: Vec<String> = args.iter().skip(1).cloned().collect();

    match cmd {
        "help" | "-h" | "--help" => cmd_help(&sub_args),
        "version" | "-v" | "--version" => cmd_version(),
        "init" => cmd_init(&sub_args),
        "runtime" => cmd_runtime(&sub_args),
        "status" => cmd_status(&sub_args),
        "inspect" => cmd_inspect(&sub_args),
        "steps" => cmd_steps(&sub_args),
        "events" => cmd_events(&sub_args),
        "export" => cmd_export(&sub_args),
        "health" => cmd_health(&sub_args),
        "compact" => cmd_compact(&sub_args),
        other => {
            eprintln!("Unknown command: {}\nRun `delite help` for usage.", other);
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------------------
// help
// ---------------------------------------------------------------------------

fn cmd_help(args: &[String]) {
    if let Some(cmd) = args.first() {
        match cmd.as_str() {
            "init" => println!("delite init <name> [--lang python|typescript]\n\n  Create a new agent project."),
            "runtime" => println!("delite runtime [--auth-token <token>]\n\n  Start the execution runtime. SDKs connect via stdio."),
            "status" => println!("delite status [--data-dir <path>] [--json]\n\n  List all executions."),
            "inspect" => println!("delite inspect <id> [--data-dir <path>] [--json]\n\n  Detailed view of an execution."),
            "steps" => println!("delite steps <id> [--data-dir <path>]\n\n  Step-by-step timeline with timing."),
            "events" => println!("delite events <id> [--data-dir <path>] [--json]\n\n  Raw event log."),
            "export" => println!("delite export <id> [--data-dir <path>] [-o <file>]\n\n  Export execution as JSON."),
            "health" => println!("delite health [--data-dir <path>]\n\n  Storage health check."),
            "compact" => println!("delite compact [--data-dir <path>]\n\n  Compact old event logs."),
            _ => eprintln!("Unknown command: {}", cmd),
        }
        return;
    }

    println!("\n{}delite{} — the sqlite of durable agent execution\n", C::bold(), C::reset());
    println!("{}Getting started:{}",       C::bold(), C::reset());
    println!("  delite init <name> [--lang python|typescript]\n");
    println!("{}Runtime:{}",               C::bold(), C::reset());
    println!("  delite runtime [--auth-token <token>]\n");
    println!("{}Inspection:{}",            C::bold(), C::reset());
    println!("  delite status     [--data-dir <path>]   List all executions");
    println!("  delite inspect <id>                     Detailed view");
    println!("  delite steps <id>                       Step timeline");
    println!("  delite events <id>                      Raw event log");
    println!("  delite export <id> [-o file]            Export as JSON\n");
    println!("{}Operations:{}",            C::bold(), C::reset());
    println!("  delite health                           Storage health");
    println!("  delite compact                          Compact event logs\n");
    println!("{}Info:{}",                  C::bold(), C::reset());
    println!("  delite version                          Show version");
    println!("  delite help <command>                   Command help\n");
    println!("{}Environment:{}",           C::bold(), C::reset());
    println!("  DELITE_DATA_DIR      Data directory (default: ./data)");
    println!("  OPENAI_API_KEY       OpenAI provider");
    println!("  ANTHROPIC_API_KEY    Anthropic provider\n");
}

fn cmd_version() {
    println!("delite {} (protocol v{})", env!("CARGO_PKG_VERSION"), delite_core::protocol::PROTOCOL_VERSION);
}

// ---------------------------------------------------------------------------
// runtime
// ---------------------------------------------------------------------------

fn cmd_runtime(args: &[String]) {
    let auth = extract_flag(args, "--auth-token")
        .map(String::from)
        .or_else(|| std::env::var("DELITE_AUTH_TOKEN").ok());
    delite_core::protocol::sdk_mode::run_sdk_mode_with_auth(auth.as_deref());
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

fn cmd_init(args: &[String]) {
    let name = positional_arg(args, 0).unwrap_or("my-agent");
    let lang = extract_flag(args, "--lang").unwrap_or("python");
    let dir = Path::new(name);

    if dir.exists() && std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        eprintln!("Error: '{}' already exists and is not empty.", name);
        std::process::exit(1);
    }
    std::fs::create_dir_all(dir).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });

    match lang {
        "python" | "py" => {
            wf(dir, "agent.py", PYTHON_AGENT_TEMPLATE);
            wf(dir, ".env.example", "# Add your API key\nOPENAI_API_KEY=sk-...\n");
            wf(dir, ".gitignore", "/data/\n.env\n__pycache__/\n");
            println!("\n  {}{}Created {}/{}\n", C::green(), C::bold(), name, C::reset());
            println!("  {}/agent.py       Your agent (ready to run)", name);
            println!("  {}/.env.example   API key template\n", name);
            println!("  {}Get started:{}", C::bold(), C::reset());
            println!("    cd {}", name);
            println!("    cp .env.example .env");
            println!("    python agent.py\n");
        }
        "typescript" | "ts" => {
            wf(dir, "agent.ts", TS_AGENT_TEMPLATE);
            wf(dir, ".env.example", "# Add your API key\nOPENAI_API_KEY=sk-...\n");
            wf(dir, ".gitignore", "/data/\n.env\nnode_modules/\n");
            println!("\n  {}{}Created {}/{} (TypeScript)\n", C::green(), C::bold(), name, C::reset());
            println!("  {}TypeScript SDK coming soon.{}", C::yellow(), C::reset());
            println!("  Use Python today: delite init {} --lang python\n", name);
        }
        other => { eprintln!("Unknown language: {}. Supported: python, typescript", other); std::process::exit(1); }
    }
}

fn wf(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap_or_else(|e| { eprintln!("Error writing {}: {}", name, e); std::process::exit(1); });
}

const PYTHON_AGENT_TEMPLATE: &str = r#"#!/usr/bin/env python3
"""Your durable agent — crash-recoverable with exactly-once tool execution.

Run:     python agent.py
Resume:  python agent.py --resume <execution-id>
Inspect: delite status --data-dir ./data
"""

import sys
from durable import Agent, tool
from durable.providers import OpenAI


@tool("search", description="Search for information on a topic")
def search(query: str) -> dict:
    print(f"  [search] {query}")
    return {"results": [{"title": f"Result for '{query}'", "snippet": "Information found."}]}


@tool("save_note", description="Save a research note")
def save_note(title: str, content: str) -> dict:
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
"#;

const TS_AGENT_TEMPLATE: &str = r#"// Delite TypeScript SDK — coming soon.
// Use Python today: delite init my-agent --lang python
console.log("TypeScript SDK coming soon.");
"#;

// ---------------------------------------------------------------------------
// status
// ---------------------------------------------------------------------------

fn cmd_status(args: &[String]) {
    let dd = find_data_dir(args);
    let storage = FileStorage::new(&dd).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });
    let event_store = FileEventStore::new(&dd).ok();
    let execs = storage.list_executions(None).unwrap_or_default();

    if has_flag(args, "--json") {
        let items: Vec<json::Value> = execs.iter().map(|m| {
            json::json_object(vec![
                ("id", json::json_str(&m.id.to_string())),
                ("status", json::json_str(&format!("{:?}", m.status))),
                ("step_count", json::json_num(m.step_count as f64)),
                ("created_at", json::json_num(m.created_at as f64)),
            ])
        }).collect();
        println!("{}", json::to_string_pretty(&json::json_array(items)));
        return;
    }

    if execs.is_empty() { println!("  No executions found in {}", dd); return; }

    println!("  {}{:<40}{} {:>6} {:>8} {:>12}", C::bold(), "ID", C::reset(), "STEPS", "EVENTS", "STATUS");
    println!("  {:<40} {:>6} {:>8} {:>12}", "─".repeat(40), "──────", "────────", "────────────");

    for m in &execs {
        let id = m.id.to_string();
        let ev = event_store.as_ref().and_then(|es| es.events(m.id).ok()).map(|e| e.len()).unwrap_or(0);
        println!("  {}{:<40}{} {:>6} {:>8} {:>12}", C::cyan(), &id[..std::cmp::min(40, id.len())], C::reset(), m.step_count, ev, fmt_status(&m.status));
    }
    println!("\n  {} executions in {}", execs.len(), dd);
}

// ---------------------------------------------------------------------------
// inspect
// ---------------------------------------------------------------------------

fn cmd_inspect(args: &[String]) {
    let id_str = positional_arg(args, 0).unwrap_or_else(|| { eprintln!("Usage: delite inspect <id>"); std::process::exit(2); });
    let dd = find_data_dir(args);
    let es = FileEventStore::new(&dd).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });
    let exec_id = parse_exec_id(id_str);
    let events = es.events(exec_id).unwrap_or_default();
    if events.is_empty() { eprintln!("No events for {}", id_str); std::process::exit(1); }

    let state = delite_core::storage::event::ExecutionState::from_events(exec_id, &events);

    println!("\n  {}Execution:{} {}", C::bold(), C::reset(), id_str);
    println!("  {}", "─".repeat(58));
    if let Some(ref a) = state.agent_id { println!("    Agent:       {}", a); }
    println!("    Status:      {}", fmt_status(&state.status));
    println!("    Created:     {}", fmt_time(state.created_at));
    if state.updated_at > state.created_at { println!("    Duration:    {}", fmt_duration(state.updated_at - state.created_at)); }
    println!("    Steps:       {}", state.step_count);
    println!("    Events:      {}", events.len());
    if let Some(h) = state.prompt_hash { println!("    Prompt hash: {:016x}", h); }
    if let Some(h) = state.tools_hash { println!("    Tools hash:  {:016x}", h); }
    println!();
}

// ---------------------------------------------------------------------------
// steps
// ---------------------------------------------------------------------------

fn cmd_steps(args: &[String]) {
    let id_str = positional_arg(args, 0).unwrap_or_else(|| { eprintln!("Usage: delite steps <id>"); std::process::exit(2); });
    let dd = find_data_dir(args);
    let es = FileEventStore::new(&dd).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });
    let events = es.events(parse_exec_id(id_str)).unwrap_or_default();
    if events.is_empty() { eprintln!("No events for {}", id_str); std::process::exit(1); }

    println!("\n  {}Timeline:{} {}\n", C::bold(), C::reset(), id_str);

    let mut prev_ts = events[0].timestamp;
    for event in &events {
        let delta = event.timestamp.saturating_sub(prev_ts);
        let ds = if delta == 0 { String::new() } else { fmt_duration(delta) };
        prev_ts = event.timestamp;

        let (icon, color, name, detail) = match &event.event_type {
            EventType::ExecutionCreated { agent_id, .. } =>
                ("●", C::green(), "execution_created", agent_id.as_deref().unwrap_or("").to_string()),
            EventType::StepStarted { step_name, step_number, .. } =>
                ("▶", C::blue(), "step_started", format!("#{} {}", step_number, step_name)),
            EventType::StepCompleted { step_name, step_number, .. } =>
                ("✓", C::green(), "step_completed", format!("#{} {}", step_number, step_name)),
            EventType::StepFailed { step_name, error, .. } =>
                ("✗", C::red(), "step_failed", format!("{}: {}", step_name, &error[..error.len().min(60)])),
            EventType::Suspended { .. } =>
                ("⏸", C::yellow(), "suspended", String::new()),
            EventType::Resumed { generation } =>
                ("▶", C::blue(), "resumed", format!("gen={}", generation)),
            EventType::ExecutionCompleted { .. } =>
                ("●", C::green(), "completed", String::new()),
            EventType::ExecutionFailed { error } =>
                ("●", C::red(), "failed", error[..error.len().min(60)].to_string()),
            EventType::LeaseAcquired { generation, .. } =>
                ("🔒", C::dim(), "lease_acquired", format!("gen={}", generation)),
            EventType::LeaseReleased { generation } =>
                ("🔓", C::dim(), "lease_released", format!("gen={}", generation)),
            _ => ("·", C::dim(), "event", String::new()),
        };

        println!("    {} {}{:<28}{} {:>8}  {}{}{}", icon, color, name, C::reset(), ds, C::dim(), detail, C::reset());
    }
    println!();
}

// ---------------------------------------------------------------------------
// events
// ---------------------------------------------------------------------------

fn cmd_events(args: &[String]) {
    let id_str = positional_arg(args, 0).unwrap_or_else(|| { eprintln!("Usage: delite events <id>"); std::process::exit(2); });
    let dd = find_data_dir(args);

    if has_flag(args, "--json") {
        let path = Path::new(&dd).join("events").join(format!("{}.ndjson", id_str));
        if let Ok(c) = std::fs::read_to_string(&path) { print!("{}", c); }
        else { eprintln!("No event file for {}", id_str); std::process::exit(1); }
    } else {
        let es = FileEventStore::new(&dd).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });
        let events = es.events(parse_exec_id(id_str)).unwrap_or_default();
        for e in &events { println!("{}", json::to_string(&e.to_json())); }
    }
}

// ---------------------------------------------------------------------------
// export
// ---------------------------------------------------------------------------

fn cmd_export(args: &[String]) {
    let id_str = positional_arg(args, 0).unwrap_or_else(|| { eprintln!("Usage: delite export <id> [-o file]"); std::process::exit(2); });
    let dd = find_data_dir(args);
    let out_file = extract_flag(args, "-o");
    let es = FileEventStore::new(&dd).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });
    let events = es.events(parse_exec_id(id_str)).unwrap_or_default();

    let export = json::json_object(vec![
        ("execution_id", json::json_str(id_str)),
        ("event_count", json::json_num(events.len() as f64)),
        ("events", json::json_array(events.iter().map(|e| e.to_json()).collect())),
    ]);
    let output = json::to_string_pretty(&export);

    if let Some(f) = out_file {
        std::fs::write(f, &output).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });
        println!("  {}✓{} Exported to {} ({})", C::green(), C::reset(), f, fmt_size(output.len() as u64));
    } else {
        println!("{}", output);
    }
}

// ---------------------------------------------------------------------------
// health
// ---------------------------------------------------------------------------

fn cmd_health(args: &[String]) {
    let dd = find_data_dir(args);
    let storage = FileStorage::new(&dd).ok();
    let exec_count = storage.as_ref().and_then(|s| s.list_executions(None).ok()).map(|e| e.len()).unwrap_or(0);

    let events_dir = Path::new(&dd).join("events");
    let mut total_events = 0u64;
    let mut total_size = 0u64;

    if events_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&events_dir) {
            for entry in entries.flatten() {
                if entry.path().extension().map(|e| e == "ndjson").unwrap_or(false) {
                    if let Ok(m) = entry.metadata() { total_size += m.len(); }
                    if let Ok(c) = std::fs::read_to_string(entry.path()) {
                        total_events += c.lines().filter(|l| !l.trim().is_empty()).count() as u64;
                    }
                }
            }
        }
    }

    println!("\n  {}Storage:{} {}", C::bold(), C::reset(), dd);
    println!("  {}", "─".repeat(46));
    println!("    Executions:    {}", exec_count);
    println!("    Total events:  {}", total_events);
    println!("    Storage size:  {}", fmt_size(total_size));

    let mut ok = true;
    if total_size > 100 * 1024 * 1024 { println!("    {}⚠ Storage exceeds 100MB{}", C::yellow(), C::reset()); ok = false; }
    if total_events > 10_000 { println!("    {}⚠ Over 10K events — consider compacting{}", C::yellow(), C::reset()); ok = false; }
    if ok { println!("\n    {}✓ Storage is healthy{}", C::green(), C::reset()); }
    println!();
}

// ---------------------------------------------------------------------------
// compact
// ---------------------------------------------------------------------------

fn cmd_compact(args: &[String]) {
    let dd = find_data_dir(args);
    let es = FileEventStore::new(&dd).unwrap_or_else(|e| { eprintln!("Error: {}", e); std::process::exit(1); });
    let ids = es.list_execution_ids().unwrap_or_default();
    let mut n = 0;
    for id in &ids {
        let events = es.events(*id).unwrap_or_default();
        if events.len() > 20 {
            match es.compact(*id) {
                Ok(_) => { println!("  Compacted {} ({} events)", id, events.len()); n += 1; }
                Err(e) => eprintln!("  {}Error compacting {}:{} {}", C::red(), id, C::reset(), e),
            }
        }
    }
    if n == 0 { println!("  {}✓{} Nothing to compact", C::green(), C::reset()); }
    else { println!("\n  Compacted {} executions", n); }
}
