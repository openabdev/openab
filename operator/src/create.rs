use anyhow::{Context, Result};
use aws_sdk_ec2::Client as Ec2Client;
use aws_sdk_s3::Client as S3Client;
use aws_sdk_secretsmanager::Client as SmClient;
use std::io::{self, Write};

const BACKENDS: &[(&str, &str)] = &[
    ("kiro", "public.ecr.aws/oablab/kiro"),
    ("claude-code", "public.ecr.aws/oablab/claude-code"),
    ("codex", "public.ecr.aws/oablab/codex"),
    ("gemini", "public.ecr.aws/oablab/gemini"),
    ("copilot", "public.ecr.aws/oablab/copilot"),
    ("opencode", "public.ecr.aws/oablab/opencode"),
];

const CHANNELS: &[&str] = &["stable", "beta"];

pub async fn run(
    config: &aws_config::SdkConfig,
    name: &str,
    namespace: &str,
    auto_apply: bool,
) -> Result<()> {
    eprintln!("🤖 Creating agent: {name}\n");

    // 1. Backend
    let backend = prompt_select(
        "Backend platform",
        &BACKENDS.iter().map(|(n, _)| *n).collect::<Vec<_>>(),
    )?;
    let image_base = BACKENDS.iter().find(|(n, _)| *n == backend).unwrap().1;

    // 2. Release channel
    let channel = prompt_select("Release channel", CHANNELS)?;
    let image = format!("{image_base}:{channel}");
    eprintln!("   → Image: {image}\n");

    // 3. Discord bot token
    let token = prompt_secret("Discord bot token")?;

    // Store in Secrets Manager (single secret with DISCORD_BOT_TOKEN key)
    let sm = SmClient::new(config);
    let secret_name = format!("oab/{namespace}/{name}");

    // 3b. STT API key (optional)
    let stt_key =
        rpassword::prompt_password("  STT API key (Groq, enter to skip): ").unwrap_or_default();
    let stt_enabled = !stt_key.is_empty();

    let mut secret_obj = serde_json::json!({ "DISCORD_BOT_TOKEN": token });
    if stt_enabled {
        secret_obj["STT_API_KEY"] = serde_json::Value::String(stt_key);
    }
    store_secret(&sm, &secret_name, &secret_obj.to_string()).await?;
    eprintln!("   → Stored in Secrets Manager: {secret_name}");
    if stt_enabled {
        eprintln!("     Keys: DISCORD_BOT_TOKEN, STT_API_KEY\n");
    } else {
        eprintln!("     Keys: DISCORD_BOT_TOKEN\n");
    }

    // 4. Runtime
    let runtime = prompt_select("Runtime", &["ecs", "kubernetes"])?;
    if runtime == "kubernetes" {
        anyhow::bail!("Kubernetes runtime not yet implemented");
    }

    // 5. Capacity provider
    let cap = prompt_select(
        "Capacity provider",
        &["FARGATE_SPOT (cost-optimized)", "FARGATE (on-demand)"],
    )?;
    let capacity_provider = if cap.starts_with("FARGATE_SPOT") {
        "FARGATE_SPOT"
    } else {
        "FARGATE"
    };

    // 6. VPC
    let ec2 = Ec2Client::new(config);
    let vpcs = list_vpcs(&ec2).await?;
    if vpcs.is_empty() {
        anyhow::bail!("No VPCs found in this region");
    }
    let vpc_labels: Vec<&str> = vpcs.iter().map(|v| v.label.as_str()).collect();
    let vpc_choice = prompt_select("VPC", &vpc_labels)?;
    let vpc = vpcs.iter().find(|v| v.label == vpc_choice).unwrap();

    // 7. Subnets (auto-select: private+NAT > private > public, 2-3 AZ)
    let subnets = select_subnets(&ec2, &vpc.id).await?;
    eprintln!("   Subnets (auto-selected):");
    for s in &subnets {
        eprintln!(
            "   ✓ {} ({}, {}, {})",
            s.id,
            s.az,
            s.kind,
            if s.has_nat { "NAT ✓" } else { "no NAT" }
        );
    }
    eprintln!();

    // 8. Security group
    let sgs = list_security_groups(&ec2, &vpc.id).await?;
    let mut sg_labels: Vec<String> = vec!["Create new (oab-{name})".to_string()];
    sg_labels.extend(sgs.iter().map(|s| format!("{} ({})", s.id, s.name)));
    let sg_labels_ref: Vec<&str> = sg_labels.iter().map(|s| s.as_str()).collect();
    let sg_choice = prompt_select("Security group", &sg_labels_ref)?;

    let sg_id = if sg_choice.starts_with("Create new") {
        let sg_name = format!("oab-{name}");
        let resp = ec2
            .create_security_group()
            .group_name(&sg_name)
            .description(format!("OAB agent {name}"))
            .vpc_id(&vpc.id)
            .send()
            .await
            .context("failed to create security group")?;
        let id = resp.group_id().unwrap_or_default().to_string();
        eprintln!("   → Created security group: {id}\n");
        id
    } else {
        sgs.iter()
            .find(|s| sg_choice.contains(&s.id))
            .unwrap()
            .id
            .clone()
    };

    // ─── Generate config.toml ──────────────────────────────────────────────
    let config_toml = generate_config(backend, name, namespace, stt_enabled);

    // ─── Resolve bucket for configFrom path ────────────────────────────────
    let s3 = S3Client::new(config);
    let bucket = resolve_bucket(&s3, config)
        .await
        .unwrap_or_else(|| "oab-control-plane-unknown".to_string());

    let config_s3_key = format!("artifacts/{namespace}/{name}/config.toml");
    let config_from = format!("s3://{bucket}/{config_s3_key}");

    // ─── Save local files ──────────────────────────────────────────────────
    let dir = name.to_string();
    std::fs::create_dir_all(&dir)?;
    std::fs::write(format!("{dir}/config.toml"), &config_toml)?;

    let subnet_ids: Vec<String> = subnets.iter().map(|s| s.id.clone()).collect();
    let manifest_yaml = generate_manifest(
        name,
        namespace,
        &image,
        &config_from,
        capacity_provider,
        &subnet_ids,
        &sg_id,
    );
    std::fs::write(format!("{dir}/manifest.yaml"), &manifest_yaml)?;

    // ─── Summary ───────────────────────────────────────────────────────────
    eprintln!("─────────────────────────────────────────");
    eprintln!("Summary:");
    eprintln!("  Agent:    {name}");
    eprintln!("  Image:    {image}");
    eprintln!("  CPU/Mem:  256 / 512");
    eprintln!("  Runtime:  ECS {capacity_provider}");
    eprintln!("  Subnets:  {}", subnet_ids.join(", "));
    eprintln!("  SG:       {sg_id}");
    eprintln!("  Secret:   aws-sm://{secret_name}#DISCORD_BOT_TOKEN");
    eprintln!("  Config:   {config_from}");
    eprintln!();

    eprint!("Proceed? [Y/n] ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    if !input.trim().is_empty()
        && !input.trim().eq_ignore_ascii_case("y")
        && !input.trim().eq_ignore_ascii_case("yes")
    {
        eprintln!("Aborted.");
        return Ok(());
    }

    eprintln!("\n✅ Created {name}/");
    eprintln!("   {dir}/manifest.yaml");
    eprintln!("   {dir}/config.toml\n");

    if auto_apply {
        // ─── Apply (with sync to upload config.toml) ───────────────────────
        crate::apply::run(config, &format!("{dir}/manifest.yaml"), true, false).await?;
        eprintln!("\n✅ Agent {name} is running!");
        eprintln!("   oabctl exec {name} -- bash");
    } else {
        eprintln!("To deploy:");
        eprintln!("   oabctl apply -f {dir}/manifest.yaml");
    }
    Ok(())
}

// ─── HELPERS ──────────────────────────────────────────────────────────────────

fn prompt_select<'a>(label: &str, options: &[&'a str]) -> Result<&'a str> {
    eprintln!("  {label}:");
    for (i, opt) in options.iter().enumerate() {
        eprintln!("    {}. {}", i + 1, opt);
    }
    eprint!("  Choice [1]: ");
    io::stdout().flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let idx = if input.trim().is_empty() {
        0
    } else {
        input.trim().parse::<usize>().unwrap_or(1).saturating_sub(1)
    };
    let choice = options.get(idx).context("invalid selection")?;
    eprintln!();
    Ok(choice)
}

