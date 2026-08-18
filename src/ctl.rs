//! `openab set/get` IPC over Unix domain socket.
//!
//! Architecture (like consul/vault):
//! - `openab run` spawns a UnixListener at a well-known path.
//! - `openab set key value` connects, sends a JSON request, reads the response.
//!
//! Phase 1 supported keys:
//! - `thread.name` — rename the current Discord/Slack thread
//!
//! Phase 2 (workflow `20260818-openab-project-aware-thread-routing`):
//! - `thread.pin` — project-aware thread/session registration API (trusted
//!   bootstrap of `ProjectContext`).
//! - `thread.message` — extended to optionally carry a `project` field that
//!   pins before sending (`ensure_pinned_project` first, then
//!   `send_message_targeted`).

#[cfg(unix)]
use openab_core::acp::project::ProjectContext;
#[cfg(unix)]
use openab_core::acp::SessionPool;
#[cfg(unix)]
use openab_core::adapter::{ChannelRef, ChatAdapter};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tracing::{debug, error, info, warn};

/// Default socket path. Overridable via `OPENAB_SOCK` env var.
pub fn socket_path() -> PathBuf {
    std::env::var("OPENAB_SOCK")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp/openab.sock"))
}

// ─── Protocol ───────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub action: Action,
    pub key: String,
    pub value: Option<String>,
    /// Target thread/channel ID — daemon uses this to route to the correct adapter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Optional Discord numeric user id that the canonical message MUST
    /// mention. Used by ``set thread.message`` to pin ``allowed_mentions`` so
    /// the recipient is the only legitimate mention Discord will surface.
    /// ``None`` for other keys.
    #[cfg(unix)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_user_id: Option<String>,
    /// Optional project bootstrap (workflow
    /// `20260818-openab-project-aware-thread-routing`). Trusted
    /// transport-neutral seam for the OpenAB/AAP integration layer. Carried
    /// by `thread.pin` (registers only) and `thread.message` (registers then
    /// sends). `None` = legacy behavior; no project hint.
    #[cfg(unix)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<ProjectRef>,
}

/// Wire-format DTO for `Request.project`. Validated and converted to
/// `ProjectContext` via `TryFrom<ProjectRef> for ProjectContext`. The
/// `project_id` MUST be non-empty — anonymous contexts are reserved for the
/// legacy `[[ws:@alias]]` directive path inside the dispatcher and are
/// deliberately not pin-able from the ctl layer.
#[cfg(unix)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectRef {
    pub project_id: String,
    pub project_root: String,
}

#[cfg(unix)]
impl TryFrom<ProjectRef> for ProjectContext {
    type Error = String;
    fn try_from(p: ProjectRef) -> Result<Self, Self::Error> {
        if p.project_id.trim().is_empty() {
            return Err("project_id must be non-empty".into());
        }
        if p.project_root.trim().is_empty() {
            return Err("project_root must be non-empty".into());
        }
        let project = ProjectContext {
            project_id: p.project_id,
            project_root: std::path::PathBuf::from(p.project_root),
        };
        // Validate surfaces the canonical absolute path. The error string
        // propagates to the ctl caller verbatim so the AAP layer can react.
        let canonical = project.validate()?;
        Ok(Self {
            project_id: project.project_id,
            project_root: canonical,
        })
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Set,
    Get,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Optional echo of the Discord message id returned by the adapter.
    /// ``set thread.message`` populates this so the AAP caller can correlate
    /// the dispatch with downstream audit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

// ─── Server (runs inside `openab run`) ──────────────────────────────────────

/// Handler trait — `openab run` provides the concrete implementation that
/// can access Discord/Slack adapters.
#[cfg(unix)]
#[async_trait::async_trait]
pub trait CtlHandler: Send + Sync + 'static {
    async fn handle_set(
        &self,
        thread_id: Option<&str>,
        key: &str,
        value: &str,
        target_user_id: Option<&str>,
        project: Option<&ProjectRef>,
    ) -> Response;
    async fn handle_get(&self, thread_id: Option<&str>, key: &str) -> Response;
}

/// Start the control socket server. Call this from `openab run` startup.
/// Returns a JoinHandle; abort it on shutdown.
#[cfg(unix)]
pub fn spawn_server(
    handler: std::sync::Arc<dyn CtlHandler>,
) -> tokio::task::JoinHandle<()> {
    spawn_server_at(socket_path(), handler)
}

/// Start the control socket server at a specific path.
#[cfg(unix)]
pub fn spawn_server_at(
    path: PathBuf,
    handler: std::sync::Arc<dyn CtlHandler>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Remove stale socket file
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                error!(path = %path.display(), error = %e, "failed to bind control socket");
                return;
            }
        };
        // Restrict socket to owner only (defense-in-depth for shared hosts).
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        info!(path = %path.display(), "control socket listening");

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(conn) => conn,
                Err(e) => {
                    warn!(error = %e, "control socket accept error");
                    continue;
                }
            };
            let handler = handler.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_conn(stream, &*handler).await {
                    debug!(error = %e, "control socket connection error");
                }
            });
        }
    })
}

#[cfg(unix)]
async fn handle_conn(
    stream: UnixStream,
    handler: &dyn CtlHandler,
) -> anyhow::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    if let Some(line) = lines.next_line().await? {
        let req: Request = serde_json::from_str(&line)?;
        let resp = match req.action {
            Action::Set => {
                let val = req.value.as_deref().unwrap_or("");
                handler.handle_set(
                    req.thread_id.as_deref(),
                    &req.key,
                    val,
                    req.target_user_id.as_deref(),
                    req.project.as_ref(),
                ).await
            }
            Action::Get => handler.handle_get(req.thread_id.as_deref(), &req.key).await,
        };
        let mut buf = serde_json::to_vec(&resp)?;
        buf.push(b'\n');
        writer.write_all(&buf).await?;
    }
    Ok(())
}

// ─── Client (used by `openab set/get` subcommands) ──────────────────────────

/// Thread registry: maps thread_id → platform name.
/// Shared between the message dispatcher (writes) and the ctl handler (reads).
#[cfg(unix)]
pub type ThreadRegistry = Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>;

/// Create an empty thread registry.
#[cfg(unix)]
pub fn new_registry() -> ThreadRegistry {
    Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()))
}

/// Register a thread→platform mapping. Called by adapters on message dispatch.
#[cfg(unix)]
#[allow(dead_code)]
pub async fn register_thread(registry: &ThreadRegistry, thread_id: &str, platform: &str) {
    registry.write().await.insert(thread_id.to_string(), platform.to_string());
}

/// Type-alias for the Discord shard slot. When the discord feature is disabled,
/// this is a no-op `()` slot that never gets populated.
#[cfg(all(unix, feature = "discord"))]
pub type ShardSlot = Arc<std::sync::OnceLock<serenity::gateway::ShardMessenger>>;
#[cfg(all(unix, not(feature = "discord")))]
pub type ShardSlot = Arc<std::sync::OnceLock<()>>;

/// Concrete handler for `openab run` — dispatches to platform adapters.
#[cfg(unix)]
pub struct RuntimeHandler {
    /// Registered adapters by platform name.
    adapters: std::collections::HashMap<String, Arc<dyn ChatAdapter>>,
    /// thread_id → platform mapping. Populated by `openab run` when it dispatches messages.
    registry: Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
    shard: ShardSlot,
    /// Optional session pool — required for `thread.pin` and `thread.message`
    /// with a `project` field. Set via `with_pool` (builder) before
    /// `Arc::new(RuntimeHandler::new(...).with_pool(pool))`. Without it,
    /// project-bootstrap keys return `pool unavailable` and the legacy
    /// `thread.message` path (no project) still works.
    pool: Option<Arc<SessionPool>>,
}

#[cfg(unix)]
impl RuntimeHandler {
    pub fn new(
        adapters: std::collections::HashMap<String, Arc<dyn ChatAdapter>>,
        registry: Arc<tokio::sync::RwLock<std::collections::HashMap<String, String>>>,
        shard: ShardSlot,
    ) -> Self {
        Self {
            adapters,
            registry,
            shard,
            pool: None,
        }
    }

