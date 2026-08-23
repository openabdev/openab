//! End-to-end ordering tests for the ACP direct-relay streaming path: a scripted
//! fake agent emits interleaved text / tool / thought session updates over real
//! stdio, and a recording ChatAdapter captures the relay order on the other side.

use anyhow::Result;
use async_trait::async_trait;
use openab_core::acp::{ContentBlock, SessionPool};
use openab_core::adapter::{AdapterRouter, ChannelRef, ChatAdapter, MessageRef};
use openab_core::config::{AgentConfig, ReactionsConfig};
use openab_core::markdown::TableMode;
use openab_core::reactions::StatusReactionController;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// Tests in this binary mutate process env (HOME, OPENAB_ACP_STREAMING); run them
/// one at a time. This file is its own test process, so no other test binary sees
/// the mutations.
fn env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Send(String),
    Edit { message_id: String, content: String },
    Update(String),
}

struct RecordingAdapter {
    calls: Mutex<Vec<Call>>,
    streaming: bool,
}

impl RecordingAdapter {
    fn new(streaming: bool) -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            streaming,
        }
    }
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }
}

#[async_trait]
impl ChatAdapter for RecordingAdapter {
    fn platform(&self) -> &'static str {
        "unified"
    }
    fn message_limit(&self) -> usize {
        4096
    }
    async fn send_message(&self, channel: &ChannelRef, content: &str) -> Result<MessageRef> {
        self.calls.lock().unwrap().push(Call::Send(content.to_string()));
        Ok(MessageRef {
            channel: channel.clone(),
            message_id: "m1".into(),
        })
    }
    async fn create_thread(
        &self,
        _: &ChannelRef,
        _: &MessageRef,
        _: &str,
    ) -> Result<ChannelRef> {
        unimplemented!()
    }
    async fn add_reaction(&self, _: &MessageRef, _: &str) -> Result<()> {
        Ok(())
    }
    async fn remove_reaction(&self, _: &MessageRef, _: &str) -> Result<()> {
        Ok(())
    }
    async fn edit_message(&self, msg: &MessageRef, content: &str) -> Result<()> {
        self.calls.lock().unwrap().push(Call::Edit {
            message_id: msg.message_id.clone(),
            content: content.to_string(),
        });
        Ok(())
    }
    async fn forward_agent_update(
        &self,
        _: &ChannelRef,
        update: serde_json::Value,
    ) -> Result<()> {
        let kind = update
            .get("sessionUpdate")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        self.calls.lock().unwrap().push(Call::Update(kind));
        Ok(())
    }
    fn use_streaming(&self, _other_bot_present: bool) -> bool {
        self.streaming
    }
}

/// A line-delimited JSON-RPC fake agent: answers initialize / session/new, and on
/// session/prompt emits text chunks interleaved with tool + thought updates before
/// the final response — the exact shape that reproduced the production reordering.
const FAKE_AGENT: &str = r##"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"agentInfo":{"name":"fake","version":"0"},"agentCapabilities":{}}}\n' "$id"
      ;;
    *'"session/new"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"sessionId":"sess_fake"}}\n' "$id"
      ;;
    *'"session/prompt"'*)
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"Linode C"}}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"check availability","status":"pending"}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"thinking"}}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"LI ok"}}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"tool_call_update","toolCallId":"t1","title":"check availability","status":"completed"}}}\n'
      printf '{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess_fake","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":" done"}}}}\n'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"stopReason":"end_turn"}}\n' "$id"
      ;;
  esac
done
"##;

struct Fixture {
    router: AdapterRouter,
    channel: ChannelRef,
    _tmp: tempfile::TempDir,
}

