use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Top-level manifest that can be either OABService or OABFleet
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawManifest {
    pub api_version: String,
    pub kind: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OABServiceManifest {
    pub api_version: String,
    pub kind: String,
    pub metadata: Metadata,
    pub spec: Spec,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OABFleetManifest {
    pub api_version: String,
    pub kind: String,
    pub metadata: FleetMetadata,
    pub spec: FleetSpec,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct FleetMetadata {
    pub name: String,
    pub namespace: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FleetSpec {
    pub template: FleetTemplate,
    pub agents: Vec<AgentOverride>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FleetTemplate {
    pub image: String,
    #[serde(default)]
    pub resources: Option<Resources>,
    #[serde(default)]
    pub bootstrap_from: Option<String>,
    #[serde(default)]
    pub secrets: HashMap<String, String>,
    pub runtime: Runtime,
    #[serde(default)]
    pub ingress: Option<Ingress>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentOverride {
    pub name: String,
    pub config_from: String,
    #[serde(default)]
    pub resources: Option<Resources>,
    #[serde(default)]
    pub bootstrap_from: Option<String>,
    #[serde(default)]
    pub secrets: Option<HashMap<String, String>>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub ingress: Option<Ingress>,
}

impl OABFleetManifest {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.api_version != "oab.dev/v2" {
            anyhow::bail!("unsupported apiVersion: {} (expected oab.dev/v2)", self.api_version);
        }
        if self.kind != "OABFleet" {
            anyhow::bail!("unsupported kind: {}", self.kind);
        }
        if self.metadata.name.is_empty() {
            anyhow::bail!("metadata.name is required");
        }
        if self.spec.agents.is_empty() {
            anyhow::bail!("spec.agents must not be empty");
        }
        for agent in &self.spec.agents {
            if agent.name.is_empty() {
                anyhow::bail!("each agent must have a name");
            }
            if agent.config_from.is_empty() {
                anyhow::bail!("agent '{}': configFrom is required", agent.name);
            }
        }
        Ok(())
    }

    /// Expand fleet into individual OABService manifests
    pub fn expand(&self) -> Vec<OABServiceManifest> {
        self.spec.agents.iter().map(|agent| {
            let resources = agent.resources.clone()
                .or(self.spec.template.resources.clone())
                .unwrap_or(Resources { cpu: "256".into(), memory: "512".into() });
            let base_secrets = agent.secrets.clone()
                .unwrap_or_else(|| self.spec.template.secrets.clone());
            // Interpolate ${name} in secret values
            let secrets = base_secrets.into_iter().map(|(k, v)| {
                (k, v.replace("${name}", &agent.name))
            }).collect();

            OABServiceManifest {
                api_version: self.api_version.clone(),
                kind: "OABService".to_string(),
                metadata: Metadata {
                    name: agent.name.clone(),
                    namespace: self.metadata.namespace.clone(),
                    generation: 0,
                },
                spec: Spec {
                    image: agent.image.clone()
                        .unwrap_or_else(|| self.spec.template.image.clone()),
                    resources,
                    config_from: agent.config_from.replace("${name}", &agent.name),
                    bootstrap_from: agent.bootstrap_from.clone()
                        .or(self.spec.template.bootstrap_from.clone())
                        .map(|s| s.replace("${name}", &agent.name)),
                    secrets,
                    runtime: self.spec.template.runtime.clone(),
                    ingress: agent
                        .ingress
                        .clone()
                        .or_else(|| self.spec.template.ingress.clone()),
                },
            }
        }).collect()
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Metadata {
    pub name: String,
    pub namespace: String,
    #[serde(default)]
    pub generation: u64,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    pub image: String,
    pub resources: Resources,
    pub config_from: String,
    #[serde(default)]
    pub bootstrap_from: Option<String>,
    #[serde(default)]
    pub secrets: HashMap<String, String>,
    pub runtime: Runtime,
    /// Optional inbound webhook ingress (Telegram/LINE/etc.).
    /// When omitted, the service is outbound-only (Discord behavior) — no ingress
    /// resources are created. This keeps existing deployments unchanged.
    #[serde(default)]
    pub ingress: Option<Ingress>,
}

/// Inbound HTTPS ingress for webhook-based platforms (Telegram, LINE, ...).
///
/// Provisions the cheapest AWS-native path: API Gateway HTTP API → VPC Link →
/// Cloud Map → the ECS task on `containerPort`. See `operator/README.md`
/// ("Ingress — inbound webhooks") for the manifest schema and operational
/// notes.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Ingress {
    /// Ingress implementation. Currently only `apigateway` is supported.
    #[serde(default = "default_ingress_type")]
    pub r#type: String,
    /// Cloud Map private DNS namespace. Created if missing and reused across
    /// services in the same VPC. Defaults to `oab`.
    #[serde(default = "default_cloud_map_namespace")]
    pub cloud_map_namespace: String,
    /// Webhook route paths to expose, e.g. `["/webhook/telegram", "/webhook/line"]`.
    pub paths: Vec<String>,
    /// Container port the OpenAB binary listens on. Defaults to `8080`.
    #[serde(default = "default_container_port")]
    pub container_port: u16,
}

fn default_ingress_type() -> String {
    "apigateway".to_string()
}

fn default_cloud_map_namespace() -> String {
    "oab".to_string()
}

fn default_container_port() -> u16 {
    8080
}

impl Ingress {
    /// Ingress implementations recognized by oabctl.
    pub const SUPPORTED_TYPES: &'static [&'static str] = &["apigateway"];

    pub fn validate(&self) -> anyhow::Result<()> {
        if !Self::SUPPORTED_TYPES.contains(&self.r#type.as_str()) {
            anyhow::bail!(
                "ingress.type must be one of {:?} (got '{}')",
                Self::SUPPORTED_TYPES,
                self.r#type
            );
        }
        if self.cloud_map_namespace.is_empty() {
            anyhow::bail!("ingress.cloudMapNamespace must not be empty");
        }
        if self.paths.is_empty() {
            anyhow::bail!("ingress.paths must not be empty");
        }
        for p in &self.paths {
            if !p.starts_with('/') {
                anyhow::bail!("ingress path '{}' must start with '/'", p);
            }
        }
        if self.container_port == 0 {
            anyhow::bail!("ingress.containerPort must be non-zero");
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Resources {
    pub cpu: String,
    pub memory: String,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Runtime {
    Ecs(EcsRuntime),
    Kubernetes(KubernetesRuntime),
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EcsRuntime {
    #[serde(default = "default_capacity_provider")]
    pub capacity_provider: String,
    pub networking: EcsNetworking,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EcsNetworking {
    pub subnets: Vec<String>,
    pub security_groups: Vec<String>,
    #[serde(default)]
    pub assign_public_ip: bool,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct KubernetesRuntime {
    #[serde(default)]
    pub node_selector: HashMap<String, String>,
    #[serde(default)]
    pub service_account: Option<String>,
    #[serde(default)]
    pub tolerations: Vec<serde_yaml::Value>,
}

fn default_capacity_provider() -> String {
    "FARGATE".to_string()
}

/// Valid ECS Fargate CPU/memory combinations
const VALID_ECS_CPU: &[&str] = &["256", "512", "1024", "2048", "4096"];

impl OABServiceManifest {
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.api_version != "oab.dev/v2" {
            anyhow::bail!("unsupported apiVersion: {} (expected oab.dev/v2)", self.api_version);
        }
        if self.kind != "OABService" {
            anyhow::bail!("unsupported kind: {}", self.kind);
        }
        if self.metadata.name.is_empty() {
            anyhow::bail!("metadata.name is required");
        }
        if self.metadata.namespace.is_empty() {
            anyhow::bail!("metadata.namespace is required");
        }
        if self.spec.image.is_empty() {
            anyhow::bail!("spec.image is required");
        }
        if self.spec.config_from.is_empty() {
            anyhow::bail!("spec.configFrom is required");
        }
        match &self.spec.runtime {
            Runtime::Ecs(ecs) => {
                let valid_cp = ["FARGATE", "FARGATE_SPOT"];
                if !valid_cp.contains(&ecs.capacity_provider.as_str()) {
                    anyhow::bail!("runtime.capacityProvider must be FARGATE or FARGATE_SPOT");
                }
                if ecs.networking.subnets.is_empty() {
                    anyhow::bail!("runtime.networking.subnets must not be empty");
                }
                if ecs.networking.security_groups.is_empty() {
                    anyhow::bail!("runtime.networking.securityGroups must not be empty");
                }
                if !VALID_ECS_CPU.contains(&self.spec.resources.cpu.as_str()) {
                    anyhow::bail!(
                        "spec.resources.cpu must be one of {:?} for ECS runtime",
                        VALID_ECS_CPU
                    );
                }
            }
            Runtime::Kubernetes(_) => {
                // K8S: cpu/memory format validated at deploy time by K8S API
            }
        }
        if let Some(ingress) = &self.spec.ingress {
            ingress.validate()?;
            if !matches!(&self.spec.runtime, Runtime::Ecs(_)) {
                anyhow::bail!(
                    "spec.ingress is only supported with ECS runtime (use native Kubernetes Ingress otherwise)"
                );
            }
        }
        Ok(())
    }

    pub fn ecs_service_name(&self) -> String {
        format!("oab-{}-{}", self.metadata.namespace, self.metadata.name)
    }

    /// Cloud Map service name for this manifest (unique per namespace+name).
    /// Resolves to `<name>.<cloudMapNamespace>` in private DNS.
    pub fn cloud_map_service_name(&self) -> String {
        format!("oab-{}-{}", self.metadata.namespace, self.metadata.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ECS_SERVICE_WITH_INGRESS: &str = r#"
apiVersion: oab.dev/v2
kind: OABService
metadata:
  name: mybot
  namespace: prod
spec:
  image: public.ecr.aws/oablab/kiro:beta
  resources:
    cpu: "256"
    memory: "512"
  configFrom: s3://bucket/config.toml
  runtime:
    type: ecs
    capacityProvider: FARGATE_SPOT
    networking:
      subnets: ["subnet-a", "subnet-b"]
      securityGroups: ["sg-1"]
  ingress:
    paths:
      - /webhook/telegram
      - /webhook/line
"#;

    fn parse(yaml: &str) -> OABServiceManifest {
        serde_yaml::from_str(yaml).expect("parse")
    }

    #[test]
    fn parses_ingress_with_defaults() {
        let m = parse(ECS_SERVICE_WITH_INGRESS);
        let ing = m.spec.ingress.as_ref().expect("ingress present");
        assert_eq!(ing.r#type, "apigateway");
        assert_eq!(ing.cloud_map_namespace, "oab");
        assert_eq!(ing.container_port, 8080);
        assert_eq!(ing.paths, vec!["/webhook/telegram", "/webhook/line"]);
        m.validate().expect("valid");
    }

    #[test]
    fn ingress_is_optional_and_backward_compatible() {
        let yaml = ECS_SERVICE_WITH_INGRESS.split("  ingress:").next().unwrap();
        let m = parse(yaml);
        assert!(m.spec.ingress.is_none());
        m.validate().expect("valid without ingress");
    }

    #[test]
    fn rejects_unknown_ingress_type() {
        let ing = Ingress {
            r#type: "nginx".into(),
            cloud_map_namespace: "oab".into(),
            paths: vec!["/webhook".into()],
            container_port: 8080,
        };
        assert!(ing.validate().is_err());
    }

    #[test]
    fn rejects_empty_paths() {
        let ing = Ingress {
            r#type: "apigateway".into(),
            cloud_map_namespace: "oab".into(),
            paths: vec![],
            container_port: 8080,
        };
        assert!(ing.validate().is_err());
    }

    #[test]
    fn rejects_path_without_leading_slash() {
        let ing = Ingress {
            r#type: "apigateway".into(),
            cloud_map_namespace: "oab".into(),
            paths: vec!["webhook/telegram".into()],
            container_port: 8080,
        };
        assert!(ing.validate().is_err());
    }

    #[test]
    fn rejects_ingress_on_kubernetes_runtime() {
        let yaml = r#"
apiVersion: oab.dev/v2
kind: OABService
metadata:
  name: mybot
  namespace: prod
spec:
  image: img:tag
  resources:
    cpu: "256"
    memory: "512"
  configFrom: s3://bucket/config.toml
  runtime:
    type: kubernetes
    nodeSelector: {}
  ingress:
    paths: ["/webhook/telegram"]
"#;
        let m = parse(yaml);
        assert!(m.validate().is_err());
    }

    #[test]
    fn fleet_passes_ingress_from_template_and_override_wins() {
        let yaml = r#"
apiVersion: oab.dev/v2
kind: OABFleet
metadata:
  name: bots
  namespace: prod
spec:
  template:
    image: img:tag
    runtime:
      type: ecs
      capacityProvider: FARGATE_SPOT
      networking:
        subnets: ["subnet-a"]
        securityGroups: ["sg-1"]
    ingress:
      paths: ["/webhook/telegram"]
  agents:
    - name: fromtemplate
      configFrom: s3://b/${name}.toml
    - name: overridden
      configFrom: s3://b/${name}.toml
      ingress:
        cloudMapNamespace: custom
        paths: ["/webhook/line"]
"#;
        let fleet: OABFleetManifest = serde_yaml::from_str(yaml).expect("parse fleet");
        fleet.validate().expect("valid fleet");
        let expanded = fleet.expand();
        assert_eq!(expanded.len(), 2);

        let from_template = &expanded[0];
        let ing = from_template.spec.ingress.as_ref().expect("template ingress");
        assert_eq!(ing.paths, vec!["/webhook/telegram"]);
        assert_eq!(ing.cloud_map_namespace, "oab");

        let overridden = &expanded[1];
        let ing = overridden.spec.ingress.as_ref().expect("override ingress");
        assert_eq!(ing.paths, vec!["/webhook/line"]);
        assert_eq!(ing.cloud_map_namespace, "custom");
    }
}
