//! Shared logical→physical identity rules for apply and delete.
//!
//! Apply and delete derive every physical resource identity — the ECS service
//! name, the Cloud Map service name, and the control-plane S3 keys — from the
//! logical `namespace`/`name` pair via `oab-{namespace}-{name}`. Because `-`
//! is the delimiter and names may legitimately contain `-`, that mapping is
//! injective only while namespaces stay hyphen-free: `prod/team-bot` and
//! `prod-team/bot` would otherwise both resolve to `oab-prod-team-bot`.
//!
//! Two rules enforce injectivity across every entry point:
//!
//! 1. **Domain rule** — [`validate_injective_identity`] rejects hyphenated
//!    namespaces before any AWS call. It is enforced by
//!    [`crate::manifest::OABServiceManifest::validate`] (covering CLI apply,
//!    fleet expansion, and programmatic
//!    [`crate::apply::apply_manifests`]) and by programmatic
//!    [`crate::delete::delete_services`].
//! 2. **Ownership rule** — [`ensure_exclusive_physical_identity`] runs before
//!    any destructive mutation on both the apply and delete paths. It fails
//!    closed when the control plane records any *other* logical pair (for
//!    example a legacy hyphenated namespace) claiming the same physical name.
//!
//! # Legacy hyphenated namespaces
//!
//! Deployments created under a hyphenated namespace before the domain rule
//! existed can no longer be applied — apply fails with a migration message.
//! They remain deletable through the `oabctl delete` CLI, which accepts a
//! hyphenated namespace for teardown and relies on the ownership rule to
//! refuse when the physical name is contested by another recorded logical
//! pair. The migration path is: `oabctl delete` the legacy deployment, then
//! re-create it under a hyphen-free namespace.

use anyhow::Context;
use aws_sdk_s3::error::ProvideErrorMetadata;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

/// Physical service identity shared by ECS and Cloud Map:
/// `oab-{namespace}-{name}`.
pub(crate) fn physical_service_name(namespace: &str, name: &str) -> String {
    format!("oab-{namespace}-{name}")
}

/// Control-plane key of the stored desired-state manifest.
pub(crate) fn manifest_key(namespace: &str, name: &str) -> String {
    format!("manifests/{namespace}/{name}.yaml")
}

/// Control-plane key of the durable delete checkpoint.
pub(crate) fn delete_checkpoint_key(namespace: &str, name: &str) -> String {
    format!("delete-checkpoints/{namespace}/{name}.json")
}

/// Control-plane key of the apply-side ingress-teardown checkpoint.
pub(crate) fn ingress_teardown_checkpoint_key(namespace: &str, name: &str) -> String {
    format!("ingress-teardown-checkpoints/{namespace}/{name}.json")
}

/// Domain rule: `namespace` and `name` must be non-empty and `namespace` must
/// not contain `-`, so that `oab-{namespace}-{name}` parses back to exactly
/// one logical pair.
pub(crate) fn validate_injective_identity(namespace: &str, name: &str) -> anyhow::Result<()> {
    if namespace.trim().is_empty() {
        anyhow::bail!("namespace must not be empty (got '{namespace}'/'{name}')");
    }
    if name.trim().is_empty() {
        anyhow::bail!("name must not be empty (got '{namespace}'/'{name}')");
    }
    if namespace.contains('-') {
        anyhow::bail!(
            "namespace '{namespace}' must not contain '-': it is the delimiter of the physical \
             identity '{physical}', so a hyphenated namespace collides with other logical \
             targets (for example 'a-b/c' and 'a/b-c'). Legacy deployments created under a \
             hyphenated namespace remain deletable with `oabctl delete`, which verifies \
             exclusive ownership in the control plane; re-create them under a hyphen-free \
             namespace",
            physical = physical_service_name(namespace, name),
        );
    }
    Ok(())
}

