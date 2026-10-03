//! Standalone control-plane binary.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};

use openab_cp::config::CpConfig;
use openab_cp::server::{app, graceful_shutdown, run_sweeper, AppState};

#[derive(Parser)]
#[command(name = "openab-cp", about = "OpenAB Agent Control Plane")]
struct Cli {
    /// Path to the CP config file (TOML).
    #[arg(short, long, default_value = "cp.toml")]
    config: String,
}

/// Slack the listener gets, after the drain has finished, to complete its own
/// graceful shutdown.
///
/// The CP has no long-lived HTTP route: `/health` answers immediately and
/// `/cp` completes its response as soon as the handshake is done (the WebSocket
/// runs detached). So anything still pending once the drain ends is a peer that
/// connected and then stopped talking — not work worth waiting for. The
/// orchestrator's grace period is the real deadline; this only decides how much
/// of it the CP spends on a socket that will never answer.
const LISTENER_SHUTDOWN_SLACK: Duration = Duration::from_secs(1);

/// The signals that mean "shut down": SIGINT (ctrl-c) on every platform, plus
/// SIGTERM and SIGHUP on unix — what `docker stop`, ECS and k8s send before
/// escalating to SIGKILL. Without handlers the default disposition kills the
/// process outright: no close frames, no terminal results, runtimes see TCP
/// resets, and in-flight delegations are left to their clients' deadlines.
///
/// Registration is separated from waiting on purpose. tokio's signal registry
/// discards an event that has no listener registered, so a watcher built
/// *inside* the handler would leave a window in which a repeat signal — the
/// operator's "stop waiting" — is dropped on the floor. [`ShutdownSignals`]
/// registers once and can then be awaited any number of times, which is what
/// makes "signal it twice" reliable.
struct ShutdownSignals {
    #[cfg(unix)]
    interrupt: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    terminate: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    hangup: Option<tokio::signal::unix::Signal>,
}

impl ShutdownSignals {
    /// Register the signal streams. Synchronous: the listeners exist from this
    /// moment on, whatever the caller awaits next.
    fn register() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            // SIGINT goes through the same `Signal` machinery as SIGTERM and
            // SIGHUP rather than through `tokio::signal::ctrl_c()`, which
            // re-registers its handler on every call: a repeat arriving between
            // one wait returning and the next being created would be dropped by
            // the registry — the gap the double-signal escape hatch cannot
            // have. A stream that cannot be registered is simply absent: the
            // others still work, and the failure is logged rather than fatal,
            // because a CP that cannot be stopped politely should still run.
            let interrupt = signal(SignalKind::interrupt())
                .inspect_err(|e| warn!(error = %e, "failed to install SIGINT handler"))
                .ok();
            let terminate = signal(SignalKind::terminate())
                .inspect_err(|e| warn!(error = %e, "failed to install SIGTERM handler"))
                .ok();
            let hangup = signal(SignalKind::hangup())
                .inspect_err(|e| warn!(error = %e, "failed to install SIGHUP handler"))
                .ok();
            Self {
                interrupt,
                terminate,
                hangup,
            }
        }
        #[cfg(not(unix))]
        {
            Self {}
        }
    }

    /// `pending()` for an unregistered stream: it can never fire, and
    /// selecting on it keeps the arm shape uniform.
    #[cfg(unix)]
    async fn arm(signal: Option<&mut tokio::signal::unix::Signal>) {
        match signal {
            Some(s) => {
                s.recv().await;
            }
            None => std::future::pending::<()>().await,
        }
    }

    /// Wait for the next shutdown signal and name it.
    async fn recv(&mut self) -> &'static str {
        #[cfg(unix)]
        {
            // Exactly ONE listener per signal, always. On unix
            // `tokio::signal::ctrl_c()` IS `signal(SignalKind::interrupt())`, so
            // selecting on both a registered SIGINT stream and ctrl-c would
            // hand the SAME ctrl-c to two listeners — and the force-quit task
            // would read the first signal's notification as a second one and
            // exit immediately, skipping the drain entirely. (Measured: one
            // ctrl-c force-exited 8 times out of 10.) ctrl-c is therefore the
            // arm only where the stream could not be registered, where it is
            // also the only thing that can work.
            let interrupt = async {
                match self.interrupt.as_mut() {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => {
                        // A failed registration resolves immediately with an
                        // error — reporting that as a SIGINT would look like a
                        // shutdown signal that never arrived. Wait instead.
                        if let Err(e) = tokio::signal::ctrl_c().await {
                            warn!(
                                error = %e,
                                "cannot wait for SIGINT — ctrl-c will no longer stop the CP \
                                 gracefully (the OS default still kills it)"
                            );
                            std::future::pending::<()>().await;
                        }
                    }
                }
            };
            tokio::select! {
                _ = interrupt => "SIGINT",
                _ = Self::arm(self.terminate.as_mut()) => "SIGTERM",
                _ = Self::arm(self.hangup.as_mut()) => "SIGHUP",
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            "SIGINT"
        }
    }
}

