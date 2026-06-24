use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

use codi_mock_server::MockConfig;

#[derive(Parser)]
#[command(name = "codi-mock-server", about = "OpenAI-compatible mock server for codi testing")]
struct Cli {
    #[arg(long, default_value = "0")]
    port: u16,
    #[arg(long, default_value = "Mock assistant reply.")]
    reply: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    let cfg = MockConfig {
        assistant_reply: cli.reply,
        embed_dim: 8,
    };

    let listener = tokio::net::TcpListener::bind(format!("127.0.0.1:{}", cli.port)).await?;
    let addr = listener.local_addr()?;
    println!("codi-mock-server listening on http://{addr}");

    axum::serve(listener, codi_mock_server::router(cfg)).await?;
    Ok(())
}