/// Every logical pair distinct from `(namespace, name)` that maps to the same
/// physical identity. These are the alternative split points of
/// `{namespace}-{name}` and only exist when a component contains `-`.
pub(crate) fn collision_aliases(namespace: &str, name: &str) -> Vec<(String, String)> {
    let joined = format!("{namespace}-{name}");
    let mut aliases = Vec::new();
    for (index, _) in joined.match_indices('-') {
        let alias_namespace = &joined[..index];
        let alias_name = &joined[index + 1..];
        if alias_namespace.is_empty() || alias_name.is_empty() {
            continue;
        }
        if alias_namespace == namespace && alias_name == name {
            continue;
        }
        aliases.push((alias_namespace.to_string(), alias_name.to_string()));
    }
    aliases
}

/// Control-plane keys whose presence records ownership of a logical pair.
pub(crate) fn ownership_keys(namespace: &str, name: &str) -> [String; 3] {
    [
        manifest_key(namespace, name),
        delete_checkpoint_key(namespace, name),
        ingress_teardown_checkpoint_key(namespace, name),
    ]
}

fn contested_identity_error(
    namespace: &str,
    name: &str,
    alias_namespace: &str,
    alias_name: &str,
    bucket: &str,
    key: &str,
) -> anyhow::Error {
    anyhow::anyhow!(
        "physical identity '{physical}' is contested: the control plane records \
         '{alias_namespace}/{alias_name}' (s3://{bucket}/{key}), which maps to the same \
         physical name as '{namespace}/{name}'. Refusing to mutate. Resolve the collision \
         first: delete the legacy '{alias_namespace}/{alias_name}' deployment with \
         `oabctl delete`, or pick a namespace/name pair that does not collide",
        physical = physical_service_name(namespace, name),
    )
}

const OWNERSHIP_PROBE_CONCURRENCY: usize = 8;

async fn probe_ownership_key(
    s3: aws_sdk_s3::Client,
    bucket: String,
    namespace: String,
    name: String,
    alias_namespace: String,
    alias_name: String,
    key: String,
) -> anyhow::Result<()> {
    match s3.get_object().bucket(&bucket).key(&key).send().await {
        Ok(_) => Err(contested_identity_error(
            &namespace,
            &name,
            &alias_namespace,
            &alias_name,
            &bucket,
            &key,
        )),
        Err(error) if matches!(error.code(), Some("NoSuchKey" | "NotFound")) => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to verify exclusive ownership of '{}' via s3://{bucket}/{key}",
                physical_service_name(&namespace, &name)
            )
        }),
    }
}

