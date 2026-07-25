//! Native Gmail adapter (Capability Plugin, OAB MCP Adapter ADR §5.3/§6.1 and
//! rollout step 5): the six-capability Gmail profile of ADR §6.5 served from
//! Gmail's **generally-available REST API** instead of the Developer-Preview
//! hosted MCP server (`gmailmcp.googleapis.com`).
//!
//! Why this exists (validated live, PR #1448 discussion):
//! - the hosted Gmail MCP server gates `tools/call` behind Workspace
//!   Developer Preview Program enrollment (per account *and* per GCP
//!   project), while the same OAuth token works against Gmail REST;
//! - DPP terms prohibit shipping preview features in public applications, so
//!   the hosted path cannot reach OAB users before Google announces GA;
//! - tool names and argument shapes below mirror the hosted server
//!   (`search_threads`, `get_thread`, `get_message`, `list_labels`,
//!   `list_drafts`, `create_draft`), so swapping this adapter for the hosted
//!   entry at GA is an `mcp.json` change with no agent-facing difference.
//!
//! Packaging: a **loopback Streamable HTTP MCP server** (`openab
//! gmail-native serve --listen 127.0.0.1:<port>`), registered in `mcp.json`
//! as a normal `"type": "http"` entry pointing at `http://<addr>/mcp`. The
//! outbound runtime therefore applies `tool_filter`, JSON-Schema argument
//! validation, timeouts, the circuit breaker, and secret redaction to this
//! adapter exactly as it does to any external server — no new integration
//! path, no policy bypass.
//!
//! Safety profile (ADR §6.5): read + drafts only. `create_draft` never
//! sends mail; there is no send, delete, or label-mutation surface at all.
//!
//! Credentials: Google OAuth (PKCE paste-back) with `access_type=offline` +
//! `prompt=consent`, so Google issues a **refresh token** and the adapter can
//! run headless (the rmcp-driven login flow cannot request these parameters —
//! see PR #1448 findings). Tokens live in the shared `auth.json` under the
//! `gmail-native` key via [`McpCredentialStore`]; refresh responses that omit
//! a rotated refresh token keep the previous one (RFC 6749 §6).

use anyhow::{anyhow, bail, Context as _, Result};
use base64::engine::general_purpose::{STANDARD as B64_STD, URL_SAFE_NO_PAD as B64_URL};
use base64::Engine as _;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::CredentialStore as _;
use rmcp::ErrorData as McpError;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::auth::{auth_path, McpCredentialStore};
use crate::mcp::flow::parse_redirect_params;
use crate::mcp::redact_secrets;
use crate::mcp::runtime::classify_stored_creds;

/// Gmail REST base for the authorized user. Overridable for tests.
const GMAIL_API_BASE: &str = "https://gmail.googleapis.com/gmail/v1/users/me";
const GOOGLE_AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Credential key in `auth.json`. Distinct from any hosted `gmail` entry so
/// the two paths can coexist during the GA transition.
pub const CREDENTIAL_KEY: &str = "gmail-native";
/// ADR §6.5 Gmail profile scopes — read + compose (drafts), nothing else.
pub const SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/gmail.readonly",
    "https://www.googleapis.com/auth/gmail.compose",
];
/// Default paste-back redirect; must be pre-registered on the OAuth client.
pub const DEFAULT_REDIRECT_URI: &str = "http://localhost:53692/callback";

/// OAuth app configuration for token refresh (and login). Google requires
/// the client secret on both code exchange and refresh for "Web application"
/// clients; a secret-less ("Desktop") client also works — leave it empty.
#[derive(Clone)]
pub struct OauthApp {
    pub client_id: String,
    pub client_secret: Option<String>,
}

impl OauthApp {
    /// Resolve from environment (the `mcp.json` stdio `env` block delivers
    /// these to the subprocess): `GMAIL_OAUTH_CLIENT_ID` (required),
    /// `GMAIL_OAUTH_CLIENT_SECRET` (optional for public clients).
    pub fn from_env() -> Result<Self> {
        let client_id = std::env::var("GMAIL_OAUTH_CLIENT_ID")
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                anyhow!(
                    "GMAIL_OAUTH_CLIENT_ID is not set — pass it via the `env` block of the \
                     gmail-native entry in mcp.json (and to `gmail-native login`)"
                )
            })?;
        let client_secret = std::env::var("GMAIL_OAUTH_CLIENT_SECRET")
            .ok()
            .filter(|s| !s.is_empty());
        Ok(Self {
            client_id,
            client_secret,
        })
    }
}

