use anyhow::Result;
use axum::extract::State;
use crate::media::format_bytes;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

pub struct WecomConfig {
    pub corp_id: String,
    pub agent_id: String,
    pub secret: String,
    pub token: String,
    pub encoding_aes_key: String,
    pub webhook_path: String,
    pub streaming_enabled: bool,
    pub debounce_secs: u64,
}

impl WecomConfig {
    pub fn from_env() -> Option<Self> {
        Self::from_reader(|k| std::env::var(k).ok())
    }

    /// Build config from an arbitrary string reader. Tests use this with a
    /// HashMap so they don't mutate process-wide environment variables —
    /// `env::set_var` races other tests under cargo's parallel runner.
    fn from_reader<F: Fn(&str) -> Option<String>>(read: F) -> Option<Self> {
        let corp_id = read("WECOM_CORP_ID")?;
        let secret = read("WECOM_SECRET")?;
        let token = read("WECOM_TOKEN")?;
        let encoding_aes_key = read("WECOM_ENCODING_AES_KEY")?;
        let agent_id = read("WECOM_AGENT_ID")?;
        if agent_id.parse::<u64>().is_err() {
            warn!("WECOM_AGENT_ID must be a numeric value, got '{}'", agent_id);
            return None;
        }
        let webhook_path = read("WECOM_WEBHOOK_PATH").unwrap_or_else(|| "/webhook/wecom".into());
        // Streaming opts-in: WeCom callback mode has no edit-message API, so
        // streaming is implemented via thinking-placeholder + recall + resend,
        // which causes a brief client flicker. Default off; set to true only if
        // the UX tradeoff is acceptable.
        let streaming_enabled = read("WECOM_STREAMING_ENABLED")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false);
        let debounce_secs = read("WECOM_DEBOUNCE_SECS")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(3);

        if encoding_aes_key.len() != 43 {
            warn!("WECOM_ENCODING_AES_KEY must be 43 characters, got {}", encoding_aes_key.len());
            return None;
        }

        info!(
            corp_id = %corp_id,
            agent_id = %agent_id,
            streaming_enabled,
            debounce_secs,
            "wecom adapter configured"
        );
        Some(Self {
            corp_id,
            agent_id,
            secret,
            token,
            encoding_aes_key,
            webhook_path,
            streaming_enabled,
            debounce_secs,
        })
    }
}

fn decode_aes_key(encoding_aes_key: &str) -> anyhow::Result<Vec<u8>> {
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    use base64::Engine;
    // WeCom's EncodingAESKey is 43 base64 chars without trailing padding.
    // Append "=" to make it a 44-char standard base64 string before decoding.
    // Indifferent + allow_trailing_bits accommodate WeCom's non-standard
    // encoding: the 43rd char's last 2 bits are not part of the output and
    // must be ignored rather than rejected.
    let padded = format!("{}=", encoding_aes_key);
    let config = GeneralPurposeConfig::new()
        .with_decode_padding_mode(DecodePaddingMode::Indifferent)
        .with_decode_allow_trailing_bits(true);
    let engine = GeneralPurpose::new(&base64::alphabet::STANDARD, config);
    let key = engine
        .decode(&padded)
        .map_err(|e| anyhow::anyhow!("encoding_aes_key base64 decode failed: {e}"))?;
    anyhow::ensure!(
        key.len() == 32,
        "encoding_aes_key must decode to 32 bytes, got {}",
        key.len()
    );
    Ok(key)
}

fn compute_signature(token: &str, timestamp: &str, nonce: &str, encrypt: &str) -> String {
    use sha1::Digest;
    let mut parts = [token, timestamp, nonce, encrypt];
    parts.sort_unstable();
    let joined: String = parts.concat();
    let hash = sha1::Sha1::digest(joined.as_bytes());
    format!("{:x}", hash)
}

fn verify_signature(
    token: &str,
    timestamp: &str,
    nonce: &str,
    encrypt: &str,
    expected: &str,
) -> bool {
    let computed = compute_signature(token, timestamp, nonce, encrypt);
    tracing::debug!(
        computed = %computed,
        expected = %expected,
        token_len = token.len(),
        encrypt_len = encrypt.len(),
        "signature comparison"
    );
    subtle::ConstantTimeEq::ct_eq(computed.as_bytes(), expected.as_bytes()).into()
}

fn decrypt_message(
    encoding_aes_key: &str,
    encrypted: &str,
    expected_corp_id: &str,
) -> anyhow::Result<String> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit};
    use base64::Engine;

    let key = decode_aes_key(encoding_aes_key)?;
    let iv = &key[..16];

    let cipher_bytes = base64::engine::general_purpose::STANDARD
        .decode(encrypted)
        .map_err(|e| anyhow::anyhow!("base64 decode failed: {e}"))?;

    if cipher_bytes.is_empty() || cipher_bytes.len() % 16 != 0 {
        anyhow::bail!("ciphertext length {} not a multiple of 16", cipher_bytes.len());
    }

    type Aes256CbcDec = cbc::Decryptor<aes::Aes256>;
    let decryptor = Aes256CbcDec::new_from_slices(&key, iv)
        .map_err(|e| anyhow::anyhow!("aes init failed: {e}"))?;

    let mut buf = cipher_bytes.to_vec();
    // WeCom uses PKCS7 with block_size=32, not 16. Decrypt without padding validation
    // and strip padding manually.
    let plaintext = decryptor
        .decrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf)
        .map_err(|e| anyhow::anyhow!("aes decrypt failed: {e}"))?;

    // Strip WeCom PKCS7 padding (block_size=32): last byte indicates pad length (1-32)
    let pad_byte = *plaintext.last().ok_or_else(|| anyhow::anyhow!("empty plaintext"))? as usize;
    if pad_byte == 0 || pad_byte > 32 || pad_byte > plaintext.len() {
        anyhow::bail!("invalid wecom padding value: {pad_byte}");
    }
    let pad_start = plaintext.len() - pad_byte;
    if !plaintext[pad_start..].iter().all(|&b| b as usize == pad_byte) {
        anyhow::bail!("invalid PKCS#7 padding: not all padding bytes match");
    }
    let plaintext = &plaintext[..pad_start];

    // Plaintext structure: random(16) + msg_len(4, big-endian) + msg + corp_id
    if plaintext.len() < 20 {
        anyhow::bail!("decrypted payload too short");
    }
    let msg_len =
        u32::from_be_bytes([plaintext[16], plaintext[17], plaintext[18], plaintext[19]]) as usize;
    if plaintext.len() < 20 + msg_len {
        anyhow::bail!("msg_len exceeds payload size");
    }
    let msg = &plaintext[20..20 + msg_len];
    let corp_id = &plaintext[20 + msg_len..];

    let corp_id_str =
        std::str::from_utf8(corp_id).map_err(|e| anyhow::anyhow!("corp_id not utf8: {e}"))?;
    if corp_id_str != expected_corp_id {
        anyhow::bail!("corp_id mismatch: expected {expected_corp_id}, got {corp_id_str}");
    }

    String::from_utf8(msg.to_vec()).map_err(|e| anyhow::anyhow!("message not utf8: {e}"))
}

