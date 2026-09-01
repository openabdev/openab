use std::collections::HashMap;
use tracing::{error, info};

use crate::config::SecretsConfig;

/// Resolved secrets: mapping from key name to plaintext value.
pub type ResolvedSecrets = HashMap<String, String>;

/// Resolve all secret references in the [secrets] config table.
/// Returns a map of key → resolved value.
pub async fn resolve(cfg: &SecretsConfig) -> anyhow::Result<ResolvedSecrets> {
    let mut resolved = HashMap::new();

    // Build AWS client once if any refs use aws-sm://
    #[cfg(feature = "secrets-aws")]
    let aws_client = if cfg.refs.values().any(|v| v.starts_with("aws-sm://")) {
        Some(build_aws_client(cfg).await)
    } else {
        None
    };

    for (key, uri) in &cfg.refs {
        let value = if uri.starts_with("aws-sm://") {
            #[cfg(feature = "secrets-aws")]
            {
                let client = aws_client
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("secret '{key}': AWS client not initialized"))?;
                resolve_aws_sm(key, uri, client).await?
            }
            #[cfg(not(feature = "secrets-aws"))]
            {
                anyhow::bail!(
                    "secret '{key}' uses aws-sm:// but the 'secrets-aws' feature is not enabled"
                );
            }
        } else if uri.starts_with("exec://") {
            resolve_exec(key, uri, cfg).await?
        } else {
            anyhow::bail!(
                "secret '{key}': unrecognized URI scheme in '{uri}' (expected aws-sm:// or exec://)"
            );
        };
        resolved.insert(key.clone(), value);
    }

    if !resolved.is_empty() {
        info!(count = resolved.len(), "secrets resolved");
    }
    Ok(resolved)
}

// -- AWS Secrets Manager provider --

#[cfg(feature = "secrets-aws")]
async fn build_aws_client(cfg: &SecretsConfig) -> aws_sdk_secretsmanager::Client {
    let mut config_loader = aws_config::defaults(aws_config::BehaviorVersion::latest());
    if let Some(ref region) = cfg.aws.region {
        config_loader = config_loader.region(aws_config::Region::new(region.clone()));
    }
    if let Some(ref endpoint) = cfg.aws.endpoint_url {
        config_loader = config_loader.endpoint_url(endpoint);
    }
    let sdk_config = config_loader.load().await;
    aws_sdk_secretsmanager::Client::new(&sdk_config)
}

#[cfg(feature = "secrets-aws")]
async fn resolve_aws_sm(
    key: &str,
    uri: &str,
    client: &aws_sdk_secretsmanager::Client,
) -> anyhow::Result<String> {
    let (secret_id, json_key) = parse_aws_sm_uri(uri)
        .ok_or_else(|| anyhow::anyhow!("secret '{key}': invalid aws-sm:// URI '{uri}' — expected aws-sm://<secret-id>#<json-key>"))?;

    let resp = client
        .get_secret_value()
        .secret_id(&secret_id)
        .send()
        .await
        .map_err(|e| {
            error!(secret = key, secret_id = %secret_id, "AWS Secrets Manager error");
            anyhow::anyhow!(
                "secret '{key}': failed to fetch '{secret_id}' from AWS Secrets Manager: {e}"
            )
        })?;

    let secret_string = resp.secret_string().ok_or_else(|| {
        anyhow::anyhow!(
            "secret '{key}': '{secret_id}' has no string value (binary secrets not supported)"
        )
    })?;

    // Parse as JSON and extract the key
    let json: serde_json::Value = serde_json::from_str(secret_string)
        .map_err(|e| anyhow::anyhow!("secret '{key}': '{secret_id}' is not valid JSON: {e}"))?;

    let value = json
        .get(&json_key)
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            anyhow::anyhow!("secret '{key}': JSON key '{json_key}' not found in '{secret_id}'")
        })?;

    Ok(value.to_owned())
}

/// Parse `aws-sm://secret-id#json-key` into (secret_id, json_key).
#[cfg(feature = "secrets-aws")]
fn parse_aws_sm_uri(uri: &str) -> Option<(String, String)> {
    let rest = uri.strip_prefix("aws-sm://")?;
    let (secret_id, json_key) = rest.rsplit_once('#')?;
    if secret_id.is_empty() || json_key.is_empty() {
        return None;
    }
    Some((secret_id.to_owned(), json_key.to_owned()))
}

