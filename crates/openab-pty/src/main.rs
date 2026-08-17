//! `openab-pty` binary.
//!
//! **Startup order is load-bearing**, and it is the reason this is not a
//! three-line `#[tokio::main]`:
//!
//! 1. `PR_SET_DUMPABLE=0` (fail-closed on Linux) — **before any secret is read**.
//!    On Linux this is the only barrier that stops a same-UID session child from
//!    reading this process through `/proc/<pid>/mem`, `/proc/<pid>/fd`, or
//!    `ptrace`. It must precede reading the projection, because the projection
//!    carries the admin verifier, and it must precede minting anything.
//! 2. Validate the delivered projection (fail-closed). A poisoned projection —
//!    `[secrets.refs]`, a `${secrets.*}` interpolation, a broker section, an
//!    interpolated or malformed `admin_credential_hash` — is a startup error,
//!    never a silently degraded auth posture.
//! 3. Kill-domain tier selection, logging the active tier and what it does not
//!    promise. An operator who required Tier 2 is refused here: it is not
//!    implemented, and best effort must never be served under a guarantee's name.
//! 4. Bind, behind the fail-closed listener guard.
//!
//! Graceful shutdown runs the same order in reverse: notice → grace →
//! `close_code::RUNTIME_REPLACED` → session teardown.

use anyhow::{bail, Context, Result};
use clap::Parser;
use openab_pty::admin_auth::AdminAuthenticator;
use openab_pty::audit::AuditLogger;
use openab_pty::config::{self, PtyConfig};
use openab_pty::containment::{self, ContainmentStatus};
use openab_pty::killdomain::{self, establish_subreaper, KillDomain, TrackingLimits};
use openab_pty::server::{self, AppState, ServerConfig};
use openab_pty::session::{PortablePtySpawner, SessionManager, SessionPolicy};
use openab_pty::token::TokenStore;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser, Debug)]
#[command(
    name = "openab-pty",
    version,
    about = "OpenAB PTY runtime — remote sandboxed terminal sessions (internal, unpublished)"
)]
struct Cli {
    /// Path to the delivered `[pty]` projection.
    #[arg(
        short,
        long,
        value_name = "FILE",
        default_value = "/etc/openab-pty/config.toml"
    )]
    config: PathBuf,

    /// Validate a projection and exit. Same validator the runtime applies at
    /// startup, exposed so an operator can check a hand-generated projection and
    /// CI can reuse it as a guard test.
    #[arg(long, value_name = "FILE")]
    validate_projection: Option<PathBuf>,

    /// Generate a bootstrap admin credential and its verifier hash, then exit.
    /// The credential is printed once, to the operator's terminal, and is never
    /// stored by the runtime; only the `sha256:` verifier belongs in a projection.
    #[arg(long)]
    generate_admin_credential: bool,
}

