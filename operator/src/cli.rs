use crate::{apply, bootstrap, config, create, delete, get, scale};
use anyhow::Context;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "oabctl", about = "OAB agent provisioner for ECS")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Create or update OAB services from manifest files
    Apply {
        /// Path to manifest file or directory
        #[arg(short, long)]
        file: String,
        /// Skip syncing local config.toml to S3 before applying
        #[arg(long)]
        no_sync: bool,
        /// Wait for deployment to stabilize
        #[arg(long)]
        wait: bool,
    },
    /// Interactive wizard to create a new agent
    Create {
        /// Agent name
        name: String,
        /// Namespace
        #[arg(long, default_value = "prod")]
        namespace: String,
        /// Automatically apply after generating (default: just create files)
        #[arg(long)]
        auto_apply: bool,
    },
    /// List OAB services and their status
    Get {
        /// Resource type
        resource: String,
        /// Optional resource name
        name: Option<String>,
        /// ECS cluster name (default: from ~/.oabctl/config.toml)
        #[arg(long)]
        cluster: Option<String>,
    },
    /// Delete an OAB service
    Delete {
        /// Resource type (omit when using -f)
        resource: Option<String>,
        /// Resource name (omit when using -f)
        name: Option<String>,
        /// Delete all services defined in a manifest file or directory,
        /// instead of specifying <resource> <name> directly (mirrors `apply -f`)
        #[arg(short, long, conflicts_with_all = ["resource", "name"])]
        file: Option<String>,
        /// ECS cluster name (default: from ~/.oabctl/config.toml; ignored when using -f)
        #[arg(long)]
        cluster: Option<String>,
        /// Namespace (default: from ~/.oabctl/config.toml; ignored when using -f)
        #[arg(long)]
        namespace: Option<String>,
    },
    /// Execute a command in an agent container (via ecsctl)
    Exec {
        /// Agent name (alias)
        agent: String,
        /// Command to run (default: /bin/sh). Use -- to separate args.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        command: Vec<String>,
    },
    /// Copy files to/from agent containers (via ecsctl)
    Cp {
        /// Source path (local or agent:/path)
        src: String,
        /// Destination path (local or agent:/path)
        dst: String,
    },
    /// Sync directories between local machine and agent containers (via ecsctl)
    Sync {
        /// Source: local dir or agent:/path
        src: String,
        /// Destination: agent:/path or local dir
        dst: String,
    },
    /// Scale an OAB service (set desired task count)
    Scale {
        /// Agent name (OAB service)
        alias: String,
        /// Desired task count (0 or 1)
        #[arg(value_parser = clap::value_parser!(i32).range(0..=1))]
        size: i32,
    },
    /// Manage scaling schedules
    Schedule {
        #[command(subcommand)]
        action: ScheduleAction,
    },
    /// Bootstrap OAB infrastructure (cluster, IAM roles, S3, security group)
    Bootstrap {
        /// Delete all bootstrap resources
        #[arg(long)]
        delete: bool,
        /// Show current bootstrap status
        #[arg(long)]
        status: bool,
        /// AWS region (defaults to AWS_DEFAULT_REGION or us-east-1)
        #[arg(long)]
        region: Option<String>,
        /// Use existing ECS cluster (skip creation)
        #[arg(long)]
        cluster: Option<String>,
        /// Use existing VPC
        #[arg(long)]
        vpc: Option<String>,
        /// Use existing subnets (comma-separated)
        #[arg(long, value_delimiter = ',')]
        subnets: Option<Vec<String>>,
        /// Use existing security group
        #[arg(long, alias = "sg")]
        security_group: Option<String>,
        /// Use existing task execution role ARN
        #[arg(long)]
        execution_role: Option<String>,
        /// Use existing task role ARN
        #[arg(long)]
        task_role: Option<String>,
    },
}

#[derive(Subcommand)]
enum ScheduleAction {
    /// Create a recurring scaling schedule
    Create {
        /// Agent name (OAB service)
        alias: String,
        /// Desired task count (0 or 1)
        #[arg(value_parser = clap::value_parser!(i32).range(0..=1))]
        size: i32,
        /// Schedule expression: cron(...), rate(...), or at(...)
        #[arg(long = "expression", alias = "expr")]
        expression: String,
        /// IANA timezone for schedule expression (default: UTC)
        #[arg(long, default_value = "UTC")]
        timezone: String,
    },
    /// List all scaling schedules
    List,
    /// Delete a scaling schedule
    Delete {
        /// Schedule name to delete
        name: String,
    },
}

