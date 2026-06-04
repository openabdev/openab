//! OAuth 2.1 paste-back flow primitives (ADR §6.4). PKCE comes from
//! `crate::auth::generate_pkce` — shared with the Codex paths so a
//! security-primitive change can't drift between modules. Device
//! polling orchestration lands in a subsequent slice.

use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use url::Url;

use super::oauth::ResolvedProvider;
use crate::auth::generate_pkce;

/// 16-byte URL-safe `state` nonce for the OAuth authorize URL.
fn generate_state() -> String {
    let mut buf = [0u8; 16];
    getrandom::fill(&mut buf).expect("getrandom failed");
    URL_SAFE_NO_PAD.encode(buf)
}

/// Canonical resource URI for RFC 8707 §2 / MCP 2025-11-25 — the value sent
/// as `resource=` on the authorize URL and every token request so the AS
/// can audience-bind the issued token to this one MCP server. Normalizes
/// per RFC 3986 §6.2: lowercase scheme + host (the `url` crate does this on
/// parse), drop the default port (likewise normalized away), strip any
/// trailing slash from the path, and drop the fragment. Query is preserved
/// (RFC 8707 permits it). A bare-host URL with only a `/` path collapses to
/// no path so `https://mcp.example.com/` and `https://mcp.example.com` agree.
pub fn canonical_resource(server_url: &str) -> Result<String> {
    let url = Url::parse(server_url)
        .map_err(|e| anyhow!("invalid mcp server url for resource indicator: {e}"))?;
    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("mcp server url has no host for resource indicator"))?;
    let mut out = format!("{}://{}", url.scheme(), host);
    if let Some(port) = url.port() {
        out.push_str(&format!(":{port}"));
    }
    out.push_str(url.path().trim_end_matches('/'));
    if let Some(query) = url.query() {
        out.push('?');
        out.push_str(query);
    }
    Ok(out)
}

/// Result of `init_paste_authorize`: the URL to surface to the user, plus
/// the `code_verifier` + `state` the caller must persist under the
/// pending-login key for `complete_login` to validate the callback.
pub struct PasteAuthorize {
    pub url: String,
    pub code_verifier: String,
    pub state: String,
}