// --- Deduplication ---

const DEDUPE_TTL_SECS: u64 = 30;
const DEDUPE_MAX_SIZE: usize = 10_000;

struct DedupeCache {
    entries: std::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>,
}

impl DedupeCache {
    fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    fn check_and_insert(&self, msg_id: &str) -> bool {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let now = std::time::Instant::now();

        if entries.len() >= DEDUPE_MAX_SIZE {
            entries.retain(|_, t| now.duration_since(*t).as_secs() < DEDUPE_TTL_SECS);
        }

        if let Some(t) = entries.get(msg_id) {
            if now.duration_since(*t).as_secs() < DEDUPE_TTL_SECS {
                return false;
            }
        }

        entries.insert(msg_id.to_string(), now);
        true
    }
}

// --- Token cache ---

pub const WECOM_API_BASE: &str = "https://qyapi.weixin.qq.com";
const TOKEN_REFRESH_MARGIN_SECS: u64 = 300;

pub struct WecomTokenCache {
    inner: RwLock<Option<(String, std::time::Instant, u64)>>,
    base_url: String,
}

impl WecomTokenCache {
    fn new() -> Self {
        Self {
            inner: RwLock::new(None),
            base_url: WECOM_API_BASE.into(),
        }
    }

    #[cfg(test)]
    fn with_base_url(base_url: String) -> Self {
        Self {
            inner: RwLock::new(None),
            base_url,
        }
    }

    pub async fn get_token(
        &self,
        client: &reqwest::Client,
        corp_id: &str,
        secret: &str,
    ) -> Result<String> {
        // Fast path: read lock
        {
            let guard = self.inner.read().await;
            if let Some((ref token, created_at, expires_in)) = *guard {
                let elapsed = created_at.elapsed().as_secs();
                if elapsed + TOKEN_REFRESH_MARGIN_SECS < expires_in {
                    return Ok(token.clone());
                }
            }
        }

        // Slow path: write lock + refresh
        let mut guard = self.inner.write().await;
        // Double-check after acquiring write lock
        if let Some((ref token, created_at, expires_in)) = *guard {
            let elapsed = created_at.elapsed().as_secs();
            if elapsed + TOKEN_REFRESH_MARGIN_SECS < expires_in {
                return Ok(token.clone());
            }
        }

        // WeCom's gettoken API requires `corpsecret` as a query parameter — the
        // protocol mandates this, we can't move it to a header. Operators must
        // configure their reverse proxy / load balancer to redact query strings
        // on `/cgi-bin/gettoken` paths before logging access logs. We do not log
        // this URL anywhere from the gateway side.
        let url = format!(
            "{}/cgi-bin/gettoken?corpid={}&corpsecret={}",
            self.base_url, corp_id, secret
        );
        let resp: serde_json::Value = client.get(&url).send().await?.json().await?;

        let errcode = resp["errcode"].as_i64().unwrap_or(-1);
        if errcode != 0 {
            anyhow::bail!(
                "wecom gettoken failed: errcode={}, errmsg={}",
                errcode,
                resp["errmsg"]
            );
        }

        let token = resp["access_token"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("missing access_token in response"))?
            .to_string();
        let expires_in = resp["expires_in"].as_u64().unwrap_or(7200);

        *guard = Some((token.clone(), std::time::Instant::now(), expires_in));
        Ok(token)
    }

    pub async fn force_refresh(
        &self,
        client: &reqwest::Client,
        corp_id: &str,
        secret: &str,
    ) -> Result<String> {
        let mut guard = self.inner.write().await;
        *guard = None;
        drop(guard);
        self.get_token(client, corp_id, secret).await
    }
}

// --- Adapter ---

struct PendingStream {
    text_watch: tokio::sync::watch::Sender<String>,
}

type PendingMap = Arc<std::sync::Mutex<std::collections::HashMap<String, PendingStream>>>;

pub struct WecomAdapter {
    pub config: WecomConfig,
    pub token_cache: Arc<WecomTokenCache>,
    client: reqwest::Client,
    dedupe: DedupeCache,
    pending_streams: PendingMap,
}

impl WecomAdapter {
    pub fn new(config: WecomConfig) -> Self {
        Self {
            token_cache: Arc::new(WecomTokenCache::new()),
            client: reqwest::Client::new(),
            dedupe: DedupeCache::new(),
            pending_streams: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            config,
        }
    }