pub async fn run_cli() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = aws_config::load_defaults(aws_config::BehaviorVersion::latest()).await;

    match cli.command {
        Commands::Apply {
            file,
            no_sync,
            wait,
        } => apply::run(&config, &file, !no_sync, wait).await,
        Commands::Create {
            name,
            namespace,
            auto_apply,
        } => create::run(&config, &name, &namespace, auto_apply).await,
        Commands::Get {
            resource,
            name,
            cluster,
        } => {
            let oab_cfg =
                config::OabConfig::load().context("failed to load ~/.oabctl/config.toml")?;
            let cluster = cluster.unwrap_or(oab_cfg.defaults.cluster);
            get::run(&config, &resource, name.as_deref(), &cluster).await
        }
        Commands::Delete {
            resource,
            name,
            file,
            cluster,
            namespace,
        } => {
            if let Some(file) = file {
                delete::run_from_file(&config, &file).await
            } else {
                let resource = resource.context("<RESOURCE> is required when not using -f")?;
                let name = name.context("<NAME> is required when not using -f")?;
                let oab_cfg =
                    config::OabConfig::load().context("failed to load ~/.oabctl/config.toml")?;
                let cluster = cluster.unwrap_or(oab_cfg.defaults.cluster);
                let namespace = namespace.unwrap_or(oab_cfg.defaults.namespace);
                delete::run(&config, &resource, &name, &cluster, &namespace).await
            }
        }
        Commands::Exec { agent, command } => {
            let resolved = ecsctl::alias::resolve(&config, &agent).await?;
            let cmd = if command.is_empty() {
                None
            } else {
                // Join args with single-quote escaping to prevent shell interpretation
                let joined = command
                    .iter()
                    .map(|a| format!("'{}'", a.replace('\'', "'\\''")))
                    .collect::<Vec<_>>()
                    .join(" ");
                Some(joined)
            };
            ecsctl::exec::run(&config, &resolved, cmd.as_deref()).await
        }
        Commands::Cp { src, dst } => {
            let src = ecsctl::alias::resolve_remote(&config, &src).await?;
            let dst = ecsctl::alias::resolve_remote(&config, &dst).await?;
            eprintln!("⇄ Copying {} → {} ...", src, dst);
            ecsctl::cp::run(&config, &src, &dst, None, 60).await?;
            eprintln!("✓ Done");
            Ok(())
        }
        Commands::Sync { src, dst } => {
            let src = ecsctl::alias::resolve_remote(&config, &src).await?;
            let dst = ecsctl::alias::resolve_remote(&config, &dst).await?;
            let src_remote = ecsctl::cp::is_remote(&src);
            let dst_remote = ecsctl::cp::is_remote(&dst);
            eprintln!("⇄ Syncing {} → {} ...", src, dst);
            match (src_remote, dst_remote) {
                (false, true) => {
                    ecsctl::sync::run(&config, &src, &dst, None, 60).await?;
                }
                (true, false) => {
                    ecsctl::sync::run_download(&config, &src, &dst, None, 60).await?;
                }
                _ => anyhow::bail!("exactly one of src/dst must be a remote path (agent:/path)"),
            }
            eprintln!("✓ Done");
            Ok(())
        }
        Commands::Scale { alias, size } => scale::run(&config, &alias, size).await,
        Commands::Schedule { action } => match action {
            ScheduleAction::Create {
                alias,
                size,
                expression,
                timezone,
            } => {
                scale::run_with_schedule(&config, &alias, size, &expression, Some(&timezone)).await
            }
            ScheduleAction::List => scale::list_schedules(&config).await,
            ScheduleAction::Delete { name } => scale::delete_schedule(&config, &name).await,
        },
        Commands::Bootstrap {
            delete,
            status,
            region,
            cluster,
            vpc,
            subnets,
            security_group,
            execution_role,
            task_role,
        } => {
            let cfg = if let Some(ref r) = region {
                aws_config::defaults(aws_config::BehaviorVersion::latest())
                    .region(aws_config::Region::new(r.clone()))
                    .load()
                    .await
            } else {
                config.clone()
            };
            let imports = bootstrap::ImportOptions {
                cluster,
                vpc,
                subnets,
                security_group,
                execution_role,
                task_role,
            };
            bootstrap::run(&cfg, delete, status, imports).await
        }
    }
}