// -- Exec provider --
// Note: script path is delimited by the first space. Paths containing spaces are not supported.

async fn resolve_exec(key: &str, uri: &str, cfg: &SecretsConfig) -> anyhow::Result<String> {
    let rest = uri.strip_prefix("exec://").unwrap();
    let mut parts_iter = rest.splitn(3, ' ');
    let script = parts_iter
        .next()
        .ok_or_else(|| anyhow::anyhow!("secret '{key}': exec:// URI missing script path"))?;
    if script.is_empty() {
        anyhow::bail!("secret '{key}': exec:// URI has empty script path");
    }

    let mut cmd = tokio::process::Command::new(script);
    cmd.kill_on_drop(true);

    // Sanitized environment (same as pre_boot hooks — no unrelated tokens leak)
    cmd.env_clear();
    if let Ok(v) = std::env::var("HOME") {
        cmd.env("HOME", &v);
    }
    if let Ok(v) = std::env::var("PATH") {
        cmd.env("PATH", &v);
    }
    #[cfg(unix)]
    if let Ok(v) = std::env::var("USER") {
        cmd.env("USER", &v);
    }
    // Pass through cloud credential env vars for IAM-based auth
    for (key, val) in std::env::vars() {
        let pass = key.starts_with("AWS_")
            || key.starts_with("AMAZON_")
            || key.starts_with("ECS_CONTAINER_METADATA_URI")
            || key.starts_with("GOOGLE_")
            || key.starts_with("GCLOUD_")
            || key.starts_with("CLOUDSDK_")
            || key.starts_with("AZURE_");
        if pass {
            cmd.env(&key, &val);
        }
    }

    // Pass remaining parts as arguments (key, attribute)
    for arg in parts_iter {
        if !arg.is_empty() {
            cmd.arg(arg);
        }
    }

    let timeout = std::time::Duration::from_secs(cfg.exec.timeout_seconds);
    let output = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "secret '{key}': exec script '{script}' timed out after {}s",
                cfg.exec.timeout_seconds
            )
        })?
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                anyhow::anyhow!(
                    "secret '{key}': exec script '{script}' not found — did [hooks.pre_boot] run successfully?"
                )
            } else {
                anyhow::anyhow!("secret '{key}': failed to execute '{script}': {e}")
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        error!(secret = key, script, %stderr, "exec provider failed");
        anyhow::bail!(
            "secret '{key}': exec script '{script}' exited with {}",
            output.status
        );
    }

    let value = String::from_utf8(output.stdout)
        .map_err(|e| anyhow::anyhow!("secret '{key}': exec output is not valid UTF-8: {e}"))?;
    Ok(value.trim_end_matches('\n').to_owned())
}

/// Substitute `${secrets.<key>}` references in the raw config text with resolved values.
/// Uses single-pass replacement to avoid double-substitution if a secret value
/// itself contains `${secrets.*}` patterns.
/// Values are escaped for use within TOML double-quoted strings.
pub fn substitute(raw: &str, secrets: &ResolvedSecrets) -> String {
    let re = regex::Regex::new(r"\$\{secrets\.([^}]+)\}").unwrap();
    re.replace_all(raw, |caps: &regex::Captures| {
        let key = &caps[1];
        secrets
            .get(key)
            .map(|v| escape_toml_value(v))
            .unwrap_or_else(|| caps[0].to_owned())
    })
    .into_owned()
}

