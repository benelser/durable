//! Minimal example: a durable agent in under 10 lines of setup.

use durable_runtime::*;

fn main() -> Result<(), DurableError> {
    let agent = durable_runtime::agent_in_memory(MockLlmClient::new(vec![
        LlmResponse::text("Hello! How can I help you today?"),
    ]));

    let response = agent.run("Hello, world!")?;
    println!("Agent: {response}");
    println!("Execution {}: {:?}", response.execution_id(), response.status());
    Ok(())
}
