//! Shared, transport-neutral ingress into the canonical turn dispatcher.
//!
//! A successful admission acknowledgement means `Dispatcher::submit` accepted
//! the event into its single per-thread consumer channel. It deliberately does
//! not wait for ACP/LLM completion.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;

use crate::adapter::{ChannelRef, ChatAdapter};
use crate::dispatch::{BufferedMessage, DispatchError, Dispatcher};

/// Narrow acceptance seam between transport-neutral admission and the
/// canonical dispatcher. Production delegates to `Dispatcher::submit`; tests
/// can deterministically record or reject an admission without a consumer.
#[async_trait]
pub trait DispatchSubmitPort: Send + Sync {
    async fn submit(
        &self,
        thread_key: String,
        thread_channel: ChannelRef,
        adapter: Arc<dyn ChatAdapter>,
        message: BufferedMessage,
    ) -> Result<(), DispatchError>;
}

#[async_trait]
impl DispatchSubmitPort for Dispatcher {
    async fn submit(
        &self,
        thread_key: String,
        thread_channel: ChannelRef,
        adapter: Arc<dyn ChatAdapter>,
        message: BufferedMessage,
    ) -> Result<(), DispatchError> {
        Dispatcher::submit(self, thread_key, thread_channel, adapter, message).await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeWorkflowMetadata {
    pub dispatch_id: String,
    /// Canonical native conversation identity from admitted ``agent.work``.
    /// This is distinct from the ACP session-pool key.
    pub conversation_key: String,
    pub workflow_run_id: String,
    pub task_id: String,
    pub role: String,
    pub agent: String,
    pub lease_id: String,
    pub lease_generation: u64,
    pub expected_revision: u64,
    pub language: Option<String>,
    /// Optional project metadata supplied by the canonical native admission
    /// path.  It is telemetry only: recovery authority remains the fenced
    /// workflow/lease tuple, so absent metadata must stay absent.
    pub project_id: Option<String>,
    pub project_root: Option<String>,
    /// Phase 6.2.9: per-dispatch ACP execution-session key.
    ///
    /// Native workflow turns MUST NOT inherit historical ACP conversation
    /// state merely because the same OpenAB daemon, Discord delivery target,
    /// or ACP process was previously used.  When the native-work admission
    /// path sets this field, the pool guarantees:
    ///
    ///   - a fresh ACP `session/new` is spawned (no `session/load`);
    ///   - the resulting session id is never written to
    ///     `state.persisted` so daemon restart cannot replay it;
    ///   - the same dispatch id, retried by the scheduler with an
    ///     identical payload fingerprint, lands on the same pool key
    ///     (idempotency is owned by the ctl-side ledger, not the pool).
    ///
    /// `None` means the request is from a Discord (or other non-native)
    /// adapter and the legacy session-pool key derivation applies.
    pub native_execution_session_key: Option<String>,
    /// Phase 6.4.1B — authoritative transport identity carried in by the
    /// structured dispatch metadata (the `AgentWorkRequest` JSON). The
    /// value is propagated unchanged into the
    /// `NativeCompletionEvent.transport` field so AAP Runtime can
    /// perform transport-aware conversation identity validation
    /// (Phase 6.4.1). The transport is NEVER derived from the
    /// `conversation_key` prefix or any other heuristic.
    ///
    /// `None` means the request originated from a daemon build that did
    /// not yet plumb transport through `agent.work`; Runtime defaults
    /// such records to legacy OPENAB semantics.
    pub transport: Option<String>,
    /// Phase 6.4.1D — authoritative structured delivery destination
    /// carried in by AAP Runtime from the upstream
    /// ``ConversationBinding``. The dispatcher uses this as the
    /// ``BufferedMessage.trigger_msg.channel`` for this turn INSTEAD
    /// of the daemon-wide ``native_delivery_target`` fallback so
    /// every role handoff lands in the actual workflow's originating
    /// Discord channel. Sourced from trusted structured admission
    /// metadata only; NEVER parsed from ``conversation_key``.
    pub delivery_destination: Option<crate::adapter::ChannelRef>,
    /// Phase 6.4.1F — structured native scope authority. The
    /// Runtime scheduler is the source of truth. OpenAB renders
    /// this in the ``<native_work_authority>`` block and propagates
    /// ``write_policy`` into the ACP tool-permission gate. ``None``
    /// preserves the pre-6.4.1F default (no enforcement). Persistent
    /// memory MUST NOT override this for the current turn.
    pub scope_policy: Option<NativeScopePolicy>,
}

/// Phase 6.4.1F — canonical structured native scope authority.
/// Mirrors the AAP Runtime ``NativeScopePolicy`` shape and the
/// ``AgentWorkScopePolicy`` wire DTO. The three fields are
/// intentionally narrow so the surface cannot drift into free-form
/// prose:
///   * ``scope_mode`` — currently only ``BOUNDED``.
///   * ``write_policy`` — ``READ_ONLY`` or ``MODIFY_ALLOWED``.
///     The ACP tool-permission gate reads this value.
///   * ``historical_context_policy`` — currently only
///     ``ADVISORY_ONLY``. The Runtime declares that persistent memory
///     is advisory only and MUST NOT redefine the current work
///     identity, change ``requested_work``, expand the current scope,
///     reopen historical unfinished work, override ``write_policy``,
///     execute historical ``next_action`` values, or override explicit
///     current-turn restrictions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeScopePolicy {
    pub scope_mode: String,
    pub write_policy: String,
    pub historical_context_policy: String,
}

impl NativeScopePolicy {
    pub const READ_ONLY: &'static str = "READ_ONLY";
    pub const MODIFY_ALLOWED: &'static str = "MODIFY_ALLOWED";