    pub async fn handle_reply(
        &self,
        reply: &crate::schema::GatewayReply,
        event_tx: &tokio::sync::broadcast::Sender<String>,
    ) {
        if let Some(cmd) = reply.command.as_deref() {
            match cmd {
                "add_reaction" | "remove_reaction" | "create_topic" => {
                    info!(command = cmd, "wecom: ignoring unsupported command");
                    return;
                }
                "edit_message" => {
                    self.handle_edit_message(reply);
                    return;
                }
                _ => {}
            }
        }

        let text = &reply.content.text;
        if text.is_empty() {
            return;
        }

        let to_user = reply
            .channel
            .id
            .rsplit(':')
            .next()
            .unwrap_or(&reply.channel.id);

        let has_pending = {
            let pending = self.pending_streams.lock().unwrap_or_else(|e| e.into_inner());
            pending.contains_key(&reply.channel.id)
        };
        let is_streaming_placeholder = reply.request_id.is_some() && !has_pending;
        if is_streaming_placeholder {
            // Optionally send a thinking placeholder. With streaming disabled
            // (default), buffer chunks silently and send the consolidated text
            // when the debounce settles — no recall/flicker.
            let placeholder_id = if self.config.streaming_enabled {
                info!(to_user = to_user, "wecom: sending thinking placeholder");
                match self.send_text(to_user, "⏳...").await {
                    Ok(id) => Some(id),
                    Err(e) => {
                        warn!("wecom send thinking failed: {e}");
                        return;
                    }
                }
            } else {
                None
            };

            let (text_tx, text_rx) = tokio::sync::watch::channel(String::new());
            {
                let mut pending = self.pending_streams.lock().unwrap_or_else(|e| e.into_inner());
                pending.insert(reply.channel.id.clone(), PendingStream {
                    text_watch: text_tx,
                });
            }
            let client = self.client.clone();
            let token_cache = self.token_cache.clone();
            let corp_id = self.config.corp_id.clone();
            let secret = self.config.secret.clone();
            let agent_id = self.config.agent_id.clone();
            let thinking_id = placeholder_id.clone();
            let flush_to_user = to_user.to_string();
            let channel_id_clone = reply.channel.id.clone();
            let pending_clone = self.pending_streams.clone();
            let debounce_secs = self.config.debounce_secs;
            tokio::spawn(async move {
                let mut rx = text_rx;
                let debounce = std::time::Duration::from_secs(debounce_secs);
                let mut last_text = String::new();
                let max_idle = std::time::Duration::from_secs(300);
                let started = std::time::Instant::now();
                loop {
                    match tokio::time::timeout(debounce, rx.changed()).await {
                        Ok(Ok(())) => {
                            last_text = rx.borrow().clone();
                        }
                        Ok(Err(_)) => break,
                        Err(_) => {
                            if !last_text.is_empty() {
                                break;
                            }
                            if started.elapsed() > max_idle {
                                warn!("wecom: debounce task timed out after 5 minutes");
                                break;
                            }
                        }
                    }
                }
                // Acquire pending lock first, then capture any late writes
                // that landed between the loop break and now. Holding the
                // lock blocks handle_reply from sending more chunks for this
                // channel, so this read is the last writeable moment. Then
                // remove the entry, which drops text_tx and closes the channel.
                {
                    let mut pending = pending_clone.lock().unwrap_or_else(|e| e.into_inner());
                    let final_text = rx.borrow().clone();
                    if !final_text.is_empty() {
                        last_text = final_text;
                    }
                    pending.remove(&channel_id_clone);
                }
                if last_text.is_empty() {
                    return;
                }
                flush_thinking(
                    &client, &token_cache, &corp_id, &secret, &agent_id,
                    thinking_id.as_deref(), &flush_to_user, &last_text,
                ).await;
            });

            if let Some(ref req_id) = reply.request_id {
                let resp = crate::schema::GatewayResponse {
                    schema: "openab.gateway.response.v1".into(),
                    request_id: req_id.clone(),
                    success: true,
                    thread_id: None,
                    message_id: placeholder_id,
                    error: None,
                };
                if let Ok(json) = serde_json::to_string(&resp) {
                    let _ = event_tx.send(json);
                }
            }
            return;
        }

        if has_pending {
            // Re-check under lock: the debounce task may have removed the entry
            // between our earlier read of `has_pending` and now. If it did,
            // fall through to the direct-send path so the chunk isn't lost.
            let appended = {
                let pending = self.pending_streams.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(stream) = pending.get(&reply.channel.id) {
                    let current = stream.text_watch.borrow().clone();
                    let combined = if current.is_empty() {
                        text.to_string()
                    } else {
                        format!("{}\n{}", current, text)
                    };
                    let _ = stream.text_watch.send(combined);
                    true
                } else {
                    false
                }
            };
            if appended {
                if let Some(ref req_id) = reply.request_id {
                    let resp = crate::schema::GatewayResponse {
                        schema: "openab.gateway.response.v1".into(),
                        request_id: req_id.clone(),
                        success: true,
                        thread_id: None,
                        message_id: None,
                        error: None,
                    };
                    if let Ok(json) = serde_json::to_string(&resp) {
                        let _ = event_tx.send(json);
                    }
                }
                return;
            }
            // Pending entry was already removed (debounce flushed) — fall
            // through to direct-send below so this chunk still reaches the user.
        }

        info!(to_user = to_user, "wecom: sending reply");
        let chunks = split_text_lines(text, 2048);
        let mut msg_id = None;

        for chunk in &chunks {
            match self.send_text(to_user, chunk).await {
                Ok(id) => {
                    if msg_id.is_none() {
                        msg_id = Some(id);
                    }
                }
                Err(e) => warn!("wecom send failed: {e}"),
            }
        }

        if let Some(ref req_id) = reply.request_id {
            let resp = crate::schema::GatewayResponse {
                schema: "openab.gateway.response.v1".into(),
                request_id: req_id.clone(),
                success: msg_id.is_some(),
                thread_id: None,
                message_id: msg_id,
                error: None,
            };
            if let Ok(json) = serde_json::to_string(&resp) {
                let _ = event_tx.send(json);
            }
        }
    }

    fn handle_edit_message(&self, reply: &crate::schema::GatewayReply) {
        let text = reply.content.text.trim();
        if text.is_empty() {
            return;
        }
        let pending = self.pending_streams.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stream) = pending.get(&reply.channel.id) {
            let _ = stream.text_watch.send(text.to_string());
        }
    }


    async fn send_text(&self, to_user: &str, text: &str) -> Result<String> {
        let agent_id: u64 = self.config.agent_id.parse().expect("agent_id validated at startup");
        let body = serde_json::json!({
            "touser": to_user,
            "msgtype": "text",
            "agentid": agent_id,
            "text": { "content": text }
        });

        let resp = post_with_token_retry(
            &self.client,
            &self.token_cache,
            &self.config.corp_id,
            &self.config.secret,
            "/cgi-bin/message/send",
            &body,
        )
        .await?;
        Ok(resp["msgid"].as_str().unwrap_or("").to_string())
    }
}

/// POST a JSON body to a WeCom API endpoint with automatic token refresh
/// on errcode 42001 (access_token expired). Used by both `send_text` and
/// the streaming flush path so a long-running stream can't lose its final
/// reply if the cached token expires mid-flight.
async fn post_with_token_retry(
    client: &reqwest::Client,
    token_cache: &WecomTokenCache,
    corp_id: &str,
    secret: &str,
    api_path: &str,
    body: &serde_json::Value,
) -> Result<serde_json::Value> {
    let token = token_cache.get_token(client, corp_id, secret).await?;
    let url = format!("{}{}?access_token={}", token_cache.base_url, api_path, token);
    let resp: serde_json::Value = client.post(&url).json(body).send().await?.json().await?;
    let errcode = resp["errcode"].as_i64().unwrap_or(-1);

    if errcode == 42001 {
        warn!(api_path, "wecom: access_token expired, refreshing and retrying");
        let new_token = token_cache.force_refresh(client, corp_id, secret).await?;
        let retry_url = format!("{}{}?access_token={}", token_cache.base_url, api_path, new_token);
        let retry_resp: serde_json::Value =
            client.post(&retry_url).json(body).send().await?.json().await?;
        let retry_code = retry_resp["errcode"].as_i64().unwrap_or(-1);
        if retry_code != 0 {
            anyhow::bail!(
                "wecom {} retry failed: errcode={}, errmsg={}",
                api_path,
                retry_code,
                retry_resp["errmsg"]
            );
        }
        Ok(retry_resp)
    } else if errcode != 0 {
        anyhow::bail!(
            "wecom {} failed: errcode={}, errmsg={}",
            api_path,
            errcode,
            resp["errmsg"]
        );
    } else {
        Ok(resp)
    }
}

// --- Handlers ---

fn handle_verify_request(
    token: &str,
    encoding_aes_key: &str,
    corp_id: &str,
    msg_signature: &str,
    timestamp: &str,
    nonce: &str,
    echostr: &str,
) -> anyhow::Result<String> {
    if !verify_signature(token, timestamp, nonce, echostr, msg_signature) {
        anyhow::bail!("signature verification failed");
    }
    decrypt_message(encoding_aes_key, echostr, corp_id)
}

// --- XML parsing ---

struct CallbackEnvelope {
    to_user_name: String,
    encrypt: String,
}

struct WecomMessage {
    from_user: String,
    msg_type: String,
    content: String,
    msg_id: String,
    pic_url: String,
    media_id: String,
    file_name: String,
}

