//! Real-path regression test for issue #1527 (Claim 1: `MockDispatchTarget`).
//!
//! `consumer_loop` / `dispatch_batch` are covered in `dispatch.rs`'s unit tests
//! through `MockDispatchTarget`, which records calls instead of touching a real
//! `SessionPool` / ACP subprocess. That leaves the production wiring —
//! `Dispatcher::submit` → real `AdapterRouter` → `SessionPool::get_or_create` →
//! `AcpConnection::spawn` → ACP handshake → `stream_prompt_blocks` → reader loop
//! → `ChatAdapter` delivery — entirely unexercised: a break anywhere along it
//! would leave the mock-target suite green.
//!
//! This test closes that gap using a tiny stub ACP agent subprocess
//! (`src/bin/oab_acp_stub_agent.rs`) plus a record-only `ChatAdapter`, keeping
//! only the platform adapter faked — the platform is the external boundary, the
//! dispatcher/router/session machinery is the product-owned seam under test. A
//! real agent reply reaching `send_message` proves the end-to-end path.
//!
//! Lives in `tests/` rather than the module's unit tests because only
//! integration-test targets get `CARGO_BIN_EXE_<name>` at compile time; unit
//! tests inside `src/` do not.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use async_trait::async_trait;

use openab_core::acp::SessionPool;
use openab_core::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef};
use openab_core::config::{self, ReactionsConfig};
use openab_core::dispatch::{
    BatchGrouping, BufferedMessage, Dispatcher, DEFAULT_CONSUMER_IDLE_TIMEOUT,
};
use openab_core::markdown::TableMode;

/// Records every `send_message` payload so the real-delivery path can be asserted.
struct RecordingChatAdapter {
    sent: Arc<Mutex<Vec<String>>>,
}

impl RecordingChatAdapter {
    fn new() -> (Self, Arc<Mutex<Vec<String>>>) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                sent: Arc::clone(&sent),
            },
            sent,
        )
    }
}

#[async_trait]
impl ChatAdapter for RecordingChatAdapter {
    fn platform(&self) -> &'static str {
        "mock"
    }
    fn message_limit(&self) -> usize {
        2000
    }

    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        self.sent.lock().unwrap().push(content.to_string());
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: "mock-msg".into(),
        })
    }

    async fn create_thread(
        &self,
        channel: &ChannelRef,
        _trigger_msg: &MessageRef,
        _title: &str,
    ) -> Result<ChannelRef> {
        Ok(channel.clone())
    }

    async fn add_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        Ok(())
    }
    async fn remove_reaction(&self, _msg: &MessageRef, _emoji: &str) -> Result<()> {
        Ok(())
    }
    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        false
    }
}

/// Build a `Dispatcher` wired to a REAL `AdapterRouter` + `SessionPool` pointed
/// at `command` — the same construction `main.rs` performs for a configured
/// agent, minus platform adapters.
fn make_dispatcher_with_command(
    grouping: BatchGrouping,
    command: &str,
    workdir: &str,
) -> Dispatcher {
    let agent_cfg = config::AgentConfig {
        command: command.into(),
        args: vec![],
        working_dir: workdir.into(),
        env: HashMap::new(),
        inherit_env: vec![],
        command_explicit: true,
    };
    // Timeout values mirror the production defaults (30min hard timeout + 2min
    // hung grace, 30s liveness) — the `config::default_*_secs` helpers are
    // `pub(crate)` and unreachable from an integration test.
    const PROMPT_HARD_TIMEOUT_SECS: u64 = 30 * 60;
    const HUNG_GRACE_SECS: u64 = 120;
    const LIVENESS_CHECK_SECS: u64 = 30;
    let pool = Arc::new(SessionPool::new(
        agent_cfg,
        1,
        PROMPT_HARD_TIMEOUT_SECS.saturating_add(HUNG_GRACE_SECS),
        HashMap::new(),
    ));
    let router = Arc::new(AdapterRouter::new(
        pool,
        ReactionsConfig::default(),
        TableMode::Off,
        PROMPT_HARD_TIMEOUT_SECS,
        LIVENESS_CHECK_SECS,
        HashMap::new(),
        PathBuf::from(workdir),
    ));
    Dispatcher::with_idle_timeout(router, 10, 24_000, grouping, DEFAULT_CONSUMER_IDLE_TIMEOUT)
}