    /// Returns ``true`` iff the policy declares ``READ_ONLY`` and is
    /// therefore a binding write-restriction for the ACP layer.
    pub fn is_read_only(&self) -> bool {
        self.write_policy == Self::READ_ONLY
    }
}

impl Default for NativeScopePolicy {
    fn default() -> Self {
        // Phase 6.4.1F backward-compat default: legacy callers that
        // do not populate the field keep the pre-6.4.1F behaviour
        // (mutation allowed, no prompt-level fence, no ACP gate).
        Self {
            scope_mode: "BOUNDED".to_string(),
            write_policy: Self::MODIFY_ALLOWED.to_string(),
            historical_context_policy: "ADVISORY_ONLY".to_string(),
        }
    }
}

/// Render the fenced execution authority for a native ``agent.work`` turn.
///
/// This is deliberately separate from the legacy workflow context: native
/// turns are admitted from AAP's WorkflowRun + AgentLease authority, whereas
/// on-disk assignment files remain compatibility projections only.
///
/// Phase 6.4.1F — the rendered block now states the explicit precedence
/// hierarchy that governs this turn:
///
///   1. Current native work authority + structured native scope (this block).
///   2. The AAP Task snapshot embedded in the WORKFLOW_DISPATCH envelope.
///   3. Repository policy (AGENTS.md / CLAUDE.md).
///   4. Persistent / auto-memory and historical context (advisory only).
///
/// Persistent memory MUST NOT redefine the current task/workflow
/// identity, change ``requested_work``, expand the current scope,
/// reopen historical unfinished work, override ``write_policy``,
/// execute historical ``next_action`` values, or override explicit
/// current-turn restrictions. When persistent memory conflicts with
/// the current native authority, the conflicting memory MUST be
/// ignored for this turn. Legacy ``.agents/workflow_assignment.json``
/// and ``.openab/workflow_assignment.json`` files remain
/// non-authoritative projections.
pub fn render_native_workflow_authority(metadata: &NativeWorkflowMetadata) -> String {
    let scope_block = match metadata.scope_policy.as_ref() {
        Some(p) => format!(
            "NATIVE SCOPE (structured)\n\
scope_mode: {}\n\
write_policy: {}\n\
historical_context_policy: {}\n",
            p.scope_mode, p.write_policy, p.historical_context_policy
        ),
        None => "NATIVE SCOPE (structured)\n\
scope_mode: <unset — legacy caller>\n\
write_policy: <unset — defaults to MODIFY_ALLOWED; no ACP tool-permission gate>\n\
historical_context_policy: <unset — defaults to ADVISORY_ONLY>\n"
            .to_string(),
    };
    let write_policy_directive = match metadata.scope_policy.as_ref() {
        Some(p) if p.is_read_only() => {
            "WRITE POLICY DIRECTIVE\n\
This native dispatch declares write_policy=READ_ONLY. The ACP layer MUST\n\
deny any tool invocation whose title or kind matches: Edit, Write,\n\
NotebookEdit, MultiEdit, apply_patch, or Bash. Deterministic denial is\n\
applied at the session/request_permission seam BEFORE the tool can\n\
mutate the filesystem. Do NOT attempt to bypass this with shell\n\
commands, alternate tool names, or partial edits — every known\n\
write-capable tool name is on the deny-list.\n"
        }
        Some(_) => {
            "WRITE POLICY DIRECTIVE\n\
This native dispatch declares write_policy=MODIFY_ALLOWED. Mutation is\n\
permitted subject to the rest of this authority block and the canonical\n\
role + scope. Do NOT expand the scope, reopen historical unfinished\n\
work, or execute historical next_action values.\n"
        }
        None => {
            "WRITE POLICY DIRECTIVE\n\
This native dispatch did not declare a structured scope policy (legacy\n\
caller). Mutation remains permitted subject to the rest of this\n\
authority block. The ACP tool-permission gate is NOT active for this\n\
turn.\n"
        }
    };
    format!(
        "<native_work_authority>\n\
NATIVE WORK AUTHORITY\n\
This turn was admitted by AAP Phase-5 fenced execution.\n\
Authoritative:\n\
dispatch_id: {}\n\
workflow_run_id: {}\n\
task_id: {}\n\
role: {}\n\
agent: {}\n\
lease_id: {}\n\
lease_generation: {}\n\
expected_revision: {}\n\
language: {}\n\
\
{}\n\
{}\n\
\
PRECEDENCE HIERARCHY (binding for this turn)\n\
1. Current native work authority and structured native scope (this block).\n\
2. The AAP Task snapshot embedded in the WORKFLOW_DISPATCH envelope\n\
   (dispatch_id, workflow_run_id, task_id, role, agent,\n\
   requested_work, remaining_work, next_action, write_policy).\n\
3. Repository policy — AGENTS.md and CLAUDE.md.\n\
4. Persistent / auto-memory and historical task context — ADVISORY ONLY.\n\
\
Persistent memory (Claude Code auto-memory, prior reports, historical\n\
task context, MEMORY.md, legacy workflow_assignment projections) is\n\
advisory only for this turn. It MUST NOT:\n\
  - redefine the current task or workflow identity\n\
  - change the AAP Task snapshot's requested_work\n\
  - expand the current scope\n\
  - reopen historical unfinished work whose remaining_work is now empty\n\
  - override the structured write_policy\n\
  - execute historical next_action values\n\
  - override explicit current-turn restrictions\n\
When persistent memory conflicts with the current native authority or\n\
the AAP Task snapshot, the conflicting memory MUST be ignored for this\n\
turn. Legacy .agents/workflow_assignment.json and\n\
.openab/workflow_assignment.json files are non-authoritative projections.\n\
AGENTS.md remains the policy and workflow-rules authority. Follow the\n\
native role and assignment for this turn.\n\
\
For a VERIFIER or FINAL_REVIEWER terminal verdict, emit exactly one standalone \
canonical line: VERIFIER_PASS, VERIFIER_FAIL, FINAL_REVIEWER_PASS, or \
FINAL_REVIEWER_FAIL, matching your assigned role. ACP end_turn alone is not a \
workflow verdict.\n\
</native_work_authority>",
        metadata.dispatch_id,
        metadata.workflow_run_id,
        metadata.task_id,
        metadata.role,
        metadata.agent,
        metadata.lease_id,
        metadata.lease_generation,
        metadata.expected_revision,
        metadata.language.as_deref().unwrap_or("<none>"),
        scope_block,
        write_policy_directive,
    )
}

/// Transport-neutral turn already normalized by its ingress adapter.
pub struct WorkAdmissionRequest {
    pub conversation: ChannelRef,
    pub sender_id: String,
    pub adapter: Arc<dyn ChatAdapter>,
    pub message: BufferedMessage,
    /// Reserved for the future native `agent.work` command. Discord callers
    /// leave this unset and are never subjected to native workflow fencing.
    pub native_workflow: Option<NativeWorkflowMetadata>,
    /// Phase 6.2.9: explicit per-dispatch ACP execution-session key.
    ///
    /// Set by `set agent.work` to a deterministic key derived from
    /// `agent + dispatch_id`. When set, this key is used as the
    /// session-pool key instead of the Discord platform+channel pool
    /// key, so the pool cannot replay unrelated historical turns.
    ///
    /// `None` for Discord (and other non-native) adapters — those keep
    /// using the legacy `discord:<channel>:<thread>` pool key so human
    /// conversational sessions behave exactly as before.
    pub native_execution_session_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkAdmissionAck {
    pub admission_id: String,
    pub conversation_key: String,
    pub accepted: bool,
    pub native_workflow: Option<NativeWorkflowMetadata>,
}

#[derive(Debug)]
pub enum WorkAdmissionError {
    /// The shared composition handle has not received its sole dispatcher port.
    NotInstalled,
    /// The transport-neutral request is missing the canonical conversation or sender identity.
    InvalidRequest(&'static str),
    DispatchRejected(DispatchError),
    /// Reserved for failures inside the admission seam that are neither input
    /// validation nor a dispatcher rejection.
    Internal(&'static str),
}

impl std::fmt::Display for WorkAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInstalled => {
                write!(f, "ADMISSION_NOT_INSTALLED: admission service is not ready")
            }
            Self::InvalidRequest(reason) => write!(f, "INVALID_ADMISSION_REQUEST: {reason}"),
            Self::DispatchRejected(error) => write!(f, "DISPATCH_REJECTED: {error}"),
            Self::Internal(reason) => write!(f, "ADMISSION_INTERNAL_ERROR: {reason}"),
        }
    }
}

impl std::error::Error for WorkAdmissionError {}

impl WorkAdmissionError {
    /// Stable, admission-layer-only taxonomy token.  Transport and control
    /// plane errors deliberately remain outside this seam.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotInstalled => "ADMISSION_NOT_INSTALLED",
            Self::InvalidRequest(_) => "INVALID_ADMISSION_REQUEST",
            Self::DispatchRejected(_) => "DISPATCH_REJECTED",
            Self::Internal(_) => "ADMISSION_INTERNAL_ERROR",
        }
    }
}