/// Ownership rule: fail closed if the control plane records any logical pair,
/// distinct from `(namespace, name)`, that claims the same physical identity.
///
/// Runs before any destructive mutation on the apply and delete paths. Probes
/// only the exact alias keys (stored manifest, delete checkpoint, and
/// ingress-teardown checkpoint), so hyphen-free pairs with hyphen-free names
/// cost zero requests. Alias probes are bounded to
/// [`OWNERSHIP_PROBE_CONCURRENCY`] in flight; probe failures other than a
/// missing key fail closed.
pub(crate) async fn ensure_exclusive_physical_identity(
    s3: &aws_sdk_s3::Client,
    bucket: &str,
    namespace: &str,
    name: &str,
) -> anyhow::Result<()> {
    let semaphore = Arc::new(Semaphore::new(OWNERSHIP_PROBE_CONCURRENCY));
    let mut probes = JoinSet::new();

    for (alias_namespace, alias_name) in collision_aliases(namespace, name) {
        for key in ownership_keys(&alias_namespace, &alias_name) {
            let permit = match semaphore.clone().acquire_owned().await {
                Ok(permit) => permit,
                Err(error) => {
                    probes.abort_all();
                    while probes.join_next().await.is_some() {}
                    return Err(anyhow::anyhow!(
                        "ownership probe concurrency gate closed: {error}"
                    ));
                }
            };
            let probe_s3 = s3.clone();
            let probe_bucket = bucket.to_owned();
            let probe_namespace = namespace.to_owned();
            let probe_name = name.to_owned();
            let probe_alias_namespace = alias_namespace;
            let probe_alias_name = alias_name;
            probes.spawn(async move {
                let _permit = permit;
                probe_ownership_key(
                    probe_s3,
                    probe_bucket,
                    probe_namespace,
                    probe_name,
                    probe_alias_namespace,
                    probe_alias_name,
                    key,
                )
                .await
            });
        }
    }

    while let Some(result) = probes.join_next().await {
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                probes.abort_all();
                while probes.join_next().await.is_some() {}
                return Err(error);
            }
            Err(error) => {
                probes.abort_all();
                while probes.join_next().await.is_some() {}
                return Err(anyhow::anyhow!("ownership probe task failed: {error}"));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_hyphen_free_namespace_with_hyphenated_name() {
        validate_injective_identity("prod", "nest-my-oab").expect("valid identity");
    }

    #[test]
    fn rejects_empty_components() {
        assert!(validate_injective_identity("", "bot").is_err());
        assert!(validate_injective_identity("prod", "  ").is_err());
    }

    #[test]
    fn rejects_hyphenated_namespace_with_migration_hint() {
        let error = validate_injective_identity("prod-team", "bot").unwrap_err();
        let message = error.to_string();
        assert!(message.contains("must not contain '-'"), "{message}");
        assert!(message.contains("oab-prod-team-bot"), "{message}");
        assert!(message.contains("oabctl delete"), "{message}");
    }

    #[test]
    fn hyphen_free_pair_has_no_collision_aliases() {
        assert!(collision_aliases("prod", "bot").is_empty());
    }

    #[test]
    fn collision_aliases_enumerate_every_alternative_split() {
        assert_eq!(
            collision_aliases("prod", "team-bot"),
            vec![("prod-team".to_string(), "bot".to_string())]
        );
        assert_eq!(
            collision_aliases("a", "b-c-d"),
            vec![
                ("a-b".to_string(), "c-d".to_string()),
                ("a-b-c".to_string(), "d".to_string()),
            ]
        );
    }

    #[test]
    fn long_hyphenated_names_keep_alias_probe_shape_bounded_by_scheduler() {
        let name = (0..101).map(|_| "x").collect::<Vec<_>>().join("-");
        assert_eq!(name.len(), 201);
        let aliases = collision_aliases("prod", &name);
        assert_eq!(aliases.len(), 100);
        assert_eq!(
            aliases
                .iter()
                .map(|(namespace, name)| ownership_keys(namespace, name).len())
                .sum::<usize>(),
            300
        );
        assert!(OWNERSHIP_PROBE_CONCURRENCY < 300);
    }

    #[test]
    fn collision_aliases_are_symmetric_across_the_colliding_pair() {
        // The new-domain pair sees the legacy pair…
        assert!(collision_aliases("prod", "team-bot")
            .contains(&("prod-team".to_string(), "bot".to_string())));
        // …and the legacy pair sees the new-domain pair, so the ownership
        // probe protects both directions of the collision.
        assert!(collision_aliases("prod-team", "bot")
            .contains(&("prod".to_string(), "team-bot".to_string())));
    }

    #[test]
    fn colliding_pairs_share_one_physical_identity() {
        assert_eq!(
            physical_service_name("prod", "team-bot"),
            physical_service_name("prod-team", "bot"),
        );
    }

    #[test]
    fn ownership_keys_cover_manifest_and_both_checkpoints() {
        assert_eq!(
            ownership_keys("prod-team", "bot"),
            [
                "manifests/prod-team/bot.yaml".to_string(),
                "delete-checkpoints/prod-team/bot.json".to_string(),
                "ingress-teardown-checkpoints/prod-team/bot.json".to_string(),
            ]
        );
    }

    #[test]
    fn contested_identity_error_names_both_pairs_and_the_evidence_key() {
        let error = contested_identity_error(
            "prod",
            "team-bot",
            "prod-team",
            "bot",
            "control-plane",
            "manifests/prod-team/bot.yaml",
        );
        let message = error.to_string();
        assert!(message.contains("oab-prod-team-bot"), "{message}");
        assert!(message.contains("prod-team/bot"), "{message}");
        assert!(
            message.contains("s3://control-plane/manifests/prod-team/bot.yaml"),
            "{message}"
        );
    }
}
