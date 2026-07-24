//! OAuth 2.1 paste-back helpers (ADR §6.4). The authorize-URL build and
//! code exchange now run through rmcp's `AuthorizationManager` (PKCE + CSRF
//! state live in its in-memory `StateStore`); this module only normalizes
//! the RFC 8707 resource indicator and parses the pasted redirect.

use anyhow::{anyhow, Result};
use url::Url;

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

/// Parse a paste-back callback URL into its `(code, state)` pair. The CSRF
/// `state` echo is validated downstream by rmcp's `exchange_code_for_token`
/// against the `StateStore` entry it stored at authorize time, so this only
/// extracts the two values and surfaces an `error=` response. Tolerates
/// extra query params (vendor tracking, `iss`, etc.).
pub fn parse_redirect_params(redirect_url: &str) -> Result<(String, String)> {
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
    let code = code.ok_or_else(|| anyhow!("callback missing code"))?;
    let state = state.ok_or_else(|| anyhow!("callback missing state"))?;
    Ok((code, state))
}

/// PKCE downgrade gate (ADR §6.4). rmcp only *warns* when an authorization
/// server advertises `code_challenge_methods_supported` without `S256`; openab
/// refuses outright so a paste-back login never proceeds on a downgraded
/// `plain` challenge. A server that omits the field entirely is left to the
/// "send S256, trust the AS" path (`None` → `Ok`).
pub fn ensure_s256_supported(name: &str, methods: Option<&[String]>) -> Result<()> {
    if let Some(methods) = methods {
        if !methods.iter().any(|m| m == "S256") {
            return Err(anyhow!(
                "mcp server {name:?} authorization server does not advertise S256 in \
                 code_challenge_methods_supported ({methods:?}); refusing to downgrade PKCE"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn parse_redirect_params_extracts_code_and_state() {
        let url = "http://localhost:53692/callback?code=abc123&state=xyz";
        let (code, state) = parse_redirect_params(url).unwrap();
        assert_eq!(code, "abc123");
        assert_eq!(state, "xyz");
    }

    #[test]
    fn parse_redirect_params_tolerates_extra_query_params() {
        let url = "http://localhost:53692/cb?iss=https%3A%2F%2Fauth&state=s&code=c&tracking=1";
        let (code, state) = parse_redirect_params(url).unwrap();
        assert_eq!(code, "c");
        assert_eq!(state, "s");
    }

    #[test]
    fn parse_redirect_params_rejects_missing_state() {
        let url = "http://localhost:53692/cb?code=c";
        let err = parse_redirect_params(url).unwrap_err().to_string();
        assert!(err.contains("missing state"), "got: {err}");
    }

    #[test]
    fn parse_redirect_params_rejects_missing_code() {
        let url = "http://localhost:53692/cb?state=x";
        let err = parse_redirect_params(url).unwrap_err().to_string();
        assert!(err.contains("missing code"), "got: {err}");
    }

    #[test]
    fn parse_redirect_params_surfaces_authorize_error() {
        let url = "http://localhost:53692/cb?error=access_denied&state=x";
        let err = parse_redirect_params(url).unwrap_err().to_string();
        assert!(err.contains("access_denied"), "got: {err}");
    }

    #[test]
    fn parse_redirect_params_rejects_unparseable_url() {
        let url = "not a url";
        let err = parse_redirect_params(url).unwrap_err().to_string();
        assert!(err.contains("invalid redirect URL"), "got: {err}");
    }

    #[test]
    fn ensure_s256_rejects_methods_advertised_without_s256() {
        let methods = vec!["plain".to_string()];
        let err = ensure_s256_supported("linear", Some(&methods))
            .unwrap_err()
            .to_string();
        assert!(err.contains("refusing to downgrade PKCE"), "got: {err}");
        assert!(err.contains("S256"), "got: {err}");
    }

    #[test]
    fn ensure_s256_accepts_when_s256_is_among_advertised_methods() {
        let methods = vec!["plain".to_string(), "S256".to_string()];
        assert!(ensure_s256_supported("linear", Some(&methods)).is_ok());
    }

    #[test]
    fn ensure_s256_accepts_when_field_is_absent() {
        // AS omits the field entirely → trust-the-AS path, we still send S256.
        assert!(ensure_s256_supported("linear", None).is_ok());
    }
}