    /// Builder: attach the session pool so `thread.pin` and
    /// `thread.message(project=...)` can call `SessionPool::get_or_create`.
    pub fn with_pool(mut self, pool: Arc<SessionPool>) -> Self {
        self.pool = Some(pool);
        self
    }

    /// Resolve which adapter to use for a given thread_id.
    async fn resolve(&self, thread_id: Option<&str>) -> Option<(Arc<dyn ChatAdapter>, String)> {
        let tid = thread_id?;
        let platform = {
            let registry = self.registry.read().await;
            let platforms: Vec<String> = self.adapters.keys().cloned().collect();
            resolve_platform(tid, &registry, &platforms)?
        };
        let adapter = self.adapters.get(&platform)?.clone();
        Some((adapter, tid.to_string()))
    }

    /// Resolve the platform for a given thread_id (no adapter returned).
    /// Same precedence as `resolve_platform`: registry hit → single-adapter
    /// fallback → `None`.
    async fn resolve_platform_for_thread(&self, thread_id: &str) -> Option<String> {
        let registry = self.registry.read().await;
        let platforms: Vec<String> = self.adapters.keys().cloned().collect();
        resolve_platform(thread_id, &registry, &platforms)
    }

    /// Trusted-bootstrap seam for project-aware thread routing.
    ///
    /// Required invariant (workflow `20260818-openab-project-aware-thread-routing`
    /// §ACP SESSION INVALIDATION):
    ///   `thread.pin` may return success only if:
    ///     A. no existing ACP session exists and a new session is created using
    ///        the requested ProjectContext, OR
    ///     B. an existing session is already pinned to the same canonical
    ///        ProjectContext.
    ///
    /// If an active/resumable session exists with NO trusted project binding,
    /// this returns an explicit error and does NOT mutate the session. The
    /// pool is not silently reset or recreated from the ctl layer.
    ///
    /// Reusability is detected via `SessionPool::has_reusable_session` — the
    /// SINGLE source of truth for "could `get_or_create` reuse this?". This
    /// avoids duplicating SessionPool lifecycle knowledge outside the pool.
    ///
    /// Race safety: the post-check after `pool.get_or_create` re-reads
    /// `session_projects[<key>]`; if a concurrent caller (e.g. dispatcher
    /// with no project) won the race and the project was not persisted,
    /// the bootstrap is reported as failed. The ctl layer does not retry.
    async fn ensure_pinned_project(
        &self,
        thread_id: &str,
        project: &ProjectContext,
    ) -> Result<(), String> {
        let pool = self.pool.as_ref().ok_or_else(|| {
            "pool unavailable (RuntimeHandler not built with .with_pool)".to_string()
        })?;

        // Resolve platform via the existing registry + single-adapter fallback.
        let platform = self.resolve_platform_for_thread(thread_id).await.ok_or_else(|| {
            "unknown thread (no registry entry, multiple adapters configured)".to_string()
        })?;

        // Use the same canonical session-key shape as the dispatcher /
        // AdapterRouter via `ChannelRef::session_pool_key()` (test M).
        let channel = ChannelRef {
            platform: platform.clone(),
            channel_id: thread_id.to_string(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        };
        let session_key = channel.session_pool_key();

        // Pre-check: existing pinned state + existing reusable state.
        let pinned = pool.get_pinned_project(&session_key).await;
        let has_reusable = pool.has_reusable_session(&session_key).await;

        // Case B: existing pinned to same canonical project → idempotent success.
        if let Some(existing) = pinned.as_ref() {
            if existing == project {
                return Ok(());
            }
            // Case C: existing pinned to a DIFFERENT project — fail closed.
            // (The pool's mismatch gate would also catch this when we call
            // get_or_create, but short-circuiting here gives the ctl layer
            // a clean error path).
            return Err(format!(
                "project mismatch: thread is pinned to project_id={:?} project_root={:?}, \
                 incoming is project_id={:?} project_root={:?}",
                existing.project_id,
                existing.project_root,
                project.project_id,
                project.project_root,
            ));
        }

        // pinned = None.
        if has_reusable {
            // Case D: unpinned legacy session exists → fail closed.
            // Covers active, suspended, AND persisted session_ids (the
            // full shape of `has_reusable_session`).
            return Err(
                "session already exists without trusted project binding; reset/recreate \
                 required before pinning"
                    .to_string(),
            );
        }

        // Case A: no session, no pinning → bootstrap.
        pool.get_or_create(&session_key, Some(project))
            .await
            .map_err(|e| format!("bootstrap failed: {e}"))?;

        // Post-check: confirm the bootstrap actually persisted the binding.
        // Catches the race where a concurrent caller (e.g. dispatcher with
        // no project) won the active-session fast path between the
        // pre-check and our get_or_create call. Reject in that case
        // instead of silently returning Ok(false).
        let pinned_after = pool.get_pinned_project(&session_key).await;
        match pinned_after.as_ref() {
            Some(p) if p == project => Ok(()),
            Some(p) => Err(format!(
                "bootstrap raced and pinned to a different context: {p:?} vs incoming {project:?}"
            )),
            None => Err(
                "bootstrap did not persist project binding (likely won by a concurrent \
                 unpinned caller); retry after reset"
                    .to_string(),
            ),
        }
    }
}

/// Decide which platform should handle a control request for `thread_id`.
///
/// 1. Exact registry hit — the thread was recorded during message dispatch.
/// 2. Single-adapter fallback — if exactly one adapter is configured there is
///    no ambiguity, so resolve to it even without a registry entry. This makes
///    `openab set/get --thread <id>` work for single-platform bots (the common
///    case) without depending on the registry being populated.
///
/// Returns `None` only when the thread is unknown AND multiple adapters are
/// configured (genuinely ambiguous), or when no adapters are configured.
#[cfg(unix)]
fn resolve_platform(
    thread_id: &str,
    registry: &std::collections::HashMap<String, String>,
    platforms: &[String],
) -> Option<String> {
    if let Some(platform) = registry.get(thread_id) {
        if platforms.contains(platform) {
            return Some(platform.clone());
        }
    }
    if platforms.len() == 1 {
        return Some(platforms[0].clone());
    }
    None
}

#[cfg(unix)]
#[async_trait::async_trait]
impl CtlHandler for RuntimeHandler {
    async fn handle_set(
        &self,
        thread_id: Option<&str>,
        key: &str,
        value: &str,
        target_user_id: Option<&str>,
        project: Option<&ProjectRef>,
    ) -> Response {
        match key {
            "thread.name" => {
                let Some((adapter, tid)) = self.resolve(thread_id).await else {
                    return Response {
                        ok: false,
                        message: "unknown thread (use --thread or register via message dispatch)".into(),
                        value: None,
                        message_id: None,
                    };
                };
                let channel = ChannelRef {
                    platform: String::new(),
                    channel_id: tid,
                    thread_id: None,
                    parent_id: None,
                    origin_event_id: None,
                };
                match adapter.rename_thread(&channel, value).await {
                    Ok(()) => Response {
                        ok: true,
                        message: format!("thread renamed to: {value}"),
                        value: None,
                        message_id: None,
                    },
                    Err(e) => Response {
                        ok: false,
                        message: format!("rename failed: {e}"),
                        value: None,
                        message_id: None,
                    },
                }
            }
            "thread.archived" => {
                let Some((_adapter, tid)) = self.resolve(thread_id).await else {
                    return Response {
                        ok: false,
                        message: "unknown thread (use --thread or register via message dispatch)".into(),
                        value: None,
                        message_id: None,
                    };
                };
                let _archived = match value {
                    "true" | "1" | "yes" => true,
                    "false" | "0" | "no" => false,
                    _ => {
                        return Response {
                            ok: false,
                            message: format!("invalid value: {value} (expected true/false)"),
                            value: None,
                            message_id: None,
                        };
                    }
                };
                let _channel = ChannelRef {
                    platform: String::new(),
                    channel_id: tid,
                    thread_id: None,
                    parent_id: None,
                    origin_event_id: None,
                };
                Response {
                    ok: false,
                    message: "archive_thread not supported in workspace mode".into(),
                    value: None,
                    message_id: None,
                }
            }
            "thread.pin" => {
                // Project-aware thread/session registration API (workflow
                // `20260818-openab-project-aware-thread-routing`).
                //
                // Trusted bootstrap: validates the project, fails closed on
                // any existing reusable-but-unpinned session, and persists
                // the binding via `SessionPool::session_projects` (the
                // existing canonical store). No outbound Discord message.
                let Some(project_ref) = project else {
                    return Response {
                        ok: false,
                        message: "thread.pin requires a project field".into(),
                        value: None,
                        message_id: None,
                    };
                };
                let Some(tid) = thread_id else {
                    return Response {
                        ok: false,
                        message: "thread.pin requires a thread_id".into(),
                        value: None,
                        message_id: None,
                    };
                };
                let project_ctx = match ProjectContext::try_from(project_ref.clone()) {
                    Ok(p) => p,
                    Err(e) => {
                        return Response {
                            ok: false,
                            message: format!("invalid project: {e}"),
                            value: None,
                            message_id: None,
                        };
                    }
                };
                match self.ensure_pinned_project(tid, &project_ctx).await {
                    Ok(()) => Response {
                        ok: true,
                        message: format!(
                            "thread pinned to project_id={:?}",
                            project_ctx.project_id
                        ),
                        value: None,
                        message_id: None,
                    },
                    Err(e) => Response {
                        ok: false,
                        message: e,
                        value: None,
                        message_id: None,
                    },
                }
            }
            "thread.message" => {
                // Canonical bot-to-bot handoff control-plane primitive.
                //
                // ``value`` carries the rendered HANDOFF body (produced by
                // ``render_handoff_for_discord`` in AAP). ``target_user_id``
                // is the numeric Discord user id of the single recipient — the
                // daemon pins ``allowed_mentions`` to that user via the
                // adapter's ``send_message_targeted`` so Discord's REST
                // pipeline tags ``mentions: [{user_id: <X>}]`` and the
                // receiving bot's MultibotMentions check accepts the dispatch
                // without the LLM ever authoring a raw Discord ID.
                let Some((adapter, tid)) = self.resolve(thread_id).await else {
                    return Response {
                        ok: false,
                        message: "unknown thread (use --thread or register via message dispatch)".into(),
                        value: None,
                        message_id: None,
                    };
                };
                let content = if !value.is_empty() {
                    value
                } else {
                    return Response {
                        ok: false,
                        message: "thread.message requires a non-empty value".into(),
                        value: None,
                        message_id: None,
                    };
                };
                // Pin-first semantics: if a project is supplied, validate
                // and pin BEFORE sending. A pin failure here means we
                // never send the Discord message — preserving the
                // fail-closed contract (test N).
                if let Some(project_ref) = project {
                    let project_ctx = match ProjectContext::try_from(project_ref.clone()) {
                        Ok(p) => p,
                        Err(e) => {
                            return Response {
                                ok: false,
                                message: format!("invalid project: {e}"),
                                value: None,
                                message_id: None,
                            };
                        }
                    };
                    if let Err(e) = self.ensure_pinned_project(&tid, &project_ctx).await {
                        return Response {
                            ok: false,
                            message: format!("pin failed (no message sent): {e}"),
                            value: None,
                            message_id: None,
                        };
                    }
                }
                let channel = ChannelRef {
                    platform: String::new(),
                    channel_id: tid,
                    thread_id: None,
                    parent_id: None,
                    origin_event_id: None,
                };
                match adapter
                    .send_message_targeted(&channel, content, target_user_id)
                    .await
                {
                    Ok(msg_ref) => Response {
                        ok: true,
                        message: "thread.message dispatched".into(),
                        value: None,
                        message_id: Some(msg_ref.message_id),
                    },
                    Err(e) => Response {
                        ok: false,
                        message: format!("thread.message dispatch failed: {e}"),
                        value: None,
                        message_id: None,
                    },
                }
            }
            "agent.status" => {
                #[cfg(feature = "discord")]
                {
                    let Some(shard) = self.shard.get() else {
                        return Response {
                            ok: false,
                            message: "agent.status only supported on Discord".into(),
                            value: None,
                            message_id: None,
                        };
                    };
                    use serenity::gateway::ActivityData;
                    use serenity::model::user::OnlineStatus;
                    let activity = if value.is_empty() {
                        None
                    } else {
                        Some(ActivityData::custom(value))
                    };
                    shard.set_presence(activity, OnlineStatus::Online);
                    Response {
                        ok: true,
                        message: if value.is_empty() {
                            "status cleared".into()
                        } else {
                            format!("status set to: {value}")
                        },
                        value: None,
                        message_id: None,
                    }
                }
                #[cfg(not(feature = "discord"))]
                {
                    let _ = value;
                    Response {
                        ok: false,
                        message: "agent.status requires discord feature".into(),
                        value: None,
                        message_id: None,
                    }
                }
            }
            _ => Response {
                ok: false,
                message: format!("unknown key: {key}"),
                value: None,
                message_id: None,
            },
        }
    }

