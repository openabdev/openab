use anyhow::{anyhow, Result};
use process_wrap::tokio::{CommandWrap, KillOnDrop};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::sync::oneshot;
use tokio::task::{AbortHandle, JoinHandle};
use tracing::debug;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessSession;

use crate::llm::ToolDef;

#[cfg(any(windows, test))]
use base64::Engine as _;

/// Validate that a path is within the allowed working directory.
/// This function has NO side-effects — it never creates directories or files.
fn validate_path(path: &str, working_dir: &Path) -> Result<PathBuf> {
    let target = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        working_dir.join(path)
    };

    // For existing paths, canonicalize directly
    if target.exists() {
        let canonical = target.canonicalize()?;
        let canonical_working = working_dir.canonicalize()?;
        if !canonical.starts_with(&canonical_working) {
            return Err(anyhow!(
                "path traversal denied: {} is outside working directory",
                path
            ));
        }
        return Ok(canonical);
    }

    // For non-existent paths, walk up to find the nearest existing ancestor
    let mut ancestor = target.parent();
    while let Some(p) = ancestor {
        if p.exists() {
            let canonical_ancestor = p.canonicalize()?;
            let canonical_working = working_dir.canonicalize()?;
            if !canonical_ancestor.starts_with(&canonical_working) {
                return Err(anyhow!(
                    "path traversal denied: {} is outside working directory",
                    path
                ));
            }
            // Reconstruct the full path relative to the canonicalized ancestor
            let remainder = target.strip_prefix(p).unwrap_or(target.as_path());
            return Ok(canonical_ancestor.join(remainder));
        }
        ancestor = p.parent();
    }

    Err(anyhow!(
        "path traversal denied: no valid ancestor for {}",
        path
    ))
}

/// Build a filtered environment for shell tool execution.
fn build_env(allow_list: &[String]) -> HashMap<String, String> {
    let mut env = HashMap::new();
    #[cfg(unix)]
    let baseline = ["PATH", "HOME", "USER", "LANG", "TERM", "SHELL"];
    #[cfg(windows)]
    let baseline = [
        "PATH",
        "SystemRoot",
        "WINDIR",
        "ComSpec",
        "PATHEXT",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "HOMEDRIVE",
        "HOMEPATH",
        "HOME",
        "USERNAME",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "PSModulePath",
        "LANG",
    ];

    for key in baseline {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    for key in allow_list {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env
}

/// Execute a tool call and return the result as a string.
pub async fn execute_tool(name: &str, input: &Value, working_dir: &Path) -> Result<String> {
    match name {
        "read" => tool_read(input, working_dir),
        "write" => tool_write(input, working_dir),
        "edit" => tool_edit(input, working_dir),
        "bash" => tool_bash(input, working_dir).await,
        _ => Err(anyhow!("unknown tool: {name}")),
    }
}

/// Read file contents or list directory.
fn tool_read(input: &Value, working_dir: &Path) -> Result<String> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("read: missing 'path' parameter"))?;

    let path = validate_path(path_str, working_dir)?;

    if path.is_dir() {
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(&path)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                entries.push(format!("{name}/"));
            } else {
                entries.push(name);
            }
        }
        entries.sort();
        Ok(entries.join("\n"))
    } else {
        let content =
            std::fs::read_to_string(&path).map_err(|e| anyhow!("read {}: {e}", path.display()))?;

        // Apply optional line range
        let offset = input.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = input.get("limit").and_then(|v| v.as_u64());

        let lines: Vec<&str> = content.lines().collect();
        let start = offset.min(lines.len());
        let end = match limit {
            Some(l) => (start + l as usize).min(lines.len()),
            None => lines.len(),
        };

        Ok(lines[start..end].join("\n"))
    }
}

