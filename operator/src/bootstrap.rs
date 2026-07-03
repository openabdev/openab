use anyhow::{Context, Result};
use aws_sdk_cloudwatchlogs::Client as LogsClient;
use aws_sdk_ec2::Client as Ec2Client;
use aws_sdk_ecs::Client as EcsClient;
use aws_sdk_iam::Client as IamClient;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_sts::Client as StsClient;
use serde::{Deserialize, Serialize};

const CLUSTER_NAME: &str = "oab";
const EXECUTION_ROLE: &str = "oab-task-execution";
const TASK_ROLE: &str = "oab-task-role";
const SG_NAME: &str = "oab-agents";
const LOG_GROUP: &str = "/oab/agents";
const STATE_KEY: &str = "bootstrap/state.json";

const ASSUME_ROLE_POLICY: &str = r#"{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Principal": {"Service": "ecs-tasks.amazonaws.com"},
    "Action": "sts:AssumeRole"
  }]
}"#;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapState {
    pub version: u32,
    pub account: String,
    pub region: String,
    pub bucket: String,
    pub resources: BootstrapResources,
    pub managed: ManagedFlags,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BootstrapResources {
    pub cluster_arn: String,
    pub execution_role_arn: String,
    pub task_role_arn: String,
    pub security_group_id: String,
    pub log_group: String,
    pub subnets: Vec<String>,
    pub vpc_id: String,
}

/// Tracks which resources were created by bootstrap vs imported
#[derive(Debug, Serialize, Deserialize, Default, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ManagedFlags {
    pub cluster: bool,
    pub execution_role: bool,
    pub task_role: bool,
    pub security_group: bool,
    pub log_group: bool,
    pub bucket: bool,
}

/// Options to import existing resources instead of creating new ones
#[derive(Default)]
pub struct ImportOptions {
    pub cluster: Option<String>,
    pub vpc: Option<String>,
    pub subnets: Option<Vec<String>>,
    pub security_group: Option<String>,
    pub execution_role: Option<String>,
    pub task_role: Option<String>,
}

pub async fn run(config: &aws_config::SdkConfig, delete: bool, status: bool, imports: ImportOptions) -> Result<()> {
    if status {
        return show_status(config).await;
    }
    if delete {
        return teardown(config).await;
    }
    create(config, imports).await
}

async fn get_account_and_region(config: &aws_config::SdkConfig) -> Result<(String, String)> {
    let sts = StsClient::new(config);
    let identity = sts.get_caller_identity().send().await?;
    let account = identity.account().context("no account ID")?.to_string();
    let region = config.region().map(|r| r.to_string()).unwrap_or_else(|| "us-east-1".to_string());
    Ok((account, region))
}

fn bucket_name(account: &str) -> String {
    format!("oab-control-plane-{account}")
}

async fn load_state(s3: &S3Client, bucket: &str) -> Result<Option<BootstrapState>> {
    match s3.get_object().bucket(bucket).key(STATE_KEY).send().await {
        Ok(resp) => {
            let bytes = resp.body.collect().await?.into_bytes();
            let state: BootstrapState = serde_json::from_slice(&bytes)?;
            Ok(Some(state))
        }
        Err(_) => Ok(None),
    }
}

/// Public accessor for other modules
pub async fn load_state_pub(s3: &S3Client, bucket: &str) -> Result<Option<BootstrapState>> {
    load_state(s3, bucket).await
}

async fn save_state(s3: &S3Client, bucket: &str, state: &BootstrapState) -> Result<()> {
    let json = serde_json::to_string_pretty(state)?;
    s3.put_object()
        .bucket(bucket)
        .key(STATE_KEY)
        .body(json.into_bytes().into())
        .content_type("application/json")
        .send()
        .await
        .context("failed to save bootstrap state")?;
    Ok(())
}

// ─── CREATE ───────────────────────────────────────────────────────────────────

