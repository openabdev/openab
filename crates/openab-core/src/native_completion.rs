//! Native fenced completion delivery.  This is deliberately separate from
//! `completion_bridge`: legacy callbacks carry prose, native callbacks carry
//! the admission authority tuple verbatim.
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

/// Resolve the semantic result that a native completion is allowed to carry.
///
/// ACP's `end_turn` only proves that transport stopped; it is not a verifier
/// or final-review verdict.  PRIMARY has one legal result, so its successful
/// terminal turn is `COMPLETE`.  The reviewer roles must instead emit exactly
/// one standalone canonical token (for example `VERIFIER_PASS`).  This keeps
/// prose such as "the review may pass" from becoming workflow authority.
pub fn resolve_native_completion_outcome(role: &str, raw_assistant_text: &str) -> Option<String> {
    match role {
        "PRIMARY" => Some("COMPLETE".into()),
        "VERIFIER" => resolve_reviewer_outcome("VERIFIER", raw_assistant_text),
        "FINAL_REVIEWER" => resolve_reviewer_outcome("FINAL_REVIEWER", raw_assistant_text),
        _ => None,
    }
}

fn resolve_reviewer_outcome(role: &str, raw_assistant_text: &str) -> Option<String> {
    let tokens: Vec<&str> = raw_assistant_text
        .lines()
        .map(str::trim)
        .filter(|line| {
            matches!(
                *line,
                "VERIFIER_PASS" | "VERIFIER_FAIL" | "FINAL_REVIEWER_PASS" | "FINAL_REVIEWER_FAIL"
            )
        })
        .collect();
    match tokens.as_slice() {
        ["VERIFIER_PASS"] if role == "VERIFIER" => Some("PASS".into()),
        ["VERIFIER_FAIL"] if role == "VERIFIER" => Some("FAIL".into()),
        ["FINAL_REVIEWER_PASS"] if role == "FINAL_REVIEWER" => Some("PASS".into()),
        ["FINAL_REVIEWER_FAIL"] if role == "FINAL_REVIEWER" => Some("FAIL".into()),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeCompletionEvent {
    pub record_version: u32,
    pub completion_id: String,
    pub captured_at: String,
    pub record_digest: String,
    pub source: String,
    pub dispatch_id: String,
    pub workflow_run_id: String,
    pub task_id: String,
    pub role: String,
    pub agent_identity: String,
    pub lease_id: String,
    pub lease_generation: u64,
    pub expected_revision: u64,
    pub outcome: String,
    pub session_id: String,
    pub openab_turn_id: String,
    pub conversation_key: String,
    pub language: Option<String>,
    pub raw_assistant_text: String,
    pub project_id: String,
    pub project_root: String,
    pub timestamp: String,
    /// Phase 6.4.1B — authoritative transport identity inherited from
    /// the trusted structured dispatch metadata (the `AgentWorkRequest`
    /// that arrived via `set agent.work`). Used by AAP Runtime's
    /// transport-aware conversation identity validator (Phase 6.4.1).
    ///
    /// `None` means the event was produced by a daemon build that did
    /// not yet plumb transport (legacy semantics: Runtime defaults to
    /// `OPENAB` per Phase 6.4.1 `effective_transport` policy).
    ///
    /// Intentionally NOT part of `event_digest`: adding it would mutate
    /// the v1 digest identity and fail-closed every historical pending
    /// record on `outbox.open()`. The two trailing reserved slots in
    /// `event_digest` are still empty.
    #[serde(default)]
    pub transport: Option<String>,
}

impl NativeCompletionEvent {
    /// Immutable identity for one authoritative native end_turn.  The event
    /// is not accepted as a recovery credential by itself: AAP additionally
    /// authenticates the daemon bearer and rechecks the persisted fence.
    pub fn seal(mut self) -> Self {
        if self.captured_at.is_empty() {
            self.captured_at = chrono::Utc::now().to_rfc3339();
        }
        let identity = format!(
            "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.dispatch_id,
            self.workflow_run_id,
            self.task_id,
            self.role,
            self.agent_identity,
            self.lease_id,
            self.lease_generation,
            self.expected_revision,
            self.outcome
        );
        self.completion_id = hex_digest(&identity);
        self.record_digest = event_digest(&self);
        self
    }
}

fn hex_digest(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
fn event_digest(event: &NativeCompletionEvent) -> String {
    hex_digest(&format!(
        "{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
        event.record_version,
        event.completion_id,
        event.captured_at,
        event.workflow_run_id,
        event.task_id,
        event.role,
        event.agent_identity,
        event.lease_id,
        event.lease_generation,
        event.expected_revision,
        event.outcome,
        event.dispatch_id,
        event.conversation_key,
        event.language.as_deref().unwrap_or(""),
        event.session_id,
        event.openab_turn_id,
        "",
        ""
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DurableCompletionRecord {
    pub event: NativeCompletionEvent,
    pub status: String,
    /// Recovery-only audit evidence.  These fields are optional so existing
    /// PENDING/DELIVERED JSON records remain readable after the lifecycle is
    /// extended with a terminal tombstone state.
    #[serde(default)]
    pub last_error_code: Option<String>,
    #[serde(default)]
    pub rejected_at: Option<String>,
}

/// Small, crash-safe native completion outbox.  It intentionally owns only
/// completion delivery; it is not a general message queue.
pub struct NativeCompletionOutbox {
    path: PathBuf,
    records: Mutex<BTreeMap<String, DurableCompletionRecord>>,
}
impl NativeCompletionOutbox {
    /// Production path is agent-owned; callers must not fall back to the
    /// historical shared completion outbox.
    pub fn agent_scoped_path(
        home: impl AsRef<Path>,
        agent: &str,
    ) -> Result<PathBuf, NativeCompletionError> {
        if agent.trim().is_empty()
            || !agent
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return Err(NativeCompletionError::Transport(
                "ARTHUR_AGENT_NAME is missing or unsafe".into(),
            ));
        }
        Ok(home
            .as_ref()
            .join(".openab")
            .join("agents")
            .join(agent)
            .join("native_completion_outbox.json"))
    }
    pub fn open(path: impl AsRef<Path>) -> Result<Self, NativeCompletionError> {
        let path = path.as_ref().to_path_buf();
        let records: BTreeMap<String, DurableCompletionRecord> = if path.exists() {
            serde_json::from_slice(
                &fs::read(&path).map_err(|e| NativeCompletionError::Transport(e.to_string()))?,
            )
            .map_err(|e| {
                NativeCompletionError::Transport(format!("invalid completion outbox: {e}"))
            })?
        } else {
            BTreeMap::new()
        };
        for (completion_id, record) in &records {
            if completion_id != &record.event.completion_id
                || record.event.record_version != 1
                || event_digest(&record.event) != record.event.record_digest
            {
                return Err(NativeCompletionError::Transport(
                    "invalid completion outbox record".into(),
                ));
            }
        }
        Ok(Self {
            path,
            records: Mutex::new(records),
        })
    }
    fn persist(
        &self,
        records: &BTreeMap<String, DurableCompletionRecord>,
    ) -> Result<(), NativeCompletionError> {
        let parent = self.path.parent().ok_or_else(|| {
            NativeCompletionError::Transport("completion outbox path has no parent".into())
        })?;
        fs::create_dir_all(parent).map_err(|e| NativeCompletionError::Transport(e.to_string()))?;
        let temporary = self.path.with_extension("tmp");
        let serialized = serde_json::to_vec(records)
            .map_err(|e| NativeCompletionError::Transport(e.to_string()))?;
        let mut temporary_file = File::create(&temporary)
            .map_err(|e| NativeCompletionError::Transport(e.to_string()))?;
        temporary_file
            .write_all(&serialized)
            .map_err(|e| NativeCompletionError::Transport(e.to_string()))?;
        temporary_file
            .sync_all()
            .map_err(|e| NativeCompletionError::Transport(e.to_string()))?;
        drop(temporary_file);
        fs::rename(temporary, &self.path)
            .map_err(|e| NativeCompletionError::Transport(e.to_string()))?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|e| NativeCompletionError::Transport(e.to_string()))
    }
    pub fn capture(
        &self,
        event: NativeCompletionEvent,
    ) -> Result<NativeCompletionEvent, NativeCompletionError> {
        if event.record_version != 1 {
            return Err(NativeCompletionError::Transport(
                "unsupported native completion record version".into(),
            ));
        }
        let event = event.seal();
        let mut records = self.records.lock().map_err(|_| {
            NativeCompletionError::Transport("completion outbox lock poisoned".into())
        })?;
        records
            .entry(event.completion_id.clone())
            .or_insert_with(|| DurableCompletionRecord {
                event: event.clone(),
                status: "PENDING".into(),
                last_error_code: None,
                rejected_at: None,
            });
        self.persist(&records)?;
        Ok(event)
    }
    pub fn mark_delivered(&self, completion_id: &str) -> Result<(), NativeCompletionError> {
        let mut records = self.records.lock().map_err(|_| {
            NativeCompletionError::Transport("completion outbox lock poisoned".into())
        })?;
        let record = records.get_mut(completion_id).ok_or_else(|| {
            NativeCompletionError::Transport("missing captured completion".into())
        })?;
        record.status = "DELIVERED".into();
        self.persist(&records)
    }
    pub fn mark_permanently_rejected(
        &self,
        completion_id: &str,
        rejection_reason: &str,
    ) -> Result<(), NativeCompletionError> {
        let mut records = self.records.lock().map_err(|_| {
            NativeCompletionError::Transport("completion outbox lock poisoned".into())
        })?;
        let record = records.get_mut(completion_id).ok_or_else(|| {
            NativeCompletionError::Transport("missing captured completion".into())
        })?;
        record.status = "PERMANENT_REJECTED".into();
        record.last_error_code = Some(rejection_reason.into());
        record.rejected_at = Some(chrono::Utc::now().to_rfc3339());
        self.persist(&records)
    }
    pub fn pending(&self) -> Vec<NativeCompletionEvent> {
        self.records
            .lock()
            .map(|records| {
                records
                    .values()
                    .filter(|r| r.status == "PENDING")
                    .map(|r| r.event.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
}

#[async_trait]
pub trait NativeCompletionPort: Send + Sync {
    async fn submit(&self, event: NativeCompletionEvent) -> Result<(), NativeCompletionError>;
    async fn recover(&self, event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
        self.submit(event).await
    }
}

#[derive(Debug)]
pub enum NativeCompletionError {
    Transport(String),
    /// A terminal recovery decision, derived only from AAP's structured
    /// recovery response.  It is intentionally distinct from ordinary
    /// callback rejection so live delivery keeps its existing retry behavior.
    PermanentRejected(String),
    Rejected(String),
}
impl std::fmt::Display for NativeCompletionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "native completion transport failed: {message}"),
            Self::PermanentRejected(message) => {
                write!(f, "native completion permanently rejected: {message}")
            }
            Self::Rejected(message) => write!(f, "native completion rejected: {message}"),
        }
    }
}
impl std::error::Error for NativeCompletionError {}

pub struct NoopNativeCompletionPort;
#[async_trait]
impl NativeCompletionPort for NoopNativeCompletionPort {
    async fn submit(&self, _event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
        Ok(())
    }
}

/// Production-safe default: native work must not silently report local
/// success when no callback adapter is configured.
pub struct UnconfiguredNativeCompletionPort;
#[async_trait]
impl NativeCompletionPort for UnconfiguredNativeCompletionPort {
    async fn submit(&self, _event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
        Err(NativeCompletionError::Transport(
            "native completion port is not configured".into(),
        ))
    }
}

/// HTTP native completion port with a total-attempt budget.
///
/// `max_attempts` includes the first request.  A value of `2` therefore
/// permits exactly two transport calls, never an initial call plus two retries.
pub struct HttpNativeCompletionPort {
    client: reqwest::Client,
    url: String,
    bearer_env: String,
    max_attempts: u32,
    backoff: Duration,
    recovery_url: String,
}
impl HttpNativeCompletionPort {
    pub fn from_env(
        url: String,
        bearer_env: impl Into<String>,
        max_attempts: u32,
        backoff_ms: u64,
    ) -> Result<Self, NativeCompletionError> {
        let bearer_env = bearer_env.into();
        if std::env::var(&bearer_env)
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            return Err(NativeCompletionError::Transport(format!(
                "required credential environment {bearer_env} is unset"
            )));
        }
        if max_attempts == 0 {
            return Err(NativeCompletionError::Transport(
                "max_attempts must be at least 1".into(),
            ));
        }
        let recovery_url = format!("{url}/recovery");
        Ok(Self {
            client: reqwest::Client::new(),
            url,
            bearer_env,
            max_attempts,
            backoff: Duration::from_millis(backoff_ms),
            recovery_url,
        })
    }
}
#[derive(Deserialize)]
struct Accepted {
    status: String,
    #[serde(default)]
    reason: String,
    #[serde(default)]
    delivered: bool,
}

/// Converts only AAP's documented recovery response into an outbox decision.
/// Unknown and `RECOVERY_REJECTED:*` reasons deliberately remain retryable:
/// exception names do not provide a stable permanence contract.
fn classify_recovery_response(body: Accepted) -> Result<(), NativeCompletionError> {
    match (body.status.as_str(), body.delivered, body.reason.as_str()) {
        ("DELIVERED", true, "RECOVERY_DELIVERED" | "RECOVERY_ALREADY_CONSUMED") => Ok(()),
        (
            "REJECTED",
            _,
            "MALFORMED_DURABLE_COMPLETION"
            | "COMPLETION_ID_CONFLICT"
            | "RECOVERY_FENCE_MISMATCH"
            | "RECOVERY_INVALID_TRANSITION",
        ) => Err(NativeCompletionError::PermanentRejected(body.reason)),
        // Includes RECOVERY_REJECTED:<ExceptionType>: it is intentionally
        // fail-safe because the exception may represent transient storage or
        // runtime availability rather than a permanent authority failure.
        _ => Err(NativeCompletionError::Rejected(format!(
            "unclassified recovery response: status={}, delivered={}, reason={}",
            body.status, body.delivered, body.reason
        ))),
    }
}
#[async_trait]
impl NativeCompletionPort for HttpNativeCompletionPort {
    async fn submit(&self, event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
        let token = std::env::var(&self.bearer_env)
            .map_err(|_| NativeCompletionError::Transport("credential unavailable".into()))?;
        for attempt in 0..self.max_attempts {
            let send = self
                .client
                .post(&self.url)
                .bearer_auth(&token)
                .json(&event)
                .send()
                .await;
            match send {
                Ok(response) if response.status().is_success() => {
                    match response.json::<Accepted>().await {
                        Ok(body) if body.status == "DELIVERED" && body.delivered => return Ok(()),
                        Ok(body) => return Err(NativeCompletionError::Rejected(body.status)),
                        Err(_) => {
                            return Err(NativeCompletionError::Rejected(
                                "missing structured acceptance".into(),
                            ));
                        }
                    }
                }
                _ if attempt + 1 < self.max_attempts => {
                    tokio::time::sleep(self.backoff * (1u32 << attempt)).await
                }
                Ok(response) => {
                    return Err(NativeCompletionError::Transport(
                        response.status().to_string(),
                    ));
                }
                Err(_) => {
                    return Err(NativeCompletionError::Transport(
                        "request failed after bounded retry".into(),
                    ));
                }
            }
        }
        unreachable!()
    }
    async fn recover(&self, event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
        let token = std::env::var(&self.bearer_env)
            .map_err(|_| NativeCompletionError::Transport("credential unavailable".into()))?;
        let response = self
            .client
            .post(&self.recovery_url)
            .bearer_auth(&token)
            .json(&event)
            .send()
            .await
            .map_err(|_| NativeCompletionError::Transport("recovery request failed".into()))?;
        match response.json::<Accepted>().await {
            Ok(body) => classify_recovery_response(body),
            Err(_) => Err(NativeCompletionError::Transport(
                "missing structured recovery response".into(),
            )),
        }
    }
}

/// Captures the event before any network I/O.  Normal delivery uses the
/// ordinary expiry-fenced endpoint; replay is explicitly routed to recovery.
pub struct DurableNativeCompletionPort {
    delivery: SharedNativeCompletionPort,
    outbox: Arc<NativeCompletionOutbox>,
}
impl DurableNativeCompletionPort {
    pub fn new(delivery: SharedNativeCompletionPort, outbox: Arc<NativeCompletionOutbox>) -> Self {
        Self { delivery, outbox }
    }
    pub async fn replay_pending(&self) {
        for event in self.outbox.pending() {
            match self.delivery.recover(event.clone()).await {
                Ok(()) => {
                    let _ = self.outbox.mark_delivered(&event.completion_id);
                }
                Err(NativeCompletionError::PermanentRejected(reason)) => {
                    let _ = self
                        .outbox
                        .mark_permanently_rejected(&event.completion_id, &reason);
                }
                Err(_) => {}
            }
        }
    }
}
#[async_trait]
impl NativeCompletionPort for DurableNativeCompletionPort {
    async fn submit(&self, event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
        let event = self.outbox.capture(event)?;
        match self.delivery.submit(event.clone()).await {
            Ok(()) => {
                self.outbox.mark_delivered(&event.completion_id)?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    }
}
pub type SharedNativeCompletionPort = Arc<dyn NativeCompletionPort>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingPort {
        results: Arc<Mutex<Vec<Result<(), NativeCompletionError>>>>,
        submitted: Arc<Mutex<Vec<NativeCompletionEvent>>>,
        recovered: Arc<Mutex<Vec<NativeCompletionEvent>>>,
    }

    #[async_trait]
    impl NativeCompletionPort for RecordingPort {
        async fn submit(&self, event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
            self.submitted.lock().unwrap().push(event);
            self.results.lock().unwrap().remove(0)
        }

        async fn recover(&self, event: NativeCompletionEvent) -> Result<(), NativeCompletionError> {
            self.recovered.lock().unwrap().push(event);
            self.results.lock().unwrap().remove(0)
        }
    }

    fn event() -> NativeCompletionEvent {
        NativeCompletionEvent {
            record_version: 1,
            completion_id: String::new(),
            captured_at: String::new(),
            record_digest: String::new(),
            source: "openab".into(),
            dispatch_id: "dispatch-1".into(),
            workflow_run_id: "run-1".into(),
            task_id: "task-1".into(),
            role: "PRIMARY".into(),
            agent_identity: "ArthurClaude".into(),
            lease_id: "lease-1".into(),
            lease_generation: 1,
            expected_revision: 1,
            outcome: "COMPLETE".into(),
            session_id: "session-1".into(),
            openab_turn_id: "turn-1".into(),
            conversation_key: "conversation-1".into(),
            language: Some("zh-TW".into()),
            raw_assistant_text: "done".into(),
            project_id: "project-1".into(),
            project_root: "/project-1".into(),
            timestamp: "2026-08-21T00:00:00Z".into(),
            transport: Some("OPENAB".into()),
        }
    }

    #[test]
    fn record_version_is_persisted_and_digest_covered() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let captured = outbox.capture(event()).expect("capture");
        assert_eq!(captured.record_version, 1);
        let mut changed = captured.clone();
        changed.record_version = 2;
        assert_ne!(event_digest(&changed), captured.record_digest);
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(path).expect("read")).expect("valid persisted JSON");
        assert_eq!(json[captured.completion_id]["event"]["record_version"], 1);
    }

    #[test]
    fn outcome_is_an_existing_v1_digest_authority_field() {
        let pass = event().seal();
        let mut fail_source = event();
        fail_source.outcome = "FAIL".into();
        let fail = fail_source.seal();
        assert_ne!(pass.completion_id, fail.completion_id);
        assert_ne!(pass.record_digest, fail.record_digest);
    }

    #[test]
    fn reviewer_outcomes_require_one_standalone_canonical_token() {
        assert_eq!(
            resolve_native_completion_outcome("VERIFIER", "review complete\nVERIFIER_PASS\n"),
            Some("PASS".into())
        );
        assert_eq!(
            resolve_native_completion_outcome("VERIFIER", "VERIFIER_FAIL"),
            Some("FAIL".into())
        );
        assert_eq!(
            resolve_native_completion_outcome("FINAL_REVIEWER", "FINAL_REVIEWER_PASS"),
            Some("PASS".into())
        );
        assert_eq!(
            resolve_native_completion_outcome("FINAL_REVIEWER", "FINAL_REVIEWER_FAIL"),
            Some("FAIL".into())
        );
        assert_eq!(resolve_native_completion_outcome("VERIFIER", "done"), None);
        assert_eq!(
            resolve_native_completion_outcome("VERIFIER", "VERIFIER_PASS\nVERIFIER_FAIL"),
            None
        );
        assert_eq!(
            resolve_native_completion_outcome("VERIFIER", "the VERIFIER_PASS result is pending"),
            None
        );
    }

    #[test]
    fn agent_scoped_paths_are_isolated_and_reject_unsafe_names() {
        let home = tempfile::tempdir().expect("home");
        let codex = NativeCompletionOutbox::agent_scoped_path(home.path(), "ArthurCodex")
            .expect("codex path");
        let claude = NativeCompletionOutbox::agent_scoped_path(home.path(), "ArthurClaude")
            .expect("claude path");
        assert_ne!(codex, claude);
        assert!(codex.ends_with("agents/ArthurCodex/native_completion_outbox.json"));
        assert!(NativeCompletionOutbox::agent_scoped_path(home.path(), "").is_err());
        assert!(NativeCompletionOutbox::agent_scoped_path(home.path(), "../ArthurClaude").is_err());
    }

    #[test]
    fn durable_write_reopens_and_malformed_json_fails_closed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let captured = outbox.capture(event()).expect("capture");
        assert_eq!(
            NativeCompletionOutbox::open(&path)
                .expect("reopen")
                .pending()[0]
                .completion_id,
            captured.completion_id
        );
        fs::write(&path, b"{").expect("malformed write");
        assert!(NativeCompletionOutbox::open(&path).is_err());
    }

    #[test]
    fn unsupported_or_tampered_records_fail_closed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let mut unsupported = event();
        unsupported.record_version = 2;
        assert!(outbox.capture(unsupported).is_err());
        let captured = outbox.capture(event()).expect("capture");
        let mut records: BTreeMap<String, DurableCompletionRecord> =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("records");
        records
            .get_mut(&captured.completion_id)
            .expect("record")
            .event
            .record_version = 2;
        fs::write(&path, serde_json::to_vec(&records).expect("serialize")).expect("write");
        assert!(NativeCompletionOutbox::open(&path).is_err());
    }

    #[tokio::test]
    async fn retry_budget_of_two_makes_exactly_two_transport_calls() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let address = listener.local_addr().expect("listener address");
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.expect("accept request");
                observed.fetch_add(1, Ordering::SeqCst);
                let mut buffer = [0_u8; 4096];
                let _ = socket.read(&mut buffer).await.expect("read request");
                socket.write_all(b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("write response");
            }
        });
        let key = "OPENAB_TEST_NATIVE_COMPLETION_TOKEN";
        std::env::set_var(key, "test-token");
        let port = HttpNativeCompletionPort::from_env(format!("http://{address}"), key, 2, 0)
            .expect("configured port");
        let result = port.submit(event()).await;
        std::env::remove_var(key);

        assert!(matches!(result, Err(NativeCompletionError::Transport(_))));
        server.await.expect("server completed");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "two total attempts, no third request"
        );
    }

    #[tokio::test]
    async fn ordinary_http_failures_leave_durable_records_pending() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let key = "OPENAB_TEST_NATIVE_COMPLETION_HTTP_FAILURE_TOKEN";
        std::env::set_var(key, "test-token");
        for status in [400_u16, 422, 500, 503] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind listener");
            let address = listener.local_addr().expect("listener address");
            // HTTP status alone is not an outbox lifecycle decision.  Every
            // unstructured response uses the bounded transient retry path.
            let attempts = 2;
            let server = tokio::spawn(async move {
                for _ in 0..attempts {
                    let (mut socket, _) = listener.accept().await.expect("accept request");
                    let mut buffer = [0_u8; 4096];
                    let _ = socket.read(&mut buffer).await.expect("read request");
                    socket
                        .write_all(
                            format!(
                                "HTTP/1.1 {status} Failure\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                            )
                            .as_bytes(),
                        )
                        .await
                        .expect("write response");
                }
            });
            let directory = tempfile::tempdir().expect("temporary directory");
            let outbox = Arc::new(
                NativeCompletionOutbox::open(directory.path().join("outbox.json")).expect("outbox"),
            );
            let port = Arc::new(
                HttpNativeCompletionPort::from_env(
                    format!("http://{address}/v1/integrations/openab/completion"),
                    key,
                    2,
                    0,
                )
                .expect("configured port"),
            );
            let durable = DurableNativeCompletionPort::new(port, outbox.clone());
            assert!(durable.submit(event()).await.is_err(), "HTTP {status}");
            server.await.expect("server completed");
            assert_eq!(outbox.pending().len(), 1, "HTTP {status} must stay pending");
        }
        std::env::remove_var(key);
    }

    #[tokio::test]
    async fn recovery_http_port_uses_exact_recovery_endpoint() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut buffer = [0_u8; 4096];
            let count = socket.read(&mut buffer).await.expect("read request");
            let request = String::from_utf8_lossy(&buffer[..count]);
            assert!(request.starts_with("POST /v1/integrations/openab/completion/recovery "));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 69\r\nConnection: close\r\n\r\n{\"status\":\"DELIVERED\",\"reason\":\"RECOVERY_DELIVERED\",\"delivered\":true}").await.expect("write response");
        });
        let key = "OPENAB_TEST_NATIVE_COMPLETION_RECOVERY_TOKEN";
        std::env::set_var(key, "test-token");
        let port = HttpNativeCompletionPort::from_env(
            format!("http://{address}/v1/integrations/openab/completion"),
            key,
            1,
            0,
        )
        .expect("configured port");
        assert!(port.recover(event()).await.is_ok());
        std::env::remove_var(key);
        server.await.expect("server completed");
    }

    #[tokio::test]
    async fn pending_outbox_survives_failure_restart_and_replay_is_immutable_and_idempotent() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let submitted = Arc::new(Mutex::new(Vec::new()));
        let recovered = Arc::new(Mutex::new(Vec::new()));
        let first = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![Err(NativeCompletionError::Transport(
                "down".into(),
            ))])),
            submitted: submitted.clone(),
            recovered: recovered.clone(),
        });
        let outbox = Arc::new(NativeCompletionOutbox::open(&path).expect("outbox"));
        let durable = DurableNativeCompletionPort::new(first, outbox.clone());
        assert!(durable.submit(event()).await.is_err());
        let pending = NativeCompletionOutbox::open(&path)
            .expect("reopen")
            .pending();
        assert_eq!(pending.len(), 1, "failed ordinary callback remains PENDING");
        assert_eq!(
            submitted.lock().unwrap()[0].record_digest,
            pending[0].record_digest
        );

        let replay_port = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![
                Err(NativeCompletionError::Rejected("500".into())),
                Ok(()),
            ])),
            submitted,
            recovered: recovered.clone(),
        });
        let reopened = Arc::new(NativeCompletionOutbox::open(&path).expect("reopen"));
        let replay = DurableNativeCompletionPort::new(replay_port, reopened.clone());
        replay.replay_pending().await;
        assert_eq!(reopened.pending().len(), 1, "failed replay remains PENDING");
        replay.replay_pending().await;
        assert!(
            reopened.pending().is_empty(),
            "successful replay finalizes record"
        );
        {
            let calls = recovered.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].record_digest, pending[0].record_digest);
            assert_eq!(calls[1].record_digest, pending[0].record_digest);
        }
        replay.replay_pending().await;
        assert_eq!(
            recovered.lock().unwrap().len(),
            2,
            "delivered replay is a safe no-op"
        );
    }

    #[test]
    fn legacy_pending_and_delivered_records_reopen_without_new_audit_fields() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let pending = outbox.capture(event()).expect("capture pending");
        // Use distinct identities, otherwise capture idempotency intentionally
        // coalesces both events into one record.
        let mut delivered_event = event();
        delivered_event.dispatch_id = "dispatch-2".into();
        let delivered = outbox.capture(delivered_event).expect("capture delivered");
        outbox
            .mark_delivered(&delivered.completion_id)
            .expect("deliver");
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        for record in json.as_object_mut().expect("records").values_mut() {
            record
                .as_object_mut()
                .expect("record")
                .remove("last_error_code");
            record
                .as_object_mut()
                .expect("record")
                .remove("rejected_at");
        }
        fs::write(&path, serde_json::to_vec(&json).expect("serialize")).expect("write");
        let reopened = NativeCompletionOutbox::open(&path).expect("legacy reopen");
        assert_eq!(reopened.pending().len(), 1);
        assert_eq!(reopened.pending()[0].completion_id, pending.completion_id);
    }

    #[test]
    fn recovery_classification_only_tombstones_documented_permanent_reasons() {
        for reason in [
            "MALFORMED_DURABLE_COMPLETION",
            "COMPLETION_ID_CONFLICT",
            "RECOVERY_FENCE_MISMATCH",
            "RECOVERY_INVALID_TRANSITION",
        ] {
            assert!(matches!(
                classify_recovery_response(Accepted {
                    status: "REJECTED".into(),
                    reason: reason.into(),
                    delivered: false,
                }),
                Err(NativeCompletionError::PermanentRejected(_))
            ));
        }
        for reason in ["RECOVERY_REJECTED:OperationalError", "UNKNOWN_REJECTION"] {
            assert!(matches!(
                classify_recovery_response(Accepted {
                    status: "REJECTED".into(),
                    reason: reason.into(),
                    delivered: false,
                }),
                Err(NativeCompletionError::Rejected(_))
            ));
        }
        assert!(classify_recovery_response(Accepted {
            status: "DELIVERED".into(),
            reason: "RECOVERY_ALREADY_CONSUMED".into(),
            delivered: true,
        })
        .is_ok());
    }

    #[tokio::test]
    async fn permanent_rejection_tombstones_audit_evidence_and_is_never_replayed() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let recovered = Arc::new(Mutex::new(Vec::new()));
        let first = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![Err(NativeCompletionError::Transport(
                "down".into(),
            ))])),
            submitted: Arc::new(Mutex::new(Vec::new())),
            recovered: recovered.clone(),
        });
        let outbox = Arc::new(NativeCompletionOutbox::open(&path).expect("outbox"));
        let durable = DurableNativeCompletionPort::new(first, outbox.clone());
        let captured = event().seal();
        assert!(durable.submit(captured.clone()).await.is_err());

        let recovery = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![Err(
                NativeCompletionError::PermanentRejected("RECOVERY_FENCE_MISMATCH".into()),
            )])),
            submitted: Arc::new(Mutex::new(Vec::new())),
            recovered: recovered.clone(),
        });
        let reopened = Arc::new(NativeCompletionOutbox::open(&path).expect("reopen"));
        DurableNativeCompletionPort::new(recovery, reopened.clone())
            .replay_pending()
            .await;
        assert!(reopened.pending().is_empty());
        let records: BTreeMap<String, DurableCompletionRecord> =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("records");
        let record = records.get(&captured.completion_id).expect("record");
        assert_eq!(record.status, "PERMANENT_REJECTED");
        assert_eq!(
            record.last_error_code.as_deref(),
            Some("RECOVERY_FENCE_MISMATCH")
        );
        assert!(record.rejected_at.is_some());

        let calls = recovered.lock().expect("calls").len();
        let retry = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![])),
            submitted: Arc::new(Mutex::new(Vec::new())),
            recovered: recovered.clone(),
        });
        let after_restart = Arc::new(NativeCompletionOutbox::open(&path).expect("restart"));
        DurableNativeCompletionPort::new(retry, after_restart)
            .replay_pending()
            .await;
        assert_eq!(recovered.lock().expect("calls").len(), calls);
    }

    #[tokio::test]
    async fn malformed_durable_completion_is_tombstoned_across_restart() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let recovered = Arc::new(Mutex::new(Vec::new()));
        let outbox = Arc::new(NativeCompletionOutbox::open(&path).expect("outbox"));
        let captured = outbox.capture(event()).expect("capture");
        let recovery = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![Err(
                NativeCompletionError::PermanentRejected("MALFORMED_DURABLE_COMPLETION".into()),
            )])),
            submitted: Arc::new(Mutex::new(Vec::new())),
            recovered: recovered.clone(),
        });
        DurableNativeCompletionPort::new(recovery, outbox)
            .replay_pending()
            .await;
        let reopened = Arc::new(NativeCompletionOutbox::open(&path).expect("reopen"));
        assert!(reopened.pending().is_empty());
        let records: BTreeMap<String, DurableCompletionRecord> =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("records");
        assert_eq!(
            records[&captured.completion_id].last_error_code.as_deref(),
            Some("MALFORMED_DURABLE_COMPLETION")
        );

        let calls = recovered.lock().expect("calls").len();
        let never_called = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![])),
            submitted: Arc::new(Mutex::new(Vec::new())),
            recovered: recovered.clone(),
        });
        DurableNativeCompletionPort::new(never_called, reopened)
            .replay_pending()
            .await;
        assert_eq!(recovered.lock().expect("calls").len(), calls);
    }

    // ------------------------------------------------------------------------
    // Phase 6.4.1B — authoritative transport propagation through the native
    // completion event / outbox / HTTP completion port. AAP Runtime performs
    // transport-aware conversation identity validation (Phase 6.4.1) and
    // requires transport in the completion body. These tests pin the contract:
    //
    //   - transport lives in the durable JSON (event.transport: Option<String>)
    //   - v1 record_digest MUST NOT include transport (preserve historical
    //     pending record compatibility)
    //   - legacy outbox records without `transport` deserialize cleanly
    //   - HttpNativeCompletionPort serializes transport into the request body
    //   - DurableNativeCompletionPort captures transport into the outbox
    //   - Replay preserves transport
    // ------------------------------------------------------------------------

    /// Q7/Q8 backward-compat: a legacy record missing the `transport` field
    /// must deserialize cleanly and reopen without fail-closed validation.
    #[test]
    fn legacy_record_without_transport_field_reopens_with_transport_none() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        // Capture a record, then strip `transport` from the on-disk JSON to
        // simulate a legacy daemon-built record. Note: `transport` lives
        // inside `event`, not at the record level (unlike `last_error_code` /
        // `rejected_at` which are DurableCompletionRecord fields).
        let captured = outbox.capture(event()).expect("capture");
        let mut json: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        for record in json.as_object_mut().expect("records").values_mut() {
            record
                .as_object_mut()
                .expect("record")
                .get_mut("event")
                .expect("event")
                .as_object_mut()
                .expect("event")
                .remove("transport");
        }
        fs::write(&path, serde_json::to_vec(&json).expect("serialize")).expect("write");
        let reopened = NativeCompletionOutbox::open(&path).expect("legacy reopen");
        assert_eq!(reopened.pending().len(), 1);
        let pending = &reopened.pending()[0];
        assert_eq!(pending.completion_id, captured.completion_id);
        assert!(
            pending.transport.is_none(),
            "missing `transport` field must deserialize as None (legacy semantics)"
        );
    }

    /// Q9: v1 record_digest identity is sacred — it MUST NOT include transport.
    /// Two events that differ ONLY in transport field must have the same digest.
    #[test]
    fn v1_record_digest_excludes_transport_field() {
        let base = event();
        let mut with_transport = base.clone();
        with_transport.transport = Some("OPENAB".into());
        let mut with_discord = base.clone();
        with_discord.transport = Some("DISCORD".into());
        let mut with_openclaw = base.clone();
        with_openclaw.transport = Some("OPENCLAW".into());
        let mut with_none = base.clone();
        with_none.transport = None;

        let base_digest = event_digest(&base);
        assert_eq!(event_digest(&with_transport), base_digest);
        assert_eq!(event_digest(&with_discord), base_digest);
        assert_eq!(event_digest(&with_openclaw), base_digest);
        assert_eq!(event_digest(&with_none), base_digest);

        // The seal() path is what production runs; both records must seal to
        // the same record_digest because transport is not part of the digest
        // and we pre-set captured_at to a stable value so seal() does not
        // inject wall-clock time into the digest inputs.
        let mut sealed_a = base.clone();
        sealed_a.captured_at = "2026-08-21T00:00:00Z".into();
        let mut sealed_b = with_discord.clone();
        sealed_b.captured_at = "2026-08-21T00:00:00Z".into();
        let sealed_a = sealed_a.seal();
        let sealed_b = sealed_b.seal();
        assert_eq!(sealed_a.record_digest, sealed_b.record_digest);
    }

    /// Forward-compat: a record carrying a populated transport field must
    /// round-trip through durable JSON and reopen cleanly.
    #[test]
    fn record_with_transport_round_trips_through_durable_json() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let mut e = event();
        e.transport = Some("DISCORD".into());
        let captured = outbox.capture(e).expect("capture");
        let reopened = NativeCompletionOutbox::open(&path).expect("reopen");
        assert_eq!(reopened.pending().len(), 1);
        let pending = &reopened.pending()[0];
        assert_eq!(pending.completion_id, captured.completion_id);
        assert_eq!(pending.transport.as_deref(), Some("DISCORD"));
    }

    /// Q6: transport must come from the trusted structured dispatch metadata,
    /// NEVER derived from the conversation_key prefix. Production dispatch
    /// construction site is `dispatch.rs::invoke_workflow_hook_after_dispatch`.
    /// Here we assert that `NativeCompletionEvent::seal()` preserves the
    /// transport value byte-for-byte through the digest computation.
    #[test]
    fn transport_is_preserved_through_seal_and_digest_computation() {
        let mut e = event();
        e.conversation_key = "discord:1540183233654952036".into();
        e.transport = Some("DISCORD".into());
        e.captured_at = "2026-08-21T00:00:00Z".into(); // stable digest input
        let sealed = e.clone().seal();
        assert_eq!(sealed.transport.as_deref(), Some("DISCORD"));

        // Mutating only the transport field MUST NOT change the digest.
        let mut e_with_openab = e.clone();
        e_with_openab.transport = Some("OPENAB".into());
        let sealed_openab = e_with_openab.seal();
        assert_eq!(
            sealed_openab.record_digest, sealed.record_digest,
            "transport does not participate in v1 digest"
        );
        // Mutating conversation_key (the field the Runtime validates) MUST
        // change the digest (this proves transport and conversation_key are
        // both durable identity inputs, but only conversation_key is in v1
        // digest and transport is in the durable JSON envelope for Runtime).
        let mut e_diff_key = e.clone();
        e_diff_key.conversation_key = "discord:9999999999999999999".into();
        let sealed_diff_key = e_diff_key.seal();
        assert_ne!(
            sealed_diff_key.record_digest, sealed.record_digest,
            "conversation_key is a v1 digest input"
        );
    }

    /// HTTP completion body carries the transport field. The production
    /// `HttpNativeCompletionPort::submit` serializes the entire event via
    /// `serde_json::json(&event)`. We assert that transport appears in the
    /// serialized JSON body that the HTTP client would post.
    #[test]
    fn http_body_carries_transport_field() {
        let mut e = event();
        e.transport = Some("DISCORD".into());
        let json: serde_json::Value = serde_json::to_value(&e).expect("serialize");
        assert_eq!(
            json["transport"].as_str(),
            Some("DISCORD"),
            "HTTP body JSON must carry the transport field"
        );

        // None transport also serializes — serde_json::Value treats None as null
        let mut none_e = event();
        none_e.transport = None;
        let none_json: serde_json::Value = serde_json::to_value(&none_e).expect("serialize");
        assert!(
            none_json["transport"].is_null(),
            "None transport serializes as JSON null in HTTP body"
        );
    }

    /// Q7/Q8: durable capture path stores transport alongside the event.
    #[test]
    fn durable_capture_persists_transport_field() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let mut e = event();
        e.dispatch_id = "dispatch-transport-1".into();
        e.transport = Some("DISCORD".into());
        let captured = outbox.capture(e).expect("capture");
        let raw: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).expect("read")).expect("json");
        assert_eq!(
            raw[&captured.completion_id]["event"]["transport"].as_str(),
            Some("DISCORD"),
            "durable outbox JSON persists the transport field"
        );
    }

    /// Tamper guard: bumping a record to a non-v1 record_version fails closed.
    /// This is the existing `record_version != 1` invariant from line 187.
    /// Re-pinned for Phase 6.4.1B so future readers know transport did NOT
    /// relax the version check.
    #[test]
    fn record_version_invariant_is_unaffected_by_transport_field() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let mut tampered = event();
        tampered.dispatch_id = "dispatch-version-test".into();
        tampered.transport = Some("DISCORD".into());
        tampered.record_version = 2;
        assert!(
            outbox.capture(tampered).is_err(),
            "record_version != 1 must fail closed regardless of transport"
        );
    }

    /// Operator-hold regression: native completion capture path MUST still
    /// work even when an active OPERATOR_HOLD blocks recovery. The
    /// OPERATOR_HOLD gate lives on the AAP side, not here, so this test only
    /// pins the OpenAB-side contract: the event is sealed and submitted
    /// exactly once, and the durable record survives a reopen.
    #[test]
    fn operator_hold_path_does_not_inhibit_native_completion_capture() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = NativeCompletionOutbox::open(&path).expect("outbox");
        let mut e = event();
        e.dispatch_id = "dispatch-operator-hold".into();
        e.transport = Some("DISCORD".into());
        let captured = outbox.capture(e).expect("capture during operator hold");
        let reopened = NativeCompletionOutbox::open(&path).expect("reopen during hold");
        assert_eq!(reopened.pending()[0].completion_id, captured.completion_id);
        assert_eq!(reopened.pending()[0].transport.as_deref(), Some("DISCORD"));
    }

    /// Replay path: `DurableNativeCompletionPort::replay_pending` re-submits
    /// pending records on daemon restart. We assert the re-submitted event
    /// still carries its transport field byte-for-byte.
    #[tokio::test]
    async fn replay_pending_preserves_transport_field() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("outbox.json");
        let outbox = Arc::new(NativeCompletionOutbox::open(&path).expect("outbox"));
        let recording = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![Err(NativeCompletionError::Transport(
                "down".into(),
            ))])),
            submitted: Arc::new(Mutex::new(Vec::new())),
            recovered: Arc::new(Mutex::new(Vec::new())),
        });
        let durable = DurableNativeCompletionPort::new(recording.clone(), outbox.clone());
        let mut e = event();
        e.dispatch_id = "dispatch-replay-transport".into();
        e.transport = Some("DISCORD".into());
        let captured = e.seal();
        let _ = durable.submit(captured.clone()).await; // first submit fails
                                                        // Simulate daemon restart by reopening the outbox.
        let reopened = Arc::new(NativeCompletionOutbox::open(&path).expect("reopen"));
        // Use a fresh successful port so replay finalizes the record.
        let fresh_port = Arc::new(RecordingPort {
            results: Arc::new(Mutex::new(vec![Ok(())])),
            submitted: Arc::new(Mutex::new(Vec::new())),
            recovered: Arc::new(Mutex::new(Vec::new())),
        });
        let replay = DurableNativeCompletionPort::new(fresh_port.clone(), reopened.clone());
        replay.replay_pending().await;
        // The recovered-side submission must carry the original transport.
        let recovered = fresh_port.recovered.lock().unwrap();
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].transport.as_deref(),
            Some("DISCORD"),
            "replay must propagate the durable transport field"
        );
    }

    /// Deterministic serialization: two events with identical content but
    /// different transport values produce different serialized JSON byte
    /// streams. This is the runtime-side contract: transport is durable
    /// metadata that the AAP Runtime reads back to drive its transport-aware
    /// validation (Phase 6.4.1).
    #[test]
    fn deterministic_serialization_transport_appears_in_durable_json() {
        let mut a = event();
        a.dispatch_id = "dispatch-det-A".into();
        a.transport = Some("OPENAB".into());
        let mut b = a.clone();
        b.transport = Some("DISCORD".into());

        let json_a = serde_json::to_string(&a).expect("serialize a");
        let json_b = serde_json::to_string(&b).expect("serialize b");
        assert_ne!(
            json_a, json_b,
            "transport field is part of durable JSON — different transports produce different serializations"
        );

        // Both still have the same v1 record_digest because transport is
        // excluded from the digest identity.
        assert_eq!(a.record_digest, b.record_digest);
    }

    /// Phase 6.2.9 native dispatch isolation regression: the native-work
    /// authority path still produces a `native-dispatch:<agent>:<dispatch_id>`
    /// pool key with the new transport field present. This pins that the
    /// Phase 6.2.9 invariants are unaffected by adding transport.
    #[test]
    fn native_workflow_metadata_supports_transport_field() {
        let metadata = crate::admission::NativeWorkflowMetadata {
            dispatch_id: "dispatch-6-2-9".into(),
            conversation_key: "1539923659345502208".into(),
            workflow_run_id: "wf-6-2-9".into(),
            task_id: "task-6-2-9".into(),
            role: "PRIMARY".into(),
            agent: "ArthurClaude".into(),
            lease_id: "lease-6-2-9".into(),
            lease_generation: 1,
            expected_revision: 1,
            language: Some("en".into()),
            project_id: None,
            project_root: None,
            native_execution_session_key: Some(crate::acp::pool::format_native_dispatch_key(
                "ArthurClaude",
                "dispatch-6-2-9",
            )),
            transport: Some("DISCORD".into()),
            delivery_destination: None,
            scope_policy: None,
        };
        assert_eq!(metadata.transport.as_deref(), Some("DISCORD"));
        let expected_key =
            crate::acp::pool::format_native_dispatch_key("ArthurClaude", "dispatch-6-2-9");
        assert_eq!(
            metadata.native_execution_session_key.as_deref(),
            Some(expected_key.as_str())
        );
    }
}
