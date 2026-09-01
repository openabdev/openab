use crate::config::{HookConfig, OnFailure};
use sha2::{Digest, Sha256};
use std::io::Write;
use std::path::PathBuf;
use tokio::process::Command;
use tracing::{error, info, warn};

/// Maximum size for a remote hook script (1 MiB).
const MAX_SCRIPT_SIZE: usize = 1024 * 1024;

/// Run a hook. Returns Ok(()) if the hook succeeds or is not configured.
/// Returns Err only if on_failure=abort and the hook fails.
pub async fn run_hook(name: &str, hook: &HookConfig) -> anyhow::Result<()> {
    info!(hook = name, "running hook");

    let resolved = match resolve_script(name, hook).await {
        Ok(r) => r,
        Err(e) => return handle_failure(name, hook.on_failure, e),
    };

    let result = execute(&resolved.path, hook.timeout_seconds).await;

    // Clean up temp files
    if resolved.temp {
        let _ = std::fs::remove_file(&resolved.path);
    }

    match result {
        Ok(()) => {
            info!(hook = name, "hook completed successfully");
            Ok(())
        }
        Err(e) => handle_failure(name, hook.on_failure, e),
    }
}

/// Validate hook config at parse time.
pub fn validate_hook(name: &str, hook: &HookConfig) -> anyhow::Result<()> {
    let sources = [
        hook.script.is_some(),
        hook.inline.is_some(),
        hook.url.is_some(),
    ];
    let count = sources.iter().filter(|&&b| b).count();
    if count == 0 {
        anyhow::bail!("hooks.{name}: exactly one of script, inline, or url must be set");
    }
    if count > 1 {
        anyhow::bail!(
            "hooks.{name}: only one of script, inline, or url may be set (found {count})"
        );
    }
    if hook.url.is_some() && hook.sha256.is_none() {
        anyhow::bail!("hooks.{name}: sha256 is required when using url");
    }
    if let Some(ref path) = hook.script {
        if !PathBuf::from(path).is_absolute() {
            anyhow::bail!("hooks.{name}: script path must be absolute, got: {path}");
        }
    }
    Ok(())
}

struct ResolvedScript {
    path: PathBuf,
    temp: bool,
}

async fn resolve_script(name: &str, hook: &HookConfig) -> anyhow::Result<ResolvedScript> {
    if let Some(ref path) = hook.script {
        let p = PathBuf::from(path);
        if !p.exists() {
            anyhow::bail!("hooks.{name}: script not found: {path}");
        }
        return Ok(ResolvedScript {
            path: p,
            temp: false,
        });
    }

    if let Some(ref content) = hook.inline {
        let path = write_temp_script(name, content)?;
        return Ok(ResolvedScript { path, temp: true });
    }

    if let Some(ref url) = hook.url {
        let expected_hash = hook.sha256.as_deref().unwrap();
        let content = fetch_and_verify(url, expected_hash).await?;
        let path = write_temp_script(name, &content)?;
        return Ok(ResolvedScript { path, temp: true });
    }

    anyhow::bail!("hooks.{name}: no script source configured");
}

fn write_temp_script(name: &str, content: &str) -> anyhow::Result<PathBuf> {
    #[cfg(unix)]
    let suffix = ".sh";
    #[cfg(windows)]
    let suffix = ".cmd";

    let prefix = format!("openab-hook-{name}-");
    let mut builder = tempfile::Builder::new();
    builder.prefix(prefix.as_str()).suffix(suffix);

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }

    let mut f = builder.tempfile()?;
    f.write_all(content.as_bytes())?;
    let path = f
        .into_temp_path()
        .keep()
        .map_err(|e| anyhow::anyhow!("failed to persist temp script: {}", e.error))?;
    Ok(path)
}

