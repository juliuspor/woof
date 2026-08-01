use std::process::ExitCode;

use tokio::io::BufReader;
use woof_core::{ApiToken, WoofPaths};
use woof_mcp::{McpBridge, DEFAULT_DAEMON_URL};

#[tokio::main]
async fn main() -> ExitCode {
    if let Err(error) = run().await {
        eprintln!("woof-mcp failed: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let paths = WoofPaths::discover().ok_or("could not discover the macOS home directory")?;
    let token = ApiToken::load_or_replace_invalid(&paths.token_path)?;
    let bridge = McpBridge::new(DEFAULT_DAEMON_URL, token)?;
    bridge
        .serve(BufReader::new(tokio::io::stdin()), tokio::io::stdout())
        .await?;
    Ok(())
}