/// The native Gmail MCP server. One instance per `serve` process.
pub struct GmailNative {
    http: reqwest::Client,
    store: McpCredentialStore,
    oauth: OauthApp,
    api_base: String,
    /// Serializes token refresh so concurrent tool calls trigger one refresh.
    refresh_lock: Mutex<()>,
}

impl GmailNative {
    pub fn new(oauth: OauthApp) -> Self {
        Self {
            http: reqwest::Client::new(),
            store: McpCredentialStore::new(auth_path(), CREDENTIAL_KEY),
            oauth,
            api_base: GMAIL_API_BASE.to_string(),
            refresh_lock: Mutex::new(()),
        }
    }

    #[cfg(test)]
    fn with_api_base(mut self, base: &str) -> Self {
        self.api_base = base.to_string();
        self
    }

    /// Valid bearer token: cached when fresh, refreshed via the stored
    /// refresh token when near expiry (60s buffer, same policy as the MCP
    /// runtime), otherwise an actionable re-login error.
    async fn access_token(&self) -> Result<String> {
        let load = || async {
            self.store
                .load()
                .await
                .map_err(|e| anyhow!("read auth.json: {e}"))?
                .ok_or_else(|| {
                    anyhow!(
                        "no gmail-native credentials — run `openab gmail-native login` first"
                    )
                })
        };
        let creds = load().await?;
        let (has_token, has_refresh, near_expiry) = classify_stored_creds(&creds);
        if has_token && !near_expiry {
            return extract_access_token(&creds);
        }
        if !has_refresh {
            bail!(
                "gmail-native token expired and no refresh token is stored — \
                 re-run `openab gmail-native login` (the login flow requests \
                 access_type=offline, so this normally does not recur)"
            );
        }
        let _guard = self.refresh_lock.lock().await;
        // Re-check under the lock: a concurrent call may have refreshed.
        let creds = load().await?;
        let (has_token, _, near_expiry) = classify_stored_creds(&creds);
        if has_token && !near_expiry {
            return extract_access_token(&creds);
        }
        let refresh_token = extract_refresh_token(&creds)?;
        let mut form = vec![
            ("grant_type", "refresh_token".to_string()),
            ("refresh_token", refresh_token.clone()),
            ("client_id", self.oauth.client_id.clone()),
        ];
        if let Some(secret) = &self.oauth.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let resp = self
            .http
            .post(GOOGLE_TOKEN_URL)
            .form(&form)
            .send()
            .await
            .context("gmail-native token refresh request")?;
        let status = resp.status();
        let body: Value = resp
            .json()
            .await
            .context("gmail-native token refresh response")?;
        if !status.is_success() {
            bail!(
                "gmail-native token refresh failed ({status}): {} — if the grant was revoked, \
                 re-run `openab gmail-native login`",
                body["error"].as_str().unwrap_or("unknown error")
            );
        }
        let access = body["access_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow!("token refresh response missing access_token"))?
            .to_string();
        // RFC 6749 §6: a refresh response MAY rotate the refresh token; keep
        // the previous one when the provider (Google does) omits it.
        let new_refresh = body["refresh_token"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or(refresh_token);
        let stored = build_gmail_credentials(
            &self.oauth.client_id,
            &access,
            Some(&new_refresh),
            body["expires_in"].as_u64(),
            body["scope"].as_str(),
        )?;
        self.store
            .save(stored)
            .await
            .map_err(|e| anyhow!("persist refreshed gmail-native credentials: {e}"))?;
        Ok(access)
    }

    async fn api_get(&self, path_and_query: &str) -> Result<Value> {
        let token = self.access_token().await?;
        let url = format!("{}/{}", self.api_base, path_and_query);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&token)
            .send()
            .await
            .with_context(|| format!("gmail GET {path_and_query}"))?;
        read_api_response(resp).await
    }

    async fn api_post(&self, path: &str, payload: &Value) -> Result<Value> {
        let token = self.access_token().await?;
        let url = format!("{}/{}", self.api_base, path);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&token)
            .json(payload)
            .send()
            .await
            .with_context(|| format!("gmail POST {path}"))?;
        read_api_response(resp).await
    }