async fn fetch_and_verify(url: &str, expected_hex: &str) -> anyhow::Result<String> {
    info!(url = url, "fetching hook script from URL");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("hook url returned HTTP {}", resp.status());
    }
    let content_length = resp.content_length().unwrap_or(0) as usize;
    if content_length > MAX_SCRIPT_SIZE {
        anyhow::bail!("hook script too large: {content_length} bytes (max {MAX_SCRIPT_SIZE})");
    }
    let body = resp.bytes().await?;
    if body.len() > MAX_SCRIPT_SIZE {
        anyhow::bail!(
            "hook script too large: {} bytes (max {MAX_SCRIPT_SIZE})",
            body.len()
        );
    }

    let mut hasher = Sha256::new();
    hasher.update(&body);
    let actual_hex = format!("{:x}", hasher.finalize());

    if actual_hex != expected_hex.to_lowercase() {
        anyhow::bail!("hook sha256 mismatch: expected {expected_hex}, got {actual_hex}");
    }

    Ok(String::from_utf8(body.to_vec())?)
}

async fn execute(path: &PathBuf, timeout_secs: u64) -> anyhow::Result<()> {
    let mut cmd = Command::new(path);
    cmd.env_clear();

    // Baseline env (same as agent subprocess)
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
    #[cfg(windows)]
    {
        if let Ok(v) = std::env::var("USERPROFILE") {
            cmd.env("USERPROFILE", &v);
        }
        if let Ok(v) = std::env::var("USERNAME") {
            cmd.env("USERNAME", &v);
        }
        if let Ok(v) = std::env::var("SystemRoot") {
            cmd.env("SystemRoot", &v);
        }
        if let Ok(v) = std::env::var("SystemDrive") {
            cmd.env("SystemDrive", &v);
        }
    }

    // Pass through cloud credential env vars for IAM-based auth (IRSA, Workload Identity, ECS task role)
    for (key, val) in std::env::vars() {
        let pass = key.starts_with("AWS_")
            || key.starts_with("AMAZON_")
            || key.starts_with("ECS_CONTAINER_METADATA_URI")
            || key.starts_with("GOOGLE_")
            || key.starts_with("GCLOUD_")
            || key.starts_with("CLOUDSDK_")
            || key.starts_with("AZURE_")
            || key == "BOOTSTRAP_URI"
            || key == "BOOTSTRAP_BASE_URI"
            || key == "BOOTSTRAP_PERSONAL_URI"
            || key == "STATE_BUCKET"
            || key == "TASK_FAMILY"
            || key == "OPENAB_AGENT_NAME"
            || key == "OPENAB_BACKEND_AGENT";
        if pass {
            cmd.env(&key, &val);
        }
    }

    let mut child = cmd.spawn()?;

    if timeout_secs == 0 {
        let status = child.wait().await?;
        if !status.success() {
            anyhow::bail!("hook exited with {status}");
        }
        return Ok(());
    }

    let timeout = std::time::Duration::from_secs(timeout_secs);
    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            if !status.success() {
                anyhow::bail!("hook exited with {status}");
            }
            Ok(())
        }
        Ok(Err(e)) => anyhow::bail!("hook process error: {e}"),
        Err(_) => {
            let _ = child.kill().await;
            anyhow::bail!("hook timed out after {timeout_secs}s");
        }
    }
}

