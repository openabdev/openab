use anyhow::{anyhow, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{BufRead, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const REFRESH_SKEW_SECONDS: u64 = 120;

const CODEX_AUTHORIZE_URL: &str = "https://auth.openai.com/oauth/authorize";
const CODEX_TOKEN_URL: &str = "https://auth.openai.com/oauth/token";
const CODEX_DEVICE_AUTH_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/usercode";
const CODEX_DEVICE_TOKEN_URL: &str = "https://auth.openai.com/api/accounts/deviceauth/token";
const CODEX_DEVICE_REDIRECT_URI: &str = "https://auth.openai.com/deviceauth/callback";
const REDIRECT_PORT: u16 = 1455;

fn codex_client_id() -> String {
    std::env::var("OPENAB_AGENT_OAUTH_CLIENT_ID")
        .unwrap_or_else(|_| "app_EMoamEEZ73f0CkXaXp7hrann".to_string())
}

fn redirect_uri() -> String {
    format!("http://localhost:{REDIRECT_PORT}/auth/callback")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenStore {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64,
    pub token_endpoint: String,
    pub provider: String,
}

fn auth_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(home)
        .join(".openab")
        .join("agent")
        .join("auth.json")
}

pub fn load_tokens() -> Result<TokenStore> {
    let path = auth_path();
    let data = std::fs::read_to_string(&path).map_err(|_| {
        anyhow!(
            "No credentials found at {}. Run `openab-agent auth codex-oauth` first.",
            path.display()
        )
    })?;
    serde_json::from_str(&data).map_err(|e| anyhow!("Invalid auth.json: {e}"))
}

fn save_tokens(store: &TokenStore) -> Result<()> {
    let path = auth_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_string_pretty(store)?;
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(data.as_bytes())?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, &data)?;
    }
    Ok(())
}

fn is_expired(store: &TokenStore) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    now + REFRESH_SKEW_SECONDS >= store.expires_at
}

pub async fn get_valid_token() -> Result<String> {
    let mut store = load_tokens()?;
    if is_expired(&store) {
        store = refresh_token(&store).await?;
        save_tokens(&store)?;
    }
    Ok(store.access_token)
}

pub async fn force_refresh() -> Result<String> {
    let store = load_tokens()?;
    let new_store = refresh_token(&store).await?;
    save_tokens(&new_store)?;
    Ok(new_store.access_token)
}

async fn refresh_token(store: &TokenStore) -> Result<TokenStore> {
    let client_id = codex_client_id();
    let client = reqwest::Client::new();
    let resp = client
        .post(&store.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", store.refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Token refresh failed (HTTP {status}): {body}. Run `openab-agent auth codex-oauth` again."));
    }
    let payload: serde_json::Value = resp.json().await?;
    let access_token = payload["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No access_token in refresh response"))?;
    let new_refresh = payload["refresh_token"]
        .as_str()
        .unwrap_or(&store.refresh_token);
    let expires_in = payload["expires_in"].as_u64().unwrap_or(3600);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    Ok(TokenStore {
        access_token: access_token.to_string(),
        refresh_token: new_refresh.to_string(),
        expires_at: now + expires_in,
        token_endpoint: store.token_endpoint.clone(),
        provider: store.provider.clone(),
    })
}

fn generate_pkce() -> (String, String) {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("getrandom failed");
    let verifier = URL_SAFE_NO_PAD.encode(buf);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    (verifier, challenge)
}