    // ---- tool handlers -------------------------------------------------

    async fn search_threads(&self, args: &Map<String, Value>) -> Result<Value> {
        let mut q = QueryString::new();
        q.push_opt_str("q", args.get("query"));
        q.push_opt_u64("maxResults", args.get("pageSize"), 25);
        q.push_opt_str("pageToken", args.get("pageToken"));
        self.api_get(&format!("threads{}", q.finish())).await
    }

    async fn get_thread(&self, args: &Map<String, Value>) -> Result<Value> {
        let id = required_id(args, "threadId")?;
        let format = message_format(args)?;
        self.api_get(&format!("threads/{id}?format={format}")).await
    }

    async fn get_message(&self, args: &Map<String, Value>) -> Result<Value> {
        let id = required_id(args, "messageId")?;
        let format = message_format(args)?;
        self.api_get(&format!("messages/{id}?format={format}"))
            .await
    }

    async fn list_labels(&self) -> Result<Value> {
        self.api_get("labels").await
    }

    async fn list_drafts(&self, args: &Map<String, Value>) -> Result<Value> {
        let mut q = QueryString::new();
        q.push_opt_str("q", args.get("query"));
        q.push_opt_u64("maxResults", args.get("pageSize"), 25);
        q.push_opt_str("pageToken", args.get("pageToken"));
        self.api_get(&format!("drafts{}", q.finish())).await
    }

    async fn create_draft(&self, args: &Map<String, Value>) -> Result<Value> {
        let to = string_list(args, "to")?;
        if to.is_empty() {
            bail!("create_draft requires at least one `to` recipient");
        }
        let cc = string_list(args, "cc")?;
        let bcc = string_list(args, "bcc")?;
        let subject = args
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let body = args
            .get("body")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("create_draft requires a `body` string"))?;
        // Reply threading: resolve the original message's RFC 2822 Message-ID
        // and thread, mirroring the hosted server's `replyToMessageId`.
        let mut in_reply_to: Option<String> = None;
        let mut thread_id: Option<String> = None;
        if let Some(reply_id) = args.get("replyToMessageId").and_then(Value::as_str) {
            let reply_id = validate_id(reply_id)?;
            let original = self
                .api_get(&format!(
                    "messages/{reply_id}?format=metadata&metadataHeaders=Message-ID"
                ))
                .await
                .context("resolve replyToMessageId")?;
            thread_id = original["threadId"].as_str().map(str::to_string);
            in_reply_to = original["payload"]["headers"]
                .as_array()
                .and_then(|hs| {
                    hs.iter().find(|h| {
                        h["name"]
                            .as_str()
                            .is_some_and(|n| n.eq_ignore_ascii_case("message-id"))
                    })
                })
                .and_then(|h| h["value"].as_str())
                .map(str::to_string);
        }
        let mime = build_mime(&to, &cc, &bcc, subject, body, in_reply_to.as_deref())?;
        let mut message = json!({ "raw": B64_URL.encode(mime.as_bytes()) });
        if let Some(tid) = thread_id {
            message["threadId"] = json!(tid);
        }
        let created = self
            .api_post("drafts", &json!({ "message": message }))
            .await?;
        // The ADR requires drafts to be presented as drafts (§6.5): make the
        // outcome unambiguous for the calling agent.
        Ok(json!({
            "id": created["id"],
            "messageId": created["message"]["id"],
            "threadId": created["message"]["threadId"],
            "status": "draft created — NOT sent; the user must review and send it from Gmail"
        }))
    }
}

/// Bounded, sanitized query-string builder for the Gmail list endpoints.
struct QueryString {
    parts: Vec<String>,
}