/// Conventional exit status for a signal-driven exit: 128 + signal number.
fn signal_exit_code(signal: &str) -> i32 {
    match signal {
        "SIGHUP" => 129,
        "SIGINT" => 130,
        "SIGTERM" => 143,
        _ => 1,
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let cfg = CpConfig::load(&cli.config)?;
    let listen = cfg.listen.clone();
    if cfg.agents.is_empty() {
        tracing::warn!("no [[agents]] identities configured — every connection will be rejected");
    }
    info!(
        listen = %listen,
        identities = cfg.agents.len(),
        namespaces = cfg.namespaces.len(),
        "starting openab-cp"
    );

    let state = Arc::new(AppState::new(cfg));
    let sweeper = tokio::spawn(run_sweeper(state.clone()));

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;

    // Registered once, before anything is awaited: see `ShutdownSignals`. The
    // first wait consumes the signal that starts the drain; the second (in the
    // task below) is the operator's "stop waiting".
    let mut signals = ShutdownSignals::register();
    let (stop_accepting_tx, stop_accepting_rx) = tokio::sync::oneshot::channel::<()>();

    // The listener runs as its own task. Awaiting `axum::serve` here and then
    // draining would be the obvious shape and it is wrong: axum's graceful
    // shutdown ends when the last in-flight HTTP request finishes, so a peer
    // that opened a socket and then said nothing (a half-sent request, a stalled
    // upgrade) would pin that wait — and with it the drain — for as long as the
    // orchestrator lets the process live. Serving from a task lets the signal
    // be handled here, immediately, whatever hyper is doing.
    let mut serve_done = tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            axum::serve(listener, app(state))
                .with_graceful_shutdown(async move {
                    let _ = stop_accepting_rx.await;
                })
                .await
        }
    });

    let signal = signals.recv().await;
    info!(signal, "shutdown signal received — draining connections");

    // A second signal at ANY point before the process exits is "stop waiting":
    // exit immediately with the signal's conventional status (128 + signum).
    // Armed from the SAME already-registered streams — the first `recv` above
    // consumed its event, so this one waits for the next — which closes the
    // window in which a repeat would be dropped by an unregistered listener.
    // Deliberately left armed: the drain and the listener's wait are separate
    // waits and the escape hatch must cover both. The task ends with the
    // process if it never fires.
    tokio::spawn(async move {
        let signal = signals.recv().await;
        warn!(signal, "second shutdown signal — forcing immediate exit");
        std::process::exit(signal_exit_code(signal));
    });

    // Nothing new may enter a CP that is leaving. Then drain: bounded by
    // `shutdown_drain_secs`, which resolves the in-flight delegations, lets
    // every connection flush and close, and returns.
    let _ = stop_accepting_tx.send(());
    graceful_shutdown(&state).await;

    // The drain is done; give the listener a bounded moment to finish its own
    // bookkeeping, then stop regardless. The bound starts HERE, not at boot:
    // a timer armed at startup would become the process's lifetime.
    match tokio::time::timeout(LISTENER_SHUTDOWN_SLACK, &mut serve_done).await {
        Ok(Ok(Ok(()))) => {}
        Ok(Ok(Err(e))) => warn!(error = %e, "listener stopped with an error"),
        Ok(Err(join)) => warn!("listener task ended abnormally: {join:?}"),
        Err(_) => {
            warn!(
                slack_secs = LISTENER_SHUTDOWN_SLACK.as_secs(),
                "the listener's graceful wait outlived the shutdown slack — dropping it"
            );
            serve_done.abort();
        }
    }

    // Only now: the sweeper exists to keep registrations and delegations
    // honest, and during a shutdown its findings would race the closes above.
    sweeper.abort();
    let _ = sweeper.await;
    info!("openab-cp stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::signal_exit_code;

    #[test]
    fn signal_exit_codes_follow_the_128_plus_signum_convention() {
        // The force-quit path must report WHICH signal ended the process so
        // an operator (or a supervisor's exit-code log) can tell a forced
        // exit from a clean drain — and from a crash.
        assert_eq!(signal_exit_code("SIGHUP"), 129);
        assert_eq!(signal_exit_code("SIGINT"), 130);
        assert_eq!(signal_exit_code("SIGTERM"), 143);
    }
}
