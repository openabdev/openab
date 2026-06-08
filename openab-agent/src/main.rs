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
    /// Diagnose each configured server end-to-end: env vars, OAuth token,
    /// live connect. Prints actionable remediation hints and exits
    /// non-zero on any server failure (ADR §8).
    Doctor,
    /// Spawn the configured server and run the MCP handshake (smoke-test).
    Connect {
        /// Server name as configured in mcp.json
        name: String,
    },
    /// Authenticate with an MCP server's OAuth provider (paste-back flow,
    /// ADR §6.4). Prints the authorize URL, then reads the post-redirect
    /// URL from stdin.
    ///
    /// Single-invocation by design: the PKCE/CSRF state lives in-memory for
    /// this run only, so the authorize URL and the pasted redirect must be
    /// handled in the same process. Paste interactively at the prompt.
    Login {
        /// Server name as configured in mcp.json
        name: String,
        /// Use RFC 8628 device-code flow instead of paste-back. Requires
        /// the server's `oauth:` block to declare a
        /// `device_authorization_endpoint`. Useful for headless / remote
        /// hosts where the browser redirect target isn't reachable.
        #[arg(long)]
        device: bool,
        /// Extra OAuth scope to request on top of the configured set, for
        /// step-up re-auth (A3). Repeatable. When a tool call fails with
        /// `insufficient_scope`, the error names the scope to pass here.
        /// Ignored by `--device`.
        #[arg(long)]
        scope: Vec<String>,
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
            McpAction::Doctor => mcp::cli_doctor().await,
            McpAction::Connect { name } => mcp::cli_connect(name).await,
            McpAction::Login {
                name,
                device,
                scope,
            } => {
                if device {
                    mcp::cli_login_device(name).await;
                } else {
                    mcp::cli_login(name, scope).await;
                }
            }
        },
    }
}
