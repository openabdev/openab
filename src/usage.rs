//! Pluggable usage/quota reporting framework.
//!
//! Each runner is an external command that outputs a single line of JSON.
//! OpenAB runs them in parallel, parses the JSON, renders it through a
//! template string, and returns a list of results for `/usage` to display.
//!
//! Runners are fully user-defined in `config.toml` — OpenAB ships no
//! hardcoded backends. See `docs/usage-command-howto.md` for examples.

use crate::config::{UsageConfig, UsageRunnerConfig};
use serde_json::Value;
use tokio::process::Command;
use tracing::{debug, warn};

/// Per-runner result.
#[derive(Debug, Clone)]
pub enum RunnerResult {
    Ok {
        #[allow(dead_code)]
        name: String,
        label: String,
        color: u32,
        rendered: String,
    },
    Err {
        #[allow(dead_code)]
        name: String,
        label: String,
        reason: String,
    },
}

/// Execute all configured runners in parallel and collect results.
pub async fn run_all(config: &UsageConfig) -> Vec<RunnerResult> {
    let timeout = config.timeout_secs;
    let futs = config.runners.iter().map(|r| run_one(r, timeout));
    futures::future::join_all(futs).await
}

async fn run_one(runner: &UsageRunnerConfig, timeout_secs: u64) -> RunnerResult {
    debug!(name = %runner.name, cmd = %runner.command, "running usage runner");

    let exec = async {
        let mut cmd = Command::new(&runner.command);
        cmd.args(&runner.args);
        for (k, v) in &runner.env {
            cmd.env(k, v);
        }
        if let Some(cwd) = &runner.working_dir {
            cmd.current_dir(cwd);
        }
        cmd.output().await
    };

    let out = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), exec).await {
        Ok(Ok(out)) => out,
        Ok(Err(e)) => return err(runner, format!("spawn failed: {e}")),
        Err(_) => return err(runner, format!("timeout after {timeout_secs}s")),
    };

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let exit = out
            .status
            .code()
            .map(|c| c.to_string())
            .unwrap_or_else(|| "signal".into());
        let reason = format!(
            "exit {exit}: {}",
            stderr.trim().chars().take(200).collect::<String>()
        );
        return err(runner, reason);
    }

    let stdout = String::from_utf8_lossy(&out.stdout);
    let first_line = stdout.lines().next().unwrap_or("").trim();
    if first_line.is_empty() {
        return err(runner, "runner produced no output".into());
    }

    let json: Value = match serde_json::from_str(first_line) {
        Ok(v) => v,
        Err(e) => {
            return err(
                runner,
                format!(
                    "invalid JSON: {e} (got: {})",
                    first_line.chars().take(120).collect::<String>()
                ),
            );
        }
    };

    // Check ok field if present (optional contract)
    if let Some(false) = json.get("ok").and_then(|v| v.as_bool()) {
        let reason = json
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("runner reported failure")
            .to_string();
        return err(runner, reason);
    }

    let rendered = render_template(&runner.template, &json, &runner.progress_fields);

    RunnerResult::Ok {
        name: runner.name.clone(),
        label: runner.label.clone(),
        color: runner.color,
        rendered,
    }
}

/// Simple `{{ field }}` substitution. Zero new deps (no Tera/Handlebars).
///
/// Fields listed in `progress_fields` get prefixed with a 10-char unicode
/// progress bar if their value is numeric.
fn render_template(tpl: &str, data: &Value, progress_fields: &[String]) -> String {
    let mut out = tpl.to_string();

    let Some(obj) = data.as_object() else {
        return out;
    };

    for (k, v) in obj {
        let placeholder = format!("{{{{ {k} }}}}");
        let value_str = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => "null".to_string(),
            _ => v.to_string(),
        };

        let replacement = if progress_fields.iter().any(|f| f == k) {
            if let Some(pct) = v.as_f64() {
                format!("{} `{:>3}%`", progress_bar(pct), pct.round() as i64)
            } else {
                value_str
            }
        } else {
            value_str
        };

        out = out.replace(&placeholder, &replacement);
    }

    // Strip any remaining unresolved {{ field }} placeholders
    while let Some(start) = out.find("{{ ") {
        if let Some(end) = out[start..].find(" }}") {
            out.replace_range(start..start + end + 3, "N/A");
        } else {
            break;
        }
    }

    out
}

fn progress_bar(pct: f64) -> String {
    let pct = pct.clamp(0.0, 100.0);
    let filled = (pct / 10.0).round() as usize;
    let filled = filled.min(10);
    let empty = 10 - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

fn err(runner: &UsageRunnerConfig, reason: String) -> RunnerResult {
    warn!(runner = %runner.name, reason = %reason, "usage runner failed");
    RunnerResult::Err {
        name: runner.name.clone(),
        label: runner.label.clone(),
        reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_progress_bar_50pct() {
        assert_eq!(progress_bar(50.0), "█████░░░░░");
    }

    #[test]
    fn test_progress_bar_clamped() {
        assert_eq!(progress_bar(150.0), "██████████");
        assert_eq!(progress_bar(-10.0), "░░░░░░░░░░");
    }

    #[test]
    fn test_render_simple() {
        let data: Value = serde_json::from_str(r#"{"pct": 42, "name": "test"}"#).unwrap();
        let out = render_template("value={{ pct }}% name={{ name }}", &data, &[]);
        assert_eq!(out, "value=42% name=test");
    }

    #[test]
    fn test_render_unresolved_stripped() {
        let data: Value = serde_json::from_str(r#"{"name": "test"}"#).unwrap();
        let out = render_template("hi {{ name }} miss {{ gone }}", &data, &[]);
        assert_eq!(out, "hi test miss N/A");
    }

    #[test]
    fn test_render_with_progress() {
        let data: Value = serde_json::from_str(r#"{"pct": 30}"#).unwrap();
        let out = render_template("{{ pct }}", &data, &["pct".into()]);
        assert!(out.contains("███░░░░░░░"));
        assert!(out.contains("30%"));
    }
}
