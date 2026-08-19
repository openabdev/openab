//! Platform-specific process-tree lifecycle control for ACP child processes.
//!
//! Keeping the controller here gives the ACP connection and pool one platform-neutral
//! handle.  The Windows implementation owns the Job Object and waits asynchronously for
//! normal child exit; it never uses a periodic liveness poll.

#[cfg(windows)]
use anyhow::anyhow;
use anyhow::Result;

#[cfg(windows)]
use process_wrap::tokio::ChildWrapper;
#[cfg(windows)]
use tokio::sync::{mpsc, oneshot};
#[cfg(windows)]
use tracing::error;

#[cfg(windows)]
type AgentChild = Box<dyn ChildWrapper>;

#[cfg(windows)]
#[derive(Clone)]
pub struct ProcessTreeGuard {
    terminate_tx: mpsc::UnboundedSender<oneshot::Sender<std::result::Result<(), String>>>,
}

#[cfg(not(windows))]
#[derive(Clone, Default)]
pub struct ProcessTreeGuard;

#[cfg(not(windows))]
impl ProcessTreeGuard {
    pub async fn terminate(&self) -> Result<()> {
        Ok(())
    }
}

#[cfg(windows)]
impl ProcessTreeGuard {
    pub(crate) fn new(mut child: AgentChild) -> Self {
        let (terminate_tx, mut terminate_rx) =
            mpsc::unbounded_channel::<oneshot::Sender<std::result::Result<(), String>>>();

        tokio::spawn(async move {
            // Keep the wait future scoped to select!. If termination wins, the future is
            // fully dropped before borrowing the child again for kill().
            let request = tokio::select! {
                request = terminate_rx.recv() => request,
                result = child.wait() => {
                    if let Err(e) = result {
                        error!(error = %e, "failed to wait for Windows agent Job Object");
                    }
                    return;
                }
            };

            let result = Box::into_pin(child.kill()).await.map_err(|e| e.to_string());
            if let Some(reply) = request {
                let _ = reply.send(result);
            }
        });

        Self { terminate_tx }
    }

    /// Terminate the Job Object without acquiring the connection mutex.
    pub async fn terminate(&self) -> Result<()> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if self.terminate_tx.send(reply_tx).is_err() {
            return Ok(());
        }
        tokio::time::timeout(std::time::Duration::from_secs(10), reply_rx)
            .await
            .map_err(|_| anyhow!("timeout terminating Windows agent Job Object"))?
            .map_err(|_| anyhow!("Windows agent process controller exited before replying"))?
            .map_err(|e| anyhow!("failed to terminate Windows agent Job Object: {e}"))
    }
}