fn parse_envelope_xml(xml: &str) -> Result<CallbackEnvelope> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut to_user_name = String::new();
    let mut encrypt = String::new();
    let mut current_tag = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
            }
            Ok(Event::CData(e)) => {
                let text = String::from_utf8_lossy(&e).to_string();
                match current_tag.as_str() {
                    "ToUserName" => to_user_name = text,
                    "Encrypt" => encrypt = text,
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                match current_tag.as_str() {
                    "ToUserName" if to_user_name.is_empty() => to_user_name = text,
                    "Encrypt" if encrypt.is_empty() => encrypt = text,
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("xml parse error: {e}"),
            _ => {}
        }
    }

    if encrypt.is_empty() {
        anyhow::bail!("missing Encrypt field in callback XML");
    }
    Ok(CallbackEnvelope {
        to_user_name,
        encrypt,
    })
}

fn parse_message_xml(xml: &str) -> Result<WecomMessage> {
    use quick_xml::events::Event;
    use quick_xml::Reader;

    let mut reader = Reader::from_str(xml);
    let mut from_user = String::new();
    let mut msg_type = String::new();
    let mut content = String::new();
    let mut msg_id = String::new();
    let mut pic_url = String::new();
    let mut media_id = String::new();
    let mut file_name = String::new();
    let mut current_tag = String::new();

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                current_tag = String::from_utf8_lossy(e.name().as_ref()).to_string();
            }
            Ok(Event::CData(e)) => {
                let text = String::from_utf8_lossy(&e).to_string();
                match current_tag.as_str() {
                    "FromUserName" => from_user = text,
                    "MsgType" => msg_type = text,
                    "Content" => content = text,
                    "MsgId" => msg_id = text,
                    "PicUrl" => pic_url = text,
                    "MediaId" => media_id = text,
                    "FileName" => file_name = text,
                    _ => {}
                }
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                match current_tag.as_str() {
                    "FromUserName" if from_user.is_empty() => from_user = text,
                    "MsgType" if msg_type.is_empty() => msg_type = text,
                    "Content" if content.is_empty() => content = text,
                    "MsgId" if msg_id.is_empty() => msg_id = text,
                    "PicUrl" if pic_url.is_empty() => pic_url = text,
                    "MediaId" if media_id.is_empty() => media_id = text,
                    "FileName" if file_name.is_empty() => file_name = text,
                    _ => {}
                }
            }
            Ok(Event::End(_)) => {
                current_tag.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("xml parse error: {e}"),
            _ => {}
        }
    }

    Ok(WecomMessage {
        from_user,
        msg_type,
        content,
        msg_id,
        pic_url,
        media_id,
        file_name,
    })
}

#[allow(clippy::too_many_arguments)]
async fn flush_thinking(
    client: &reqwest::Client,
    token_cache: &WecomTokenCache,
    corp_id: &str,
    secret: &str,
    agent_id: &str,
    thinking_msg_id: Option<&str>,
    to_user: &str,
    text: &str,
) {
    info!(?thinking_msg_id, text_len = text.len(), "wecom: flush_thinking starting");

    // Recall thinking placeholder (only when streaming was enabled)
    if let Some(id) = thinking_msg_id {
        let body = serde_json::json!({ "msgid": id });
        match post_with_token_retry(
            client,
            token_cache,
            corp_id,
            secret,
            "/cgi-bin/message/recall",
            &body,
        )
        .await
        {
            Ok(resp) => info!(body = %resp, "wecom: recall response"),
            Err(e) => warn!(error = %e, "wecom: recall failed"),
        }
    }

    // Send final text. Each chunk goes through retry-on-token-expiry so a
    // long stream that outlives the cached token still delivers its reply.
    let aid = agent_id.parse::<u64>().unwrap_or(0);
    let chunks = split_text_lines(text, 2048);
    info!(chunk_count = chunks.len(), "wecom: sending final chunks");
    for (i, chunk) in chunks.iter().enumerate() {
        let body = serde_json::json!({
            "touser": to_user,
            "msgtype": "text",
            "agentid": aid,
            "text": { "content": chunk }
        });
        match post_with_token_retry(
            client,
            token_cache,
            corp_id,
            secret,
            "/cgi-bin/message/send",
            &body,
        )
        .await
        {
            Ok(val) => {
                let msg_id = val["msgid"].as_str().unwrap_or("");
                info!(msg_id = %msg_id, chunk_idx = i, "wecom: sent final reply chunk");
            }
            Err(e) => warn!(error = %e, chunk_idx = i, "wecom flush send failed"),
        }
    }
}