async fn create(config: &aws_config::SdkConfig, imports: ImportOptions) -> Result<()> {
    let (account, region) = get_account_and_region(config).await?;
    let bucket = bucket_name(&account);
    let mut managed = ManagedFlags::default();

    let ecs = EcsClient::new(config);
    let iam = IamClient::new(config);
    let s3 = S3Client::new(config);
    let ec2 = Ec2Client::new(config);
    let logs = LogsClient::new(config);

    // ─── PLAN PHASE: check existing resources ─────────────────────────────
    eprintln!("📋 Planning bootstrap for {region} (account: {account})...\n");

    let bucket_exists = s3.head_bucket().bucket(&bucket).send().await.is_ok();
    let cluster_name = imports.cluster.as_deref().unwrap_or(CLUSTER_NAME);
    let cluster_exists = ecs.describe_clusters().clusters(cluster_name).send().await
        .map(|r| r.clusters().first().is_some_and(|c| c.status() == Some("ACTIVE")))
        .unwrap_or(false);
    let exec_role_exists = imports.execution_role.is_some()
        || iam.get_role().role_name(EXECUTION_ROLE).send().await.is_ok();
    let task_role_exists = imports.task_role.is_some()
        || iam.get_role().role_name(TASK_ROLE).send().await.is_ok();

    let vpc_id_for_check = if let Some(ref v) = imports.vpc {
        v.clone()
    } else {
        ec2.describe_vpcs()
            .filters(aws_sdk_ec2::types::Filter::builder().name("isDefault").values("true").build())
            .send().await.ok()
            .and_then(|r| r.vpcs().first().and_then(|v| v.vpc_id()).map(|s| s.to_string()))
            .unwrap_or_default()
    };
    let sg_exists = imports.security_group.is_some()
        || ec2.describe_security_groups()
            .filters(aws_sdk_ec2::types::Filter::builder().name("group-name").values(SG_NAME).build())
            .filters(aws_sdk_ec2::types::Filter::builder().name("vpc-id").values(&vpc_id_for_check).build())
            .send().await
            .map(|r| !r.security_groups().is_empty())
            .unwrap_or(false);
    let log_group_exists = logs.describe_log_groups()
        .log_group_name_prefix(LOG_GROUP)
        .send().await
        .map(|r| r.log_groups().iter().any(|g| g.log_group_name() == Some(LOG_GROUP)))
        .unwrap_or(false);

    // ─── DISPLAY PLAN ─────────────────────────────────────────────────────
    eprintln!("  Resource                 Action");
    eprintln!("  ─────────────────────────────────────────");
    plan_line("S3 Bucket", &bucket, bucket_exists, true);
    plan_line("ECS Cluster", cluster_name, cluster_exists, imports.cluster.is_none());
    plan_line("IAM Execution Role", imports.execution_role.as_deref().unwrap_or(EXECUTION_ROLE), exec_role_exists, imports.execution_role.is_none());
    plan_line("IAM Task Role", imports.task_role.as_deref().unwrap_or(TASK_ROLE), task_role_exists, imports.task_role.is_none());
    plan_line("Security Group", imports.security_group.as_deref().unwrap_or(SG_NAME), sg_exists, imports.security_group.is_none());
    plan_line("CloudWatch Log Group", LOG_GROUP, log_group_exists, true);
    eprintln!();

    // ─── CONFIRM ──────────────────────────────────────────────────────────
    eprint!("Proceed? [Y/n] ");
    let mut input = String::new();
    std::io::stdin().read_line(&mut input)?;
    let input = input.trim().to_lowercase();
    if !input.is_empty() && input != "y" && input != "yes" {
        eprintln!("Aborted.");
        return Ok(());
    }

    eprintln!("\n🚀 Bootstrapping...\n");

    // 1. S3 Bucket
    if s3.head_bucket().bucket(&bucket).send().await.is_ok() {
        eprintln!("  ✓ S3 bucket already exists: {bucket}");
    } else {
        let mut req = s3.create_bucket().bucket(&bucket);
        if region != "us-east-1" {
            req = req.create_bucket_configuration(
                aws_sdk_s3::types::CreateBucketConfiguration::builder()
                    .location_constraint(region.parse().unwrap())
                    .build(),
            );
        }
        req.send().await.context("failed to create S3 bucket")?;
        // Block public access
        s3.put_public_access_block()
            .bucket(&bucket)
            .public_access_block_configuration(
                aws_sdk_s3::types::PublicAccessBlockConfiguration::builder()
                    .block_public_acls(true)
                    .ignore_public_acls(true)
                    .block_public_policy(true)
                    .restrict_public_buckets(true)
                    .build(),
            )
            .send().await.ok();
        eprintln!("  ✓ Created S3 bucket: {bucket} (public access blocked)");
        managed.bucket = true;
    }

    // 2. ECS Cluster — save state incrementally after this point
    let (cluster_arn, cluster_managed) = if let Some(ref name) = imports.cluster {
        let resp = ecs.describe_clusters().clusters(name).send().await?;
        let arn = resp.clusters().first()
            .and_then(|c| c.cluster_arn())
            .context(format!("cluster '{}' not found", name))?
            .to_string();
        eprintln!("  ✓ Using existing cluster: {name}");
        (arn, false)
    } else {
        match ecs.describe_clusters().clusters(CLUSTER_NAME).send().await {
            Ok(resp) if resp.clusters().first().is_some_and(|c| c.status() == Some("ACTIVE")) => {
                let arn = resp.clusters()[0].cluster_arn().unwrap_or_default().to_string();
                eprintln!("  ✓ ECS cluster already exists: {CLUSTER_NAME}");
                (arn, true)
            }
            _ => {
                let resp = ecs.create_cluster()
                    .cluster_name(CLUSTER_NAME)
                    .capacity_providers("FARGATE")
                    .capacity_providers("FARGATE_SPOT")
                    .default_capacity_provider_strategy(
                        aws_sdk_ecs::types::CapacityProviderStrategyItem::builder()
                            .capacity_provider("FARGATE_SPOT")
                            .weight(1)
                            .build()?,
                    )
                    .send()
                    .await
                    .context("failed to create ECS cluster")?;
                let arn = resp.cluster().and_then(|c| c.cluster_arn()).unwrap_or_default().to_string();
                eprintln!("  ✓ Created ECS cluster: {CLUSTER_NAME}");
                (arn, true)
            }
        }
    };
    managed.cluster = cluster_managed;

    // 3. IAM Execution Role
    let execution_role_arn = if let Some(ref arn) = imports.execution_role {
        eprintln!("  ✓ Using existing execution role: {arn}");
        arn.clone()
    } else {
        let arn = ensure_role(&iam, EXECUTION_ROLE, &account).await?;
        iam.attach_role_policy()
            .role_name(EXECUTION_ROLE)
            .policy_arn("arn:aws:iam::aws:policy/service-role/AmazonECSTaskExecutionRolePolicy")
            .send().await.ok();
        eprintln!("  ✓ IAM execution role: {EXECUTION_ROLE}");
        managed.execution_role = true;
        arn
    };
    if imports.execution_role.is_none() {
        // ECS uses the EXECUTION role (not the task role) to fetch
        // `spec.secrets` values before the container starts. The managed
        // AmazonECSTaskExecutionRolePolicy above covers ECR pulls and log
        // delivery but not Secrets Manager, so without this, any manifest
        // using `spec.secrets` fails at task launch with an AccessDenied on
        // secretsmanager:GetSecretValue. Applied unconditionally (not just on
        // first creation) so it self-heals existing bootstrap installs too —
        // `put_role_policy` is idempotent.
        iam.put_role_policy()
            .role_name(EXECUTION_ROLE)
            .policy_name("oab-secrets")
            .policy_document(r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["secretsmanager:GetSecretValue"],"Resource":"arn:aws:secretsmanager:*:*:secret:oab/*"}]}"#)
            .send().await.ok();
    }

    // Save partial state (in case subsequent steps fail)
    let mut state = BootstrapState {
        version: 1,
        account: account.clone(),
        region: region.clone(),
        bucket: bucket.clone(),
        resources: BootstrapResources {
            cluster_arn: cluster_arn.clone(),
            execution_role_arn: execution_role_arn.clone(),
            task_role_arn: String::new(),
            security_group_id: String::new(),
            log_group: String::new(),
            subnets: vec![],
            vpc_id: String::new(),
        },
        managed: managed.clone(),
        created_at: chrono_now(),
    };
    save_state(&s3, &bucket, &state).await.ok();

    // 4. IAM Task Role
    let task_role_arn = if let Some(ref arn) = imports.task_role {
        eprintln!("  ✓ Using existing task role: {arn}");
        arn.clone()
    } else {
        let arn = ensure_role(&iam, TASK_ROLE, &account).await?;
        managed.task_role = true;
        // ECS Exec permissions
        iam.put_role_policy()
        .role_name(TASK_ROLE)
        .policy_name("oab-ecs-exec")
        .policy_document(r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["ssmmessages:CreateControlChannel","ssmmessages:CreateDataChannel","ssmmessages:OpenControlChannel","ssmmessages:OpenDataChannel"],"Resource":"*"}]}"#)
        .send().await.ok();
        // S3 artifacts access (seed HOME on boot, backup on shutdown)
        let artifacts_policy = format!(
            r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":["s3:GetObject","s3:PutObject"],"Resource":["arn:aws:s3:::{bucket}/artifacts/*"]}}]}}"#
        );
        iam.put_role_policy()
            .role_name(TASK_ROLE)
            .policy_name("oab-s3-artifacts")
            .policy_document(&artifacts_policy)
            .send().await.ok();
        // Secrets Manager access (agent reads its own secrets at runtime)
        iam.put_role_policy()
            .role_name(TASK_ROLE)
            .policy_name("oab-secrets")
            .policy_document(r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":["secretsmanager:GetSecretValue"],"Resource":"arn:aws:secretsmanager:*:*:secret:oab/*"}]}"#)
            .send().await.ok();
        eprintln!("  ✓ IAM task role: {TASK_ROLE} (ECS Exec + S3 artifacts + Secrets)");
        arn
    };

    // 5. Security Group
    let (vpc_id, sg_id) = if let Some(ref sg) = imports.security_group {
        let vpc = imports.vpc.clone().unwrap_or_default();
        eprintln!("  ✓ Using existing security group: {sg}");
        (vpc, sg.clone())
    } else {
        let default_vpc = ec2.describe_vpcs()
            .filters(aws_sdk_ec2::types::Filter::builder().name("isDefault").values("true").build())
            .send().await?;
        let vid = imports.vpc.clone().unwrap_or_else(|| {
            default_vpc.vpcs().first()
                .and_then(|v| v.vpc_id())
                .unwrap_or_default()
                .to_string()
        });

        let sid = match ec2.describe_security_groups()
            .filters(aws_sdk_ec2::types::Filter::builder().name("group-name").values(SG_NAME).build())
            .filters(aws_sdk_ec2::types::Filter::builder().name("vpc-id").values(&vid).build())
            .send().await
        {
            Ok(resp) if !resp.security_groups().is_empty() => {
                let id = resp.security_groups()[0].group_id().unwrap_or_default().to_string();
                eprintln!("  ✓ Security group already exists: {id}");
                id
            }
            _ => {
                let resp = ec2.create_security_group()
                    .group_name(SG_NAME)
                    .description("OAB agent containers — managed by oabctl bootstrap")
                    .vpc_id(&vid)
                    .send().await
                    .context("failed to create security group")?;
                let id = resp.group_id().unwrap_or_default().to_string();
                managed.security_group = true;
                eprintln!("  ✓ Created security group: {id}");
                id
            }
        };
        (vid, sid)
    };

    // 6. Subnets
    let subnets = if let Some(ref s) = imports.subnets {
        eprintln!("  ✓ Using provided subnets: {}", s.join(", "));
        s.clone()
    } else {
        let subnets_resp = ec2.describe_subnets()
            .filters(aws_sdk_ec2::types::Filter::builder().name("vpc-id").values(&vpc_id).build())
            .send().await?;
        subnets_resp.subnets().iter()
            .filter_map(|s| s.subnet_id().map(|id| id.to_string()))
            .collect()
    };

    // 7. CloudWatch Log Group
    match logs.create_log_group().log_group_name(LOG_GROUP).send().await {
        Ok(_) => { managed.log_group = true; eprintln!("  ✓ Created log group: {LOG_GROUP}"); }
        Err(_) => eprintln!("  ✓ Log group already exists: {LOG_GROUP}"),
    }

    // 8. Save final state
    state.resources.task_role_arn = task_role_arn;
    state.resources.security_group_id = sg_id;
    state.resources.log_group = LOG_GROUP.to_string();
    state.resources.subnets = subnets;
    state.resources.vpc_id = vpc_id;
    state.managed = managed;
    save_state(&s3, &bucket, &state).await?;

    eprintln!("\n✅ Bootstrap complete!");
    eprintln!("   State saved to: s3://{bucket}/{STATE_KEY}");
    eprintln!("   You can now run: oabctl apply -f <manifest.yaml>");

    // Save bucket to local config for future commands
    let mut local_cfg = crate::config::OabConfig::load().unwrap_or_default();
    local_cfg.bootstrap.bucket = Some(bucket);
    local_cfg.save().ok();

    Ok(())
}

// ─── DELETE ───────────────────────────────────────────────────────────────────

async fn teardown(config: &aws_config::SdkConfig) -> Result<()> {
    let (account, _region) = get_account_and_region(config).await?;
    let bucket = bucket_name(&account);
    let s3 = S3Client::new(config);

    let state = load_state(&s3, &bucket).await?
        .context("no bootstrap state found — nothing to delete")?;

    eprintln!("🗑️  Tearing down OAB bootstrap resources...\n");

    let ecs = EcsClient::new(config);
    let iam = IamClient::new(config);
    let ec2 = Ec2Client::new(config);
    let logs = LogsClient::new(config);

    // Check no running services
    let services = ecs.list_services().cluster(CLUSTER_NAME).send().await;
    if let Ok(resp) = &services {
        if !resp.service_arns().is_empty() {
            anyhow::bail!(
                "Cannot delete bootstrap: {} services still running on cluster '{}'. Delete them first.",
                resp.service_arns().len(),
                CLUSTER_NAME
            );
        }
    }

    // Reverse order — only delete resources we created (managed)
    // 1. Log group
    if state.managed.log_group {
        match logs.delete_log_group().log_group_name(&state.resources.log_group).send().await {
            Ok(_) => eprintln!("  ✓ Deleted log group: {}", state.resources.log_group),
            Err(e) => eprintln!("  ⚠ Failed to delete log group: {e}"),
        }
    } else {
        eprintln!("  → Skipping log group (imported)");
    }

    // 2. Security group
    if state.managed.security_group {
        match ec2.delete_security_group().group_id(&state.resources.security_group_id).send().await {
            Ok(_) => eprintln!("  ✓ Deleted security group: {}", state.resources.security_group_id),
            Err(e) => eprintln!("  ⚠ Failed to delete security group: {e}"),
        }
    } else {
        eprintln!("  → Skipping security group (imported)");
    }

    // 3. IAM roles
    if state.managed.task_role {
        delete_role(&iam, TASK_ROLE).await;
        eprintln!("  ✓ Deleted IAM role: {TASK_ROLE}");
    } else {
        eprintln!("  → Skipping task role (imported)");
    }
    if state.managed.execution_role {
        delete_role(&iam, EXECUTION_ROLE).await;
        eprintln!("  ✓ Deleted IAM role: {EXECUTION_ROLE}");
    } else {
        eprintln!("  → Skipping execution role (imported)");
    }

    // 4. ECS Cluster
    if state.managed.cluster {
        match ecs.delete_cluster().cluster(CLUSTER_NAME).send().await {
            Ok(_) => eprintln!("  ✓ Deleted ECS cluster: {CLUSTER_NAME}"),
            Err(e) => eprintln!("  ⚠ Failed to delete cluster: {e}"),
        }
    } else {
        eprintln!("  → Skipping cluster (imported)");
    }

    // 5. Delete state file (keep bucket for user data)
    s3.delete_object().bucket(&bucket).key(STATE_KEY).send().await.ok();
    eprintln!("  ✓ Deleted bootstrap state");
    eprintln!("\n  ℹ️  S3 bucket '{bucket}' preserved (may contain manifests/config).");
    eprintln!("     To fully remove: aws s3 rb s3://{bucket} --force");

    eprintln!("\n✅ Bootstrap teardown complete.");
    Ok(())
}

// ─── STATUS ───────────────────────────────────────────────────────────────────

async fn show_status(config: &aws_config::SdkConfig) -> Result<()> {
    let (account, _region) = get_account_and_region(config).await?;
    let bucket = bucket_name(&account);
    let s3 = S3Client::new(config);

    match load_state(&s3, &bucket).await? {
        Some(state) => {
            eprintln!("✅ OAB Bootstrap Status\n");
            eprintln!("  Account:        {}", state.account);
            eprintln!("  Region:         {}", state.region);
            eprintln!("  Created:        {}", state.created_at);
            eprintln!("  Bucket:         {}", state.bucket);
            eprintln!("  Cluster:        {}", state.resources.cluster_arn);
            eprintln!("  Execution Role: {}", state.resources.execution_role_arn);
            eprintln!("  Task Role:      {}", state.resources.task_role_arn);
            eprintln!("  Security Group: {}", state.resources.security_group_id);
            eprintln!("  Log Group:      {}", state.resources.log_group);
            eprintln!("  VPC:            {}", state.resources.vpc_id);
            eprintln!("  Subnets:        {}", state.resources.subnets.join(", "));
        }
        None => {
            eprintln!("❌ No bootstrap state found.");
            eprintln!("   Run: oabctl bootstrap");
        }
    }
    Ok(())
}

// ─── HELPERS ──────────────────────────────────────────────────────────────────

async fn ensure_role(iam: &IamClient, name: &str, _account: &str) -> Result<String> {
    match iam.get_role().role_name(name).send().await {
        Ok(resp) => Ok(resp.role().context("no role in response")?.arn().to_string()),
        Err(_) => {
            let resp = iam.create_role()
                .role_name(name)
                .assume_role_policy_document(ASSUME_ROLE_POLICY)
                .send().await
                .with_context(|| format!("failed to create role {name}"))?;
            Ok(resp.role().context("no role in response")?.arn().to_string())
        }
    }
}

async fn delete_role(iam: &IamClient, name: &str) {
    // Detach managed policies
    if let Ok(resp) = iam.list_attached_role_policies().role_name(name).send().await {
        for p in resp.attached_policies() {
            if let Some(arn) = p.policy_arn() {
                iam.detach_role_policy().role_name(name).policy_arn(arn).send().await.ok();
            }
        }
    }
    // Delete inline policies
    if let Ok(resp) = iam.list_role_policies().role_name(name).send().await {
        for p in resp.policy_names() {
            iam.delete_role_policy().role_name(name).policy_name(p).send().await.ok();
        }
    }
    iam.delete_role().role_name(name).send().await.ok();
}

fn chrono_now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn plan_line(resource: &str, name: &str, exists: bool, will_manage: bool) {
    let action = if !will_manage {
        "→ import (existing)"
    } else if exists {
        "✓ exists (skip)"
    } else {
        "⊕ CREATE"
    };
    eprintln!("  {:<24} {} ({})", resource, action, name);
}