impl QueryString {
    fn new() -> Self {
        Self { parts: Vec::new() }
    }
    fn push_opt_str(&mut self, key: &str, v: Option<&Value>) {
        if let Some(s) = v.and_then(Value::as_str).filter(|s| !s.is_empty()) {
            self.parts.push(format!("{key}={}", urlencoding::encode(s)));
        }
    }
    fn push_opt_u64(&mut self, key: &str, v: Option<&Value>, default: u64) {
        let n = v.and_then(Value::as_u64).unwrap_or(default).clamp(1, 100);
        self.parts.push(format!("{key}={n}"));
    }
    fn finish(self) -> String {
        if self.parts.is_empty() {
            String::new()
        } else {
            format!("?{}", self.parts.join("&"))
        }
    }
}

/// Gmail resource IDs are hex-ish opaque tokens; they become URL path
/// segments, so reject anything that could alter the path or query.
fn validate_id(id: &str) -> Result<&str> {
    if id.is_empty() || id.len() > 128 {
        bail!("invalid Gmail id: empty or too long");
    }
    if !id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        bail!("invalid Gmail id {id:?}: only [A-Za-z0-9_-] permitted");
    }
    Ok(id)
}

fn required_id<'a>(args: &'a Map<String, Value>, key: &str) -> Result<&'a str> {
    let id = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("missing required `{key}` string"))?;
    validate_id(id)
}

/// `format` for get_thread / get_message. `metadata` (headers + snippet) is
/// the default: it is what an agent needs to summarize mail without pulling
/// full MIME trees; `full` is available on request.
fn message_format(args: &Map<String, Value>) -> Result<&'static str> {
    match args.get("format").and_then(Value::as_str) {
        None => Ok("metadata"),
        Some("metadata") => Ok("metadata"),
        Some("full") => Ok("full"),
        Some("minimal") => Ok("minimal"),
        Some(other) => bail!("invalid format {other:?}: expected minimal | metadata | full"),
    }
}

fn string_list(args: &Map<String, Value>, key: &str) -> Result<Vec<String>> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| anyhow!("`{key}` entries must be strings"))
            })
            .collect(),
        // Tolerate a bare string — LLMs frequently send one address unwrapped.
        Some(Value::String(s)) => Ok(vec![s.clone()]),
        Some(other) => bail!("`{key}` must be an array of strings, got {other}"),
    }
}

async fn read_api_response(resp: reqwest::Response) -> Result<Value> {
    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .unwrap_or_else(|_| json!({"error": {"message": "non-JSON response"}}));
    if !status.is_success() {
        bail!(
            "Gmail API error ({status}): {}",
            body["error"]["message"].as_str().unwrap_or("unknown")
        );
    }
    Ok(body)
}

