use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const CONFIG_DIR: &str = ".oabctl";
const CONFIG_FILE: &str = "config.toml";

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct OabConfig {
    #[serde(default)]
    pub defaults: Defaults,
    #[serde(default)]
    pub bootstrap: BootstrapConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Defaults {
    #[serde(default = "default_namespace")]
    pub namespace: String,
    #[serde(default = "default_cluster")]
    pub cluster: String,
    #[serde(default)]
    pub region: Option<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            namespace: default_namespace(),
            cluster: default_cluster(),
            region: None,
        }
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct BootstrapConfig {
    #[serde(default)]
    pub bucket: Option<String>,
}

fn default_namespace() -> String {
    "prod".to_string()
}
fn default_cluster() -> String {
    "oab".to_string()
}

impl OabConfig {
    pub fn load() -> Result<Self> {
        let path = config_path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            Ok(toml::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let path = config_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Get the control plane bucket name (config > env var > account-based default)
    pub fn bucket(&self) -> Option<String> {
        self.bootstrap
            .bucket
            .clone()
            .or_else(|| std::env::var("OAB_CONTROL_PLANE_BUCKET").ok())
    }
}

fn config_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(CONFIG_DIR)
        .join(CONFIG_FILE)
}