fn main() -> Result<()> {
    // DEFERRED to Gate B: a JSON logging layer. Nothing ingests these logs while
    // the crate is internal and unpublished; `tracing`'s text output is what an
    // operator reads with `kubectl logs`/`ecs execute-command` today, and the
    // audit trail — the part that must be machine-readable — is already
    // structured and serialised by `crate::audit`.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();

    // (1) Containment first. Nothing above this line reads a credential, and
    // nothing below it may run if the barrier could not be established.
    match containment::establish_transient_secret_barrier() {
        Ok(ContainmentStatus::Active) => {
            tracing::info!("transient-secret barrier active (PR_SET_DUMPABLE=0)")
        }
        Ok(ContainmentStatus::Unsupported) => tracing::warn!(
            "PR_SET_DUMPABLE is Linux-only and this is not Linux: the same-UID containment \
             model does NOT hold on this platform. Non-Linux targets are out of scope for the \
             MVP and must not be treated as protected"
        ),
        Err(error) => {
            // Fail closed: refuse to serve *before* reading any credential input.
            bail!("refusing to start: could not establish PR_SET_DUMPABLE=0 ({error})");
        }
    }

    if cli.generate_admin_credential {
        return print_generated_admin_credential();
    }

    if let Some(path) = cli.validate_projection.as_ref() {
        return validate_only(path);
    }

    // (2) Fail-closed projection validation.
    let projection = config::validate_projection_file(&cli.config)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .with_context(|| format!("invalid PTY projection at {}", cli.config.display()))?;
    if !projection.enabled {
        bail!(
            "[pty].enabled is false in {}: refusing to serve a disabled runtime",
            cli.config.display()
        );
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building the Tokio runtime")?;
    runtime.block_on(run(projection))
}

fn print_generated_admin_credential() -> Result<()> {
    let generated = AdminAuthenticator::generate();
    // Printed to stdout on the operator's trusted side only. It is never written
    // to a config file, never placed in an environment variable, and never
    // retained by the runtime — only the verifier below is delivered.
    println!(
        "credential: {}",
        String::from_utf8_lossy(generated.plaintext.as_bytes())
    );
    println!("admin_credential_hash = \"{}\"", generated.verifier);
    Ok(())
}

fn validate_only(path: &PathBuf) -> Result<()> {
    match config::validate_projection_file(path) {
        Ok(projection) => {
            println!("{}: OK", path.display());
            println!("  listen                  = {}", projection.listen);
            println!(
                "  command                 = {}",
                projection.command.display()
            );
            println!("  max_sessions            = {}", projection.max_sessions);
            println!(
                "  absolute_session_ttl    = {}s",
                projection.absolute_session_ttl.as_secs()
            );
            println!(
                "  kill_domain_tier        = {}",
                projection.kill_domain_requirement.as_config_value()
            );
            // The tier is never surfaced without its guarantee label, here
            // included: `describe()` carries the BEST-EFFORT wording the ADR
            // requires wherever the tier appears.
            match killdomain::resolve_tier(projection.kill_domain_requirement) {
                Ok(tier) => println!("  {}", tier.describe()),
                Err(error) => {
                    // A validator that prints OK for a projection the runtime
                    // refuses to start on is a broken measurement.
                    eprintln!("{}: REJECTED: {error}", path.display());
                    std::process::exit(1);
                }
            }
            Ok(())
        }
        Err(error) => {
            // A non-zero exit is what makes this usable as a CI guard.
            eprintln!("{}: REJECTED: {error}", path.display());
            std::process::exit(1);
        }
    }
}

async fn run(projection: PtyConfig) -> Result<()> {
    let audit = AuditLogger;

    // (3) Kill domain. Reaping first (best effort, never fail-closed), then the
    // operator-actionable log line. Tier 2 is not implemented, so an operator
    // who required it is refused here rather than served best effort.
    let subreaper = establish_subreaper();
    tracing::info!(?subreaper, "child-subreaper status");

    let tier = killdomain::resolve_tier(projection.kill_domain_requirement)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("kill-domain tier selection")?;
    tracing::info!("{}", tier.describe());
    let kill = Arc::new(KillDomain::new(TrackingLimits::default(), audit.clone()));

    let spawner = Arc::new(PortablePtySpawner);
    let tokens = TokenStore::new(config.attach_token_ttl, audit.clone());
    let verifier = tokens.attach_verifier();
    let admin =
        AdminAuthenticator::from_verifier_hash(&projection.admin_credential_hash, audit.clone())
            .map_err(|error| anyhow::anyhow!("{error}"))
            .context("admin verifier")?;
    tracing::info!(
        verifier = admin.verifier_fingerprint(),
        "remote admin plane enabled: the credential is the only boundary (no in-container admin \
         socket exists, and locality authorizes nothing)"
    );

    let policy = SessionPolicy::from_config(&projection);
    let listen = projection.listen.clone();
    let admin_hash = projection.admin_credential_hash.clone();
    let tls_terminated_upstream = projection.tls_terminated_upstream;
    let server_config = ServerConfig {
        listen: listen.clone(),
        tls_terminated_upstream,
        drain_grace: Duration::from_secs(3),
        tick_interval: Duration::from_secs(1),
    };

    let manager = SessionManager::new(projection, policy, tokens, audit.clone(), kill, spawner)
        .map_err(|error| anyhow::anyhow!("{error}"))
        .context("session manager")?;

    // (4) Bind behind the fail-closed guard.
    let listener = server::bind(&listen, &admin_hash, tls_terminated_upstream)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let local = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or_else(|_| listen.clone());
    tracing::info!(
        listen = %local,
        tls_terminated_upstream,
        "openab-pty listening: GET /pty/{{session}} attaches, /admin/* is the whole admin plane"
    );

    let state = AppState::new(manager, verifier, admin, audit, server_config);
    server::serve(state, listener, shutdown_signal())
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    tracing::info!("openab-pty stopped");
    Ok(())
}

/// SIGTERM (the container path) or Ctrl-C (the interactive one).
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(error) => {
                    tracing::warn!(%error, "SIGTERM handler unavailable; Ctrl-C only");
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
        tokio::select! {
            _ = term.recv() => tracing::info!("SIGTERM received"),
            _ = tokio::signal::ctrl_c() => tracing::info!("interrupt received"),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