fn extract_access_token(creds: &rmcp::transport::StoredCredentials) -> Result<String> {
    let v = serde_json::to_value(creds).context("serialize credentials")?;
    v["token_response"]["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("stored gmail-native credentials have no access_token"))
}

fn extract_refresh_token(creds: &rmcp::transport::StoredCredentials) -> Result<String> {
    let v = serde_json::to_value(creds).context("serialize credentials")?;
    v["token_response"]["refresh_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("stored gmail-native credentials have no refresh_token"))
}

/// Build the rmcp `StoredCredentials` shape `McpCredentialStore` persists.
/// Mirrors the runtime's device-flow lifting; kept local so the adapter has
/// no dependency on runtime internals beyond `classify_stored_creds`.
fn build_gmail_credentials(
    client_id: &str,
    access_token: &str,
    refresh_token: Option<&str>,
    expires_in: Option<u64>,
    scope: Option<&str>,
) -> Result<rmcp::transport::StoredCredentials> {
    let mut tr = json!({
        "access_token": access_token,
        "token_type": "bearer",
    });
    if let Some(rt) = refresh_token.filter(|s| !s.is_empty()) {
        tr["refresh_token"] = json!(rt);
    }
    if let Some(secs) = expires_in {
        tr["expires_in"] = json!(secs);
    }
    if let Some(scope) = scope {
        tr["scope"] = json!(scope);
    }
    let granted: Vec<&str> = scope.map(|s| s.split(' ').collect()).unwrap_or_default();
    serde_json::from_value(json!({
        "client_id": client_id,
        "token_response": tr,
        "granted_scopes": granted,
        "token_received_at": now_secs(),
    }))
    .context("build gmail-native StoredCredentials")
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---- MIME construction --------------------------------------------------

/// RFC 5322 message with UTF-8 subject (RFC 2047) and base64 body. Header
/// values are strictly validated — a CR/LF anywhere in an address or subject
/// is header injection and is refused, not sanitized.
fn build_mime(
    to: &[String],
    cc: &[String],
    bcc: &[String],
    subject: &str,
    body: &str,
    in_reply_to: Option<&str>,
) -> Result<String> {
    let mut headers = String::new();
    headers.push_str(&format!("To: {}\r\n", join_addresses(to)?));
    if !cc.is_empty() {
        headers.push_str(&format!("Cc: {}\r\n", join_addresses(cc)?));
    }
    if !bcc.is_empty() {
        headers.push_str(&format!("Bcc: {}\r\n", join_addresses(bcc)?));
    }
    headers.push_str(&format!("Subject: {}\r\n", encode_subject(subject)?));
    if let Some(orig) = in_reply_to {
        let orig = validate_header_value(orig)?;
        headers.push_str(&format!("In-Reply-To: {orig}\r\n"));
        headers.push_str(&format!("References: {orig}\r\n"));
    }
    headers.push_str("MIME-Version: 1.0\r\n");
    headers.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    headers.push_str("Content-Transfer-Encoding: base64\r\n");
    // 76-column base64 body per RFC 2045 §6.8.
    let encoded = B64_STD.encode(body.as_bytes());
    let wrapped: String = encoded
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).expect("base64 is ascii"))
        .collect::<Vec<_>>()
        .join("\r\n");
    Ok(format!("{headers}\r\n{wrapped}"))
}

fn join_addresses(addrs: &[String]) -> Result<String> {
    let mut out = Vec::with_capacity(addrs.len());
    for a in addrs {
        let a = a.trim();
        if a.is_empty() || !a.contains('@') || a.len() > 320 {
            bail!("invalid email address {a:?}");
        }
        out.push(validate_header_value(a)?.to_string());
    }
    Ok(out.join(", "))
}

fn validate_header_value(v: &str) -> Result<&str> {
    if v.chars().any(|c| c == '\r' || c == '\n' || c == '\0') {
        bail!("header value contains a control character (possible header injection)");
    }
    Ok(v)
}

/// RFC 2047 B-encoding for non-ASCII subjects; plain passthrough otherwise.
fn encode_subject(subject: &str) -> Result<String> {
    let subject = validate_header_value(subject)?;
    if subject.is_ascii() {
        Ok(subject.to_string())
    } else {
        Ok(format!(
            "=?UTF-8?B?{}?=",
            B64_STD.encode(subject.as_bytes())
        ))
    }
}

// ---- tool catalog --------------------------------------------------------

fn as_map(v: Value) -> std::sync::Arc<Map<String, Value>> {
    std::sync::Arc::new(v.as_object().expect("schema literals are objects").clone())
}

/// The six-tool §6.5 profile. Names and argument shapes mirror Google's
/// hosted Gmail MCP server so a GA cut-over is config-only.
pub fn gmail_tools() -> Vec<Tool> {
    let page = json!({
        "pageSize": { "type": "integer", "description": "Maximum results per page (1-100, default 25)." },
        "pageToken": { "type": "string", "description": "Opaque page token from a previous response." }
    });
    let fmt = json!({ "type": "string", "enum": ["minimal", "metadata", "full"],
        "description": "Payload detail: metadata (headers + snippet, default), full (complete MIME), minimal (ids only)." });
    vec![
        Tool::new(
            "search_threads",
            "Search email threads in the authenticated user's Gmail account using Gmail query syntax (e.g. \"from:alice in:inbox newer_than:7d\").",
            as_map(json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Gmail search query. Omit to list recent threads." },
                    "pageSize": page["pageSize"], "pageToken": page["pageToken"]
                }
            })),
        ),
        Tool::new(
            "get_thread",
            "Retrieve one email thread (all messages) by thread id.",
            as_map(json!({
                "type": "object",
                "properties": { "threadId": { "type": "string" }, "format": fmt },
                "required": ["threadId"]
            })),
        ),
        Tool::new(
            "get_message",
            "Retrieve one email message by message id.",
            as_map(json!({
                "type": "object",
                "properties": { "messageId": { "type": "string" }, "format": fmt },
                "required": ["messageId"]
            })),
        ),
        Tool::new(
            "list_labels",
            "List all labels in the authenticated user's Gmail account.",
            as_map(json!({ "type": "object", "properties": {} })),
        ),
        Tool::new(
            "list_drafts",
            "List draft emails in the authenticated user's Gmail account.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Gmail search query filter." },
                    "pageSize": page["pageSize"], "pageToken": page["pageToken"]
                }
            })),
        ),
        Tool::new(
            "create_draft",
            "Create a draft email (NEVER sends). The user reviews and sends it from Gmail. Supports replying within a thread via replyToMessageId.",
            as_map(json!({
                "type": "object",
                "properties": {
                    "to": { "type": "array", "items": { "type": "string" }, "description": "Recipient email addresses." },
                    "cc": { "type": "array", "items": { "type": "string" } },
                    "bcc": { "type": "array", "items": { "type": "string" } },
                    "subject": { "type": "string" },
                    "body": { "type": "string", "description": "Plain-text body (UTF-8)." },
                    "replyToMessageId": { "type": "string", "description": "Gmail message id being replied to; threads the draft and sets In-Reply-To." }
                },
                "required": ["to", "body"]
            })),
        ),
    ]
}