/// Start a paste-back OAuth 2.1 authorize flow. Generates the PKCE pair
/// and state nonce internally so the caller can't pair them up wrong;
/// builds the RFC 6749 authorize URL with `S256` PKCE and space-joined
/// scopes. `client_id` is caller-supplied: built-ins look it up via a
/// hard-coded helper (mirroring `auth::codex_client_id`); custom
/// providers carry it on `ResolvedProvider::Custom`. `redirect_uri` is
/// the provider's pinned callback for built-ins or a runtime-bound
/// `localhost:<port>` for custom paste-back flows. `resource` is the RFC
/// 8707 audience-binding indicator (`Some` = the canonical MCP server URI
/// for custom providers; `None` skips the param for built-ins whose AS may
/// reject an unrecognized `resource` with `invalid_target`).
pub fn init_paste_authorize(
    provider: &ResolvedProvider,
    client_id: &str,
    redirect_uri: &str,
    resource: Option<&str>,
) -> Result<PasteAuthorize> {
    let (code_verifier, code_challenge) = generate_pkce();
    let state = generate_state();
    let mut url = Url::parse(provider.authorize_url())?;
    url.query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("code_challenge", &code_challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("state", &state)
        .append_pair("scope", &provider.scopes().join(" "));
    if let Some(resource) = resource {
        url.query_pairs_mut().append_pair("resource", resource);
    }
    Ok(PasteAuthorize {
        url: url.to_string(),
        code_verifier,
        state,
    })
}

/// Parse a paste-back callback URL into its authorization `code` after
/// validating the `state` echo. OAuth 2.1 RFC 6749 §10.12 + §4.1.2 — a
/// mismatched `state` indicates CSRF / cross-flow contamination and MUST
/// reject the exchange before any token-endpoint round-trip. Tolerates
/// extra query params (vendor-specific tracking, `iss`, etc.).
pub fn parse_paste_callback(redirect_url: &str, expected_state: &str) -> Result<String> {
    let url = Url::parse(redirect_url).map_err(|e| anyhow!("invalid redirect URL: {e}"))?;
    let mut code = None;
    let mut state = None;
    let mut error = None;
    for (k, v) in url.query_pairs() {
        match k.as_ref() {
            "code" => code = Some(v.into_owned()),
            "state" => state = Some(v.into_owned()),
            "error" => error = Some(v.into_owned()),
            _ => {}
        }
    }
    if let Some(err) = error {
        return Err(anyhow!("authorize endpoint returned error: {err}"));
    }
    let got_state = state.ok_or_else(|| anyhow!("callback missing state"))?;
    if got_state != expected_state {
        return Err(anyhow!("state mismatch; flow rejected"));
    }
    code.ok_or_else(|| anyhow!("callback missing code"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::config::OAuthConfig;
    use crate::mcp::oauth::resolve;

    const TEST_REDIRECT: &str = "http://localhost:53692/callback";

    #[test]
    fn state_is_url_safe_and_unique() {
        let s = generate_state();
        let url_safe = s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        assert!(url_safe);
        assert_ne!(s, generate_state());
    }

    fn builtin_provider() -> ResolvedProvider {
        let cfg = OAuthConfig {
            provider: Some("anthropic-mcp".to_string()),
            ..Default::default()
        };
        resolve(&cfg).unwrap()
    }

    #[test]
    fn init_paste_authorize_threads_pkce_and_state_into_url() {
        let p = builtin_provider();
        let r = init_paste_authorize(&p, "client-xyz", TEST_REDIRECT, None).unwrap();
        assert!(r.url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(r.url.contains("response_type=code"));
        assert!(r.url.contains("client_id=client-xyz"));
        assert!(r.url.contains("code_challenge_method=S256"));
        assert!(r.url.contains(&format!("state={}", r.state)));
        assert!(!r.code_verifier.is_empty());
        // Built-in with `None` resource omits the RFC 8707 param entirely.
        assert!(!r.url.contains("resource="));
    }

    #[test]
    fn init_paste_authorize_percent_encodes_redirect_uri() {
        let p = builtin_provider();
        let r = init_paste_authorize(&p, "c", TEST_REDIRECT, None).unwrap();
        let want = "redirect_uri=http%3A%2F%2Flocalhost%3A53692%2Fcallback";
        assert!(r.url.contains(want));
    }

    #[test]
    fn init_paste_authorize_form_encodes_scope_spaces_as_plus() {
        let p = builtin_provider();
        let r = init_paste_authorize(&p, "c", TEST_REDIRECT, None).unwrap();
        assert!(r.url.contains("scope=org%3Acreate_api_key"));
        assert!(r.url.contains("user%3Amcp_servers"));
    }

    #[test]
    fn init_paste_authorize_rejects_unparseable_authorize_url() {
        let cfg = OAuthConfig {
            provider: Some("broken".to_string()),
            authorize_url: Some("not a url".to_string()),
            token_url: Some("https://example.com/token".to_string()),
            ..Default::default()
        };
        let p = resolve(&cfg).unwrap();
        assert!(init_paste_authorize(&p, "c", TEST_REDIRECT, None).is_err());
    }

    #[test]
    fn init_paste_authorize_for_custom_provider() {
        let cfg = OAuthConfig {
            provider: Some("linear".to_string()),
            authorize_url: Some("https://linear.app/oauth/authorize".to_string()),
            token_url: Some("https://api.linear.app/oauth/token".to_string()),
            client_id: Some("linear-client".to_string()),
            scopes: vec!["read".to_string(), "write".to_string()],
            ..Default::default()
        };
        let p = resolve(&cfg).unwrap();
        let r = init_paste_authorize(
            &p,
            "linear-client",
            TEST_REDIRECT,
            Some("https://mcp.linear.app/sse"),
        )
        .unwrap();
        assert!(r.url.starts_with("https://linear.app/oauth/authorize?"));
        assert!(r.url.contains("scope=read+write"));
        // RFC 8707: custom provider carries the audience-binding resource.
        assert!(r
            .url
            .contains("resource=https%3A%2F%2Fmcp.linear.app%2Fsse"));
    }

    #[test]
    fn canonical_resource_lowercases_and_strips_default_port_and_trailing_slash() {
        let r = canonical_resource("HTTPS://MCP.Example.COM:443/mcp/").unwrap();
        assert_eq!(r, "https://mcp.example.com/mcp");
    }

    #[test]
    fn canonical_resource_drops_fragment_keeps_explicit_port_and_query() {
        let r = canonical_resource("http://localhost:8080/sse?tenant=acme#frag").unwrap();
        assert_eq!(r, "http://localhost:8080/sse?tenant=acme");
    }

    #[test]
    fn canonical_resource_bare_host_has_no_trailing_slash() {
        let r = canonical_resource("https://mcp.example.com").unwrap();
        assert_eq!(r, "https://mcp.example.com");
    }

    #[test]
    fn canonical_resource_rejects_unparseable_url() {
        assert!(canonical_resource("not a url").is_err());
    }

    #[test]
    fn parse_paste_callback_extracts_code_when_state_matches() {
        let url = "http://localhost:53692/callback?code=abc123&state=xyz";
        let code = parse_paste_callback(url, "xyz").unwrap();
        assert_eq!(code, "abc123");
    }

    #[test]
    fn parse_paste_callback_tolerates_extra_query_params() {
        let url = "http://localhost:53692/cb?iss=https%3A%2F%2Fauth&state=s&code=c&tracking=1";
        let code = parse_paste_callback(url, "s").unwrap();
        assert_eq!(code, "c");
    }

    #[test]
    fn parse_paste_callback_rejects_state_mismatch() {
        let url = "http://localhost:53692/cb?code=c&state=wrong";
        let err = parse_paste_callback(url, "want").unwrap_err().to_string();
        assert!(err.contains("state mismatch"), "got: {err}");
    }

    #[test]
    fn parse_paste_callback_rejects_missing_state() {
        let url = "http://localhost:53692/cb?code=c";
        let err = parse_paste_callback(url, "x").unwrap_err().to_string();
        assert!(err.contains("missing state"), "got: {err}");
    }

    #[test]
    fn parse_paste_callback_rejects_missing_code() {
        let url = "http://localhost:53692/cb?state=x";
        let err = parse_paste_callback(url, "x").unwrap_err().to_string();
        assert!(err.contains("missing code"), "got: {err}");
    }

    #[test]
    fn parse_paste_callback_surfaces_authorize_error() {
        let url = "http://localhost:53692/cb?error=access_denied&state=x";
        let err = parse_paste_callback(url, "x").unwrap_err().to_string();
        assert!(err.contains("access_denied"), "got: {err}");
    }

    #[test]
    fn parse_paste_callback_rejects_unparseable_url() {
        let url = "not a url";
        let err = parse_paste_callback(url, "x").unwrap_err().to_string();
        assert!(err.contains("invalid redirect URL"), "got: {err}");
    }
}
