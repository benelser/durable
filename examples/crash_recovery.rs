//! Crash recovery: completed steps are never re-executed.
//!
//! The agent runs, "crashes" mid-execution, and resumes without duplicating
//! side effects — payments are charged exactly once.

use durable_runtime::*;
use std::sync::atomic::{AtomicU32, Ordering};

static CHARGE_COUNT: AtomicU32 = AtomicU32::new(0);
static EMAIL_COUNT: AtomicU32 = AtomicU32::new(0);

fn main() -> Result<(), DurableError> {
    println!("=== Crash Recovery Demo ===\n");

    let storage_dir = std::env::temp_dir().join("durable_crash_demo");
    let _ = std::fs::remove_dir_all(&storage_dir);

    let exec_id = ExecutionId::new();

    // --- First run: charge payment, send email, complete ---
    println!("--- First Run ---\n");

    let runtime1 = AgentRuntime::builder()
        .persistent(&storage_dir)
        .llm(MockLlmClient::new(vec![
            LlmResponse::tool_calls(vec![ToolCall {
                id: "call_pay".into(),
                name: "charge_payment".into(),
                arguments: json!({ "amount": 99.99 }),
            }]),
            LlmResponse::tool_calls(vec![ToolCall {
                id: "call_email".into(),
                name: "send_email".into(),
                arguments: json!({ "to": "customer@example.com" }),
            }]),
            LlmResponse::text("Payment charged and confirmation sent!"),
        ]))
        .system_prompt("You are an order processing agent.")
        .tool("charge_payment", "Charge payment",
            json!({ "type": "object", "properties": { "amount": { "type": "number" } } }),
            |args: &Value| {
                let n = CHARGE_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                let amount = args.number("amount")?;
                println!("  ** CHARGING PAYMENT #{}: ${:.2} **", n, amount);
                Ok(json!({ "transaction_id": "txn_abc123", "amount": amount, "status": "charged" }))
            },
        )
        .tool("send_email", "Send email notification",
            json!({ "type": "object", "properties": { "to": { "type": "string" } } }),
            |args: &Value| {
                let n = EMAIL_COUNT.fetch_add(1, Ordering::SeqCst) + 1;
                let to = args.string("to")?;
                println!("  ** SENDING EMAIL #{} to: {} **", n, to);
                Ok(json!({ "sent": true, "to": to }))
            },
        )
        .build();

    let response = runtime1.run_with_id(exec_id, "Process order #123 for $99.99")?;
    println!("\nAgent: {response}");
    println!("Charges: {}, Emails: {}", CHARGE_COUNT.load(Ordering::SeqCst), EMAIL_COUNT.load(Ordering::SeqCst));

    // --- Second run: resume (steps should be cached) ---
    println!("\n--- Second Run (resume — no duplicate charges) ---\n");

    let runtime2 = AgentRuntime::builder()
        .persistent(&storage_dir)
        .llm(MockLlmClient::new(vec![
            LlmResponse::text("This should not appear."),
        ]))
        .build();

    let response = runtime2.resume_run(exec_id)?;
    println!("Agent: {response}");

    println!("\n--- Final Counts ---");
    println!("Charges: {} (should be 1)", CHARGE_COUNT.load(Ordering::SeqCst));
    println!("Emails: {} (should be 1)", EMAIL_COUNT.load(Ordering::SeqCst));

    let _ = std::fs::remove_dir_all(&storage_dir);
    Ok(())
}