fn prompt_secret(label: &str) -> Result<String> {
    let val = rpassword::prompt_password(format!("  {label}: "))
        .context("failed to read secret input")?;
    if val.is_empty() {
        anyhow::bail!("{label} cannot be empty");
    }
    Ok(val)
}

async fn store_secret(sm: &SmClient, name: &str, value: &str) -> Result<()> {
    match sm
        .create_secret()
        .name(name)
        .secret_string(value)
        .send()
        .await
    {
        Ok(_) => Ok(()),
        Err(_) => {
            // Already exists — update
            sm.put_secret_value()
                .secret_id(name)
                .secret_string(value)
                .send()
                .await
                .context("failed to store secret")?;
            Ok(())
        }
    }
}

struct VpcInfo {
    id: String,
    label: String,
}

async fn list_vpcs(ec2: &Ec2Client) -> Result<Vec<VpcInfo>> {
    let resp = ec2.describe_vpcs().send().await?;
    Ok(resp
        .vpcs()
        .iter()
        .map(|v| {
            let id = v.vpc_id().unwrap_or_default().to_string();
            let cidr = v.cidr_block().unwrap_or_default();
            let is_default = v.is_default().unwrap_or(false);
            let name = v
                .tags()
                .iter()
                .find(|t| t.key() == Some("Name"))
                .and_then(|t| t.value())
                .unwrap_or("unnamed");
            let label = format!(
                "{id} ({name}, {cidr}{})",
                if is_default { ", default" } else { "" }
            );
            VpcInfo { id, label }
        })
        .collect())
}