#[async_trait]
pub trait WorkAdmissionPort: Send + Sync {
    async fn admit_work(
        &self,
        request: WorkAdmissionRequest,
    ) -> Result<WorkAdmissionAck, WorkAdmissionError>;
}

/// The sole production implementation: it delegates to an already constructed
/// canonical dispatcher, rather than constructing an execution engine itself.
pub struct DispatcherAdmissionPort {
    dispatcher: Option<Arc<Dispatcher>>,
    submitter: Arc<dyn DispatchSubmitPort>,
}

impl DispatcherAdmissionPort {
    pub fn new(dispatcher: Arc<Dispatcher>) -> Self {
        Self {
            submitter: dispatcher.clone(),
            dispatcher: Some(dispatcher),
        }
    }

    #[cfg(test)]
    fn with_submitter(submitter: Arc<dyn DispatchSubmitPort>) -> Self {
        Self {
            dispatcher: None,
            submitter,
        }
    }

    #[cfg(test)]
    fn canonical_dispatcher(&self) -> Option<&Arc<Dispatcher>> {
        self.dispatcher.as_ref()
    }
}

#[async_trait]
impl WorkAdmissionPort for DispatcherAdmissionPort {
    async fn admit_work(
        &self,
        request: WorkAdmissionRequest,
    ) -> Result<WorkAdmissionAck, WorkAdmissionError> {
        if request.conversation.platform.is_empty() {
            return Err(WorkAdmissionError::InvalidRequest("platform is required"));
        }
        if request.conversation.channel_id.is_empty() {
            return Err(WorkAdmissionError::InvalidRequest(
                "conversation channel is required",
            ));
        }
        if request.sender_id.is_empty() {
            return Err(WorkAdmissionError::InvalidRequest("sender is required"));
        }
        let conversation_key = request.conversation.session_pool_key();
        // Phase 6.2.9: native-work dispatches must not collide with human
        // Discord conversational pool keys. When `set agent.work` supplied an
        // explicit per-dispatch execution-session key, use it as the
        // session-pool key so the pool guarantees a fresh ACP session and
        // never replays historical turns for that dispatch. The Discord
        // delivery thread remains `request.conversation` (unchanged) so
        // transport delivery and ACP isolation are decoupled.
        let dispatch_key = if let Some(native_key) = request
            .native_execution_session_key
            .as_deref()
            .filter(|k| !k.is_empty())
        {
            native_key.to_string()
        } else {
            self.dispatcher.as_ref().map_or_else(
                || request.conversation.session_pool_key(),
                |dispatcher| {
                    dispatcher.key(
                        &request.conversation.platform,
                        &request.conversation.channel_id,
                        &request.sender_id,
                    )
                },
            )
        };
        let native_workflow = request.native_workflow.clone();
        let mut message = request.message;
        message.native_workflow = native_workflow.clone();
        self.submitter
            .submit(dispatch_key, request.conversation, request.adapter, message)
            .await
            .map_err(WorkAdmissionError::DispatchRejected)?;
        Ok(WorkAdmissionAck {
            admission_id: format!("{}:{}", conversation_key, request.sender_id),
            conversation_key,
            accepted: true,
            native_workflow,
        })
    }
}