// Browser PKCE flow
pub async fn login_browser_flow(no_browser: bool) -> Result<()> {
    let client_id = codex_client_id();
    let (code_verifier, code_challenge) = generate_pkce();
    let mut state_buf = [0u8; 16];
    getrandom::fill(&mut state_buf).expect("getrandom failed");
    let state = URL_SAFE_NO_PAD.encode(state_buf);
    let redir_str = redirect_uri();
    let redir = urlencoding::encode(&redir_str);
    let auth_url = format!("{CODEX_AUTHORIZE_URL}?client_id={client_id}&redirect_uri={redir}&response_type=code&scope=openid+profile+email+offline_access&code_challenge={code_challenge}&code_challenge_method=S256&state={state}&id_token_add_organizations=true&codex_cli_simplified_flow=true&originator=openab-agent");

    let listener = TcpListener::bind(format!("127.0.0.1:{REDIRECT_PORT}")).map_err(|e| {
        anyhow!("Failed to bind port {REDIRECT_PORT}: {e}. Is another instance running?")
    })?;

    if no_browser {
        println!("Open this URL in your browser:\n");
        println!("  {auth_url}\n");
        println!("After approving, your browser will redirect to a localhost URL.");
        println!("Copy the full URL from the browser address bar and paste it here:\n");

        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| anyhow!("Failed to read input: {e}"))?;
        let input = input.trim();
        if input.is_empty() {
            return Err(anyhow!("No URL provided"));
        }
        let url = url::Url::parse(input).map_err(|_| anyhow!("Invalid URL: {input}"))?;

        // Skip TCP listener for paste flow
        let code = url
            .query_pairs()
            .find(|(k, _)| k == "code")
            .map(|(_, v)| v.to_string())
            .ok_or_else(|| {
                let error = url
                    .query_pairs()
                    .find(|(k, _)| k == "error")
                    .map(|(_, v)| v.to_string());
                anyhow!(
                    "No code in URL. Error: {}",
                    error.unwrap_or_else(|| "unknown".into())
                )
            })?;
        let cb_state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.to_string());
        if cb_state.as_deref() != Some(&state) {
            return Err(anyhow!("State mismatch"));
        }

        // Exchange code for tokens
        let client = reqwest::Client::new();
        let resp = client
            .post(CODEX_TOKEN_URL)
            .form(&[
                ("grant_type", "authorization_code"),
                ("client_id", client_id.as_str()),
                ("code", code.as_str()),
                ("code_verifier", code_verifier.as_str()),
                ("redirect_uri", redirect_uri().as_str()),
            ])
            .send()
            .await?;
        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!("Token exchange failed: {body}"));
        }
        let payload: serde_json::Value = resp.json().await?;
        let access_token = payload["access_token"]
            .as_str()
            .ok_or_else(|| anyhow!("No access_token"))?;
        let refresh_token_val = payload["refresh_token"]
            .as_str()
            .ok_or_else(|| anyhow!("No refresh_token"))?;
        let expires_in = payload["expires_in"].as_u64().unwrap_or(3600);
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let store = TokenStore {
            access_token: access_token.to_string(),
            refresh_token: refresh_token_val.to_string(),
            expires_at: now + expires_in,
            token_endpoint: CODEX_TOKEN_URL.to_string(),
            provider: "codex".to_string(),
        };
        save_tokens(&store)?;
        println!(
            "\n\u{2705} Login successful! Token saved to {:?}",
            auth_path()
        );
        return Ok(());
    } else {
        println!("Opening browser for authentication...\n");
        if open::that(&auth_url).is_err() {
            println!("Could not open browser. Open this URL manually:\n");
            println!("  {auth_url}\n");
        }
        println!("Waiting for callback...");
    }

    listener.set_nonblocking(false)?;
    let (mut stream, _) = listener
        .accept()
        .map_err(|e| anyhow!("Failed to accept callback: {e}"))?;
    let mut reader = std::io::BufReader::new(&stream);
    let mut request_line = String::new();
    reader.read_line(&mut request_line)?;

    let path = request_line.split_whitespace().nth(1).unwrap_or("");
    let url = url::Url::parse(&format!("http://localhost{path}"))
        .map_err(|_| anyhow!("Invalid callback URL"))?;
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| {
            let error = url
                .query_pairs()
                .find(|(k, _)| k == "error")
                .map(|(_, v)| v.to_string());
            anyhow!(
                "No code in callback. Error: {}",
                error.unwrap_or_else(|| "unknown".into())
            )
        })?;
    let cb_state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string());
    if cb_state.as_deref() != Some(&state) {
        return Err(anyhow!("State mismatch in callback"));
    }

    let response = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\n\r\n<html><body><h1>Authentication successful!</h1><p>You can close this tab.</p></body></html>";
    let _ = stream.write_all(response.as_bytes());

    let client = reqwest::Client::new();
    let resp = client
        .post(CODEX_TOKEN_URL)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("code_verifier", code_verifier.as_str()),
            ("redirect_uri", redirect_uri().as_str()),
        ])
        .send()
        .await?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Token exchange failed: {body}"));
    }
    let payload: serde_json::Value = resp.json().await?;
    let access_token = payload["access_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No access_token"))?;
    let refresh_token_val = payload["refresh_token"]
        .as_str()
        .ok_or_else(|| anyhow!("No refresh_token"))?;
    let expires_in = payload["expires_in"].as_u64().unwrap_or(3600);
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let store = TokenStore {
        access_token: access_token.to_string(),
        refresh_token: refresh_token_val.to_string(),
        expires_at: now + expires_in,
        token_endpoint: CODEX_TOKEN_URL.to_string(),
        provider: "codex".to_string(),
    };
    save_tokens(&store)?;
    println!(
        "\n\u{2705} Login successful! Token saved to {:?}",
        auth_path()
    );
    Ok(())
}