    async fn handle_get(&self, _thread_id: Option<&str>, key: &str) -> Response {
        match key {
            "thread.name" | "thread.archived" | "agent.status" | "thread.message" => Response {
                ok: false,
                message: format!("{key} get not yet supported"),
                value: None,
                message_id: None,
            },
            _ => Response {
                ok: false,
                message: format!("unknown key: {key}"),
                value: None,
                message_id: None,
            },
        }
    }
}

#[cfg(unix)]
pub async fn send_request(req: &Request) -> anyhow::Result<Response> {
    send_request_to(&socket_path(), req).await
}

#[cfg(not(unix))]
pub async fn send_request(_req: &Request) -> anyhow::Result<Response> {
    anyhow::bail!("openab set/get is not supported on Windows (requires Unix domain sockets)")
}

/// Send a request to a specific socket path.
#[cfg(unix)]
pub async fn send_request_to(path: &PathBuf, req: &Request) -> anyhow::Result<Response> {
    let stream = UnixStream::connect(&path).await.map_err(|e| {
        anyhow::anyhow!(
            "cannot connect to openab at {}: {} (is `openab run` running?)",
            path.display(),
            e
        )
    })?;
    let (reader, mut writer) = stream.into_split();
    let mut buf = serde_json::to_vec(req)?;
    buf.push(b'\n');
    writer.write_all(&buf).await?;
    writer.shutdown().await?;

    let mut lines = BufReader::new(reader).lines();
    let line = lines
        .next_line()
        .await?
        .ok_or_else(|| anyhow::anyhow!("no response from openab"))?;
    let resp: Response = serde_json::from_str(&line)?;
    Ok(resp)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn reg(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn resolve_platform_registry_hit() {
        let r = reg(&[("123", "discord")]);
        let platforms = vec!["discord".to_string(), "slack".to_string()];
        assert_eq!(
            resolve_platform("123", &r, &platforms).as_deref(),
            Some("discord")
        );
    }

    #[test]
    fn resolve_platform_single_adapter_fallback() {
        // No registry entry, but only one adapter -> resolve to it.
        let r = reg(&[]);
        let platforms = vec!["discord".to_string()];
        assert_eq!(
            resolve_platform("999", &r, &platforms).as_deref(),
            Some("discord")
        );
    }

    #[test]
    fn resolve_platform_multi_adapter_miss_is_none() {
        // No registry entry and multiple adapters -> genuinely ambiguous.
        let r = reg(&[]);
        let platforms = vec!["discord".to_string(), "slack".to_string()];
        assert_eq!(resolve_platform("999", &r, &platforms), None);
    }

    #[test]
    fn resolve_platform_no_adapters_is_none() {
        let r = reg(&[]);
        let platforms: Vec<String> = vec![];
        assert_eq!(resolve_platform("999", &r, &platforms), None);
    }

    #[test]
    fn resolve_platform_registry_hit_wins_over_fallback() {
        // Registry takes precedence when the platform is still configured.
        let r = reg(&[("123", "slack")]);
        let platforms = vec!["discord".to_string(), "slack".to_string()];
        assert_eq!(
            resolve_platform("123", &r, &platforms).as_deref(),
            Some("slack")
        );
    }

    #[test]
    fn resolve_platform_stale_registry_entry_falls_through() {
        // Stale registry entry pointing to unconfigured platform falls through to fallback.
        let r = reg(&[("123", "slack")]);
        let platforms = vec!["discord".to_string()];
        assert_eq!(
            resolve_platform("123", &r, &platforms).as_deref(),
            Some("discord")
        );
    }

    #[test]
    fn request_serialization() {
        let req = Request {
            action: Action::Set,
            key: "thread.name".into(),
            value: Some("hello".into()),
            thread_id: Some("123".into()),
            target_user_id: None,
            project: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.action, Action::Set);
        assert_eq!(parsed.key, "thread.name");
        assert_eq!(parsed.value.as_deref(), Some("hello"));
        assert_eq!(parsed.thread_id.as_deref(), Some("123"));
        assert_eq!(parsed.target_user_id, None);
        assert!(parsed.project.is_none());
    }

    #[test]
    fn request_serialization_with_project_skips_when_none() {
        // Backward compatibility: clients that don't send `project` must
        // continue to work (the field is skipped when None).
        let req = Request {
            action: Action::Set,
            key: "thread.message".into(),
            value: Some("hi".into()),
            thread_id: Some("T".into()),
            target_user_id: None,
            project: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(
            !json.contains("project"),
            "project field must be omitted when None: {json}"
        );
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert!(parsed.project.is_none());
    }

    #[test]
    fn request_serialization_with_project_roundtrip() {
        let req = Request {
            action: Action::Set,
            key: "thread.pin".into(),
            value: None,
            thread_id: Some("T".into()),
            target_user_id: None,
            project: Some(ProjectRef {
                project_id: "openab".into(),
                project_root: "/home/arthur/openab/source".into(),
            }),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("project_id"));
        assert!(json.contains("project_root"));
        let parsed: Request = serde_json::from_str(&json).unwrap();
        let p = parsed.project.expect("project must round-trip");
        assert_eq!(p.project_id, "openab");
        assert_eq!(p.project_root, "/home/arthur/openab/source");
    }

    #[test]
    fn project_ref_rejects_empty_project_id() {
        let p = ProjectRef {
            project_id: "".into(),
            project_root: "/tmp".into(),
        };
        let err = ProjectContext::try_from(p).expect_err("empty project_id must fail");
        assert!(err.contains("project_id"), "{err}");
    }

    #[test]
    fn project_ref_rejects_empty_project_root() {
        let p = ProjectRef {
            project_id: "openab".into(),
            project_root: "".into(),
        };
        let err = ProjectContext::try_from(p).expect_err("empty project_root must fail");
        assert!(err.contains("project_root"), "{err}");
    }

    #[test]
    fn project_ref_rejects_nonexistent_project_root() {
        let p = ProjectRef {
            project_id: "openab".into(),
            project_root: "/this/path/does/not/exist/anywhere_2026_08_18".into(),
        };
        let err = ProjectContext::try_from(p).expect_err("nonexistent project_root must fail");
        assert!(err.contains("cannot be canonicalized"), "{err}");
    }

    #[test]
    fn project_ref_canonicalizes_existing_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let p = ProjectRef {
            project_id: "openab".into(),
            project_root: dir.path().to_string_lossy().to_string(),
        };
        let ctx = ProjectContext::try_from(p).expect("existing dir should canonicalize");
        assert_eq!(ctx.project_id, "openab");
        assert_eq!(ctx.project_root, std::fs::canonicalize(dir.path()).unwrap());
    }

    #[tokio::test]
    async fn server_client_roundtrip() {
        struct MockHandler;
        #[async_trait::async_trait]
        impl CtlHandler for MockHandler {
            async fn handle_set(
                &self,
                thread_id: Option<&str>,
                key: &str,
                value: &str,
                _target_user_id: Option<&str>,
                _project: Option<&ProjectRef>,
            ) -> Response {
                Response {
                    ok: true,
                    message: format!("{key} = {value} (thread: {})", thread_id.unwrap_or("none")),
                    value: None,
                    message_id: None,
                }
            }
            async fn handle_get(&self, _thread_id: Option<&str>, key: &str) -> Response {
                Response {
                    ok: true,
                    message: String::new(),
                    value: Some(format!("val-of-{key}")),
                    message_id: None,
                }
            }
        }

        // Use a temp path to avoid conflicts
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");

        let handler = std::sync::Arc::new(MockHandler);
        let server = spawn_server_at(sock.clone(), handler);
        // Give server a moment to bind
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Test set
        let resp = send_request_to(&sock, &Request {
            action: Action::Set,
            key: "thread.name".into(),
            value: Some("hello world".into()),
            thread_id: Some("999".into()),
            target_user_id: None,
            project: None,
        })
        .await
        .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.message, "thread.name = hello world (thread: 999)");

        // Test get
        let resp = send_request_to(&sock, &Request {
            action: Action::Get,
            key: "thread.name".into(),
            value: None,
            thread_id: None,
            target_user_id: None,
            project: None,
        })
        .await
        .unwrap();
        assert!(resp.ok);
        assert_eq!(resp.value.as_deref(), Some("val-of-thread.name"));

        server.abort();
    }

    #[test]
    fn protocol_carries_target_user_id() {
        let req = Request {
            action: Action::Set,
            key: "thread.message".into(),
            value: Some("HANDOFF\nto: <@1536734779607879700>\n".into()),
            thread_id: Some("1536735741642547262".into()),
            target_user_id: Some("1536734779607879700".into()),
            project: None,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("target_user_id"));
        assert!(json.contains("1536734779607879700"));
        let parsed: Request = serde_json::from_str(&json).unwrap();
        assert_eq!(
            parsed.target_user_id.as_deref(),
            Some("1536734779607879700")
        );
        assert_eq!(parsed.key, "thread.message");
        assert!(parsed.value.unwrap().starts_with("HANDOFF"));
    }

    #[tokio::test]
    async fn server_client_roundtrip_carries_target_user_id() {
        #[derive(Default)]
        struct CapturedHandler {
            captured_target_user_id: std::sync::Mutex<Option<String>>,
        }
        #[async_trait::async_trait]
        impl CtlHandler for CapturedHandler {
            async fn handle_set(
                &self,
                _thread_id: Option<&str>,
                _key: &str,
                _value: &str,
                target_user_id: Option<&str>,
                _project: Option<&ProjectRef>,
            ) -> Response {
                *self.captured_target_user_id.lock().unwrap() = target_user_id.map(str::to_string);
                Response {
                    ok: true,
                    message: "captured".into(),
                    value: None,
                    message_id: None,
                }
            }
            async fn handle_get(&self, _: Option<&str>, _: &str) -> Response {
                Response {
                    ok: false,
                    message: "no get".into(),
                    value: None,
                    message_id: None,
                }
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("test.sock");
        let handler = std::sync::Arc::new(CapturedHandler::default());
        let server = spawn_server_at(sock.clone(), handler.clone());
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.message".into(),
                value: Some("HANDOFF\n...".into()),
                thread_id: Some("1536735741642547262".into()),
                target_user_id: Some("1536734779607879700".into()),
                project: None,
            },
        )
        .await
        .unwrap();
        assert!(resp.ok);
        assert_eq!(
            handler.captured_target_user_id.lock().unwrap().as_deref(),
            Some("1536734779607879700"),
        );
        server.abort();
    }

    // ─────────────────────────────────────────────────────────────────────
    // Tests for workflow `20260818-openab-project-aware-thread-routing`.
    // A–J, K, L, M, N, O, E2E.
    // ─────────────────────────────────────────────────────────────────────

    use openab_core::acp::pool::SessionPoolTestState;
    use openab_core::acp::project::ProjectContext as CoreProjectContext;
    use openab_core::acp::SessionPool;
    use openab_core::adapter::{MessageRef as CoreMessageRef, SenderContext};
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;

    /// Recording adapter — counts every `send_message_targeted` / `rename_thread`
    /// call so tests can assert no outbound message was sent.
    #[derive(Default)]
    struct RecordingAdapter {
        send_count: StdMutex<usize>,
        last_value: StdMutex<Option<String>>,
    }

    impl RecordingAdapter {
        fn send_count(&self) -> usize {
            *self.send_count.lock().unwrap()
        }
        fn last_value(&self) -> Option<String> {
            self.last_value.lock().unwrap().clone()
        }
    }

    #[async_trait::async_trait]
    impl ChatAdapter for RecordingAdapter {
        fn platform(&self) -> &'static str {
            "discord"
        }
        fn message_limit(&self) -> usize {
            2000
        }
        async fn send_message(
            &self,
            _channel: &ChannelRef,
            _content: &str,
        ) -> anyhow::Result<CoreMessageRef> {
            Ok(CoreMessageRef {
                channel: _channel.clone(),
                message_id: "mock-id".into(),
            })
        }
        async fn create_thread(
            &self,
            channel: &ChannelRef,
            _trigger: &CoreMessageRef,
            _title: &str,
        ) -> anyhow::Result<ChannelRef> {
            Ok(channel.clone())
        }
        async fn add_reaction(&self, _msg: &CoreMessageRef, _emoji: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn remove_reaction(&self, _msg: &CoreMessageRef, _emoji: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send_message_targeted(
            &self,
            channel: &ChannelRef,
            content: &str,
            _target_user_id: Option<&str>,
        ) -> anyhow::Result<CoreMessageRef> {
            *self.send_count.lock().unwrap() += 1;
            *self.last_value.lock().unwrap() = Some(content.to_string());
            Ok(CoreMessageRef {
                channel: channel.clone(),
                message_id: format!("msg-{}", self.send_count.lock().unwrap()),
            })
        }
        async fn rename_thread(&self, _channel: &ChannelRef, _title: &str) -> anyhow::Result<()> {
            Ok(())
        }
        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            false
        }
    }

    /// Minimal ACP-compatible test agent script. Mirrors the one in
    /// `crates/openab-core/src/acp/pool.rs` tests but trimmed: no record
    /// file, just enough JSON-RPC to get `pool.get_or_create` through
    /// `initialize` → `session/new` (or `session/load`) → `session/cancel`
    /// without hanging or erroring.
    const TEST_AGENT_SCRIPT: &str = r#"#!/bin/sh
while IFS= read -r line; do
    case "$line" in
        *initialize*)    printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"test"},"agentCapabilities":{"loadSession":true}}}' ;;
        *session/new*)   printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess_test"}}' ;;
        *session/load*)  printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"sess_test"}}' ;;
        *session/cancel*) printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}' ;;
        *)               printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{}}' ;;
    esac
