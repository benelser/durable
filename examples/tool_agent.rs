//! Agent with tools: demonstrates durable tool execution with memoization.

use durable_runtime::*;
use std::sync::atomic::{AtomicU32, Ordering};

static WEATHER_CALLS: AtomicU32 = AtomicU32::new(0);
static CALC_CALLS: AtomicU32 = AtomicU32::new(0);

fn main() -> Result<(), DurableError> {
    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![
            // First: request two tool calls
            LlmResponse::tool_calls(vec![
                ToolCall {
                    id: "call_1".into(),
                    name: "get_weather".into(),
                    arguments: json!({ "location": "San Francisco" }),
                },
                ToolCall {
                    id: "call_2".into(),
                    name: "calculator".into(),
                    arguments: json!({ "expression": "72 + 45" }),
                },
            ]),
            // Then: final answer using tool results
            LlmResponse::text(
                "The weather in SF is 72F and sunny. The sum of temp and humidity is 117.",
            ),
        ]))
        .system_prompt("You are a helpful assistant with weather and calculator tools.")
        .tool("get_weather", "Get current weather for a location",
            json!({
                "type": "object",
                "properties": { "location": { "type": "string" } },
                "required": ["location"]
            }),
            |args: &Value| {
                let n = WEATHER_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
                let loc = args.string("location")?;
                println!("  [Weather #{} for: {}]", n, loc);
                Ok(json!({ "location": loc, "temperature": 72, "conditions": "sunny" }))
            },
        )
        .tool("calculator", "Evaluate a math expression",
            json!({
                "type": "object",
                "properties": { "expression": { "type": "string" } },
                "required": ["expression"]
            }),
            |args: &Value| {
                let n = CALC_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
                let expr = args.string("expression")?;
                println!("  [Calculator #{}: {}]", n, expr);
                Ok(json!({ "expression": expr, "result": 117 }))
            },
        )
        .build();

    println!("User: What's the weather in SF and what's the temp plus humidity?\n");
    let response = runtime.run("What's the weather in SF and what's the temp plus humidity?")?;
    println!("\nAgent: {response}");

    println!("\nWeather calls: {}", WEATHER_CALLS.load(Ordering::SeqCst));
    println!("Calc calls: {}", CALC_CALLS.load(Ordering::SeqCst));
    Ok(())
}