/// Split `text` into chunks that each fit within `limit` bytes (WeCom's
/// `message/send` truncates server-side at 2048 bytes). Splits prefer
/// newline boundaries; lines that exceed the limit themselves are split at
/// UTF-8 char boundaries via `char_indices()` so multibyte characters are
/// never severed mid-codepoint. The `limit` and all `len()` comparisons in
/// this function are in **bytes**, matching WeCom's server-side check.
fn split_text_lines(text: &str, limit: usize) -> Vec<String> {
    if text.len() <= limit {
        return vec![text.to_string()];
    }
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.split('\n') {
        if line.len() > limit {
            if !current.is_empty() {
                chunks.push(current);
                current = String::new();
            }
            // Split long line at char boundaries
            let mut pos = 0;
            for (i, ch) in line.char_indices() {
                if i - pos + ch.len_utf8() > limit {
                    chunks.push(line[pos..i].to_string());
                    pos = i;
                }
            }
            if pos < line.len() {
                current = line[pos..].to_string();
            }
            continue;
        }
        let candidate_len = if current.is_empty() {
            line.len()
        } else {
            current.len() + 1 + line.len()
        };
        if candidate_len > limit && !current.is_empty() {
            chunks.push(current);
            current = String::new();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

pub async fn verify(
    State(state): State<Arc<crate::AppState>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let wecom = match state.wecom.as_ref() {
        Some(w) => w,
        None => return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let msg_signature = query.get("msg_signature").map(|s| s.as_str()).unwrap_or("");
    let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
    let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");
    let echostr = query.get("echostr").map(|s| s.as_str()).unwrap_or("");

    info!(
        msg_signature = %msg_signature,
        timestamp = %timestamp,
        nonce = %nonce,
        echostr_len = echostr.len(),
        "wecom verify request received"
    );

    match handle_verify_request(
        &wecom.config.token,
        &wecom.config.encoding_aes_key,
        &wecom.config.corp_id,
        msg_signature,
        timestamp,
        nonce,
        echostr,
    ) {
        Ok(plaintext) => plaintext.into_response(),
        Err(e) => {
            warn!("wecom callback verification failed: {e}");
            axum::http::StatusCode::FORBIDDEN.into_response()
        }
    }
}

pub async fn webhook(
    State(state): State<Arc<crate::AppState>>,
    query: axum::extract::Query<std::collections::HashMap<String, String>>,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse;

    let wecom = match state.wecom.as_ref() {
        Some(w) => w,
        None => return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response(),
    };

    let msg_signature = query.get("msg_signature").map(|s| s.as_str()).unwrap_or("");
    let timestamp = query.get("timestamp").map(|s| s.as_str()).unwrap_or("");
    let nonce = query.get("nonce").map(|s| s.as_str()).unwrap_or("");

    // Reject stale callbacks. WeCom retries within ~5s, our dedup window is
    // 30s, so a 5-minute freshness check rejects replays without false-
    // positives on legitimate retries. The signature itself doesn't bind a
    // freshness expectation, so without this an attacker who captured a
    // signed payload could replay it indefinitely.
    if let Ok(ts) = timestamp.parse::<i64>() {
        let now = chrono::Utc::now().timestamp();
        if (now - ts).abs() > 300 {
            warn!(timestamp_age_secs = now - ts, "wecom webhook: rejecting stale callback");
            return axum::http::StatusCode::FORBIDDEN.into_response();
        }
    }

    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return axum::http::StatusCode::BAD_REQUEST.into_response(),
    };

    let envelope = match parse_envelope_xml(body_str) {
        Ok(e) => e,
        Err(e) => {
            warn!("wecom envelope parse error: {e}");
            return axum::http::StatusCode::BAD_REQUEST.into_response();
        }
    };

    // ToUserName in the outer envelope must match our configured Corp ID.
    // The decrypt step also validates the inner Corp ID suffix; checking here
    // first surfaces misrouted callbacks before we touch crypto.
    if envelope.to_user_name != wecom.config.corp_id {
        warn!(
            envelope_to = %envelope.to_user_name,
            expected = %wecom.config.corp_id,
            "wecom webhook: envelope ToUserName mismatch"
        );
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }

    if !verify_signature(
        &wecom.config.token,
        timestamp,
        nonce,
        &envelope.encrypt,
        msg_signature,
    ) {
        warn!("wecom webhook signature verification failed");
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }

    info!(encrypt_len = envelope.encrypt.len(), "wecom: decrypting callback");
    let decrypted = match decrypt_message(
        &wecom.config.encoding_aes_key,
        &envelope.encrypt,
        &wecom.config.corp_id,
    ) {
        Ok(d) => {
            info!("wecom: decrypt ok");
            d
        }
        Err(e) => {
            warn!(encrypt_len = envelope.encrypt.len(), "wecom decrypt failed: {e}");
            return "success".into_response();
        }
    };

    let msg = match parse_message_xml(&decrypted) {
        Ok(m) => m,
        Err(e) => {
            warn!("wecom message parse error: {e}");
            return "success".into_response();
        }
    };

    info!(
        msg_type = %msg.msg_type,
        has_pic_url = !msg.pic_url.is_empty(),
        msg_id = %msg.msg_id,
        "wecom: parsed message"
    );

    if !matches!(msg.msg_type.as_str(), "text" | "image" | "file") {
        return "success".into_response();
    }

    if !wecom.dedupe.check_and_insert(&msg.msg_id) {
        return "success".into_response();
    }

    let text = match msg.msg_type.as_str() {
        "text" => msg.content.clone(),
        "image" => "Describe this image.".to_string(),
        "file" => format!("User sent a file: {}", msg.file_name),
        _ => String::new(),
    };

    let mut attachments = Vec::new();
    if msg.msg_type == "image" && !msg.pic_url.is_empty() {
        let att = download_wecom_image(&wecom.client, &msg.pic_url).await;
        attachments.push(att);
    }
    if msg.msg_type == "file" && !msg.media_id.is_empty() {
        let att = download_wecom_file(
            &wecom.client,
            &wecom.token_cache,
            &wecom.config.corp_id,
            &wecom.config.secret,
            &msg.media_id,
            &msg.file_name,
        )
        .await;
        attachments.push(att);
    }

    if text.trim().is_empty() && attachments.is_empty() {
        return "success".into_response();
    }

    let channel_id = format!("wecom:{}:{}", wecom.config.corp_id, msg.from_user);
    let mut event = crate::schema::GatewayEvent::new(
        "wecom",
        crate::schema::ChannelInfo {
            id: channel_id,
            channel_type: "direct".into(),
            thread_id: None,
        },
        crate::schema::SenderInfo {
            id: msg.from_user.clone(),
            name: msg.from_user.clone(),
            display_name: msg.from_user.clone(),
            is_bot: false,
        },
        &text,
        &msg.msg_id,
        vec![],
    );
    event.content.attachments = attachments;

    let att_sizes: Vec<usize> = event.content.attachments.iter().map(|a| a.data.len()).collect();
    info!(
        attachments = event.content.attachments.len(),
        text_len = event.content.text.len(),
        att_data_sizes = ?att_sizes,
        att_mime = ?event.content.attachments.iter().map(|a| a.mime_type.as_str()).collect::<Vec<_>>(),
        "wecom: forwarding event to OAB"
    );
    if let Ok(json) = serde_json::to_string(&event) {
        info!(
            json_len = json.len(),
            has_attachments_in_json = json.contains("\"attachments\""),
            "wecom: event JSON ready"
        );
        let _ = state.event_tx.send(json);
    }

    "success".into_response()
}

const IMAGE_MAX_DOWNLOAD: u64 = 10 * 1024 * 1024;
const IMAGE_MAX_DIMENSION_PX: u32 = 1200;
const IMAGE_JPEG_QUALITY: u8 = 75;

async fn download_wecom_image(
    client: &reqwest::Client,
    pic_url: &str,
) -> crate::schema::Attachment {
    // Only fetch over HTTPS. WeCom's CDN serves images over HTTPS; rejecting
    // non-HTTPS URLs prevents SSRF if the AES key is ever compromised and
    // an attacker forges a callback with PicUrl pointing at an internal host.
    if !pic_url.starts_with("https://") {
        warn!(pic_url, "wecom: rejecting non-HTTPS pic_url");
        return crate::schema::Attachment::rejected(
            "image",
            "wecom_image.jpg",
            "image/jpeg",
            0,
            "security rejected: URL must use HTTPS",
        );
    }
    info!(pic_url, "wecom: downloading image");
    let resp = match client.get(pic_url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "wecom image download failed");
            return crate::schema::Attachment::rejected(
                "image",
                "wecom_image.jpg",
                "image/jpeg",
                0,
                "download failed: network error",
            );
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        warn!(status = %status, "wecom image download failed");
        return crate::schema::Attachment::rejected(
            "image",
            "wecom_image.jpg",
            "image/jpeg",
            0,
            format!("download failed: HTTP {}", status.as_u16()),
        );
    }
    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > IMAGE_MAX_DOWNLOAD {
                warn!(size, "wecom image exceeds 10MB limit, skipping");
                return crate::schema::Attachment::rejected(
                    "image",
                    "wecom_image.jpg",
                    "image/jpeg",
                    size,
                    format!("size exceeded: {} exceeds {}", format_bytes(size), format_bytes(IMAGE_MAX_DOWNLOAD)),
                );
            }
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "wecom image body read failed");
            return crate::schema::Attachment::rejected(
                "image",
                "wecom_image.jpg",
                "image/jpeg",
                0,
                "download failed: body read error",
            );
        }
    };
    if bytes.len() as u64 > IMAGE_MAX_DOWNLOAD {
        warn!(size = bytes.len(), "wecom image exceeds 10MB limit");
        return crate::schema::Attachment::rejected(
            "image",
            "wecom_image.jpg",
            "image/jpeg",
            bytes.len() as u64,
            format!("size exceeded: {} exceeds {}", format_bytes(bytes.len() as u64), format_bytes(IMAGE_MAX_DOWNLOAD)),
        );
    }
    let (compressed, mime) = match resize_and_compress(&bytes) {
        Ok(v) => v,
        Err(e) => {
            warn!(error = %e, "wecom: image resize/compress failed");
            return crate::schema::Attachment::rejected(
                "image",
                "wecom_image.jpg",
                "image/jpeg",
                bytes.len() as u64,
                "processing failed: image encoding error",
            );
        }
    };
    let path = match crate::store::store_media(&compressed).await {
        Some(p) => p,
        None => {
            warn!("wecom image store failed");
            return crate::schema::Attachment::rejected(
                "image",
                "wecom_image.jpg",
                "image/jpeg",
                compressed.len() as u64,
                "processing failed: storage error",
            );
        }
    };
    let ext = if mime == "image/gif" { "gif" } else { "jpg" };
    crate::schema::Attachment {
        attachment_type: "image".into(),
        filename: format!("wecom_{}.{}", chrono::Utc::now().timestamp(), ext),
        mime_type: mime,
        data: String::new(),
        size: compressed.len() as u64,
        path: Some(path),
        status: None,
    }
}

