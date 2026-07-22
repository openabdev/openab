//! Binding-neutral primitives for inter-OAB delegation.
//!
//! This module deliberately stops before a wire binding. HTTP/SSE, WSS, and
//! A2A serialization belong in adapters built on these task and policy
//! semantics. The registry is in-memory for the first MVP slice; persistence
//! and transport reconnect are separate concerns.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use uuid::Uuid;

/// A bounded remote task request before it is mapped to a wire protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationRequest {
    pub request_id: String,
    pub task: String,
    pub capabilities: BTreeSet<String>,
    pub deadline_ms: u64,
    pub max_output_bytes: u64,
    pub delegation_depth: u8,
    pub nonce: String,
    pub idempotency_key: String,
}

impl DelegationRequest {
    fn validate(
        &self,
        policy: &DelegationPolicy,
        limits: &DelegationLimits,
    ) -> Result<(), DelegationError> {
        if self.request_id.trim().is_empty() {
            return Err(DelegationError::InvalidRequest("request_id is required".into()));
        }
        if self.task.trim().is_empty() {
            return Err(DelegationError::InvalidRequest("task is required".into()));
        }
        if self.task.len() > limits.max_task_bytes {
            return Err(DelegationError::LimitExceeded {
                field: "task",
                limit: limits.max_task_bytes as u64,
            });
        }
        if self.deadline_ms == 0 || self.deadline_ms > limits.max_deadline_ms {
            return Err(DelegationError::LimitExceeded {
                field: "deadline_ms",
                limit: limits.max_deadline_ms,
            });
        }
        if self.max_output_bytes == 0 || self.max_output_bytes > limits.max_output_bytes {
            return Err(DelegationError::LimitExceeded {
                field: "max_output_bytes",
                limit: limits.max_output_bytes,
            });
        }
        if self.delegation_depth > policy.max_delegation_depth {
            return Err(DelegationError::DepthExceeded {
                requested: self.delegation_depth,
                maximum: policy.max_delegation_depth,
            });
        }
        if self.nonce.trim().is_empty() {
            return Err(DelegationError::InvalidRequest("nonce is required".into()));
        }
        if self.idempotency_key.trim().is_empty() {
            return Err(DelegationError::InvalidRequest(
                "idempotency_key is required".into(),
            ));
        }
        if let Some(capability) = self
            .capabilities
            .iter()
            .find(|capability| !policy.allowed_capabilities.contains(*capability))
        {
            return Err(DelegationError::CapabilityDenied(capability.clone()));
        }
        Ok(())
    }

    fn fingerprint(&self) -> String {
        let normalized = (
            &self.task,
            &self.capabilities,
            self.deadline_ms,
            self.max_output_bytes,
            self.delegation_depth,
            &self.nonce,
        );
        let bytes = serde_json::to_vec(&normalized).expect("normalized request is serializable");
        let digest = Sha256::digest(bytes);
        format!("{digest:x}")
    }
}

/// Deployment limits and the callee's allowlist for a delegation request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationPolicy {
    pub allowed_capabilities: BTreeSet<String>,
    pub max_delegation_depth: u8,
}

