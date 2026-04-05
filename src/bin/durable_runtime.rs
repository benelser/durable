//! Durable Runtime binary entrypoint.
//!
//! Usage:
//!   durable-runtime --sdk-mode                    # SDK mode, no auth
//!   durable-runtime --sdk-mode --auth-token abc   # SDK mode with auth

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if !args.contains(&"--sdk-mode".to_string()) {
        eprintln!("durable-runtime v{}", env!("CARGO_PKG_VERSION"));
        eprintln!();
        eprintln!("Usage:");
        eprintln!("  durable-runtime --sdk-mode                    SDK mode (stdin/stdout protocol)");
        eprintln!("  durable-runtime --sdk-mode --auth-token TOK   SDK mode with authentication");
        std::process::exit(1);
    }

    // Extract --auth-token value
    let auth_token = args
        .iter()
        .position(|a| a == "--auth-token")
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str());

    // Also check DURABLE_AUTH_TOKEN env var
    let env_token = std::env::var("DURABLE_AUTH_TOKEN").ok();
    let effective_token = auth_token.or(env_token.as_deref());

    durable_runtime::protocol::sdk_mode::run_sdk_mode_with_auth(effective_token);
}