async fn setup(platform: &str, thread_key: &str) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    // Isolate ~/.openab persistence (thread_map.json / session_meta.json).
    std::env::set_var("HOME", tmp.path());
    std::env::set_var("OPENAB_ACP_STREAMING", "1");
    std::env::remove_var("OPENAB_STREAM_EDIT_INTERVAL_MS");

    let script = tmp.path().join("fake_agent.sh");
    std::fs::write(&script, FAKE_AGENT).expect("write fake agent");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let agent_cfg = AgentConfig {
        command: script.to_string_lossy().into_owned(),
        args: vec![],
        working_dir: tmp.path().to_string_lossy().into_owned(),
        env: HashMap::new(),
        inherit_env: vec![],
        command_explicit: true,
    };
    let pool = Arc::new(SessionPool::new(agent_cfg, 1, 60, HashMap::new()));
    pool.get_or_create(thread_key, None, &[], None)
        .await
        .expect("session spawn against fake agent");

    let router = AdapterRouter::new(
        pool,
        ReactionsConfig::default(),
        TableMode::Code,
        30,
        1,
        HashMap::new(),
        tmp.path().to_path_buf(),
    );
    let channel = ChannelRef {
        platform: platform.into(),
        channel_id: format!("{platform}_chan"),
        thread_id: None,
        parent_id: None,
        origin_event_id: Some("evt_test".into()),
    };
    Fixture {
        router,
        channel,
        _tmp: tmp,
    }
}

async fn run_turn(fx: &Fixture, adapter: &Arc<dyn ChatAdapter>, thread_key: &str) {
    let reactions = Arc::new(StatusReactionController::new(
        false,
        adapter.clone(),
        MessageRef {
            channel: fx.channel.clone(),
            message_id: "trigger".into(),
        },
        Default::default(),
        Default::default(),
    ));
    fx.router
        .stream_prompt_blocks(
            adapter,
            thread_key,
            vec![ContentBlock::Text { text: "hi".into() }],
            &fx.channel,
            reactions,
            false,
            None,
        )
        .await
        .expect("turn completes");
}

// ACP + streaming: text snapshots are relayed inline, so they interleave with
// tool/thought updates in exact arrival order, and the terminal send repeats the
// last snapshot verbatim (the gateway diffs it to nothing → no duplicate tail).
#[tokio::test(flavor = "multi_thread")]
async fn acp_streaming_relays_text_inline_in_arrival_order() {
    let _guard = env_lock();
    let fx = setup("acp", "acp:order").await;
    let recorder = Arc::new(RecordingAdapter::new(false));
    let adapter: Arc<dyn ChatAdapter> = recorder.clone();
    run_turn(&fx, &adapter, "acp:order").await;

    let calls = recorder.calls();
    let expected = vec![
        Call::Edit {
            message_id: "draft".into(),
            content: "Linode C".into(),
        },
        Call::Update("tool_call".into()),
        Call::Update("agent_thought_chunk".into()),
        Call::Edit {
            message_id: "draft".into(),
            content: "Linode CLI ok".into(),
        },
        Call::Update("tool_call_update".into()),
        Call::Edit {
            message_id: "draft".into(),
            content: "Linode CLI ok done".into(),
        },
        Call::Send("Linode CLI ok done".into()),
    ];
    assert_eq!(
        calls, expected,
        "ACP streaming must relay text in arrival order and finish with a \
         terminal send identical to the last snapshot"
    );
}

// Non-ACP platforms keep the paced edit loop: with the default 1500ms interval a
// fast turn produces no mid-stream partial edits — text is never relayed inline
// and agent updates are not forwarded.
#[tokio::test(flavor = "multi_thread")]
async fn non_acp_streaming_keeps_paced_edit_loop() {
    let _guard = env_lock();
    let fx = setup("gateway", "gateway:paced").await;
    let recorder = Arc::new(RecordingAdapter::new(true));
    let adapter: Arc<dyn ChatAdapter> = recorder.clone();
    run_turn(&fx, &adapter, "gateway:paced").await;

    let calls = recorder.calls();
    assert!(
        !calls.iter().any(|c| matches!(c, Call::Update(_))),
        "agent updates must not be forwarded off the ACP path: {calls:?}"
    );
    assert_eq!(
        calls.first(),
        Some(&Call::Send("…".into())),
        "non-ACP streaming still opens with the placeholder: {calls:?}"
    );
    // The fake turn finishes far below the 1500ms pacing interval, so the only
    // edit is the finalize write — no inline per-chunk edits.
    let edits: Vec<&Call> = calls
        .iter()
        .filter(|c| matches!(c, Call::Edit { .. }))
        .collect();
    assert_eq!(edits.len(), 1, "expected only the finalize edit: {calls:?}");
    match edits[0] {
        Call::Edit { content, .. } => assert!(
            content.contains("Linode CLI ok done"),
            "finalize edit must carry the full text: {content}"
        ),
        _ => unreachable!(),
    }
}