const INSTRUCTIONS: &str = "Native Gmail adapter (read + drafts only). Search and read mail, list \
labels/drafts, and create draft emails for the user to review and send from Gmail. It cannot \
send, delete, or modify mail.";

impl ServerHandler for GmailNative {
    fn get_info(&self) -> ServerInfo {
        let mut server_info = Implementation::default();
        server_info.name = "oab-gmail-native".into();
        server_info.version = env!("CARGO_PKG_VERSION").into();
        let mut info = ServerInfo::default();
        info.capabilities = ServerCapabilities::builder().enable_tools().build();
        info.server_info = server_info;
        info.instructions = Some(INSTRUCTIONS.into());
        info
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult {
            tools: gmail_tools(),
            next_cursor: None,
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        let empty = Map::new();
        let args = request.arguments.as_ref().unwrap_or(&empty);
        let outcome = match request.name.as_ref() {
            "search_threads" => self.search_threads(args).await,
            "get_thread" => self.get_thread(args).await,
            "get_message" => self.get_message(args).await,
            "list_labels" => self.list_labels().await,
            "list_drafts" => self.list_drafts(args).await,
            "create_draft" => self.create_draft(args).await,
            other => {
                return Err(McpError::invalid_params(
                    format!(
                        "unknown tool {other:?} — this adapter serves the six-tool Gmail profile"
                    ),
                    None,
                ))
            }
        };
        Ok(match outcome {
            Ok(v) => CallToolResult::success(vec![Content::text(
                serde_json::to_string(&v).unwrap_or_else(|_| v.to_string()),
            )]),
            Err(e) => CallToolResult::error(vec![Content::text(redact_secrets(&format!("{e:#}")))]),
        })
    }
}

/// Serve the adapter over **loopback Streamable HTTP** at `/mcp`, mirroring
/// the OAB MCP Facade's transport (ADR §6.2 trust model: loopback-only, no
/// authentication layer — the host/pod boundary is the trust boundary; a
/// non-loopback bind is refused at startup). Register in `mcp.json` as a
/// `"type": "http"` entry pointing at `http://<addr>/mcp`.
///
/// One handler is constructed per MCP session, all sharing the same
/// credential store file; a cross-session refresh race is benign (each
/// refresh yields a valid token and the stored refresh token is preserved).
pub async fn serve_http(addr: &str) -> Result<()> {
    use rmcp::transport::streamable_http_server::{
        session::local::LocalSessionManager, StreamableHttpService,
    };
    let sock = crate::mcp::facade::require_loopback(addr)?;
    let oauth = OauthApp::from_env()?;
    let service = StreamableHttpService::new(
        move || Ok(GmailNative::new(oauth.clone())),
        LocalSessionManager::default().into(),
        Default::default(),
    );
    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = tokio::net::TcpListener::bind(sock)
        .await
        .with_context(|| format!("bind gmail-native listener on {sock}"))?;
    tracing::info!(
        addr = %sock,
        "gmail-native adapter listening (Streamable HTTP, loopback-only, no auth — host boundary is the trust boundary)"
    );
    axum::serve(listener, router)
        .await
        .context("gmail-native HTTP server terminated")?;
    Ok(())
}

// ---- interactive login ----------------------------------------------------

/// PKCE paste-back login that — unlike the generic rmcp-driven flow — sends
/// `access_type=offline` and `prompt=consent`, so Google issues a refresh
/// token and headless deployments survive access-token expiry.
pub async fn login(redirect_uri: &str) -> Result<()> {
    let oauth = OauthApp::from_env()?;
    let verifier = random_urlsafe(32);
    let challenge = B64_URL.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_urlsafe(16);
    let scope = SCOPES.join(" ");
    let authorize_url = format!(
        "{GOOGLE_AUTHORIZE_URL}?response_type=code&client_id={}&redirect_uri={}&scope={}\
         &state={}&code_challenge={}&code_challenge_method=S256&access_type=offline&prompt=consent",
        urlencoding::encode(&oauth.client_id),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(&scope),
        urlencoding::encode(&state),
        urlencoding::encode(&challenge),
    );
    println!("Open this URL in a browser to authorize (Gmail read + drafts):");
    println!();
    println!("    {authorize_url}");
    println!();
    println!("After approving, the browser lands on {redirect_uri} (a dead page — expected).");
    print!("Paste the FULL redirect URL: ");
    use std::io::Write as _;
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .context("read redirect URL")?;
    let (code, got_state) = parse_redirect_params(line.trim())?;
    if got_state != state {
        bail!("state mismatch — possible CSRF or stale login attempt; retry the login");
    }
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code),
        ("redirect_uri", redirect_uri.to_string()),
        ("client_id", oauth.client_id.clone()),
        ("code_verifier", verifier),
    ];
    if let Some(secret) = &oauth.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let resp = reqwest::Client::new()
        .post(GOOGLE_TOKEN_URL)
        .form(&form)
        .send()
        .await
        .context("token exchange")?;
    let status = resp.status();
    let body: Value = resp.json().await.context("token exchange response")?;
    if !status.is_success() {
        bail!(
            "token exchange failed ({status}): {}",
            body["error"].as_str().unwrap_or("unknown error")
        );
    }
    let access = body["access_token"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("token response missing access_token"))?;
    let refresh = body["refresh_token"].as_str().filter(|s| !s.is_empty());
    if refresh.is_none() {
        eprintln!(
            "⚠ Google returned no refresh token — headless refresh will not work. \
             This usually means a prior grant exists; revoke the app at \
             https://myaccount.google.com/permissions and log in again."
        );
    }
    let stored = build_gmail_credentials(
        &oauth.client_id,
        access,
        refresh,
        body["expires_in"].as_u64(),
        body["scope"].as_str(),
    )?;
    McpCredentialStore::new(auth_path(), CREDENTIAL_KEY)
        .save(stored)
        .await
        .map_err(|e| anyhow!("persist gmail-native credentials: {e}"))?;
    println!(
        "● logged in: {CREDENTIAL_KEY} (refresh token: {})",
        if refresh.is_some() {
            "yes"
        } else {
            "NO — see warning above"
        }
    );
    Ok(())
}