struct SubnetInfo {
    id: String,
    az: String,
    kind: String,
    has_nat: bool,
}

async fn select_subnets(ec2: &Ec2Client, vpc_id: &str) -> Result<Vec<SubnetInfo>> {
    let subnets_resp = ec2
        .describe_subnets()
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("vpc-id")
                .values(vpc_id)
                .build(),
        )
        .send()
        .await?;

    // Get route tables to determine private vs public + NAT
    let rt_resp = ec2
        .describe_route_tables()
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("vpc-id")
                .values(vpc_id)
                .build(),
        )
        .send()
        .await?;

    // Build subnet → route table mapping
    let mut subnet_routes: std::collections::HashMap<String, (bool, bool)> =
        std::collections::HashMap::new();
    for rt in rt_resp.route_tables() {
        let has_igw = rt.routes().iter().any(|r| {
            r.gateway_id()
                .map(|g| g.starts_with("igw-"))
                .unwrap_or(false)
        });
        let has_nat = rt.routes().iter().any(|r| r.nat_gateway_id().is_some());
        for assoc in rt.associations() {
            if let Some(sid) = assoc.subnet_id() {
                subnet_routes.insert(sid.to_string(), (has_igw, has_nat));
            }
        }
    }

    let mut all: Vec<SubnetInfo> = subnets_resp
        .subnets()
        .iter()
        .map(|s| {
            let id = s.subnet_id().unwrap_or_default().to_string();
            let az = s.availability_zone().unwrap_or_default().to_string();
            let (has_igw, has_nat) = subnet_routes.get(&id).copied().unwrap_or((false, false));
            let kind = if !has_igw {
                "private".to_string()
            } else {
                "public".to_string()
            };
            SubnetInfo {
                id,
                az,
                kind,
                has_nat,
            }
        })
        .collect();

    // Priority: private+NAT > private > public, pick 2-3 unique AZs
    all.sort_by(|a, b| {
        let score = |s: &SubnetInfo| -> u8 {
            match (s.kind.as_str(), s.has_nat) {
                ("private", true) => 0,
                ("private", false) => 1,
                _ => 2,
            }
        };
        score(a).cmp(&score(b))
    });

    // Pick up to 3 unique AZs
    let mut selected = Vec::new();
    let mut seen_azs = std::collections::HashSet::new();
    for s in all {
        if seen_azs.len() >= 3 {
            break;
        }
        if seen_azs.contains(&s.az) {
            continue;
        }
        seen_azs.insert(s.az.clone());
        selected.push(s);
    }

    if selected.is_empty() {
        anyhow::bail!("no subnets found in VPC {vpc_id}");
    }
    Ok(selected)
}