/// Stable composition handle. It is created before platform dispatchers and
/// installed once with the already-owned canonical dispatcher; callers before
/// installation fail explicitly instead of queuing work into a hidden engine.
pub struct AdmissionPortHandle {
    inner: OnceLock<Arc<dyn WorkAdmissionPort>>,
}

impl Default for AdmissionPortHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl AdmissionPortHandle {
    pub fn new() -> Self {
        Self {
            inner: OnceLock::new(),
        }
    }
    pub fn install(
        &self,
        port: Arc<dyn WorkAdmissionPort>,
    ) -> Result<(), Arc<dyn WorkAdmissionPort>> {
        self.inner.set(port)
    }
}

#[async_trait]
impl WorkAdmissionPort for AdmissionPortHandle {
    async fn admit_work(
        &self,
        request: WorkAdmissionRequest,
    ) -> Result<WorkAdmissionAck, WorkAdmissionError> {
        let Some(port) = self.inner.get() else {
            return Err(WorkAdmissionError::NotInstalled);
        };
        port.admit_work(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acp::pool::SessionPoolTestState;
    use crate::acp::ContentBlock;
    use crate::acp::SessionPool;
    use crate::adapter::AdapterRouter;
    use crate::adapter::MessageRef;
    use crate::config::{AgentConfig, ReactionsConfig};
    use crate::dispatch::BatchGrouping;
    use crate::markdown::TableMode;
    use anyhow::Result;
    use std::sync::Mutex;
    use std::time::Instant;

    struct RecordingPort;

    #[async_trait]
    impl WorkAdmissionPort for RecordingPort {
        async fn admit_work(
            &self,
            request: WorkAdmissionRequest,
        ) -> Result<WorkAdmissionAck, WorkAdmissionError> {
            Ok(WorkAdmissionAck {
                admission_id: "accepted-by-canonical-dispatcher".to_owned(),
                conversation_key: request.conversation.session_pool_key(),
                accepted: true,
                native_workflow: request.native_workflow,
            })
        }
    }

    #[test]
    fn install_is_one_shot() {
        let handle = AdmissionPortHandle::new();
        assert!(handle.install(Arc::new(RecordingPort)).is_ok());
        assert!(handle.install(Arc::new(RecordingPort)).is_err());
    }

    #[test]
    fn typed_failure_taxonomy_is_transport_neutral() {
        assert_eq!(
            WorkAdmissionError::NotInstalled.code(),
            "ADMISSION_NOT_INSTALLED"
        );
        assert_eq!(
            WorkAdmissionError::InvalidRequest("missing sender").code(),
            "INVALID_ADMISSION_REQUEST"
        );
        assert_eq!(
            WorkAdmissionError::DispatchRejected(DispatchError::ConsumerDead).code(),
            "DISPATCH_REJECTED"
        );
        assert_eq!(
            WorkAdmissionError::Internal("unexpected").code(),
            "ADMISSION_INTERNAL_ERROR"
        );
    }

    struct TestAdapter;

    struct RecordingSubmitter {
        calls: Mutex<Vec<(String, Option<NativeWorkflowMetadata>)>>,
        reject: bool,
    }

    #[async_trait]
    impl DispatchSubmitPort for RecordingSubmitter {
        async fn submit(
            &self,
            thread_key: String,
            _thread_channel: ChannelRef,
            _adapter: Arc<dyn ChatAdapter>,
            message: BufferedMessage,
        ) -> Result<(), DispatchError> {
            self.calls
                .lock()
                .unwrap()
                .push((thread_key, message.native_workflow));
            if self.reject {
                Err(DispatchError::ConsumerDead)
            } else {
                Ok(())
            }
        }
    }

    #[async_trait]
    impl ChatAdapter for TestAdapter {
        fn platform(&self) -> &'static str {
            "test"
        }
        fn message_limit(&self) -> usize {
            2_000
        }
        async fn send_message(&self, channel: &ChannelRef, _content: &str) -> Result<MessageRef> {
            Ok(MessageRef {
                channel: channel.clone(),
                message_id: "reply".into(),
            })
        }
        async fn create_thread(
            &self,
            channel: &ChannelRef,
            _trigger: &MessageRef,
            _title: &str,
        ) -> Result<ChannelRef> {
            Ok(channel.clone())
        }
        async fn add_reaction(&self, _message: &MessageRef, _emoji: &str) -> Result<()> {
            Ok(())
        }
        async fn remove_reaction(&self, _message: &MessageRef, _emoji: &str) -> Result<()> {
            Ok(())
        }
        fn use_streaming(&self, _other_bot_present: bool) -> bool {
            false
        }
    }

    fn request(sender_id: &str) -> WorkAdmissionRequest {
        let conversation = ChannelRef {
            platform: "native".into(),
            channel_id: "channel-id".into(),
            thread_id: Some("canonical-thread".into()),
            parent_id: None,
            origin_event_id: None,
        };
        WorkAdmissionRequest {
            adapter: Arc::new(TestAdapter),
            sender_id: sender_id.into(),
            message: BufferedMessage {
                sender_json: "{}".into(),
                sender_name: "sender".into(),
                prompt: "turn".into(),
                extra_blocks: Vec::<ContentBlock>::new(),
                trigger_msg: MessageRef {
                    channel: conversation.clone(),
                    message_id: "event".into(),
                },
                arrived_at: Instant::now(),
                estimated_tokens: 1,
                other_bot_present: false,
                recipient: None,
                native_workflow: None,
            },
            conversation,
            native_workflow: None,
            native_execution_session_key: None,
        }
    }

    #[tokio::test]
    async fn uninstalled_handle_fails_closed_without_ack() {
        let handle = AdmissionPortHandle::new();
        let result = handle.admit_work(request("native-sender")).await;
        assert!(matches!(result, Err(WorkAdmissionError::NotInstalled)));
    }

    #[tokio::test]
    async fn installed_handle_preserves_native_conversation_key_and_metadata() {
        let handle = AdmissionPortHandle::new();
        assert!(handle.install(Arc::new(RecordingPort)).is_ok());
        let mut request = request("native-sender");
        request.native_workflow = Some(NativeWorkflowMetadata {
            dispatch_id: "dispatch-sentinel".into(),
            conversation_key: "1540183233654952036".into(),
            workflow_run_id: "run-sentinel".into(),
            task_id: "task-sentinel".into(),
            role: "PRIMARY".into(),
            agent: "ArthurCodex".into(),
            lease_id: "lease-sentinel".into(),
            lease_generation: 79,
            expected_revision: 11,
            language: Some("zh-TW".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(
                "native-dispatch:ArthurCodex:dispatch-sentinel".into(),
            ),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        });
        request.native_execution_session_key =
            Some("native-dispatch:ArthurCodex:dispatch-sentinel".into());
        let ack = handle
            .admit_work(request)
            .await
            .expect("recording port admits native request");
        assert!(ack.accepted);
        assert_eq!(ack.conversation_key, "native:canonical-thread");
        assert_eq!(
            ack.native_workflow.as_ref().unwrap().dispatch_id,
            "dispatch-sentinel"
        );
        assert_eq!(ack.native_workflow.as_ref().unwrap().lease_generation, 79);
    }

    #[tokio::test]
    async fn accepted_ack_is_returned_only_after_submit_accepts() {
        let submitter = Arc::new(RecordingSubmitter {
            calls: Mutex::new(Vec::new()),
            reject: false,
        });
        let port = DispatcherAdmissionPort::with_submitter(submitter.clone());

        let ack = port.admit_work(request("native-sender")).await.unwrap();

        assert!(ack.accepted);
        assert_eq!(submitter.calls.lock().unwrap().len(), 1);
        assert_eq!(
            submitter.calls.lock().unwrap()[0].0,
            "native:canonical-thread"
        );
    }

    #[tokio::test]
    async fn rejected_submit_returns_typed_error_without_ack() {
        let submitter = Arc::new(RecordingSubmitter {
            calls: Mutex::new(Vec::new()),
            reject: true,
        });
        let port = DispatcherAdmissionPort::with_submitter(submitter.clone());

        let result = port.admit_work(request("native-sender")).await;

        assert!(matches!(
            result,
            Err(WorkAdmissionError::DispatchRejected(
                DispatchError::ConsumerDead
            ))
        ));
        assert_eq!(submitter.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn native_metadata_reaches_the_submit_boundary_exactly() {
        let submitter = Arc::new(RecordingSubmitter {
            calls: Mutex::new(Vec::new()),
            reject: false,
        });
        let port = DispatcherAdmissionPort::with_submitter(submitter.clone());
        let metadata = NativeWorkflowMetadata {
            dispatch_id: "dispatch-test-123".into(),
            conversation_key: "1540183233654952036".into(),
            workflow_run_id: "run-test-456".into(),
            task_id: "task-test-789".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-test-123".into(),
            lease_generation: 314,
            expected_revision: 271,
            language: Some("zh-TW".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(
                "native-dispatch:ArthurClaude:dispatch-test-123".into(),
            ),
            transport: Some("OPENAB".into()),
            delivery_destination: None,
            scope_policy: None,
        };
        let mut admission = request("native-sender");
        admission.native_workflow = Some(metadata.clone());

        let ack = port.admit_work(admission).await.unwrap();

        assert_eq!(ack.native_workflow, Some(metadata.clone()));
        assert_eq!(submitter.calls.lock().unwrap()[0].1, Some(metadata));
    }

    #[tokio::test]
    async fn native_execution_session_key_overrides_session_pool_key() {
        // Phase 6.2.9 invariant: when `set agent.work` supplies an explicit
        // per-dispatch execution-session key, that key MUST be used as the
        // session-pool key passed to the dispatcher — NOT the Discord
        // platform+channel pool key. The Discord conversational key is
        // preserved only as `conversation_key` metadata.
        let submitter = Arc::new(RecordingSubmitter {
            calls: Mutex::new(Vec::new()),
            reject: false,
        });
        let port = DispatcherAdmissionPort::with_submitter(submitter.clone());

        let mut admission = request("native-sender");
        admission.native_execution_session_key =
            Some("native-dispatch:ArthurClaude:dispatch-iso-1".into());

        let ack = port.admit_work(admission).await.unwrap();

        assert!(ack.accepted);
        assert_eq!(
            submitter.calls.lock().unwrap()[0].0,
            "native-dispatch:ArthurClaude:dispatch-iso-1"
        );
        // Conversation-key correlation metadata is unchanged.
        assert_eq!(ack.conversation_key, "native:canonical-thread");
    }

    #[tokio::test]
    async fn empty_native_execution_session_key_falls_back_to_session_pool_key() {
        // The ctl layer can hand us an empty string when the native-work
        // payload did not include a dispatch id. The admission port MUST
        // NOT silently coerce that into a zero-length pool key — fall back
        // to the canonical session-pool key for that conversation instead.
        let submitter = Arc::new(RecordingSubmitter {
            calls: Mutex::new(Vec::new()),
            reject: false,
        });
        let port = DispatcherAdmissionPort::with_submitter(submitter.clone());

        let mut admission = request("native-sender");
        admission.native_execution_session_key = Some(String::new());

        let _ack = port.admit_work(admission).await.unwrap();

        assert_eq!(
            submitter.calls.lock().unwrap()[0].0,
            "native:canonical-thread"
        );
    }

    #[test]
    fn canonical_dispatcher_port_preserves_the_router_and_session_pool_graph() {
        let temp = tempfile::tempdir().unwrap();
        let pool = Arc::new(SessionPool::with_test_state(
            AgentConfig {
                command: "test-agent".into(),
                args: Vec::new(),
                working_dir: temp.path().to_string_lossy().into(),
                env: std::collections::HashMap::new(),
                inherit_env: Vec::new(),
                command_explicit: true,
            },
            SessionPoolTestState::default(),
            temp.path().join("session_projects.json"),
        ));
        let router = Arc::new(AdapterRouter::new(
            pool.clone(),
            ReactionsConfig::default(),
            TableMode::default(),
            60,
            30,
            std::collections::HashMap::new(),
            temp.path().to_path_buf(),
        ));
        let dispatcher = Arc::new(Dispatcher::with_idle_timeout(
            router.clone(),
            1,
            100,
            BatchGrouping::Thread,
            std::time::Duration::from_secs(1),
        ));
        let port = DispatcherAdmissionPort::new(dispatcher.clone());
        let handle = AdmissionPortHandle::new();

        assert!(Arc::ptr_eq(
            port.canonical_dispatcher().unwrap(),
            &dispatcher
        ));
        assert!(dispatcher.targets_router(&router));
        assert!(Arc::ptr_eq(router.pool(), &pool));
        assert!(handle.install(Arc::new(port)).is_ok());
    }

    // --- Phase 6.4.1F — structured native scope authority rendering ----

    fn metadata_for_authority(
        scope: Option<NativeScopePolicy>,
        _requested_work: &str,
    ) -> NativeWorkflowMetadata {
        NativeWorkflowMetadata {
            dispatch_id: "dispatch-641f".into(),
            conversation_key: "ck-641f".into(),
            workflow_run_id: "wfr-641f".into(),
            task_id: "task-641f".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-641f".into(),
            lease_generation: 1,
            expected_revision: 1,
            language: Some("en".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: None,
            transport: None,
            delivery_destination: None,
            scope_policy: scope,
        }
    }

    #[test]
    fn native_authority_block_states_explicit_precedence_hierarchy() {
        // Test 1 / Test 6: native task A is authoritative regardless of
        // any memory / legacy projection. The rendered block must
        // state the precedence hierarchy AND explicitly forbid memory
        // from redefining the current work.
        let scope = Some(NativeScopePolicy {
            scope_mode: "BOUNDED".into(),
            write_policy: NativeScopePolicy::MODIFY_ALLOWED.into(),
            historical_context_policy: "ADVISORY_ONLY".into(),
        });
        let metadata = metadata_for_authority(scope, "phase 641f native work A");
        let block = render_native_workflow_authority(&metadata);

        assert!(block.contains("<native_work_authority>"));
        assert!(block.contains("PRECEDENCE HIERARCHY"));
        assert!(
            block.contains("Current native work authority and structured native scope"),
            "precedence #1 must be stated"
        );
        assert!(
            block.contains("AAP Task snapshot embedded in the WORKFLOW_DISPATCH envelope"),
            "precedence #2 must be stated"
        );
        assert!(
            block.contains("Repository policy — AGENTS.md and CLAUDE.md"),
            "precedence #3 must be stated"
        );
        assert!(
            block.contains("Persistent / auto-memory and historical task context — ADVISORY ONLY"),
            "precedence #4 must be stated"
        );

        // Memory MUST NOT override the requested_work, expand scope,
        // or override write_policy.
        assert!(block.contains("redefine the current task or workflow identity"));
        assert!(block.contains("change the AAP Task snapshot's requested_work"));
        assert!(block.contains("override the structured write_policy"));
        assert!(block.contains("execute historical next_action values"));
        assert!(block.contains("the conflicting memory MUST be ignored"));

        // Legacy projections remain non-authoritative (Test 6).
        assert!(block.contains("workflow_assignment.json"));
        assert!(block.contains("non-authoritative projections"));
    }

    #[test]
    fn native_authority_block_marks_empty_remaining_work_so_history_cannot_reopen_it() {
        // Test 3: even when memory contains unfinished work, the
        // current native authority block must not present historical
        // work as part of the current scope. The directive is
        // enforced by the rendered "reopen historical unfinished
        // work" prohibition.
        let scope = Some(NativeScopePolicy::default());
        let metadata = metadata_for_authority(scope, "phase 641f native work A");
        let block = render_native_workflow_authority(&metadata);

        assert!(block.contains("reopen historical unfinished work"));
        assert!(block.contains("remaining_work is now empty"));
        assert!(block.contains("historical next_action values"));
    }

    #[test]
    fn read_only_policy_renders_write_directive_for_deterministic_denial() {
        // Test 4 / Test 5: the rendered block must announce the
        // READ_ONLY directive so the agent cannot claim the policy
        // is missing. The ACP tool-permission gate does the
        // deterministic denial; the prompt block is the
        // defense-in-depth layer.
        let scope = Some(NativeScopePolicy {
            scope_mode: "BOUNDED".into(),
            write_policy: NativeScopePolicy::READ_ONLY.into(),
            historical_context_policy: "ADVISORY_ONLY".into(),
        });
        let metadata = metadata_for_authority(scope, "phase 641f native work A");
        let block = render_native_workflow_authority(&metadata);

        assert!(block.contains("NATIVE SCOPE (structured)"));
        assert!(block.contains("write_policy: READ_ONLY"));
        assert!(block.contains("WRITE POLICY DIRECTIVE"));
        // The deny-list is rendered across a soft-wrap newline so the
        // full list is still in the block. Match the exact substring
        // that appears in the rendering, including the newline.
        assert!(
            block.contains("Edit, Write,\nNotebookEdit, MultiEdit, apply_patch, or Bash"),
            "authority block must enumerate the full deny-list verbatim"
        );
        assert!(block.contains("Deterministic denial is"));
        assert!(block.contains("session/request_permission"));

        // Test 5: MODIFY_ALLOWED renders a different directive.
        let modify_metadata = metadata_for_authority(Some(NativeScopePolicy::default()), "x");
        let modify_block = render_native_workflow_authority(&modify_metadata);
        assert!(modify_block.contains("write_policy: MODIFY_ALLOWED"));
        assert!(modify_block.contains("Mutation is"));
        assert!(!modify_block.contains("Deterministic denial is"));
    }

    #[test]
    fn unset_scope_policy_renders_backward_compat_sentinel() {
        // Test 8: legacy callers (no scope_policy) get the explicit
        // "<unset>" sentinel text so the agent can audit the
        // missing policy and the ACP gate stays disabled. This
        // preserves the pre-6.4.1F behavior for callers that have
        // not yet migrated.
        let metadata = metadata_for_authority(None, "legacy dispatch");
        let block = render_native_workflow_authority(&metadata);

        assert!(block.contains("scope_mode: <unset — legacy caller>"));
        assert!(block.contains("write_policy: <unset — defaults to MODIFY_ALLOWED"));
        assert!(block.contains("historical_context_policy: <unset — defaults to ADVISORY_ONLY>"));
        assert!(block.contains("The ACP tool-permission gate is NOT active for this"));
    }

    #[test]
    fn native_scope_policy_default_is_modify_allowed() {
        // Backward-compat: Default::default() must produce
        // MODIFY_ALLOWED so legacy code paths that build a
        // NativeWorkflowMetadata struct-literal with no policy do
        // NOT silently become READ_ONLY.
        let p = NativeScopePolicy::default();
        assert_eq!(p.scope_mode, "BOUNDED");
        assert_eq!(p.write_policy, NativeScopePolicy::MODIFY_ALLOWED);
        assert_eq!(p.historical_context_policy, "ADVISORY_ONLY");
        assert!(!p.is_read_only());
    }

    #[test]
    fn native_scope_policy_read_only_constant_is_correct() {
        let p = NativeScopePolicy {
            scope_mode: "BOUNDED".into(),
            write_policy: NativeScopePolicy::READ_ONLY.into(),
            historical_context_policy: "ADVISORY_ONLY".into(),
        };
        assert!(p.is_read_only());
        assert_eq!(p.write_policy, "READ_ONLY");
    }
}