/// Create or overwrite a file.
fn tool_write(input: &Value, working_dir: &Path) -> Result<String> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("write: missing 'path' parameter"))?;
    let content = input
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("write: missing 'content' parameter"))?;

    let path = validate_path(path_str, working_dir)?;

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, content)?;

    Ok(format!(
        "wrote {} bytes to {}",
        content.len(),
        path.display()
    ))
}

/// Replace an exact string in a file.
fn tool_edit(input: &Value, working_dir: &Path) -> Result<String> {
    let path_str = input
        .get("path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("edit: missing 'path' parameter"))?;
    let old_str = input
        .get("old_str")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("edit: missing 'old_str' parameter"))?;
    let new_str = input
        .get("new_str")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("edit: missing 'new_str' parameter"))?;

    let path = validate_path(path_str, working_dir)?;
    let content = std::fs::read_to_string(&path)
        .map_err(|e| anyhow!("edit: cannot read {}: {e}", path.display()))?;

    let count = content.matches(old_str).count();
    if count == 0 {
        return Err(anyhow!("edit: old_str not found in {}", path.display()));
    }

    let new_content = content.replacen(old_str, new_str, 1);
    std::fs::write(&path, &new_content)?;

    Ok(format!(
        "replaced 1 occurrence in {} ({count} total matches)",
        path.display()
    ))
}

/// Prefix PowerShell scripts with deterministic UTF-8 console encodings, then encode the
/// complete script as UTF-16LE for `-EncodedCommand`. This keeps user input out of the Windows
/// command-line quoting layer.
#[cfg(any(windows, test))]
fn powershell_encoded_command(command: &str) -> String {
    const PREFIX: &str = concat!(
        "[Console]::InputEncoding = [System.Text.UTF8Encoding]::new($false); ",
        "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); ",
        "$OutputEncoding = [Console]::OutputEncoding;\n"
    );
    let script = format!("{PREFIX}{command}");
    let utf16le: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16le)
}

#[cfg(unix)]
fn platform_shell_command(command: &str) -> Result<Command> {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(command);
    Ok(cmd)
}

#[cfg(windows)]
fn platform_shell_command(command: &str) -> Result<Command> {
    let system_root = std::env::var_os("SystemRoot")
        .or_else(|| std::env::var_os("WINDIR"))
        .ok_or_else(|| {
            anyhow!("bash: SystemRoot is unavailable; refusing PATH-based shell lookup")
        })?;
    let powershell = PathBuf::from(system_root)
        .join("System32")
        .join("WindowsPowerShell")
        .join("v1.0")
        .join("powershell.exe");
    let mut cmd = Command::new(powershell);
    cmd.args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-EncodedCommand",
        &powershell_encoded_command(command),
    ]);
    Ok(cmd)
}

fn format_shell_output(status: ExitStatus, stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout);
    let stderr = String::from_utf8_lossy(stderr);
    let code = status.code().unwrap_or(-1);

    let mut result = String::new();
    if !stdout.is_empty() {
        result.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("[stderr]\n");
        result.push_str(&stderr);
    }
    if code != 0 {
        result.push_str(&format!("\n[exit code: {code}]"));
    }
    result
}

struct ShellExecution {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum ShellSupervisorOutcome {
    Completed(ShellExecution),
    Cancelled,
}

/// Controller-owned cancellation guard for one shell process tree.
///
/// The supervisor task, rather than the caller's future, owns the child handle. Dropping an
/// in-flight caller therefore sends an explicit cancellation request while the supervisor remains
/// alive to kill and reap the complete process tree. Callers with an orderly shutdown path should
/// use `cancel_and_wait` so cleanup failures are observable.
struct ShellCommandController {
    cancel_tx: Option<oneshot::Sender<()>>,
    outcome_rx: oneshot::Receiver<Result<ShellSupervisorOutcome>>,
}

impl ShellCommandController {
    fn request_cancel(&mut self) {
        if let Some(cancel_tx) = self.cancel_tx.take() {
            let _ = cancel_tx.send(());
        }
    }