// Device code flow
pub async fn login_codex_device_flow() -> Result<()> {
    println!("Starting OpenAI Codex device-code login...\n");
    let client = reqwest::Client::new();
    let client_id = codex_client_id();

    let resp = client
        .post(CODEX_DEVICE_AUTH_URL)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({"client_id": client_id}))
        .send()
        .await?;
    if !resp.status().is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(anyhow!("Device authorization request failed: {body}"));
    }
    let device_resp: serde_json::Value = resp.json().await?;
    let device_auth_id = device_resp["device_auth_id"]
        .as_str()
        .ok_or_else(|| anyhow!("No device_auth_id"))?;
    let user_code = device_resp["user_code"]
        .as_str()
        .ok_or_else(|| anyhow!("No user_code"))?;
    let interval = device_resp["interval"]
        .as_str()
        .and_then(|s| s.parse::<u64>().ok())
        .or_else(|| device_resp["interval"].as_u64())
        .unwrap_or(5)
        .max(5);

    println!("  Go to:      https://auth.openai.com/codex/device");
    println!("  Enter code: {}\n", user_code);
    println!("Waiting for authorization...");

    let deadline = tokio::time::Instant::now() + tokio::time::Duration::from_secs(600);
    let mut poll_interval = interval;
    loop {
        if tokio::time::Instant::now() >= deadline {
            return Err(anyhow!("Device flow timed out after 10 minutes."));
        }
        tokio::time::sleep(tokio::time::Duration::from_secs(poll_interval)).await;
        let resp = client.post(CODEX_DEVICE_TOKEN_URL)
            .json(&serde_json::json!({"client_id": client_id, "device_auth_id": device_auth_id, "user_code": user_code}))
            .send().await?;
        let status = resp.status();
        let payload: serde_json::Value = resp.json().await?;
        if status.is_success() {
            let auth_code = payload["authorization_code"]
                .as_str()
                .ok_or_else(|| anyhow!("No authorization_code: {payload}"))?;
            let code_verifier = payload["code_verifier"]
                .as_str()
                .ok_or_else(|| anyhow!("No code_verifier: {payload}"))?;
            let token_resp = client
                .post(CODEX_TOKEN_URL)
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", client_id.as_str()),
                    ("code", auth_code),
                    ("code_verifier", code_verifier),
                    ("redirect_uri", CODEX_DEVICE_REDIRECT_URI),
                ])
                .send()
                .await?;
            if !token_resp.status().is_success() {
                let body = token_resp.text().await.unwrap_or_default();
                return Err(anyhow!("Token exchange failed: {body}"));
            }
            let token_payload: serde_json::Value = token_resp.json().await?;
            let access_token = token_payload["access_token"]
                .as_str()
                .ok_or_else(|| anyhow!("No access_token: {token_payload}"))?;
            let refresh_token_val = token_payload["refresh_token"]
                .as_str()
                .ok_or_else(|| anyhow!("No refresh_token"))?;
            let expires_in = token_payload["expires_in"].as_u64().unwrap_or(3600);
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
            let store = TokenStore {
                access_token: access_token.to_string(),
                refresh_token: refresh_token_val.to_string(),
                expires_at: now + expires_in,
                token_endpoint: CODEX_TOKEN_URL.to_string(),
                provider: "codex".to_string(),
            };
            save_tokens(&store)?;
            println!(
                "\n\u{2705} Login successful! Token saved to {:?}",
                auth_path()
            );
            return Ok(());
        }
        let error_code = payload["error"]["code"]
            .as_str()
            .or_else(|| payload["error"].as_str())
            .unwrap_or_default();
        match error_code {
            "authorization_pending" | "deviceauth_authorization_pending" => continue,
            "slow_down" => {
                poll_interval += 5;
                continue;
            }
            "expired_token" | "deviceauth_expired" => return Err(anyhow!("Device code expired.")),
            "access_denied" => return Err(anyhow!("Authorization denied by user.")),
            _ => {
                if status.as_u16() == 403 || status.as_u16() == 404 {
                    continue;
                }
                return Err(anyhow!(
                    "Device-code error: {error_code} \u{2014} {payload}"
                ));
            }
        }
    }
}

