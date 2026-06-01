mod acp;
mod agent;
mod auth;
mod llm;
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
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
}

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
    /// Show per-server runtime status
    Status,
    /// Spawn the configured server and run the MCP handshake (smoke-test).
    Connect {
        /// Server name as configured in mcp.json
        name: String,
    },
    /// Authenticate with an MCP server's OAuth provider (paste-back flow,
    /// ADR §6.4). Prints the authorize URL, then reads the post-redirect
    /// URL from stdin.
    ///
    /// For non-interactive use, prefer piping the URL via stdin
    /// (`echo "<url>" | openab-agent mcp login <name>`) over `--paste` —
    /// pipes leave no trace in shell history or `ps` output. PKCE makes
    /// either route safe in theory; the pipe form is defense-in-depth.
    Login {
        /// Server name as configured in mcp.json
        name: String,
        /// Pre-fill the redirect URL (skip the stdin prompt). Convenient
        /// for ad-hoc testing; CI / scripts should prefer the stdin pipe
        /// form to keep `code` + `state` out of shell history and `ps`.
        #[arg(long, value_name = "URL")]
        paste: Option<String>,
        /// Use RFC 8628 device-code flow instead of paste-back. Requires
        /// the server's `oauth:` block to declare a
        /// `device_authorization_endpoint`. Useful for headless / remote
        /// hosts where the browser redirect target isn't reachable.
        #[arg(long, conflicts_with = "paste")]
        device: bool,
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
        Some(Commands::Mcp { action }) => match action {
            McpAction::List { resolve } => mcp::cli_list_servers(resolve),
            McpAction::Status => mcp::cli_show_status().await,
            McpAction::Connect { name } => mcp::cli_connect(name).await,
            McpAction::Login {
                name,
                paste,
                device,
            } => {
                if device {
                    mcp::cli_login_device(name).await;
                } else {
                    mcp::cli_login(name, paste).await;
                }
            }
        },
    }
}