struct SgInfo {
    id: String,
    name: String,
}

async fn list_security_groups(ec2: &Ec2Client, vpc_id: &str) -> Result<Vec<SgInfo>> {
    let resp = ec2
        .describe_security_groups()
        .filters(
            aws_sdk_ec2::types::Filter::builder()
                .name("vpc-id")
                .values(vpc_id)
                .build(),
        )
        .send()
        .await?;
    Ok(resp
        .security_groups()
        .iter()
        .map(|sg| SgInfo {
            id: sg.group_id().unwrap_or_default().to_string(),
            name: sg.group_name().unwrap_or_default().to_string(),
        })
        .collect())
}

fn generate_config(_backend: &str, name: &str, namespace: &str, stt_enabled: bool) -> String {
    let stt_section = if stt_enabled {
        r#"[stt]
enabled = true
api_key = "${secrets.stt_api_key}"
model = "whisper-large-v3-turbo"
base_url = "https://api.groq.com/openai/v1"
"#
        .to_string()
    } else {
        "[stt]\nenabled = false\n".to_string()
    };

    let secrets_refs = if stt_enabled {
        format!(
            r#"[secrets.refs]
discord_bot_token = "aws-sm://oab/{namespace}/{name}#DISCORD_BOT_TOKEN"
stt_api_key = "aws-sm://oab/{namespace}/{name}#STT_API_KEY"
"#
        )
    } else {
        format!(
            r#"[secrets.refs]
discord_bot_token = "aws-sm://oab/{namespace}/{name}#DISCORD_BOT_TOKEN"
"#
        )
    };

    format!(
        r#"{secrets_refs}
[discord]
bot_token = "${{secrets.discord_bot_token}}"
allow_all_channels = true
allow_all_users = true
allowed_channels = []
allowed_users = []
allow_bot_messages = "mentions"
max_bot_turns = 1000
message_processing_mode = "per-thread"

[agent]
inherit_env = ["AWS_CONTAINER_CREDENTIALS_RELATIVE_URI", "AWS_DEFAULT_REGION", "AWS_EXECUTION_ENV", "AWS_REGION"]

[pool]
max_sessions = 5
session_ttl_hours = 1

[reactions]
enabled = true
remove_after_reply = false

{stt_section}
[cron]
usercron_enabled = true
usercron_path = "cronjob.toml"
"#
    )
}

fn generate_manifest(
    name: &str,
    namespace: &str,
    image: &str,
    config_from: &str,
    cap: &str,
    subnets: &[String],
    sg: &str,
) -> String {
    let subnets_yaml = subnets
        .iter()
        .map(|s| format!("\"{}\"", s))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"apiVersion: oab.dev/v2
kind: OABService
metadata:
  name: {name}
  namespace: {namespace}
spec:
  image: {image}
  resources:
    cpu: "256"
    memory: "512"
  configFrom: {config_from}
  runtime:
    type: ecs
    capacityProvider: {cap}
    networking:
      subnets: [{subnets_yaml}]
      securityGroups: ["{sg}"]
"#
    )
}

async fn resolve_bucket(_s3: &S3Client, config: &aws_config::SdkConfig) -> Option<String> {
    let oab_cfg = crate::config::OabConfig::load().ok()?;
    if let Some(b) = oab_cfg.bucket() {
        return Some(b);
    }
    let sts = aws_sdk_sts::Client::new(config);
    let account = sts
        .get_caller_identity()
        .send()
        .await
        .ok()?
        .account()?
        .to_string();
    Some(format!("oab-control-plane-{account}"))
}