fn random_urlsafe(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("os rng");
    B64_URL.encode(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_basic_shape() {
        let m = build_mime(
            &["a@example.com".into()],
            &[],
            &[],
            "Hello",
            "line1\nline2",
            None,
        )
        .unwrap();
        assert!(m.starts_with("To: a@example.com\r\n"));
        assert!(m.contains("Subject: Hello\r\n"));
        assert!(m.contains("Content-Type: text/plain; charset=UTF-8\r\n"));
        let body_b64 = m.split("\r\n\r\n").nth(1).unwrap().replace("\r\n", "");
        assert_eq!(B64_STD.decode(body_b64).unwrap(), b"line1\nline2");
    }

    #[test]
    fn mime_utf8_subject_is_rfc2047_encoded() {
        let m = build_mime(
            &["a@example.com".into()],
            &[],
            &[],
            "回覆：測試",
            "hi",
            None,
        )
        .unwrap();
        let line = m.lines().find(|l| l.starts_with("Subject:")).unwrap();
        assert!(line.starts_with("Subject: =?UTF-8?B?"), "{line}");
        assert!(line.ends_with("?="));
    }

    #[test]
    fn mime_rejects_header_injection() {
        let err = build_mime(
            &["a@example.com\r\nBcc: evil@example.com".into()],
            &[],
            &[],
            "s",
            "b",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("control character"), "{err}");
        let err = build_mime(
            &["a@example.com".into()],
            &[],
            &[],
            "s\r\nX-Evil: 1",
            "b",
            None,
        )
        .unwrap_err();
        assert!(err.to_string().contains("control character"), "{err}");
    }

    #[test]
    fn mime_reply_headers() {
        let m = build_mime(
            &["a@example.com".into()],
            &[],
            &[],
            "Re: x",
            "b",
            Some("<orig@mail.gmail.com>"),
        )
        .unwrap();
        assert!(m.contains("In-Reply-To: <orig@mail.gmail.com>\r\n"));
        assert!(m.contains("References: <orig@mail.gmail.com>\r\n"));
    }

    #[test]
    fn id_validation_blocks_path_traversal() {
        assert!(validate_id("18c1a2b3d4e5f6a7").is_ok());
        assert!(validate_id("../settings").is_err());
        assert!(validate_id("a?format=full").is_err());
        assert!(validate_id("").is_err());
    }

    #[test]
    fn tool_catalog_is_the_six_tool_profile() {
        let names: Vec<_> = gmail_tools().iter().map(|t| t.name.to_string()).collect();
        assert_eq!(
            names,
            vec![
                "search_threads",
                "get_thread",
                "get_message",
                "list_labels",
                "list_drafts",
                "create_draft"
            ]
        );
        // No send/delete/label mutation surface — §6.5 safety profile.
        for forbidden in ["send", "delete", "modify", "label_"] {
            assert!(
                names.iter().all(|n| !n.contains(forbidden)),
                "forbidden capability {forbidden:?} leaked into the profile"
            );
        }
    }

    #[test]
    fn stored_credentials_roundtrip_keeps_refresh_token() {
        let creds =
            build_gmail_credentials("cid", "at", Some("rt"), Some(3599), Some("s1 s2")).unwrap();
        let v = serde_json::to_value(&creds).unwrap();
        assert_eq!(v["token_response"]["access_token"], "at");
        assert_eq!(v["token_response"]["refresh_token"], "rt");
        assert_eq!(v["client_id"], "cid");
        let (has_token, has_refresh, near_expiry) = classify_stored_creds(&creds);
        assert!(has_token && has_refresh && !near_expiry);
    }

    #[test]
    fn stored_credentials_without_refresh_classify_as_no_refresh() {
        let creds = build_gmail_credentials("cid", "at", None, Some(10), None).unwrap();
        let (_, has_refresh, _) = classify_stored_creds(&creds);
        assert!(!has_refresh);
    }

    #[test]
    fn query_string_clamps_and_encodes() {
        let mut q = QueryString::new();
        q.push_opt_str("q", Some(&json!("from:a b&c")));
        q.push_opt_u64("maxResults", Some(&json!(9999)), 25);
        assert_eq!(q.finish(), "?q=from%3Aa%20b%26c&maxResults=100");
    }

    #[test]
    fn string_list_tolerates_bare_string() {
        let mut m = Map::new();
        m.insert("to".into(), json!("a@example.com"));
        assert_eq!(string_list(&m, "to").unwrap(), vec!["a@example.com"]);
        m.insert("to".into(), json!([1]));
        assert!(string_list(&m, "to").is_err());
    }
}
