//! Request-scoped, secret-free context for the Unix control plane.
//!
//! The context is deliberately small: it lets the concrete adapter correlate
//! a production send with its accepted control request without changing the
//! platform-neutral `ChatAdapter` method signatures.

use sha2::{Digest as _, Sha256};

#[derive(Clone, Debug)]
pub struct ControlRequestContext {
    request_id: String,
}

impl ControlRequestContext {
    pub fn new(request_id: String) -> Self {
        Self { request_id }
    }

    pub fn request_id(&self) -> &str {
        &self.request_id
    }
}

tokio::task_local! {
    static ACTIVE_CONTROL_REQUEST: ControlRequestContext;
}

pub async fn scope<R>(
    context: ControlRequestContext,
    future: impl std::future::Future<Output = R>,
) -> R {
    ACTIVE_CONTROL_REQUEST.scope(context, future).await
}

pub fn request_id() -> Option<String> {
    ACTIVE_CONTROL_REQUEST
        .try_with(|context| context.request_id.clone())
        .ok()
}

/// Hex SHA-256 for an unlogged message body. Callers must log only the digest.
pub fn sha256_hex(content: &str) -> String {
    format!("{:x}", Sha256::digest(content.as_bytes()))
}