done
"#;

    /// Recording variant of the test agent. When invoked as
    /// `test-acp-agent.sh <record_file>`, every received JSON-RPC line is
    /// appended to `record_file` (truncated on start). Drives the E2E
    /// proof that `session/new.params.cwd` reaches the agent with the
    /// canonical project root (workflow
    /// `20260818-openab-project-aware-thread-routing` test E2E).
    const TEST_AGENT_RECORD_SCRIPT: &str = r#"#!/bin/sh
RECORD="${1:-}"
if [ -n "$RECORD" ]; then
    : > "$RECORD"
fi
while IFS= read -r line; do
    if [ -n "$RECORD" ]; then
        printf '%s\n' "$line" >> "$RECORD"
    fi
    case "$line" in
        *initialize*)    printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentInfo":{"name":"test"},"agentCapabilities":{"loadSession":true}}}' ;;
        *session/new*)   printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"sessionId":"sess_test"}}' ;;
        *session/load*)  printf '%s\n' '{"jsonrpc":"2.0","id":3,"result":{"sessionId":"sess_test"}}' ;;
        *session/cancel*) printf '%s\n' '{"jsonrpc":"2.0","id":4,"result":{}}' ;;
        *)               printf '%s\n' '{"jsonrpc":"2.0","id":0,"result":{}}' ;;
    esac