fn make_channel(thread: &str) -> ChannelRef {
    ChannelRef {
        platform: "mock".into(),
        channel_id: thread.into(),
        thread_id: Some(thread.into()),
        parent_id: None,
        origin_event_id: None,
    }
}

fn make_msg(prompt: &str, tokens: usize) -> BufferedMessage {
    BufferedMessage {
        sender_json: r#"{"schema":"openab.sender.v1","sender_id":"u","sender_name":"u"}"#.into(),
        sender_name: "u".into(),
        prompt: prompt.into(),
        extra_blocks: vec![],
        trigger_msg: MessageRef {
            channel: make_channel("T"),
            message_id: format!("m-{prompt}"),
        },
        arrived_at: Instant::now(),
        estimated_tokens: tokens,
        other_bot_present: false,
        recipient: None,
    }
}

/// A user prompt dispatched through `Dispatcher::submit` must traverse the real
/// router → session pool → ACP subprocess → reader loop and arrive at
/// `ChatAdapter::send_message` with the agent's reply text.
#[tokio::test]
async fn dispatcher_submit_reaches_stub_agent_through_real_router() {
    // Resolves at compile time to the stub agent built alongside this crate's
    // test targets — the reason this test lives in tests/ and not in the
    // module's unit tests.
    let stub = env!("CARGO_BIN_EXE_oab_acp_stub_agent");
    let workdir = tempfile::tempdir().unwrap();
    let workdir_str = workdir.path().to_str().unwrap();

    // `SessionPool::new` reads and writes `$HOME/.openab/thread_map.json`.
    // Point HOME at the tempdir so the test is hermetic: without it the first
    // run persists `mock:issue1527-seam → sess-1` and every later run takes the
    // resume path (no loadSession support → "session expired" reset notice
    // prepended to the reply), while on a developer machine it could also read
    // that machine's real pool state.
    //
    // `set_var` is sound here because `#[tokio::test]` drives a current_thread
    // runtime: every task in this process — the consumer loop, the ACP reader
    // task, the subprocess spawn with its env_clear + whitelist — is sequenced
    // on this one thread, so nothing reads `environ` concurrently with the
    // mutation. And this test binary carries exactly one test, so no sibling
    // test observes the override either.
    std::env::set_var("HOME", workdir_str);

    let d = make_dispatcher_with_command(BatchGrouping::Thread, stub, workdir_str);
    let (adapter, sent) = RecordingChatAdapter::new();
    let adapter: Arc<dyn ChatAdapter> = Arc::new(adapter);
    let thread_channel = make_channel("issue1527-seam");

    // Real submit path: lazily spawns a consumer → dispatch_batch →
    // ensure_session (real subprocess spawn + initialize/session/new handshake)
    // → stream_prompt_blocks (real session/prompt + recv loop) → send_message.
    d.submit(
        d.key("mock", "issue1527-seam", "u"),
        thread_channel,
        adapter,
        make_msg("hi", 10),
    )
    .await
    .expect("submit should not error; the stub answers every request");

    // The consumer is async; poll for the agent's reply with a hard ceiling so
    // a broken seam fails the suite fast instead of hanging CI. If the route
    // failed it instead delivers a ⚠️ error message — surface that immediately
    // rather than waiting for the timeout.
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            {
                let sent = sent.lock().unwrap();
                if let Some(found) = sent.iter().find(|m| m.contains("Hello, world!")) {
                    return found.clone();
                }
                if sent.iter().any(|m| m.contains('⚠')) {
                    return format!("ERROR: {}", sent.join("\n"));
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("consumer should have dispatched within 10s");

    assert_eq!(
        got, "Hello, world!",
        "real router→session→stub-agent reply must reach ChatAdapter.send_message unchanged"
    );
}
