//! Standalone ACP server for testing with Cherry.
//!
//! Run: cargo run -p hermes-acp-server --example standalone

use hermes_acp_server::executor::llm::LlmExecutor;
use hermes_acp_server::{AcpPipeServer, AcpServerConfig, AgentInfo};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter("hermes_acp_server=debug,info")
        .init();

    println!("=== Hermes ACP Server (Standalone) ===");

    let executor = match LlmExecutor::from_hermes_config().await {
        Some(e) => {
            println!("LLM: {} @ {}", e.model, e.base_url);
            std::sync::Arc::new(e)
        }
        None => {
            eprintln!("ERROR: Could not load LLM config from ~/.hermes/config.yaml");
            eprintln!(
                "Make sure llm_providers.custom is configured with api_key, base_url, model."
            );
            std::process::exit(1);
        }
    };

    let pipe_path = hermes_acp_server::default_pipe_path();
    println!("Pipe: {}", pipe_path);

    let config = AcpServerConfig {
        pipe_path: pipe_path.clone(),
        max_connections: 5,
        prompt_timeout_secs: 300,
        agent_info: AgentInfo {
            name: "hermes-agent".to_string(),
            title: "Hermes Agent Ultra".to_string(),
            version: "0.1.0".to_string(),
        },
        executor,
        event_sink: None,
    };

    let server = AcpPipeServer::new(config).expect("Failed to create server");

    println!("Listening on: {}", server.endpoint());
    println!("Waiting for Cherry (AI_Router) to connect...");
    println!("Press Ctrl+C to stop.\n");

    if let Err(e) = server.run().await {
        eprintln!("Server error: {}", e);
    }
}
