mod acp;
mod agent;
mod auth;
mod llm;
#[cfg(feature = "mcp")]
mod mcp;
mod skills;
mod tools;

use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "openab-agent", about = "Native Rust coding agent with ACP")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with an LLM provider
    Auth {
        #[command(subcommand)]
        provider: AuthProvider,
    },
    /// Inspect / manage configured MCP servers
    #[cfg(feature = "mcp")]
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

#[cfg(feature = "mcp")]
#[derive(Subcommand)]
enum McpAction {
    /// List configured MCP servers (loads global + project mcp.json)
    List {
        /// Substitute ${env:VAR} placeholders with real values.
        /// WARNING: output will contain secrets if your config references
        /// tokens via env vars — do not paste publicly.
        #[arg(long)]
        resolve: bool,
    },
}

#[derive(Subcommand)]
enum AuthProvider {
    /// OpenAI Codex via browser PKCE flow (recommended, full scopes)
    CodexOauth {
        /// Print URL instead of opening browser
        #[arg(long)]
        no_browser: bool,
    },
    /// OpenAI Codex via device code (headless servers)
    CodexDevice,
    /// Show stored credentials
    Status,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        None => {
            // Default: run ACP server
            let mut server = acp::AcpServer::new();
            server.run().await;
        }
        Some(Commands::Auth { provider }) => match provider {
            AuthProvider::CodexOauth { no_browser } => {
                if let Err(e) = auth::login_browser_flow(no_browser).await {
                    eprintln!("❌ Authentication failed: {e}");
                    std::process::exit(1);
                }
            }
            AuthProvider::CodexDevice => {
                if let Err(e) = auth::login_codex_device_flow().await {
                    eprintln!("❌ Authentication failed: {e}");
                    std::process::exit(1);
                }
            }
            AuthProvider::Status => {
                auth::show_status();
            }
        },
        #[cfg(feature = "mcp")]
        Some(Commands::Mcp { action }) => match action {
            McpAction::List { resolve } => mcp::cli_list_servers(resolve),
        },
    }
}
