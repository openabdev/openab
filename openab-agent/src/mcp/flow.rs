//! OAuth 2.1 paste-back flow primitives (ADR §6.4). PKCE comes from
//! `crate::auth::generate_pkce` — shared with the Codex paths so a
//! security-primitive change can't drift between modules. Orchestration
//! (device polling, callback parsing) lands in subsequent slices.

// First prod caller (§6.4 login orchestration) lands in the next slice;
// until then every item is reachable only via tests, so
// `clippy --features mcp -D warnings` would flag dead_code.
#![allow(dead_code)]

use anyhow::Result;
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
/// `localhost:<port>` for custom paste-back flows.
pub fn init_paste_authorize(
    provider: &ResolvedProvider,
    client_id: &str,
    redirect_uri: &str,
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
    Ok(PasteAuthorize {
        url: url.to_string(),
        code_verifier,
        state,
    })
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
        let r = init_paste_authorize(&p, "client-xyz", TEST_REDIRECT).unwrap();
        assert!(r.url.starts_with("https://claude.ai/oauth/authorize?"));
        assert!(r.url.contains("response_type=code"));
        assert!(r.url.contains("client_id=client-xyz"));
        assert!(r.url.contains("code_challenge_method=S256"));
        assert!(r.url.contains(&format!("state={}", r.state)));
        assert!(!r.code_verifier.is_empty());
    }

    #[test]
    fn init_paste_authorize_percent_encodes_redirect_uri() {
        let p = builtin_provider();
        let r = init_paste_authorize(&p, "c", TEST_REDIRECT).unwrap();
        let want = "redirect_uri=http%3A%2F%2Flocalhost%3A53692%2Fcallback";
        assert!(r.url.contains(want));
    }

    #[test]
    fn init_paste_authorize_form_encodes_scope_spaces_as_plus() {
        let p = builtin_provider();
        let r = init_paste_authorize(&p, "c", TEST_REDIRECT).unwrap();
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
        assert!(init_paste_authorize(&p, "c", TEST_REDIRECT).is_err());
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
        let r = init_paste_authorize(&p, "linear-client", TEST_REDIRECT).unwrap();
        assert!(r.url.starts_with("https://linear.app/oauth/authorize?"));
        assert!(r.url.contains("scope=read+write"));
    }
}