    async fn wait(&mut self) -> Result<ShellSupervisorOutcome> {
        let outcome = (&mut self.outcome_rx)
            .await
            .map_err(|_| anyhow!("bash: process supervisor stopped before reporting cleanup"))?;
        self.cancel_tx.take();
        outcome
    }

    async fn cancel_and_wait(&mut self) -> Result<ShellSupervisorOutcome> {
        self.request_cancel();
        self.wait().await
    }
}

impl Drop for ShellCommandController {
    fn drop(&mut self) {
        self.request_cancel();
    }
}

async fn read_shell_pipe<R>(mut pipe: R) -> std::io::Result<Vec<u8>>
where
    R: AsyncRead + Unpin,
{
    let mut output = Vec::new();
    pipe.read_to_end(&mut output).await?;
    Ok(output)
}

const POST_KILL_PIPE_JOIN: Duration = Duration::from_secs(3);
const POST_EXIT_PIPE_JOIN: Duration = Duration::from_secs(6);
const POST_TIMEOUT_CLEANUP: Duration = Duration::from_secs(5);

async fn join_shell_pipe(
    name: &str,
    task: JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Vec<u8>> {
    task.await
        .map_err(|e| anyhow!("bash: {name} reader task failed: {e}"))?
        .map_err(|e| anyhow!("bash: {name} read failed: {e}"))
}

async fn join_shell_pipes(
    stdout_task: JoinHandle<std::io::Result<Vec<u8>>>,
    stderr_task: JoinHandle<std::io::Result<Vec<u8>>>,
    stdout_abort: AbortHandle,
    stderr_abort: AbortHandle,
    limit: Option<Duration>,
) -> Result<(Vec<u8>, Vec<u8>)> {
    let join_both = async {
        let stdout = join_shell_pipe("stdout", stdout_task).await?;
        let stderr = join_shell_pipe("stderr", stderr_task).await?;
        Ok((stdout, stderr))
    };
    match limit {
        None => join_both.await,
        Some(deadline) => match tokio::time::timeout(deadline, join_both).await {
            Ok(result) => result,
            Err(_) => {
                stdout_abort.abort();
                stderr_abort.abort();
                Ok((Vec::new(), Vec::new()))
            }
        },
    }
}

async fn supervise_shell_process(
    mut child: Box<dyn process_wrap::tokio::ChildWrapper>,
    stdout_task: JoinHandle<std::io::Result<Vec<u8>>>,
    stderr_task: JoinHandle<std::io::Result<Vec<u8>>>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> Result<ShellSupervisorOutcome> {
    enum ProcessOutcome {
        Completed(ExitStatus),
        Cancelled,
    }

    let stdout_abort = stdout_task.abort_handle();
    let stderr_abort = stderr_task.abort_handle();

    let process_outcome = tokio::select! {
        status = child.wait() => match status {
            Ok(status) => ProcessOutcome::Completed(status),
            Err(wait_error) => {
                return match Box::into_pin(child.kill()).await {
                    Ok(()) => Err(anyhow!("bash: execution error: {wait_error}")),
                    Err(cleanup_error) => Err(anyhow!(
                        "bash: execution error: {wait_error}; process-tree cleanup failed: {cleanup_error}"
                    )),
                };
            }
        },
        _ = &mut cancel_rx => {
            Box::into_pin(child.kill())
                .await
                .map_err(|e| anyhow!("bash: process-tree cleanup failed: {e}"))?;
            ProcessOutcome::Cancelled
        }
    };

    let pipe_limit = match process_outcome {
        ProcessOutcome::Completed(_) => Some(POST_EXIT_PIPE_JOIN),
        ProcessOutcome::Cancelled => Some(POST_KILL_PIPE_JOIN),
    };
    let (stdout, stderr) = join_shell_pipes(
        stdout_task,
        stderr_task,
        stdout_abort,
        stderr_abort,
        pipe_limit,
    )
    .await?;

    Ok(match process_outcome {
        ProcessOutcome::Completed(status) => ShellSupervisorOutcome::Completed(ShellExecution {
            status,
            stdout,
            stderr,
        }),
        ProcessOutcome::Cancelled => ShellSupervisorOutcome::Cancelled,
    })
}

/// Spawn the platform shell inside a dedicated Unix session or Windows Job Object, then transfer
/// process-tree ownership to a supervisor that survives cancellation of the calling future.
fn spawn_shell_command(
    command: &str,
    working_dir: &Path,
    env: &HashMap<String, String>,
) -> Result<ShellCommandController> {
    let mut cmd = platform_shell_command(command)?;
    cmd.current_dir(working_dir)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut wrapped = CommandWrap::from(cmd);
    wrapped.wrap(KillOnDrop);
    #[cfg(unix)]
    wrapped.wrap(ProcessSession);
    #[cfg(windows)]
    wrapped.wrap(JobObject);

    let mut child = wrapped
        .spawn()
        .map_err(|e| anyhow!("bash: spawn failed: {e}"))?;
    let stdout = child
        .stdout()
        .take()
        .ok_or_else(|| anyhow!("bash: stdout pipe unavailable"))?;
    let stderr = child
        .stderr()
        .take()
        .ok_or_else(|| anyhow!("bash: stderr pipe unavailable"))?;

    let stdout_task = tokio::spawn(read_shell_pipe(stdout));
    let stderr_task = tokio::spawn(read_shell_pipe(stderr));
    let (cancel_tx, cancel_rx) = oneshot::channel();
    let (outcome_tx, outcome_rx) = oneshot::channel();
    tokio::spawn(async move {
        let outcome = supervise_shell_process(child, stdout_task, stderr_task, cancel_rx).await;
        let _ = outcome_tx.send(outcome);
    });

    Ok(ShellCommandController {
        cancel_tx: Some(cancel_tx),
        outcome_rx,
    })
}

/// Run a supervised shell command. Timeout is an orderly controller cancellation: kill and reap
/// must complete before this function returns.
async fn run_shell_command(
    command: &str,
    working_dir: &Path,
    timeout_duration: Duration,
    env: &HashMap<String, String>,
) -> Result<String> {
    let mut controller = spawn_shell_command(command, working_dir, env)?;

    match tokio::time::timeout(timeout_duration, controller.wait()).await {
        Ok(Ok(ShellSupervisorOutcome::Completed(execution))) => Ok(format_shell_output(
            execution.status,
            &execution.stdout,
            &execution.stderr,
        )),
        Ok(Ok(ShellSupervisorOutcome::Cancelled)) => Err(anyhow!("bash: command cancelled")),
        Ok(Err(e)) => Err(e),
        Err(_) => match tokio::time::timeout(POST_TIMEOUT_CLEANUP, controller.cancel_and_wait())
            .await
        {
            Ok(Ok(ShellSupervisorOutcome::Cancelled | ShellSupervisorOutcome::Completed(_))) => {
                Err(anyhow!(
                    "bash: command timed out after {}s",
                    timeout_duration.as_secs()
                ))
            }
            Ok(Err(e)) => Err(anyhow!(
                "bash: command timed out after {}s; {e}",
                timeout_duration.as_secs()
            )),
            Err(_) => Err(anyhow!(
                "bash: command timed out after {}s; process-tree cleanup exceeded the post-kill deadline",
                timeout_duration.as_secs()
            )),
        },
    }
}

/// Execute a shell command with process-tree isolation and env filtering.
async fn tool_bash(input: &Value, working_dir: &Path) -> Result<String> {
    let command = input
        .get("command")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("bash: missing 'command' parameter"))?;

    let cmd_working_dir = input
        .get("working_dir")
        .and_then(|v| v.as_str())
        .map(|p| validate_path(p, working_dir))
        .transpose()?
        .unwrap_or_else(|| working_dir.to_path_buf());

    let timeout_secs = std::env::var("OPENAB_AGENT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(120);

    let env_allow: Vec<String> = std::env::var("OPENAB_AGENT_BASH_ENV_ALLOW")
        .unwrap_or_default()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

    let env = build_env(&env_allow);

    debug!("bash: executing '{}' in {:?}", command, cmd_working_dir);
    run_shell_command(
        command,
        &cmd_working_dir,
        Duration::from_secs(timeout_secs),
        &env,
    )
    .await
}

/// Return tool definitions for the LLM.
pub fn tool_definitions() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "read".to_string(),
            description: "Read file contents or list a directory.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File or directory path" },
                    "offset": { "type": "integer", "description": "Line offset to start reading from (0-indexed)" },
                    "limit": { "type": "integer", "description": "Number of lines to read" }
                },
                "required": ["path"]
            }),
        },
        ToolDef {
            name: "write".to_string(),
            description: "Create or overwrite a file with the given content.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to write" },
                    "content": { "type": "string", "description": "Content to write" }
                },
                "required": ["path", "content"]
            }),
        },
        ToolDef {
            name: "edit".to_string(),
            description: "Replace the first occurrence of old_str with new_str in a file."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "File path to edit" },
                    "old_str": { "type": "string", "description": "Exact string to find" },
                    "new_str": { "type": "string", "description": "Replacement string" }
                },
                "required": ["path", "old_str", "new_str"]
            }),
        },
        ToolDef {
            name: "bash".to_string(),
            description: "Execute a shell command and return stdout/stderr.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "working_dir": { "type": "string", "description": "Working directory (optional, defaults to agent working dir)" }
                },
                "required": ["command"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_validate_path_within_working_dir() {
        let tmp = TempDir::new().unwrap();
        let result = validate_path("test.txt", tmp.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_path_traversal_denied() {
        let tmp = TempDir::new().unwrap();
        let result = validate_path("../../etc/passwd", tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("path traversal"));
    }

    #[test]
    fn test_powershell_encoded_command_preserves_unicode_and_quotes() {
        let command = "Write-Output 'héllo 世界'; Write-Output \"quoted\"";
        let encoded = powershell_encoded_command(command);
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        assert_eq!(bytes.len() % 2, 0);
        let utf16: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let decoded = String::from_utf16(&utf16).unwrap();
        assert!(decoded.ends_with(command));
        assert!(decoded.contains("UTF8Encoding"));
    }

    #[test]
    #[ignore] // Integration test: filesystem access
    fn test_tool_write_and_read() {
        let tmp = TempDir::new().unwrap();
        let input = json!({ "path": "hello.txt", "content": "hello world" });
        let result = tool_write(&input, tmp.path()).unwrap();
        assert!(result.contains("11 bytes"));

        let read_input = json!({ "path": "hello.txt" });
        let content = tool_read(&read_input, tmp.path()).unwrap();
        assert_eq!(content, "hello world");
    }

    #[test]
    #[ignore] // Integration test: filesystem access
    fn test_tool_edit() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("test.rs");
        std::fs::write(&file_path, "fn main() {\n    println!(\"old\");\n}\n").unwrap();

        let input = json!({
            "path": "test.rs",
            "old_str": "println!(\"old\")",
            "new_str": "println!(\"new\")"
        });
        let result = tool_edit(&input, tmp.path()).unwrap();
        assert!(result.contains("replaced 1 occurrence"));

        let content = std::fs::read_to_string(&file_path).unwrap();
        assert!(content.contains("println!(\"new\")"));
    }

    #[test]
    #[ignore] // Integration test: filesystem access
    fn test_tool_read_directory() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.txt"), "").unwrap();
        std::fs::write(tmp.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(tmp.path().join("subdir")).unwrap();

        let input = json!({ "path": "." });
        let result = tool_read(&input, tmp.path()).unwrap();
        assert!(result.contains("a.txt"));
        assert!(result.contains("b.txt"));
        assert!(result.contains("subdir/"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_tool_bash_simple() {
        let tmp = TempDir::new().unwrap();
        let input = json!({ "command": "echo hello" });
        let result = tool_bash(&input, tmp.path()).await.unwrap();
        assert_eq!(result.trim(), "hello");
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_tool_bash_windows_unicode_and_quoting() {
        let tmp = TempDir::new().unwrap();
        let input =
            json!({ "command": "Write-Output 'héllo 世界'; Write-Output \"quoted value\"" });
        let result = tool_bash(&input, tmp.path()).await.unwrap();
        assert!(result.contains("héllo 世界"));
        assert!(result.contains("quoted value"));
    }

    #[cfg(windows)]
    async fn warm_windows_shell(working_dir: &Path) {
        let result = run_shell_command(
            "Write-Output 'warm'",
            working_dir,
            Duration::from_secs(60),
            &build_env(&[]),
        )
        .await
        .expect("Windows PowerShell warmup failed");
        assert!(result.contains("warm"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_unix_future_drop_requests_supervised_process_tree_cleanup() {
        let tmp = TempDir::new().unwrap();
        let command = concat!(
            "(sleep 2; printf escaped > drop-orphan.txt) & ",
            "printf ready > drop-ready.txt; ",
            "sleep 120"
        );
        let task_dir = tmp.path().to_path_buf();
        let task = tokio::spawn(async move {
            run_shell_command(
                command,
                &task_dir,
                Duration::from_secs(120),
                &build_env(&[]),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            while !tmp.path().join("drop-ready.txt").exists() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("parent shell did not start");
        task.abort();
        let _ = task.await;

        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(
            !tmp.path().join("drop-orphan.txt").exists(),
            "descendant escaped supervised cleanup when the shell future was dropped"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_windows_timeout_kills_descendant_process() {
        let tmp = TempDir::new().unwrap();
        warm_windows_shell(tmp.path()).await;
        let descendant = powershell_encoded_command(
            "Start-Sleep -Seconds 10; [IO.File]::WriteAllText('orphan.txt', 'escaped')",
        );
        let command = format!(
            concat!(
                "$child = Join-Path $PSHOME 'powershell.exe'; ",
                "Start-Process -FilePath $child -ArgumentList @(",
                "'-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand','{descendant}'); ",
                "[IO.File]::WriteAllText('ready.txt', 'ready'); ",
                "Start-Sleep -Seconds 120"
            ),
            descendant = descendant
        );
        let error = run_shell_command(
            &command,
            tmp.path(),
            Duration::from_secs(5),
            &build_env(&[]),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(tmp.path().join("ready.txt").exists());

        tokio::time::sleep(Duration::from_secs(12)).await;
        assert!(
            !tmp.path().join("orphan.txt").exists(),
            "descendant escaped the Windows Job Object"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_windows_future_drop_kills_descendant_process() {
        let tmp = TempDir::new().unwrap();
        warm_windows_shell(tmp.path()).await;
        let descendant = powershell_encoded_command(
            "Start-Sleep -Seconds 10; [IO.File]::WriteAllText('drop-orphan.txt', 'escaped')",
        );
        let command = format!(
            concat!(
                "$child = Join-Path $PSHOME 'powershell.exe'; ",
                "Start-Process -FilePath $child -ArgumentList @(",
                "'-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand','{descendant}'); ",
                "[IO.File]::WriteAllText('drop-ready.txt', 'ready'); ",
                "Start-Sleep -Seconds 120"
            ),
            descendant = descendant
        );
        let task_dir = tmp.path().to_path_buf();
        let task = tokio::spawn(async move {
            run_shell_command(
                &command,
                &task_dir,
                Duration::from_secs(120),
                &build_env(&[]),
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(10), async {
            while !tmp.path().join("drop-ready.txt").exists() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("parent shell did not start");
        task.abort();
        let _ = task.await;

        tokio::time::sleep(Duration::from_secs(12)).await;
        assert!(
            !tmp.path().join("drop-orphan.txt").exists(),
            "descendant escaped cleanup when the shell future was dropped"
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn test_windows_controller_cancel_waits_for_descendant_cleanup() {
        let tmp = TempDir::new().unwrap();
        warm_windows_shell(tmp.path()).await;
        let descendant = powershell_encoded_command(
            "Start-Sleep -Seconds 10; [IO.File]::WriteAllText('cancel-orphan.txt', 'escaped')",
        );
        let command = format!(
            concat!(
                "$child = Join-Path $PSHOME 'powershell.exe'; ",
                "Start-Process -FilePath $child -ArgumentList @(",
                "'-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand','{descendant}'); ",
                "[IO.File]::WriteAllText('cancel-ready.txt', 'ready'); ",
                "Start-Sleep -Seconds 120"
            ),
            descendant = descendant
        );
        let mut controller = spawn_shell_command(&command, tmp.path(), &build_env(&[])).unwrap();

        tokio::time::timeout(Duration::from_secs(10), async {
            while !tmp.path().join("cancel-ready.txt").exists() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("parent shell did not start");
        let outcome = controller.cancel_and_wait().await.unwrap();
        assert!(matches!(outcome, ShellSupervisorOutcome::Cancelled));

        tokio::time::sleep(Duration::from_secs(12)).await;
        assert!(
            !tmp.path().join("cancel-orphan.txt").exists(),
            "controller returned before the Windows process tree was cleaned up"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_unix_completed_returns_when_setsid_descendant_holds_pipes() {
        let tmp = TempDir::new().unwrap();
        let command = "setsid sh -c 'echo $$ > escaped.pid; exec sleep 30' & exit 0";
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(8),
            run_shell_command(
                command,
                tmp.path(),
                Duration::from_secs(10),
                &build_env(&[]),
            ),
        )
        .await
        .expect("completed path hung waiting for escaped descendant pipes");
        assert!(
            result.is_ok(),
            "normal exit must not hang on leftover pipes: {result:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "completed-path pipe join must be bounded"
        );

        if let Ok(raw) = std::fs::read_to_string(tmp.path().join("escaped.pid")) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status();
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_unix_timeout_returns_when_setsid_descendant_holds_pipes() {
        let tmp = TempDir::new().unwrap();
        let command = "setsid sh -c 'echo $$ > escaped.pid; exec sleep 30' & exec sleep 120";
        let started = std::time::Instant::now();
        let error = tokio::time::timeout(
            Duration::from_secs(8),
            run_shell_command(command, tmp.path(), Duration::from_secs(1), &build_env(&[])),
        )
        .await
        .expect("timeout path hung waiting for escaped descendant pipes")
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(
            started.elapsed() < Duration::from_secs(8),
            "post-kill pipe join must be bounded"
        );

        if let Ok(raw) = std::fs::read_to_string(tmp.path().join("escaped.pid")) {
            if let Ok(pid) = raw.trim().parse::<i32>() {
                let _ = std::process::Command::new("kill")
                    .args(["-9", &pid.to_string()])
                    .status();
            }
        }
    }

    #[tokio::test]
    #[ignore] // Integration test: subprocess execution
    async fn test_tool_bash_env_filtered() {
        let tmp = TempDir::new().unwrap();
        // Verify that arbitrary env vars are NOT passed through (env is cleared)
        let input = json!({ "command": "env | grep -c ANTHROPIC || true" });
        let result = tool_bash(&input, tmp.path()).await.unwrap();
        // With env_clear(), no ANTHROPIC vars should exist in child
        assert!(result.trim() == "0" || result.trim().is_empty() || result.contains("[exit code:"));
    }
}