fn handle_failure(name: &str, policy: OnFailure, err: anyhow::Error) -> anyhow::Result<()> {
    match policy {
        OnFailure::Abort => {
            error!(hook = name, error = %err, "hook failed (on_failure=abort)");
            Err(err)
        }
        OnFailure::Warn => {
            warn!(hook = name, error = %err, "hook failed (on_failure=warn), continuing");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{HookConfig, OnFailure};

    fn hook_with_script(path: &str) -> HookConfig {
        HookConfig {
            script: Some(path.into()),
            inline: None,
            url: None,
            sha256: None,
            timeout_seconds: 60,
            on_failure: OnFailure::Abort,
        }
    }

    fn hook_with_inline(content: &str) -> HookConfig {
        HookConfig {
            script: None,
            inline: Some(content.into()),
            url: None,
            sha256: None,
            timeout_seconds: 60,
            on_failure: OnFailure::Abort,
        }
    }

    #[test]
    fn validate_rejects_no_source() {
        let hook = HookConfig {
            script: None,
            inline: None,
            url: None,
            sha256: None,
            timeout_seconds: 60,
            on_failure: OnFailure::Abort,
        };
        assert!(validate_hook("test", &hook).is_err());
    }

    #[test]
    fn validate_rejects_multiple_sources() {
        let hook = HookConfig {
            script: Some("/bin/true".into()),
            inline: Some("echo hi".into()),
            url: None,
            sha256: None,
            timeout_seconds: 60,
            on_failure: OnFailure::Abort,
        };
        assert!(validate_hook("test", &hook).is_err());
    }

    #[test]
    fn validate_rejects_url_without_sha256() {
        let hook = HookConfig {
            script: None,
            inline: None,
            url: Some("https://example.com/script.sh".into()),
            sha256: None,
            timeout_seconds: 60,
            on_failure: OnFailure::Abort,
        };
        assert!(validate_hook("test", &hook).is_err());
    }

    #[test]
    fn validate_rejects_relative_script_path() {
        let hook = hook_with_script("relative/path.sh");
        assert!(validate_hook("test", &hook).is_err());
    }

    #[test]
    fn validate_accepts_absolute_script_path() {
        let hook = hook_with_script("/usr/local/bin/bootstrap.sh");
        assert!(validate_hook("test", &hook).is_ok());
    }

    #[test]
    fn validate_accepts_inline() {
        let hook = hook_with_inline("#!/bin/sh\necho hello");
        assert!(validate_hook("test", &hook).is_ok());
    }

    #[test]
    fn validate_accepts_url_with_sha256() {
        let hook = HookConfig {
            script: None,
            inline: None,
            url: Some("https://example.com/script.sh".into()),
            sha256: Some("abc123".into()),
            timeout_seconds: 60,
            on_failure: OnFailure::Abort,
        };
        assert!(validate_hook("test", &hook).is_ok());
    }

    #[tokio::test]
    async fn run_inline_script_success() {
        let hook = hook_with_inline("#!/bin/sh\nexit 0");
        let result = run_hook("test", &hook).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_inline_script_failure_abort() {
        let hook = hook_with_inline("#!/bin/sh\nexit 1");
        let result = run_hook("test", &hook).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn run_inline_script_failure_warn() {
        let hook = HookConfig {
            script: None,
            inline: Some("#!/bin/sh\nexit 1".into()),
            url: None,
            sha256: None,
            timeout_seconds: 60,
            on_failure: OnFailure::Warn,
        };
        let result = run_hook("test", &hook).await;
        assert!(result.is_ok()); // warn mode continues
    }

    #[tokio::test]
    async fn run_inline_script_timeout() {
        let hook = HookConfig {
            script: None,
            inline: Some("#!/bin/sh\nsleep 10".into()),
            url: None,
            sha256: None,
            timeout_seconds: 1,
            on_failure: OnFailure::Abort,
        };
        let result = run_hook("test", &hook).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("timed out"));
    }

    #[tokio::test]
    async fn run_script_file_success() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("openab-test-hook-success.sh");
        std::fs::write(&path, "#!/bin/sh\nexit 0").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let hook = hook_with_script(path.to_str().unwrap());
        let result = run_hook("test", &hook).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn run_script_file_not_found() {
        let hook = hook_with_script("/tmp/openab-nonexistent-hook-12345.sh");
        let result = run_hook("test", &hook).await;
        assert!(result.is_err());
    }

    #[test]
    fn config_parses_hooks() {
        let toml_str = "[agent]\ncommand = \"echo\"\n\n[hooks.pre_boot]\ninline = \"echo hello\"\ntimeout_seconds = 30\non_failure = \"warn\"\n";
        let cfg: crate::config::Config = toml::from_str(toml_str).unwrap();
        let hook = cfg.hooks.pre_boot.unwrap();
        assert_eq!(hook.inline.unwrap(), "echo hello");
        assert_eq!(hook.timeout_seconds, 30);
        assert_eq!(hook.on_failure, OnFailure::Warn);
    }

    #[test]
    fn config_parses_no_hooks() {
        let toml_str = "[agent]\ncommand = \"echo\"\n";
        let cfg: crate::config::Config = toml::from_str(toml_str).unwrap();
        assert!(cfg.hooks.pre_boot.is_none());
        assert!(cfg.hooks.pre_shutdown.is_none());
    }
}