impl Default for DelegationPolicy {
    fn default() -> Self {
        Self {
            allowed_capabilities: BTreeSet::new(),
            max_delegation_depth: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationLimits {
    pub max_task_bytes: usize,
    pub max_deadline_ms: u64,
    pub max_output_bytes: u64,
}

impl Default for DelegationLimits {
    fn default() -> Self {
        Self {
            max_task_bytes: 64 * 1024,
            max_deadline_ms: 15 * 60 * 1000,
            max_output_bytes: 1024 * 1024,
        }
    }
}

/// Computes the authority that survives caller, callee, and request policy.
pub fn effective_capabilities(
    caller_grants: &BTreeSet<String>,
    callee_policy: &BTreeSet<String>,
    requested: &BTreeSet<String>,
) -> BTreeSet<String> {
    requested
        .iter()
        .filter(|capability| {
            caller_grants.contains(*capability) && callee_policy.contains(*capability)
        })
        .cloned()
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskState {
    Accepted,
    Running,
    Completed,
    Failed,
    Cancelled,
    Expired,
    Unknown,
    Rejected,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Cancelled
                | Self::Expired
                | Self::Unknown
                | Self::Rejected
        )
    }

    fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::Accepted => matches!(
                next,
                Self::Running
                    | Self::Cancelled
                    | Self::Expired
                    | Self::Rejected
                    | Self::Unknown
            ),
            Self::Running => next == Self::Running || next.is_terminal(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TerminalResult {
    pub summary: Option<String>,
    pub error: Option<String>,
    pub artifacts: Vec<String>,
    pub executed_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationTask {
    pub task_id: String,
    pub request_id: String,
    pub idempotency_key: String,
    pub state: TaskState,
    pub cursor: u64,
    pub effective_capabilities: BTreeSet<String>,
    pub terminal_result: Option<TerminalResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelegationEvent {
    pub sequence: u64,
    pub task_id: String,
    pub state: TaskState,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CreateTaskOutcome {
    Created(DelegationTask),
    Existing(DelegationTask),
}

#[derive(Debug)]
struct TaskRecord {
    task: DelegationTask,
    fingerprint: String,
    events: Vec<DelegationEvent>,
}

/// Binding-neutral task state for the first delegation MVP slice.
#[derive(Debug, Default)]
pub struct TaskRegistry {
    tasks: BTreeMap<String, TaskRecord>,
    by_idempotency: BTreeMap<String, String>,
    by_nonce: BTreeMap<String, String>,
}

impl TaskRegistry {
    pub fn create(
        &mut self,
        request: DelegationRequest,
        caller_grants: &BTreeSet<String>,
        policy: &DelegationPolicy,
        limits: &DelegationLimits,
    ) -> Result<CreateTaskOutcome, DelegationError> {
        request.validate(policy, limits)?;

        let effective = effective_capabilities(
            caller_grants,
            &policy.allowed_capabilities,
            &request.capabilities,
        );
        if effective.len() != request.capabilities.len() {
            let denied = request
                .capabilities
                .difference(&effective)
                .next()
                .cloned()
                .unwrap_or_else(|| "unknown".into());
            return Err(DelegationError::CapabilityDenied(denied));
        }

        let fingerprint = request.fingerprint();
        if let Some(task_id) = self.by_idempotency.get(&request.idempotency_key) {
            let record = self
                .tasks
                .get(task_id)
                .expect("idempotency index must reference a task");
            if record.fingerprint == fingerprint {
                return Ok(CreateTaskOutcome::Existing(record.task.clone()));
            }
            return Err(DelegationError::IdempotencyConflict {
                key: request.idempotency_key,
            });
        }

        if self.by_nonce.contains_key(&request.nonce) {
            return Err(DelegationError::NonceReplay {
                nonce: request.nonce,
            });
        }

        let task_id = Uuid::new_v4().to_string();
        let event = DelegationEvent {
            sequence: 1,
            task_id: task_id.clone(),
            state: TaskState::Accepted,
            message: Some("task accepted".into()),
        };
        let task = DelegationTask {
            task_id: task_id.clone(),
            request_id: request.request_id,
            idempotency_key: request.idempotency_key.clone(),
            state: TaskState::Accepted,
            cursor: event.sequence,
            effective_capabilities: effective,
            terminal_result: None,
        };
        self.by_idempotency
            .insert(request.idempotency_key, task_id.clone());
        self.by_nonce.insert(request.nonce, task_id.clone());
        self.tasks.insert(
            task_id,
            TaskRecord {
                task: task.clone(),
                fingerprint,
                events: vec![event],
            },
        );
        Ok(CreateTaskOutcome::Created(task))
    }

    pub fn get(&self, task_id: &str) -> Result<DelegationTask, DelegationError> {
        self.tasks
            .get(task_id)
            .map(|record| record.task.clone())
            .ok_or_else(|| DelegationError::TaskNotFound(task_id.into()))
    }

    pub fn events_after(
        &self,
        task_id: &str,
        cursor: u64,
    ) -> Result<Vec<DelegationEvent>, DelegationError> {
        let record = self
            .tasks
            .get(task_id)
            .ok_or_else(|| DelegationError::TaskNotFound(task_id.into()))?;
        Ok(record
            .events
            .iter()
            .filter(|event| event.sequence > cursor)
            .cloned()
            .collect())
    }

    pub fn update_state(
        &mut self,
        task_id: &str,
        next: TaskState,
        message: Option<String>,
    ) -> Result<DelegationTask, DelegationError> {
        let record = self
            .tasks
            .get_mut(task_id)
            .ok_or_else(|| DelegationError::TaskNotFound(task_id.into()))?;
        if !record.task.state.can_transition_to(next) {
            return Err(if record.task.state.is_terminal() {
                DelegationError::AlreadyTerminal(record.task.state)
            } else {
                DelegationError::InvalidTransition {
                    from: record.task.state,
                    to: next,
                }
            });
        }
        let sequence = record.task.cursor + 1;
        record.task.state = next;
        record.task.cursor = sequence;
        record.events.push(DelegationEvent {
            sequence,
            task_id: task_id.into(),
            state: next,
            message,
        });
        Ok(record.task.clone())
    }

    pub fn finish(
        &mut self,
        task_id: &str,
        state: TaskState,
        result: TerminalResult,
    ) -> Result<DelegationTask, DelegationError> {
        if !state.is_terminal() {
            return Err(DelegationError::InvalidRequest(
                "finish requires a terminal state".into(),
            ));
        }
        self.update_state(task_id, state, result.summary.clone())?;
        let record = self
            .tasks
            .get_mut(task_id)
            .expect("task was checked by update_state");
        record.task.terminal_result = Some(result);
        Ok(record.task.clone())
    }

    pub fn cancel(&mut self, task_id: &str) -> Result<DelegationTask, DelegationError> {
        self.update_state(task_id, TaskState::Cancelled, Some("cancellation requested".into()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationError {
    InvalidRequest(String),
    LimitExceeded { field: &'static str, limit: u64 },
    DepthExceeded { requested: u8, maximum: u8 },
    CapabilityDenied(String),
    IdempotencyConflict { key: String },
    NonceReplay { nonce: String },
    TaskNotFound(String),
    InvalidTransition { from: TaskState, to: TaskState },
    AlreadyTerminal(TaskState),
}

impl fmt::Display for DelegationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(f, "invalid delegation request: {message}"),
            Self::LimitExceeded { field, limit } => write!(f, "{field} exceeds limit {limit}"),
            Self::DepthExceeded { requested, maximum } => {
                write!(f, "delegation depth {requested} exceeds maximum {maximum}")
            }
            Self::CapabilityDenied(capability) => write!(f, "capability denied: {capability}"),
            Self::IdempotencyConflict { key } => write!(f, "idempotency conflict for key {key}"),
            Self::NonceReplay { nonce } => write!(f, "nonce replay detected: {nonce}"),
            Self::TaskNotFound(task_id) => write!(f, "task not found: {task_id}"),
            Self::InvalidTransition { from, to } => write!(f, "invalid transition: {from:?} -> {to:?}"),
            Self::AlreadyTerminal(state) => write!(f, "task is already terminal: {state:?}"),
        }
    }
}

impl std::error::Error for DelegationError {}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> DelegationPolicy {
        DelegationPolicy {
            allowed_capabilities: BTreeSet::from(["repo:read".into(), "test:run".into()]),
            max_delegation_depth: 0,
        }
    }

    fn request(key: &str) -> DelegationRequest {
        DelegationRequest {
            request_id: format!("request-{key}"),
            task: "review this change".into(),
            capabilities: BTreeSet::from(["repo:read".into()]),
            deadline_ms: 60_000,
            max_output_bytes: 10_000,
            delegation_depth: 0,
            nonce: format!("nonce-{key}"),
            idempotency_key: key.into(),
        }
    }

    fn grants() -> BTreeSet<String> {
        BTreeSet::from(["repo:read".into(), "test:run".into()])
    }

    #[test]
    fn rejects_recursive_delegation() {
        let mut registry = TaskRegistry::default();
        let mut req = request("depth");
        req.delegation_depth = 1;
        let err = registry.create(req, &grants(), &policy(), &DelegationLimits::default());
        assert_eq!(err, Err(DelegationError::DepthExceeded { requested: 1, maximum: 0 }));
    }

    #[test]
    fn intersects_caller_and_callee_authority() {
        let mut registry = TaskRegistry::default();
        let req = request("authority");
        let caller = BTreeSet::from(["test:run".into()]);
        let err = registry.create(req, &caller, &policy(), &DelegationLimits::default());
        assert_eq!(err, Err(DelegationError::CapabilityDenied("repo:read".into())));
    }

    #[test]
    fn computes_only_requested_intersection() {
        let caller = BTreeSet::from(["repo:read".into(), "network:read".into()]);
        let policy = BTreeSet::from(["repo:read".into(), "test:run".into()]);
        let requested = BTreeSet::from(["repo:read".into(), "network:read".into()]);
        assert_eq!(
            effective_capabilities(&caller, &policy, &requested),
            BTreeSet::from(["repo:read".into()])
        );
    }

    #[test]
    fn idempotent_create_returns_existing_task() {
        let mut registry = TaskRegistry::default();
        let first = registry
            .create(request("same"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap();
        let second = registry
            .create(request("same"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap();
        let first = match first {
            CreateTaskOutcome::Created(task) => task,
            _ => panic!("first request must create"),
        };
        let second = match second {
            CreateTaskOutcome::Existing(task) => task,
            _ => panic!("second request must reuse"),
        };
        assert_eq!(first.task_id, second.task_id);
        assert_eq!(second.cursor, 1);
    }

    #[test]
    fn conflicting_idempotency_key_is_rejected() {
        let mut registry = TaskRegistry::default();
        registry
            .create(request("same"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap();
        let mut conflicting = request("same");
        conflicting.task = "different task".into();
        let err = registry
            .create(conflicting, &grants(), &policy(), &DelegationLimits::default())
            .unwrap_err();
        assert_eq!(
            err,
            DelegationError::IdempotencyConflict { key: "same".into() }
        );
    }

    #[test]
    fn nonce_replay_is_rejected_even_with_a_new_idempotency_key() {
        let mut registry = TaskRegistry::default();
        registry
            .create(request("first"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap();
        let mut replay = request("second");
        replay.nonce = "nonce-first".into();
        assert_eq!(
            registry
                .create(replay, &grants(), &policy(), &DelegationLimits::default())
                .unwrap_err(),
            DelegationError::NonceReplay {
                nonce: "nonce-first".into()
            }
        );
    }

    #[test]
    fn lifecycle_and_terminal_result_are_explicit() {
        let mut registry = TaskRegistry::default();
        let task = match registry
            .create(request("lifecycle"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap()
        {
            CreateTaskOutcome::Created(task) => task,
            _ => panic!("expected creation"),
        };
        registry
            .update_state(&task.task_id, TaskState::Running, Some("working".into()))
            .unwrap();
        let finished = registry
            .finish(
                &task.task_id,
                TaskState::Completed,
                TerminalResult {
                    summary: Some("done".into()),
                    error: None,
                    artifacts: vec![],
                    executed_by: Some("oab-b".into()),
                },
            )
            .unwrap();
        assert_eq!(finished.state, TaskState::Completed);
        assert_eq!(finished.cursor, 3);
        assert_eq!(finished.terminal_result.unwrap().executed_by.as_deref(), Some("oab-b"));
    }

    #[test]
    fn terminal_tasks_cannot_transition_or_cancel() {
        let mut registry = TaskRegistry::default();
        let task = match registry
            .create(request("terminal"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap()
        {
            CreateTaskOutcome::Created(task) => task,
            _ => panic!("expected creation"),
        };
        registry
            .update_state(&task.task_id, TaskState::Running, None)
            .unwrap();
        registry
            .finish(
                &task.task_id,
                TaskState::Failed,
                TerminalResult {
                    summary: None,
                    error: Some("worker failed".into()),
                    artifacts: vec![],
                    executed_by: None,
                },
            )
            .unwrap();
        assert_eq!(
            registry.cancel(&task.task_id),
            Err(DelegationError::AlreadyTerminal(TaskState::Failed))
        );
    }

    #[test]
    fn cancellation_transitions_to_terminal_state() {
        let mut registry = TaskRegistry::default();
        let task = match registry
            .create(request("cancel"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap()
        {
            CreateTaskOutcome::Created(task) => task,
            _ => panic!("expected creation"),
        };
        let cancelled = registry.cancel(&task.task_id).unwrap();
        assert_eq!(cancelled.state, TaskState::Cancelled);
        assert_eq!(cancelled.cursor, 2);
        assert_eq!(
            registry.events_after(&task.task_id, 1).unwrap()[0].state,
            TaskState::Cancelled
        );
    }

    #[test]
    fn cursor_replay_returns_only_new_events() {
        let mut registry = TaskRegistry::default();
        let task = match registry
            .create(request("cursor"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap()
        {
            CreateTaskOutcome::Created(task) => task,
            _ => panic!("expected creation"),
        };
        registry
            .update_state(&task.task_id, TaskState::Running, Some("progress".into()))
            .unwrap();
        let events = registry.events_after(&task.task_id, 1).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence, 2);
        assert_eq!(events[0].state, TaskState::Running);
        assert!(registry.events_after(&task.task_id, 2).unwrap().is_empty());
    }

    #[test]
    fn invalid_transition_is_rejected_without_mutating_cursor() {
        let mut registry = TaskRegistry::default();
        let task = match registry
            .create(request("transition"), &grants(), &policy(), &DelegationLimits::default())
            .unwrap()
        {
            CreateTaskOutcome::Created(task) => task,
            _ => panic!("expected creation"),
        };
        let err = registry
            .update_state(&task.task_id, TaskState::Completed, None)
            .unwrap_err();
        assert_eq!(
            err,
            DelegationError::InvalidTransition {
                from: TaskState::Accepted,
                to: TaskState::Completed,
            }
        );
        assert_eq!(registry.get(&task.task_id).unwrap().cursor, 1);
    }
}