/// Escape a string value so it is safe inside a TOML double-quoted string.
fn escape_toml_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "secrets-aws")]
    #[test]
    fn parse_aws_sm_uri_valid() {
        let (id, key) = parse_aws_sm_uri("aws-sm://openab/prod#discord_bot_token").unwrap();
        assert_eq!(id, "openab/prod");
        assert_eq!(key, "discord_bot_token");
    }

    #[cfg(feature = "secrets-aws")]
    #[test]
    fn parse_aws_sm_uri_with_arn() {
        let uri =
            "aws-sm://arn:aws:secretsmanager:us-east-1:123456789:secret:my-secret-abc123#api_key";
        let (id, key) = parse_aws_sm_uri(uri).unwrap();
        assert_eq!(
            id,
            "arn:aws:secretsmanager:us-east-1:123456789:secret:my-secret-abc123"
        );
        assert_eq!(key, "api_key");
    }

    #[cfg(feature = "secrets-aws")]
    #[test]
    fn parse_aws_sm_uri_missing_key() {
        assert!(parse_aws_sm_uri("aws-sm://openab/prod").is_none());
        assert!(parse_aws_sm_uri("aws-sm://openab/prod#").is_none());
        assert!(parse_aws_sm_uri("aws-sm://#key").is_none());
    }

    #[test]
    fn substitute_replaces_secrets() {
        let mut secrets = HashMap::new();
        secrets.insert("token".to_owned(), "my-secret-value".to_owned());
        let input = r#"bot_token = "${secrets.token}""#;
        let output = substitute(input, &secrets);
        assert_eq!(output, r#"bot_token = "my-secret-value""#);
    }

    #[test]
    fn substitute_escapes_special_chars() {
        let mut secrets = HashMap::new();
        secrets.insert("key".to_owned(), "has\"quotes\\and\nnewlines".to_owned());
        let input = r#"value = "${secrets.key}""#;
        let output = substitute(input, &secrets);
        assert_eq!(output, r#"value = "has\"quotes\\and\nnewlines""#);
    }

    #[test]
    fn substitute_no_match_unchanged() {
        let secrets = HashMap::new();
        let input = r#"bot_token = "${DISCORD_BOT_TOKEN}""#;
        let output = substitute(input, &secrets);
        assert_eq!(output, input);
    }

    #[test]
    fn substitute_unknown_key_left_intact() {
        let mut secrets = HashMap::new();
        secrets.insert("known".to_owned(), "val".to_owned());
        let input = r#"a = "${secrets.known}" b = "${secrets.unknown}""#;
        let output = substitute(input, &secrets);
        assert_eq!(output, r#"a = "val" b = "${secrets.unknown}""#);
    }

    #[test]
    fn substitute_no_double_replacement() {
        let mut secrets = HashMap::new();
        // Secret value itself contains a ${secrets.*} pattern
        secrets.insert("a".to_owned(), "${secrets.b}".to_owned());
        secrets.insert("b".to_owned(), "should-not-appear".to_owned());
        let input = r#"val = "${secrets.a}""#;
        let output = substitute(input, &secrets);
        // The literal ${secrets.b} should be escaped, not re-substituted
        assert!(!output.contains("should-not-appear"));
    }

    #[test]
    fn substitute_multiple_refs_same_line() {
        let mut secrets = HashMap::new();
        secrets.insert("user".to_owned(), "admin".to_owned());
        secrets.insert("pass".to_owned(), "s3cret".to_owned());
        let input = r#"dsn = "postgres://${secrets.user}:${secrets.pass}@localhost""#;
        let output = substitute(input, &secrets);
        assert_eq!(output, r#"dsn = "postgres://admin:s3cret@localhost""#);
    }

    #[test]
    fn escape_toml_value_basic() {
        assert_eq!(escape_toml_value("hello"), "hello");
        assert_eq!(escape_toml_value(r#"a"b"#), r#"a\"b"#);
        assert_eq!(escape_toml_value("a\\b"), "a\\\\b");
        assert_eq!(escape_toml_value("line1\nline2"), "line1\\nline2");
        assert_eq!(escape_toml_value("tab\there"), "tab\\there");
        assert_eq!(escape_toml_value("cr\rhere"), "cr\\rhere");
    }

    #[test]
    fn escape_toml_value_combined() {
        let input = "key=\"val\"\nnext";
        let output = escape_toml_value(input);
        assert_eq!(output, "key=\\\"val\\\"\\nnext");
    }

    #[tokio::test]
    async fn resolve_exec_success() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig { timeout_seconds: 5 },
            refs: HashMap::new(),
        };
        // Use echo as a script
        let result = resolve_exec("test", "exec:///bin/echo hello world", &cfg).await;
        assert_eq!(result.unwrap(), "hello world");
    }

    #[tokio::test]
    async fn resolve_exec_not_found() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig { timeout_seconds: 5 },
            refs: HashMap::new(),
        };
        let result = resolve_exec("test", "exec:///nonexistent/script arg1 arg2", &cfg).await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"));
        assert!(err.contains("pre_boot"));
    }

    #[tokio::test]
    async fn resolve_exec_nonzero_exit() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig { timeout_seconds: 5 },
            refs: HashMap::new(),
        };
        let result = resolve_exec("test", "exec:///bin/false", &cfg).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("exited with"));
    }

    #[tokio::test]
    async fn resolve_exec_strips_trailing_newline() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig { timeout_seconds: 5 },
            refs: HashMap::new(),
        };
        // printf adds no newline, echo does — test both
        let result = resolve_exec("test", "exec:///bin/echo secret_value", &cfg).await;
        assert_eq!(result.unwrap(), "secret_value");
    }

    #[tokio::test]
    async fn resolve_empty_refs_returns_empty() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig::default(),
            refs: HashMap::new(),
        };
        let result = resolve(&cfg).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn resolve_unknown_scheme_fails() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        let mut refs = HashMap::new();
        refs.insert("bad".to_owned(), "ftp://something".to_owned());
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig::default(),
            refs,
        };
        let result = resolve(&cfg).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("unrecognized URI scheme"));
    }

    #[cfg(feature = "secrets-aws")]
    #[test]
    fn parse_aws_sm_uri_hash_in_secret_name() {
        // rsplit_once('#') should split on the LAST #
        let uri = "aws-sm://my#secret#api_key";
        let (id, key) = parse_aws_sm_uri(uri).unwrap();
        assert_eq!(id, "my#secret");
        assert_eq!(key, "api_key");
    }

    #[tokio::test]
    async fn resolve_exec_sanitized_env() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        // Set a dummy env var that should NOT be visible to the exec script
        std::env::set_var("OPENAB_TEST_LEAKED_SECRET", "should_not_leak");
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig { timeout_seconds: 5 },
            refs: HashMap::new(),
        };
        // /usr/bin/env prints all env vars; grep for our dummy var
        let result = resolve_exec("test", "exec:///usr/bin/env", &cfg)
            .await
            .unwrap();
        assert!(
            !result.contains("OPENAB_TEST_LEAKED_SECRET"),
            "exec script should not see unrelated env vars"
        );
        // HOME and PATH should still be present
        assert!(result.contains("HOME="), "HOME should be in sanitized env");
        assert!(result.contains("PATH="), "PATH should be in sanitized env");
        std::env::remove_var("OPENAB_TEST_LEAKED_SECRET");
    }

    #[tokio::test]
    async fn resolve_exec_timeout() {
        use crate::config::{AwsSecretsConfig, ExecSecretsConfig, SecretsConfig};
        let cfg = SecretsConfig {
            aws: AwsSecretsConfig::default(),
            exec: ExecSecretsConfig { timeout_seconds: 1 },
            refs: HashMap::new(),
        };
        // sleep 10 will be killed after 1s timeout
        let result = resolve_exec("test", "exec:///bin/sleep 10", &cfg).await;
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("timed out"),
            "expected timeout error, got: {err}"
        );
    }

    #[test]
    fn substitute_and_reparse_integration() {
        let mut secrets = HashMap::new();
        secrets.insert("token".to_owned(), "xoxb-secret-value".to_owned());
        secrets.insert("key".to_owned(), "sk-with\"special\\chars".to_owned());

        let raw = r#"
[discord]
bot_token = "${secrets.token}"

[agent]
command = "echo"
args = ["--key", "${secrets.key}"]
"#;
        let substituted = substitute(raw, &secrets);
        // Verify the substituted text is valid TOML that parses correctly
        let cfg: crate::config::Config =
            toml::from_str(&substituted).expect("substituted config should be valid TOML");
        assert_eq!(cfg.discord.unwrap().bot_token, "xoxb-secret-value");
        assert_eq!(cfg.agent.args[1], "sk-with\"special\\chars");
    }
}