const FILE_MAX_DOWNLOAD: u64 = 20 * 1024 * 1024;

const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "csv", "log", "md", "json", "jsonl", "yaml", "yml", "toml", "xml", "rs", "py", "js",
    "ts", "jsx", "tsx", "go", "java", "c", "cpp", "h", "hpp", "rb", "sh", "bash", "zsh", "fish",
    "ps1", "bat", "sql", "html", "css", "scss", "less", "ini", "cfg", "conf", "env",
    "swift", "kt", "scala", "r", "pl", "lua", "graphql", "tsv",
];

const TEXT_FILENAMES: &[&str] = &[
    "dockerfile", "makefile", "justfile", "rakefile", "gemfile",
    "procfile", "vagrantfile", ".gitignore", ".dockerignore", ".editorconfig",
];

fn is_text_file(filename: &str) -> bool {
    let lower = filename.to_lowercase();
    if lower.contains('.') {
        if let Some(ext) = lower.rsplit('.').next() {
            if TEXT_EXTENSIONS.contains(&ext) {
                return true;
            }
        }
    }
    TEXT_FILENAMES.contains(&lower.as_str())
}

/// GET /cgi-bin/media/get with token-expiry retry. The media API returns
/// JSON `{"errcode":42001,...}` instead of binary when the token is stale,
/// so we sniff Content-Type and retry once with a force-refreshed token.
async fn fetch_media_with_retry(
    client: &reqwest::Client,
    token_cache: &WecomTokenCache,
    corp_id: &str,
    secret: &str,
    media_id: &str,
) -> Result<reqwest::Response> {
    let token = token_cache.get_token(client, corp_id, secret).await?;
    let url = format!(
        "{}/cgi-bin/media/get?access_token={}&media_id={}",
        token_cache.base_url, token, media_id
    );
    let resp = client.get(&url).send().await?;
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if !content_type.contains("json") {
        return Ok(resp);
    }
    // JSON body means error path. Inspect for 42001 and retry once.
    let body = resp.text().await.unwrap_or_default();
    let val: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
    let errcode = val["errcode"].as_i64().unwrap_or(-1);
    if errcode == 42001 {
        warn!("wecom media: access_token expired, refreshing and retrying");
        let new_token = token_cache.force_refresh(client, corp_id, secret).await?;
        let retry_url = format!(
            "{}/cgi-bin/media/get?access_token={}&media_id={}",
            token_cache.base_url, new_token, media_id
        );
        return Ok(client.get(&retry_url).send().await?);
    }
    anyhow::bail!("wecom media error: {body}")
}

async fn download_wecom_file(
    client: &reqwest::Client,
    token_cache: &WecomTokenCache,
    corp_id: &str,
    secret: &str,
    media_id: &str,
    filename: &str,
) -> crate::schema::Attachment {
    info!(filename, media_id, "wecom: downloading file");
    let resp = match fetch_media_with_retry(client, token_cache, corp_id, secret, media_id).await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, "wecom file download failed");
            return crate::schema::Attachment::rejected(
                "text_file",
                filename.to_string(),
                "application/octet-stream",
                0,
                "download failed: network error",
            );
        }
    };
    if !resp.status().is_success() {
        let status = resp.status();
        warn!(status = %status, "wecom file download failed");
        return crate::schema::Attachment::rejected(
            "text_file",
            filename.to_string(),
            "application/octet-stream",
            0,
            format!("download failed: HTTP {}", status.as_u16()),
        );
    }
    if let Some(cl) = resp.headers().get(reqwest::header::CONTENT_LENGTH) {
        if let Ok(size) = cl.to_str().unwrap_or("0").parse::<u64>() {
            if size > FILE_MAX_DOWNLOAD {
                warn!(size, "wecom file exceeds 20MB limit, skipping");
                return crate::schema::Attachment::rejected(
                    "text_file",
                    filename.to_string(),
                    "application/octet-stream",
                    size,
                    format!("size exceeded: {} exceeds {}", format_bytes(size), format_bytes(FILE_MAX_DOWNLOAD)),
                );
            }
        }
    }
    let bytes = match resp.bytes().await {
        Ok(b) => b,
        Err(e) => {
            warn!(error = %e, "wecom file body read failed");
            return crate::schema::Attachment::rejected(
                "text_file",
                filename.to_string(),
                "application/octet-stream",
                0,
                "download failed: body read error",
            );
        }
    };
    if bytes.len() as u64 > FILE_MAX_DOWNLOAD {
        warn!(size = bytes.len(), "wecom file exceeds 20MB limit");
        return crate::schema::Attachment::rejected(
            "text_file",
            filename.to_string(),
            "application/octet-stream",
            bytes.len() as u64,
            format!("size exceeded: {} exceeds {}", format_bytes(bytes.len() as u64), format_bytes(FILE_MAX_DOWNLOAD)),
        );
    }

    if !is_text_file(filename) {
        info!(filename, "wecom: skipping non-text file");
        let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
        return crate::schema::Attachment::rejected(
            "text_file",
            filename.to_string(),
            "application/octet-stream",
            bytes.len() as u64,
            format!("unsupported format: {ext}"),
        );
    }

    let text_content = match String::from_utf8(bytes.to_vec()) {
        Ok(s) => s,
        Err(_) => {
            info!(filename, "wecom: file is not valid UTF-8, skipping");
            return crate::schema::Attachment::rejected(
                "text_file",
                filename.to_string(),
                "application/octet-stream",
                bytes.len() as u64,
                "invalid content: not valid UTF-8",
            );
        }
    };

    let path = match crate::store::store_media(text_content.as_bytes()).await {
        Some(p) => p,
        None => {
            warn!(filename, "wecom file store failed");
            return crate::schema::Attachment::rejected(
                "text_file",
                filename.to_string(),
                "application/octet-stream",
                text_content.len() as u64,
                "processing failed: storage error",
            );
        }
    };
    let size = text_content.len() as u64;

    crate::schema::Attachment {
        attachment_type: "text_file".into(),
        filename: filename.to_string(),
        mime_type: "text/plain".into(),
        data: String::new(),
        size,
        path: Some(path),
        status: None,
    }
}