pub fn show_status() {
    match load_tokens() {
        Ok(store) => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let expired = now + REFRESH_SKEW_SECONDS >= store.expires_at;
            let masked = if store.access_token.len() > 12 {
                format!(
                    "{}...{}",
                    &store.access_token[..8],
                    &store.access_token[store.access_token.len() - 4..]
                )
            } else {
                "****".to_string()
            };
            println!("Provider:  {}", store.provider);
            println!("Token:     {}", masked);
            println!(
                "Expires:   {} ({})",
                store.expires_at,
                if expired { "EXPIRED" } else { "valid" }
            );
            println!("File:      {:?}", auth_path());
        }
        Err(e) => {
            println!("Not authenticated: {e}\nRun: openab-agent auth codex-oauth");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_store(expires_at: u64) -> TokenStore {
        TokenStore {
            access_token: "test_access_token_value".to_string(),
            refresh_token: "test_refresh".to_string(),
            expires_at,
            token_endpoint: "https://example.com/token".to_string(),
            provider: "codex".to_string(),
        }
    }

    #[test]
    fn test_is_expired_future_token() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(!is_expired(&make_store(now + 3600)));
    }

    #[test]
    fn test_is_expired_past_token() {
        assert!(is_expired(&make_store(0)));
    }

    #[test]
    fn test_is_expired_within_skew() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        assert!(is_expired(&make_store(now + 60)));
    }

    #[test]
    fn test_auth_path() {
        assert!(auth_path()
            .to_string_lossy()
            .contains(".openab/agent/auth.json"));
    }

    #[test]
    fn test_codex_client_id_default() {
        unsafe { std::env::remove_var("OPENAB_AGENT_OAUTH_CLIENT_ID") };
        assert_eq!(codex_client_id(), "app_EMoamEEZ73f0CkXaXp7hrann");
    }

    #[test]
    fn test_codex_client_id_override() {
        unsafe { std::env::set_var("OPENAB_AGENT_OAUTH_CLIENT_ID", "custom_id") };
        assert_eq!(codex_client_id(), "custom_id");
        unsafe { std::env::remove_var("OPENAB_AGENT_OAUTH_CLIENT_ID") };
    }

    #[test]
    fn test_generate_pkce() {
        let (verifier, challenge) = generate_pkce();
        assert!(!verifier.is_empty());
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        assert_eq!(challenge, expected);
    }
}