done
"#;

    /// Write the test agent script to a tempdir and return its path.
    fn write_test_agent_script(dir: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("test-acp-agent.sh");
        std::fs::write(&script, TEST_AGENT_SCRIPT).expect("write test agent script");
        #[cfg(unix)]
        std::fs::set_permissions(
            &script,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod test agent script");
        script
    }

    /// Write the recording test agent script to a tempdir and return its path.
    /// The recording variant accepts a record file path as `$1` and writes
    /// every received JSON-RPC line to it (truncated on start).
    fn write_test_agent_record_script(dir: &std::path::Path) -> std::path::PathBuf {
        let script = dir.join("test-acp-agent-record.sh");
        std::fs::write(&script, TEST_AGENT_RECORD_SCRIPT)
            .expect("write test agent record script");
        #[cfg(unix)]
        std::fs::set_permissions(
            &script,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .expect("chmod test agent record script");
        script
    }

    /// Build a `SessionPool` whose agent command is the recording test
    /// agent script. The `record_path` is passed as the agent's `$1`
    /// argument so every JSON-RPC line the agent receives is appended
    /// to `record_path`. Used by the UDS E2E test to assert that
    /// `session/new.params.cwd` reached the agent.
    fn recording_pool(dir: &std::path::Path, record_path: &std::path::Path) -> std::sync::Arc<SessionPool> {
        let agent_script = write_test_agent_record_script(dir);
        let config = openab_core::config::AgentConfig {
            command: agent_script.to_string_lossy().into(),
            args: vec![record_path.to_string_lossy().into_owned()],
            working_dir: "/tmp".into(),
            env: HashMap::new(),
            inherit_env: vec![],
            command_explicit: true,
        };
        std::sync::Arc::new(SessionPool::with_test_state(
            config,
            SessionPoolTestState::default(),
            dir.join("session_projects.json"),
        ))
    }

    /// Read the JSON-RPC lines recorded by the test agent and extract
    /// the `cwd` field from the first `session/new` line. Returns the
    /// raw cwd string. Panics if no `session/new` line was found.
    fn cwd_from_recorded_session_new(record_path: &std::path::Path) -> String {
        let raw = std::fs::read_to_string(record_path)
            .unwrap_or_else(|e| panic!("read record file {}: {e}", record_path.display()));
        for line in raw.lines() {
            if line.contains("session/new") {
                let v: serde_json::Value = serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("parse session/new line {line}: {e}"));
                let cwd = v
                    .get("params")
                    .and_then(|p| p.get("cwd"))
                    .and_then(|c| c.as_str())
                    .unwrap_or_else(|| panic!("session/new line missing cwd: {line}"));
                return cwd.to_string();
            }
        }
        panic!(
            "no session/new line found in record file {}: lines were {:?}",
            record_path.display(),
            raw.lines().collect::<Vec<_>>(),
        );
    }

    /// Constructs a `SessionPool` with a pre-populated state. Uses the
    /// public test seam `SessionPool::with_test_state` so this works from
    /// the binary crate's tests (the in-crate `with_state_for_test` is
    /// `#[cfg(test)]` and not available cross-crate).
    ///
    /// The agent command is a small ACP-compatible shell script that
    /// responds to `initialize` / `session/new` / `session/load` with valid
    /// JSON-RPC so `pool.get_or_create` can complete the spawn path.
    fn pool_with_state(
        dir: &std::path::Path,
        state: SessionPoolTestState,
    ) -> std::sync::Arc<SessionPool> {
        let agent_script = write_test_agent_script(dir);
        let config = openab_core::config::AgentConfig {
            command: agent_script.to_string_lossy().into(),
            args: vec![],
            working_dir: "/tmp".into(),
            env: HashMap::new(),
            inherit_env: vec![],
            command_explicit: true,
        };
        std::sync::Arc::new(SessionPool::with_test_state(
            config,
            state,
            dir.join("session_projects.json"),
        ))
    }

    fn empty_pool_state() -> SessionPoolTestState {
        SessionPoolTestState::default()
    }

    /// Build a `RuntimeHandler` with a single Discord adapter and a
    /// pre-populated session pool. The `state` is taken by the pool.
    fn make_handler(
        adapter: std::sync::Arc<RecordingAdapter>,
        pool: std::sync::Arc<SessionPool>,
    ) -> RuntimeHandler {
        let mut adapters: HashMap<String, std::sync::Arc<dyn ChatAdapter>> = HashMap::new();
        adapters.insert("discord".into(), adapter);
        let registry = new_registry();
        RuntimeHandler::new(adapters, registry, Arc::new(std::sync::OnceLock::new()))
            .with_pool(pool)
    }

    fn project_root(p: &std::path::Path) -> ProjectRef {
        ProjectRef {
            project_id: "openab".into(),
            project_root: p.to_string_lossy().to_string(),
        }
    }

    // ── TEST M: canonical ctl session key matches dispatcher session key ──

    /// `ChannelRef::session_pool_key()` is the SINGLE source of truth for the
    /// session key shape. The ctl layer's `RuntimeHandler` (via
    /// `ensure_pinned_project`) and the dispatcher's `Dispatcher::session_key`
    /// both call it for the same channel, producing byte-identical keys.
    #[test]
    fn channel_ref_session_pool_key_is_dispatcher_shape() {
        // Discord: threads are channels, so thread_id is None.
        let discord = ChannelRef {
            platform: "discord".into(),
            channel_id: "T1".into(),
            thread_id: None,
            parent_id: Some("P".into()),
            origin_event_id: None,
        };
        assert_eq!(discord.session_pool_key(), "discord:T1");

        // Slack: threads have thread_ts, channel_id is the parent.
        let slack = ChannelRef {
            platform: "slack".into(),
            channel_id: "C1".into(),
            thread_id: Some("1234567890.000100".into()),
            parent_id: None,
            origin_event_id: None,
        };
        assert_eq!(slack.session_pool_key(), "slack:1234567890.000100");

        // Generic threaded channel: thread_id wins over channel_id.
        let threaded = ChannelRef {
            platform: "telegram".into(),
            channel_id: "chatid".into(),
            thread_id: Some("topicid".into()),
            parent_id: None,
            origin_event_id: None,
        };
        assert_eq!(threaded.session_pool_key(), "telegram:topicid");
    }

    /// Lane-mode dispatcher key (`<platform>:<thread_id>:<sender_id>`) is NOT
    /// the ACP session key. The session is always shared per-thread
    /// regardless of grouping, so the project binding uses the canonical
    /// `<platform>:<thread_id>` form (`session_pool_key`).
    #[test]
    fn lane_mode_dispatcher_key_is_distinct_from_session_pool_key() {
        let channel = ChannelRef {
            platform: "discord".into(),
            channel_id: "T1".into(),
            thread_id: None,
            parent_id: None,
            origin_event_id: None,
        };
        let session_key = channel.session_pool_key();
        let lane_key = format!("{}:userA", session_key);
        assert_eq!(session_key, "discord:T1");
        assert_eq!(lane_key, "discord:T1:userA");
        assert_ne!(session_key, lane_key);
    }

    /// The ctl layer's `ensure_pinned_project` builds a `ChannelRef` from
    /// the resolved platform + thread_id and calls `session_pool_key()` —
    /// this gives the same key as the dispatcher would for the same thread.
    #[tokio::test]
    async fn ensure_pinned_project_constructs_same_key_as_dispatcher() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let project = CoreProjectContext {
            project_id: "openab".into(),
            project_root: project_dir.path().to_path_buf(),
        };
        // The unused `project` above exercises the same direct
        // ProjectContext construction that non-ctl code paths use. The
        // ctl layer's `ensure_pinned_project` builds an equivalent
        // ProjectContext via `ProjectRef::try_from` (see request
        // serialization tests above).
        let _ = project;
        // Thread ID is "T1" with one configured adapter (discord), so the
        // single-adapter fallback resolves the platform to "discord".
        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(resp.ok, "pin should succeed: {resp:?}");

        // The pool's session_projects entry uses the dispatcher key shape.
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("pool must have entry under canonical key discord:T1");
        assert_eq!(pinned.project_id, "openab");
        assert_eq!(
            pinned.project_root,
            project_dir.path().canonicalize().unwrap()
        );
    }

    // ── TEST A: trusted thread bootstrap with project A ──

    #[tokio::test]
    async fn ctl_thread_pin_writes_project_to_session_pool() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(resp.ok, "pin must succeed: resp={:?}", resp.message);
        assert!(resp.message.contains("pinned"));

        // The persisted ProjectContext carries the project_root, which IS
        // the SessionPool's per-thread workdir (set via `save_meta`).
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("binding must be persisted");
        assert_eq!(pinned.project_id, "openab");
        assert_eq!(
            pinned.project_root,
            project_dir.path().canonicalize().unwrap(),
            "project_root must be the canonical absolute path"
        );
        // No outbound message sent on thread.pin.
        assert_eq!(adapter.send_count(), 0, "thread.pin must not send a message");
    }

    // ── TEST B: two threads pinned to different projects remain isolated ──

    #[tokio::test]
    async fn ctl_thread_pin_two_threads_remain_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let mut a = project_root(project_a_dir.path());
        a.project_id = "A".into();
        let mut b = project_root(project_b_dir.path());
        b.project_id = "B".into();

        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&a))
            .await;
        assert!(r1.ok, "T1 pin: {:?}", r1.message);
        let r2 = handler
            .handle_set(Some("T2"), "thread.pin", "", None, Some(&b))
            .await;
        assert!(r2.ok, "T2 pin: {:?}", r2.message);

        let pa = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("T1 must have a binding");
        assert_eq!(pa.project_id, "A");
        let pb = pool
            .get_pinned_project("discord:T2")
            .await
            .expect("T2 must have a binding");
        assert_eq!(pb.project_id, "B");
        assert_ne!(
            pa.project_root, pb.project_root,
            "T1 and T2 must have distinct project roots"
        );
    }

    // ── TEST D: pin with different project fails closed ──

    #[tokio::test]
    async fn ctl_thread_pin_with_different_project_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let mut a = project_root(project_a_dir.path());
        a.project_id = "A".into();
        let mut b = project_root(project_b_dir.path());
        b.project_id = "B".into();

        // Pin T1 to A.
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&a))
            .await;
        assert!(r1.ok);

        // Pin T1 to B → fail closed.
        let r2 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&b))
            .await;
        assert!(!r2.ok, "second pin must fail closed");
        assert!(
            r2.message.contains("mismatch"),
            "error must mention mismatch: {}",
            r2.message
        );

        // The binding must remain A.
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("T1 must still have its A binding");
        assert_eq!(pinned.project_id, "A");
    }

    // ── TEST L: same pinned A → idempotent success ──

    #[tokio::test]
    async fn same_pinned_a_thread_pin_a_is_idempotent_success() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let p = project_root(project_dir.path());
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p))
            .await;
        assert!(r1.ok);
        let r2 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p))
            .await;
        assert!(r2.ok, "second pin (same project) must be idempotent: {}", r2.message);
        // No outbound message sent.
        assert_eq!(adapter.send_count(), 0);
    }

    // ── TEST O: existing unpinned RESUMABLE session rejects thread.pin ──
    //
    // Per TL v3: `has_reusable_session` must cover active, suspended, AND
    // persisted. Test O exercises the suspended + persisted states directly
    // via `SessionPoolTestState` (doesn't spawn a real subprocess). Test K
    // (below) covers the active path via the recording test agent.
    #[tokio::test]
    async fn existing_unpinned_resumable_session_rejects_thread_pin() {
        // Test O: persisted + suspended sessionId, no project binding.
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let mut state = empty_pool_state();
        state
            .persisted
            .insert("discord:T1".into(), "sess_legacy_id".into());
        state
            .suspended
            .insert("discord:T1".into(), "sess_legacy_id".into());
        let pool = pool_with_state(dir.path(), state);
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // The reusable-session semantic must be true BEFORE the pin call.
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "test setup: pre-populated persisted+suspended must make the session reusable"
        );

        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(!resp.ok, "pin must fail closed on reusable state");
        assert!(
            resp.message.contains("session already exists without trusted project binding"),
            "error must name the invariant: {}",
            resp.message
        );

        // No project binding written.
        assert!(
            pool.get_pinned_project("discord:T1").await.is_none(),
            "no project binding must be written"
        );

        // The reusable session states are STILL there (untouched) — the
        // pin must not delete them, only reject.
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "the persisted/suspended sessionId must remain in the pool"
        );
        assert_eq!(adapter.send_count(), 0, "no outbound message must be sent");
    }

    // ── TEST K: existing unpinned ACTIVE session rejects thread.pin ──
    //
    // Tech Lead v3 mandate: a REAL active ACP session must be created
    // (via the test agent script that responds to JSON-RPC), then
    // `thread.pin(project A)` must fail closed. Helper coverage via
    // `has_reusable_session` is NOT acceptable.
    //
    // Step 1: bootstrap a real active session via the test agent script
    // (no project binding). The agent responds to `initialize` /
    // `session/new` so `pool.get_or_create` completes the spawn path.
    // Step 2: capture the active connection Arc for stability check.
    // Step 3: invoke `RuntimeHandler` ctl `thread.pin(project A)`.
    // Step 4: assert explicit failure, no project binding written,
    // active Arc unchanged, no outbound adapter message.
    #[cfg(unix)]
    #[tokio::test]
    async fn existing_unpinned_active_session_rejects_thread_pin() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        // Use the standard test agent script (no recording) — we only
        // need a real active session for the fail-closed invariant.
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // Step 1: bootstrap a real ACTIVE session for T1 via the test
        // agent script. The agent responds to `initialize` / `session/new`
        // so `pool.get_or_create` completes the spawn path.
        let created = pool
            .get_or_create("discord:T1", None)
            .await
            .expect("active session bootstrap must succeed");
        assert!(created, "T1 must be a fresh active session");

        // Sanity: the session is alive (so the pin path will hit the
        // active-session fast path inside `has_reusable_session`).
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "test setup: active session must be present"
        );
        assert!(
            pool.has_active_session("discord:T1").await,
            "test setup: active session must be alive"
        );

        // Verify the existing session has NO project binding (test setup).
        assert!(
            pool.get_pinned_project("discord:T1").await.is_none(),
            "test setup: active session must not have a project binding"
        );

        // Step 2: invoke ctl thread.pin(project A).
        let mut project_a = project_root(project_dir.path());
        project_a.project_id = "A".into();
        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_a),
            )
            .await;
        assert!(!resp.ok, "pin must fail closed on unpinned active session");
        assert!(
            resp.message.contains("session already exists without trusted project binding"),
            "error must name the invariant: {}",
            resp.message
        );

        // Step 3a: no project binding was written.
        assert!(
            pool.get_pinned_project("discord:T1").await.is_none(),
            "fail-closed path must NOT write a project binding"
        );

        // Step 3b: the active session is STILL alive (no mutation, no
        // silent re-spawn). `has_active_session` does a live connection
        // check; the fact that it returns true proves the pool's alive
        // flag hasn't flipped.
        assert!(
            pool.has_active_session("discord:T1").await,
            "active session must remain alive after pin rejection"
        );
        assert!(
            pool.has_reusable_session("discord:T1").await,
            "reusable-session state must remain true (the active connection is the reusable state)"
        );

        // Step 3c: the active session is still FUNCTIONAL — a follow-up
        // `get_or_create(T, None)` must hit the existing active connection
        // fast path and return Ok(false) (no new session).
        let created_again = pool
            .get_or_create("discord:T1", None)
            .await
            .expect("follow-up call must succeed");
        assert!(
            !created_again,
            "active session must be reused, not re-spawned"
        );

        // Step 3d: no outbound adapter message is sent.
        assert_eq!(
            adapter.send_count(),
            0,
            "thread.pin must NOT call adapter.send_message_targeted"
        );
    }

    // ── TEST G: ctl request without project fields is backward compatible ──

    #[tokio::test]
    async fn ctl_request_without_project_fields_is_backward_compatible() {
        // No project field → thread.pin must fail with a clear "requires
        // project" error, while thread.message (without project) continues
        // to work via the legacy send_message_targeted path.
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // thread.pin without project → error.
        let resp = handler
            .handle_set(Some("T1"), "thread.pin", "", None, None)
            .await;
        assert!(!resp.ok);
        assert!(resp.message.contains("requires a project field"));

        // thread.message without project → sends via the legacy path.
        let resp = handler
            .handle_set(Some("T1"), "thread.message", "hello", None, None)
            .await;
        assert!(resp.ok, "legacy thread.message must still work: {}", resp.message);
        assert_eq!(adapter.send_count(), 1);
        assert_eq!(adapter.last_value().as_deref(), Some("hello"));
    }

    // ── TEST F: ctl request with project fields propagates to SessionPool ──

    #[tokio::test]
    async fn ctl_request_with_project_fields_propagates_to_session_pool() {
        // thread.pin and thread.message both propagate the project to
        // SessionPool. For thread.message, the message is sent AFTER the
        // pin succeeds.
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // thread.pin with project.
        let r1 = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(r1.ok);
        assert!(pool.get_pinned_project("discord:T1").await.is_some());

        // thread.message with project on the SAME thread — idempotent
        // pin, then send.
        let r2 = handler
            .handle_set(
                Some("T1"),
                "thread.message",
                "hello world",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(r2.ok, "thread.message with project: {}", r2.message);
        assert_eq!(adapter.send_count(), 1);
        assert_eq!(adapter.last_value().as_deref(), Some("hello world"));
    }

    // ── TEST N: thread.message(project=B) on thread pinned A → no message sent ──

    #[tokio::test]
    async fn thread_message_with_mismatched_project_does_not_send_discord_message() {
        let dir = tempfile::tempdir().unwrap();
        let project_a_dir = tempfile::tempdir().unwrap();
        let project_b_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // Pin T1 to A.
        let mut a = project_root(project_a_dir.path());
        a.project_id = "A".into();
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&a))
            .await;
        assert!(r1.ok);

        // thread.message with project B → pin must fail closed, and the
        // adapter MUST NOT receive any send_message_targeted call.
        let mut b = project_root(project_b_dir.path());
        b.project_id = "B".into();
        let r2 = handler
            .handle_set(Some("T1"), "thread.message", "should not send", None, Some(&b))
            .await;
        assert!(!r2.ok, "mismatch must reject");
        assert!(
            r2.message.contains("pin failed"),
            "error must surface the pin failure: {}",
            r2.message
        );
        assert_eq!(
            adapter.send_count(),
            0,
            "adapter.send_message_targeted MUST NOT be called when pin fails"
        );
        // The original binding must remain A.
        let pinned = pool.get_pinned_project("discord:T1").await.unwrap();
        assert_eq!(pinned.project_id, "A");
    }

    // ── TEST I: project-root canonicalization preserves equivalence ──

    #[tokio::test]
    async fn project_root_canonicalization_preserves_equivalence() {
        // `project_root` written via `ProjectRef::try_from` is canonicalized.
        // A second pin with a trailing-slash variant of the SAME directory
        // must canonicalize to the same ProjectContext and be idempotent.
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        // First pin with the canonical path.
        let p1 = project_root(project_dir.path());
        let r1 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p1))
            .await;
        assert!(r1.ok);

        // Second pin with the SAME logical path plus a trailing slash.
        let trailing = format!("{}/", project_dir.path().to_string_lossy());
        let p2 = ProjectRef {
            project_id: "openab".into(),
            project_root: trailing,
        };
        let r2 = handler
            .handle_set(Some("T1"), "thread.pin", "", None, Some(&p2))
            .await;
        assert!(
            r2.ok,
            "trailing-slash variant of canonical project must be idempotent: {}",
            r2.message
        );

        // Exactly one binding (idempotent — no second create).
        let pinned = pool.get_pinned_project("discord:T1").await.unwrap();
        assert_eq!(pinned.project_root, project_dir.path().canonicalize().unwrap());
    }

    // ── TEST E: legacy no project uses agent working_dir ──

    #[tokio::test]
    async fn legacy_no_project_uses_agent_working_dir_at_pool_level() {
        // No project, no stored binding → pool falls back to
        // config.working_dir. This is exercised at the SessionPool layer
        // (test `legacy_session_new_receives_configured_working_dir` in
        // pool.rs). Here we just verify the ctl layer does not interfere
        // when there's no project field on a thread.message.
        let dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let resp = handler
            .handle_set(Some("T1"), "thread.message", "hi", None, None)
            .await;
        assert!(resp.ok);
        // No project binding written by the ctl layer.
        assert!(pool.get_pinned_project("discord:T1").await.is_none());
    }

    // ── TEST J: project binding survives restart via session_projects.json ──

    #[tokio::test]
    async fn project_binding_survives_restart_via_session_projects_json() {
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let pool = pool_with_state(dir.path(), empty_pool_state());
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let resp = handler
            .handle_set(
                Some("T1"),
                "thread.pin",
                "",
                None,
                Some(&project_root(project_dir.path())),
            )
            .await;
        assert!(resp.ok);

        // The writer (RuntimeHandler::ensure_pinned_project via pool.get_or_create)
        // saves to the projects_path. Read the JSON file directly to verify
        // the binding is persisted across the daemon's restart lifecycle.
        let projects_path = dir.path().join("session_projects.json");
        let raw = std::fs::read_to_string(&projects_path)
            .expect("session_projects.json must be present after a successful pin");
        let persisted: HashMap<String, CoreProjectContext> =
            serde_json::from_str(&raw).expect("projects file must round-trip");
        assert!(persisted.contains_key("discord:T1"));
        let p = &persisted["discord:T1"];
        assert_eq!(p.project_id, "openab");
        assert_eq!(
            p.project_root,
            project_dir.path().canonicalize().unwrap()
        );
    }

    // ── TEST H: untrusted Discord message text cannot inject project_root ──

    /// The dispatcher's `parse_directives` only recognizes `[[ws:@alias]]` and
    /// `[[title:...]]` — there is no `[[project_id=...]]` directive. The ctl
    /// layer's `project` field is the only path that supplies a project to
    /// `SessionPool`. A message that happens to contain the substring
    /// `project_id: openab` in its body must not be picked up by the
    /// dispatcher's anonymous-context seam.
    #[test]
    fn untrusted_discord_message_text_cannot_inject_project_root() {
        // Sanity-check that the dispatcher path's directive parser only
        // accepts `[[ws:...]]` and `[[title:...]]` — see
        // `crates/openab-core/src/directives.rs`. Anything resembling
        // `project_id=...` is just user text and never reaches the pool.
        let untrusted_body = "please pin project_id=openab project_root=/etc/passwd";
        let parsed = openab_core::directives::parse_directives(untrusted_body);
        let raw = &parsed.metadata.raw;
        assert!(
            raw.get("ws").is_none(),
            "ws must not be set; the dispatcher must not extract a project hint from arbitrary text"
        );
        assert!(
            raw.get("project_id").is_none(),
            "project_id must not be set; no such directive exists"
        );
        // The prompt is preserved verbatim (no silent stripping).
        assert_eq!(parsed.prompt, untrusted_body);
    }

    // ── Sentinel: SenderContext is reachable through the workspace's
    // adapter surface (used by future sender-bound tests). ──
    #[allow(dead_code)]
    fn _ensure_sender_context_in_scope() -> SenderContext {
        SenderContext {
            schema: "openab.sender.v1".into(),
            sender_id: "u".into(),
            sender_name: "u".into(),
            display_name: "u".into(),
            channel: "c".into(),
            channel_id: "c".into(),
            thread_id: None,
            is_bot: false,
            timestamp: None,
            message_id: None,
            receiver_id: None,
        }
    }

    // ── E2E: real UDS chain ──────────────────────────────────────────────
    //
    // The 12-point Tech Lead mandate requires the E2E to actually cross:
    //   Unix ctl socket → send_request_to → RuntimeHandler
    //     → ensure_pinned_project → SessionPool::get_or_create
    //     → real recording ACP test agent → session/new.params.cwd
    //
    // Helper-only tests (e.g. calling `handle_set` directly) are NOT
    // acceptable E2E coverage. This test wires the real
    // `spawn_server_at` + `send_request_to` UDS path AND verifies the
    // `session/new.params.cwd` value the agent actually received.
    //
    // Also exercises `thread.message(project=A)` through the same UDS
    // path so the pin-first + outbound-adapter sequencing is observable
    // end-to-end.
    #[cfg(unix)]
    #[tokio::test]
    async fn e2e_trusted_thread_pin_drives_session_new_cwd() {
        // ── Wire the real UDS server with a recording test agent ──
        let dir = tempfile::tempdir().unwrap();
        let project_dir = tempfile::tempdir().unwrap();
        let canonical_project_root = project_dir.path().canonicalize().unwrap();

        let record_path = dir.path().join("agent-rpc.log");
        let pool = recording_pool(dir.path(), &record_path);
        let adapter = std::sync::Arc::new(RecordingAdapter::default());
        let handler = make_handler(adapter.clone(), pool.clone());

        let sock = dir.path().join("test.sock");
        let server = spawn_server_at(sock.clone(), std::sync::Arc::new(handler));
        // Give the server a moment to bind.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // ── Step 1: `thread.pin` via the UDS protocol ──
        let pin_resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.pin".into(),
                value: None,
                thread_id: Some("T1".into()),
                target_user_id: None,
                project: Some(project_root(project_dir.path())),
            },
        )
        .await
        .expect("UDS send_request_to must succeed");
        assert!(pin_resp.ok, "thread.pin must succeed via UDS: {}", pin_resp.message);
        assert!(
            pin_resp.message.contains("pinned"),
            "pin response must confirm the pin"
        );

        // Proves the pin crossed the UDS protocol path AND reached the
        // pool: `session_projects[discord:T1]` must exist.
        let pinned = pool
            .get_pinned_project("discord:T1")
            .await
            .expect("pool must have entry under canonical key discord:T1 after UDS pin");
        assert_eq!(pinned.project_id, "openab");
        assert_eq!(pinned.project_root, canonical_project_root);

        // ── Step 2: validate the agent actually received the canonical cwd ──
        //
        // The recording test agent writes every JSON-RPC line it received
        // to `record_path`. Reading the file directly proves the project
        // root reached the agent — not just that the pool's in-memory
        // state looks right.
        let cwd = cwd_from_recorded_session_new(&record_path);
        assert_eq!(
            cwd,
            canonical_project_root.to_string_lossy(),
            "session/new.params.cwd must be the canonical project_root \
             (recorded by the agent, not inferred from pool state)"
        );

        // ── Step 3: `thread.message(project=A)` through the same UDS ──
        //
        // Same project (idempotent pin), then send. The pin path must
        // not corrupt the binding and the outbound adapter call must
        // happen exactly once.
        let msg_resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.message".into(),
                value: Some("HANDOFF via UDS".into()),
                thread_id: Some("T1".into()),
                target_user_id: None,
                project: Some(project_root(project_dir.path())),
            },
        )
        .await
        .expect("UDS send_request_to must succeed");
        assert!(
            msg_resp.ok,
            "thread.message with idempotent project must succeed via UDS: {}",
            msg_resp.message
        );

        // Pin survived (same project).
        let pinned_after = pool.get_pinned_project("discord:T1").await.unwrap();
        assert_eq!(pinned_after.project_root, canonical_project_root);

        // Outbound adapter dispatched exactly once.
        assert_eq!(
            adapter.send_count(),
            1,
            "thread.message with project must dispatch exactly once"
        );
        assert_eq!(
            adapter.last_value().as_deref(),
            Some("HANDOFF via UDS"),
            "the message body reaches the adapter verbatim"
        );

        // ── Step 4: `thread.message(project=B)` against pinned A fails
        // closed AND does NOT send ──
        let mut b = project_root(project_dir.path());
        b.project_id = "B".into();
        let bad_resp = send_request_to(
            &sock,
            &Request {
                action: Action::Set,
                key: "thread.message".into(),
                value: Some("should not send".into()),
                thread_id: Some("T1".into()),
                target_user_id: None,
                project: Some(b),
            },
        )
        .await
        .expect("UDS send_request_to must succeed");
        assert!(
            !bad_resp.ok,
            "mismatched project must reject; UDS must surface the pin failure"
        );
        assert!(
            bad_resp.message.contains("pin failed"),
            "error must surface the pin failure: {}",
            bad_resp.message
        );
        // Adapter dispatch count UNCHANGED — the unrejected `HANDOFF via UDS`
        // is the only outbound message.
        assert_eq!(
            adapter.send_count(),
            1,
            "a mismatched thread.message must NOT call adapter.send_message_targeted"
        );

        // Cleanup.
        server.abort();
    }
}
