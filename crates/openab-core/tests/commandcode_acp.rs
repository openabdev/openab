//! Opt-in live test against the real CLI and the current user's login.
//! Runs two model turns and writes only into a disposable project directory.

use anyhow::{ensure, Context, Result};
use openab_core::acp::connection::{AcpConnection, ContentBlock};
use openab_core::acp::{classify_notification, parse_turn_result, AcpEvent};
use std::collections::HashMap;
use std::time::Duration;

struct Reply {
    text: String,
    chunks: usize,
    tools_started: usize,
    tools_completed: usize,
}

async fn prompt(connection: &mut AcpConnection, text: String) -> Result<Reply> {
    let (mut notifications, request_id) = connection
        .session_prompt(vec![ContentBlock::Text { text }])
        .await?;
    let streamed = tokio::time::timeout(Duration::from_secs(240), async {
        let mut reply = Reply {
            text: String::new(),
            chunks: 0,
            tools_started: 0,
            tools_completed: 0,
        };
        while let Some(message) = notifications.recv().await {
            if message.id.as_ref().and_then(|id| id.as_u64()) == Some(request_id) {
                if let Some(error) = message.error {
                    anyhow::bail!("prompt failed: {error}");
                }
                let result = message.result.context("prompt result missing")?;
                let turn = parse_turn_result(&result);
                ensure!(turn.stop_reason.as_deref() == Some("end_turn"));
                ensure!(!turn.is_silent_failure(), "silent provider failure");
                return Ok(reply);
            }
            match classify_notification(&message) {
                Some(AcpEvent::Text(chunk)) => {
                    reply.text.push_str(&chunk);
                    reply.chunks += 1;
                }
                Some(AcpEvent::ToolStart { .. }) => reply.tools_started += 1,
                Some(AcpEvent::ToolDone { status, .. }) if status == "completed" => {
                    reply.tools_completed += 1;
                }
                _ => {}
            }
        }
        anyhow::bail!("ACP closed before prompt completion")
    })
    .await;
    if streamed.is_err() {
        connection.abandon_request(request_id).await;
    }
    connection.prompt_done().await;
    streamed.context("prompt timed out")?
}

#[tokio::test]
#[ignore = "requires Command Code login; makes two real model calls"]
async fn commandcode_authenticated_roundtrip() -> Result<()> {
    let project = tempfile::tempdir()?;
    let cwd = project
        .path()
        .to_str()
        .context("project path is not UTF-8")?;
    let command = std::env::var("COMMANDCODE_ACP_COMMAND").unwrap_or("command-code".into());
    let env = HashMap::new();
    let args = vec!["acp".to_string()];
    let inherit_env = vec!["COMMAND_CODE_API_KEY".to_string()];
    let mut connection = AcpConnection::spawn(&command, &args, cwd, &env, &inherit_env).await?;
    connection.initialize().await?;
    ensure!(connection.agent_name == "Command Code");
    ensure!(connection.supports_load_session);
    println!("PASS initialize through OpenAB AcpConnection");

    let session_id = connection.session_new(cwd).await?;
    ensure!(!session_id.is_empty());
    println!("PASS authenticated session/new");
    for option in &connection.config_options {
        if option.id == "model" {
            println!("Model: {}", option.current_value);
        }
    }

    let file_token = format!("FILE_{}", uuid::Uuid::new_v4().simple());
    let memory_token = format!("MEMORY_{}", uuid::Uuid::new_v4().simple());
    let ready = format!("READY_{}", uuid::Uuid::new_v4().simple());
    let first = prompt(
        &mut connection,
        format!(
            "This is an ACP integration test in a disposable project. Do not delegate, \
             run unrelated commands, or access files outside the current directory. \
             Use a file-writing tool to create proof.txt containing exactly {file_token}. \
             Remember this separate token for our next turn: {memory_token}. \
             Do not write the memory token to any file. Then reply with only {ready}."
        ),
    )
    .await?;
    ensure!(first.text.contains(&ready), "expected reply marker missing");
    ensure!(first.chunks > 0, "no streamed text");
    ensure!(first.tools_started > 0, "no tool-call notifications");
    ensure!(first.tools_completed > 0, "no successful tool completion");
    ensure!(std::fs::read_to_string(project.path().join("proof.txt"))?.trim() == file_token);
    println!(
        "PASS live prompt: {} text chunks, {} completed tool updates, verified file contents",
        first.chunks, first.tools_completed
    );

    drop(connection);
    tokio::time::sleep(Duration::from_secs(2)).await;
    let mut resumed = AcpConnection::spawn(&command, &args, cwd, &env, &inherit_env).await?;
    resumed.initialize().await?;
    resumed.session_load(&session_id, cwd).await?;
    println!("PASS session/load in a new agent process");

    let second = prompt(
        &mut resumed,
        "Reply with only the separate memory token I asked you to remember in our previous turn. \
         Do not use tools, read files, or delegate."
            .into(),
    )
    .await?;
    ensure!(
        second.text.contains(&memory_token),
        "restored conversation memory missing"
    );
    ensure!(second.chunks > 0);
    println!("PASS resumed prompt: recovered conversation-only memory token");
    Ok(())
}
