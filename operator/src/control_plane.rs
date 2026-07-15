use anyhow::{Context, Result};

pub(crate) fn select_bucket(configured: Option<&str>, env: Option<&str>) -> Option<String> {
    configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| env.map(str::trim).filter(|value| !value.is_empty()))
        .map(str::to_owned)
}

pub(crate) async fn resolve_bucket(
    aws_config: &aws_config::SdkConfig,
    configured: Option<&str>,
) -> Result<String> {
    let env_bucket = std::env::var("OAB_CONTROL_PLANE_BUCKET").ok();
    if let Some(bucket) = select_bucket(configured, env_bucket.as_deref()) {
        return Ok(bucket);
    }

    let identity = aws_sdk_sts::Client::new(aws_config)
        .get_caller_identity()
        .send()
        .await
        .context("failed to resolve control-plane bucket: STS get_caller_identity failed")?;
    let account = identity
        .account()
        .context("failed to resolve control-plane bucket: STS response missing account")?;
    Ok(format!("oab-control-plane-{account}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_bucket_wins_over_environment() {
        assert_eq!(
            select_bucket(Some("configured"), Some("environment")).as_deref(),
            Some("configured")
        );
    }

    #[test]
    fn environment_bucket_is_used_without_config_override() {
        assert_eq!(
            select_bucket(None, Some("environment")).as_deref(),
            Some("environment")
        );
    }

    #[test]
    fn blank_overrides_are_ignored() {
        assert_eq!(select_bucket(Some("  "), Some(" \t")), None);
    }
}