fn resize_and_compress(raw: &[u8]) -> Result<(Vec<u8>, String), image::ImageError> {
    use image::ImageReader;
    use std::io::Cursor;

    let reader = ImageReader::new(Cursor::new(raw)).with_guessed_format()?;
    let format = reader.format();
    if format == Some(image::ImageFormat::Gif) {
        return Ok((raw.to_vec(), "image/gif".to_string()));
    }
    let img = reader.decode()?;
    let (w, h) = (img.width(), img.height());
    let img = if w > IMAGE_MAX_DIMENSION_PX || h > IMAGE_MAX_DIMENSION_PX {
        let max_side = std::cmp::max(w, h);
        let ratio = f64::from(IMAGE_MAX_DIMENSION_PX) / f64::from(max_side);
        let new_w = (f64::from(w) * ratio) as u32;
        let new_h = (f64::from(h) * ratio) as u32;
        img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, IMAGE_JPEG_QUALITY);
    img.write_with_encoder(encoder)?;
    Ok((buf.into_inner(), "image/jpeg".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: std::collections::HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k: &str| map.get(k).cloned()
    }

    #[test]
    fn config_from_env_all_present() {
        let env = make_env(&[
            ("WECOM_CORP_ID", "ww_test_corp"),
            ("WECOM_SECRET", "test_secret"),
            ("WECOM_TOKEN", "test_token"),
            ("WECOM_ENCODING_AES_KEY", "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG"),
            ("WECOM_AGENT_ID", "1000002"),
        ]);
        let config = WecomConfig::from_reader(env).unwrap();
        assert_eq!(config.corp_id, "ww_test_corp");
        assert_eq!(config.agent_id, "1000002");
        assert_eq!(config.webhook_path, "/webhook/wecom");
        assert!(!config.streaming_enabled, "streaming defaults off");
        assert_eq!(config.debounce_secs, 3);
    }

    #[test]
    fn config_from_env_missing_required() {
        let env = make_env(&[]);
        assert!(WecomConfig::from_reader(env).is_none());
    }

    fn encrypt_for_test(encoding_aes_key: &str, msg: &str, corp_id: &str) -> String {
        use aes::cipher::{BlockEncryptMut, KeyIvInit};
        use base64::Engine;

        let key = decode_aes_key(encoding_aes_key).unwrap();
        let iv = &key[..16];

        let msg_bytes = msg.as_bytes();
        let corp_id_bytes = corp_id.as_bytes();
        let msg_len = (msg_bytes.len() as u32).to_be_bytes();

        let mut plaintext = Vec::new();
        plaintext.extend_from_slice(&[0u8; 16]); // random bytes (zeros for test)
        plaintext.extend_from_slice(&msg_len);
        plaintext.extend_from_slice(msg_bytes);
        plaintext.extend_from_slice(corp_id_bytes);

        // WeCom uses PKCS7 padding with block_size=32
        let block_size = 32;
        let pad_len = block_size - (plaintext.len() % block_size);
        for _ in 0..pad_len {
            plaintext.push(pad_len as u8);
        }

        // Encrypt with NoPadding since we already padded manually
        let total_len = plaintext.len();
        let mut buf = vec![0u8; total_len + 16]; // extra space just in case
        buf[..total_len].copy_from_slice(&plaintext);

        type Aes256CbcEnc = cbc::Encryptor<aes::Aes256>;
        let encryptor = Aes256CbcEnc::new_from_slices(&key, iv).unwrap();
        let encrypted = encryptor
            .encrypt_padded_mut::<aes::cipher::block_padding::NoPadding>(&mut buf, total_len)
            .unwrap();

        base64::engine::general_purpose::STANDARD.encode(encrypted)
    }

    #[test]
    fn aes_key_decode() {
        let key_str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let key_bytes = decode_aes_key(key_str).unwrap();
        assert_eq!(key_bytes.len(), 32);
    }

    #[test]
    fn signature_verify() {
        let token = "testtoken";
        let timestamp = "1409659813";
        let nonce = "1372623149";
        let encrypt = "msg_encrypt_content";

        let sig = compute_signature(token, timestamp, nonce, encrypt);
        assert!(verify_signature(token, timestamp, nonce, encrypt, &sig));
        assert!(!verify_signature(
            token,
            timestamp,
            nonce,
            encrypt,
            "wrong_signature_value_here"
        ));
    }

    #[test]
    fn decrypt_wecom_payload() {
        let key_str = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let corp_id = "ww_test_corp";
        let msg = "hello world";

        let encrypted = encrypt_for_test(key_str, msg, corp_id);
        let decrypted = decrypt_message(key_str, &encrypted, corp_id).unwrap();
        assert_eq!(decrypted, msg);
    }

    #[test]
    fn verify_callback_echostr() {
        let token = "testtoken";
        let encoding_aes_key = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let corp_id = "ww_test_corp";
        let echostr_plain = "success_echo_string";

        let echostr_encrypted = encrypt_for_test(encoding_aes_key, echostr_plain, corp_id);
        let sig = compute_signature(token, "1409659813", "nonce123", &echostr_encrypted);

        let result = handle_verify_request(
            token,
            encoding_aes_key,
            corp_id,
            &sig,
            "1409659813",
            "nonce123",
            &echostr_encrypted,
        );
        assert_eq!(result.unwrap(), echostr_plain);
    }

    #[test]
    fn parse_text_message_xml() {
        let xml = r#"<xml><ToUserName><![CDATA[ww_test_corp]]></ToUserName><FromUserName><![CDATA[user001]]></FromUserName><CreateTime>1348831860</CreateTime><MsgType><![CDATA[text]]></MsgType><Content><![CDATA[hello bot]]></Content><MsgId>1234567890123456</MsgId><AgentID>1000002</AgentID></xml>"#;

        let msg = parse_message_xml(xml).unwrap();
        assert_eq!(msg.from_user, "user001");
        assert_eq!(msg.msg_type, "text");
        assert_eq!(msg.content, "hello bot");
        assert_eq!(msg.msg_id, "1234567890123456");
    }

    #[test]
    fn parse_callback_envelope() {
        let xml = r#"<xml><ToUserName><![CDATA[ww_test_corp]]></ToUserName><Encrypt><![CDATA[some_encrypted_base64]]></Encrypt><AgentID><![CDATA[1000002]]></AgentID></xml>"#;

        let envelope = parse_envelope_xml(xml).unwrap();
        assert_eq!(envelope.to_user_name, "ww_test_corp");
        assert_eq!(envelope.encrypt, "some_encrypted_base64");
    }

    #[test]
    fn dedupe_rejects_duplicates() {
        let cache = DedupeCache::new();
        assert!(cache.check_and_insert("msg_001"));
        assert!(!cache.check_and_insert("msg_001"));
        assert!(cache.check_and_insert("msg_002"));
    }

    #[tokio::test]
    async fn token_refresh_success() {
        use wiremock::matchers::{method, query_param};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(query_param("corpid", "ww_test_corp"))
            .and(query_param("corpsecret", "test_secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "errcode": 0,
                "errmsg": "ok",
                "access_token": "test_token_abc",
                "expires_in": 7200
            })))
            .expect(1)
            .mount(&server)
            .await;

        let cache = WecomTokenCache::with_base_url(server.uri());
        let client = reqwest::Client::new();
        let token = cache.get_token(&client, "ww_test_corp", "test_secret").await.unwrap();
        assert_eq!(token, "test_token_abc");

        // Second call uses cache (mock expects exactly 1 call)
        let token2 = cache.get_token(&client, "ww_test_corp", "test_secret").await.unwrap();
        assert_eq!(token2, "test_token_abc");
    }

    #[test]
    fn split_text_lines_multi() {
        let text = "line1\nline2\nline3";
        let chunks = split_text_lines(text, 11);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0], "line1\nline2");
        assert_eq!(chunks[1], "line3");
    }

    #[test]
    fn split_text_lines_within_limit() {
        let text = "short";
        let chunks = split_text_lines(text, 100);
        assert_eq!(chunks, vec!["short"]);
    }

    #[test]
    fn split_text_lines_long_line() {
        let text = "abcdefghij";
        let chunks = split_text_lines(text, 4);
        assert_eq!(chunks, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn split_text_lines_long_line_utf8() {
        let text = "你好世界測試"; // 18 bytes, 6 chars
        let chunks = split_text_lines(text, 6);
        assert_eq!(chunks, vec!["你好", "世界", "測試"]);
    }

    #[test]
    fn is_text_file_check() {
        assert!(is_text_file("readme.md"));
        assert!(is_text_file("config.json"));
        assert!(is_text_file("data.csv"));
        assert!(is_text_file("MAIN.PY"));
        assert!(!is_text_file("photo.png"));
        assert!(!is_text_file("archive.zip"));
        assert!(!is_text_file("doc.pdf"));
    }

    #[test]
    fn parse_file_message() {
        let xml = r#"<xml><ToUserName><![CDATA[ww_test_corp]]></ToUserName><FromUserName><![CDATA[user42]]></FromUserName><CreateTime>1348831860</CreateTime><MsgType><![CDATA[file]]></MsgType><MediaId><![CDATA[media_abc123]]></MediaId><FileName><![CDATA[report.csv]]></FileName><MsgId>6666</MsgId><AgentID>1000002</AgentID></xml>"#;
        let msg = parse_message_xml(xml).unwrap();
        assert_eq!(msg.msg_type, "file");
        assert_eq!(msg.media_id, "media_abc123");
        assert_eq!(msg.file_name, "report.csv");
    }

    #[test]
    fn full_webhook_decrypt_and_parse() {
        let token = "testtoken";
        let encoding_aes_key = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let corp_id = "ww_test_corp";
        let timestamp = "1409659813";
        let nonce = "nonce123";

        // Simulate the inner message
        let inner_xml = "<xml><ToUserName><![CDATA[ww_test_corp]]></ToUserName><FromUserName><![CDATA[user42]]></FromUserName><CreateTime>1348831860</CreateTime><MsgType><![CDATA[text]]></MsgType><Content><![CDATA[ping]]></Content><MsgId>9999</MsgId><AgentID>1000002</AgentID></xml>";

        // Encrypt it
        let encrypted = encrypt_for_test(encoding_aes_key, inner_xml, corp_id);

        // Compute signature
        let sig = compute_signature(token, timestamp, nonce, &encrypted);

        // Verify signature
        assert!(verify_signature(token, timestamp, nonce, &encrypted, &sig));

        // Decrypt
        let decrypted = decrypt_message(encoding_aes_key, &encrypted, corp_id).unwrap();
        assert_eq!(decrypted, inner_xml);

        // Parse
        let msg = parse_message_xml(&decrypted).unwrap();
        assert_eq!(msg.from_user, "user42");
        assert_eq!(msg.msg_type, "text");
        assert_eq!(msg.content, "ping");
        assert_eq!(msg.msg_id, "9999");
    }

    #[test]
    fn parse_image_message() {
        let encoding_aes_key = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let corp_id = "ww_test_corp";

        let inner_xml = "<xml><ToUserName><![CDATA[ww_test_corp]]></ToUserName><FromUserName><![CDATA[user42]]></FromUserName><CreateTime>1348831860</CreateTime><MsgType><![CDATA[image]]></MsgType><PicUrl><![CDATA[http://example.com/pic.jpg]]></PicUrl><MsgId>8888</MsgId><AgentID>1000002</AgentID></xml>";

        let encrypted = encrypt_for_test(encoding_aes_key, inner_xml, corp_id);
        let decrypted = decrypt_message(encoding_aes_key, &encrypted, corp_id).unwrap();
        let msg = parse_message_xml(&decrypted).unwrap();
        assert_eq!(msg.msg_type, "image");
        assert_eq!(msg.pic_url, "http://example.com/pic.jpg");
        assert_eq!(msg.from_user, "user42");
    }

    #[test]
    fn unsupported_msg_type_skipped() {
        let xml = "<xml><ToUserName><![CDATA[ww_test_corp]]></ToUserName><FromUserName><![CDATA[user42]]></FromUserName><CreateTime>1348831860</CreateTime><MsgType><![CDATA[voice]]></MsgType><MsgId>7777</MsgId><AgentID>1000002</AgentID></xml>";
        let msg = parse_message_xml(xml).unwrap();
        assert_eq!(msg.msg_type, "voice");
        assert!(!matches!(msg.msg_type.as_str(), "text" | "image"));
    }

    #[test]
    fn verify_rejects_wrong_signature() {
        let token = "testtoken";
        let encoding_aes_key = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let corp_id = "ww_test_corp";
        let echostr_plain = "test_echo";

        let echostr_encrypted = encrypt_for_test(encoding_aes_key, echostr_plain, corp_id);

        let result = handle_verify_request(
            token,
            encoding_aes_key,
            corp_id,
            "completely_wrong_signature",
            "1409659813",
            "nonce123",
            &echostr_encrypted,
        );
        assert!(result.is_err());
    }

    #[test]
    fn decrypt_with_large_padding_value() {
        // Verifies decryption works when WeCom's 32-byte padding exceeds 16
        let encoding_aes_key = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let corp_id = "ww_test_corp";
        // Choose a message where (16 + 4 + msg_len + corp_id_len) % 32 < 16,
        // producing a pad value > 16 which would fail with PKCS7/block_size=16.
        // 16 + 4 + 1 + 12 = 33 → 33 % 32 = 1 → pad = 31
        let msg = "x";
        let encrypted = encrypt_for_test(encoding_aes_key, msg, corp_id);
        let decrypted = decrypt_message(encoding_aes_key, &encrypted, corp_id).unwrap();
        assert_eq!(decrypted, msg);
    }

    #[test]
    fn decrypt_rejects_wrong_corp_id() {
        let encoding_aes_key = "QUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUFBQUE";
        let corp_id = "ww_test_corp";
        let msg = "hello";

        let encrypted = encrypt_for_test(encoding_aes_key, msg, corp_id);
        let result = decrypt_message(encoding_aes_key, &encrypted, "ww_other_corp");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("corp_id mismatch"));
    }
}
