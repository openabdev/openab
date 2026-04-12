use crate::acp::{classify_notification, AcpEvent, ContentBlock, SessionPool};
use crate::config::ReactionsConfig;
use crate::error_display::{format_coded_error, format_user_error};
use crate::format;
use crate::reactions::StatusReactionController;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use image::ImageReader;
use std::io::Cursor;
use std::sync::LazyLock;
use serenity::async_trait;
use serenity::builder::{
    AutocompleteChoice, CreateAutocompleteResponse, CreateCommand, CreateCommandOption,
    CreateEmbed, CreateInteractionResponse, CreateInteractionResponseMessage,
    EditInteractionResponse,
};
use serenity::model::application::{
    CommandDataOptionValue, CommandInteraction, CommandOptionType, Interaction,
};
use serenity::model::channel::{Message, ReactionType};
use serenity::model::gateway::Ready;
use serenity::model::id::{ChannelId, MessageId};
use serenity::prelude::*;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

/// Reusable HTTP client for downloading Discord attachments.
/// Built once with a 30s timeout and rustls TLS (no native-tls deps).
static HTTP_CLIENT: LazyLock<reqwest::Client> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .expect("static HTTP client must build")
});

/// Alias shortcuts disabled: autocomplete shows full model IDs only.
const MODEL_ALIASES: &[(&str, &str)] = &[];

/// Read-only top-level slash commands (no args, just display).
const COPILOT_READONLY_COMMANDS: &[(&str, &str)] = &[
    ("status",      "Show Copilot CLI version, auth, and model count"),
    ("plugins",     "List installed Copilot plugins"),
    ("plan",        "Read the current session plan.md"),
    ("files",       "List files in the session workspace"),
    ("auth",        "Show Copilot GitHub auth status"),
];

/// Map read-only command name → copilot-rpc.js subcommand.
fn copilot_readonly_to_rpc(name: &str) -> Option<&'static str> {
    match name {
        "status"      => Some("status"),
        "plugins"     => Some("plugins"),
        "plan"        => Some("plan-read"),
        "files"       => Some("files"),
        "auth"        => Some("auth"),
        _             => None,
    }
}

/// Interactive commands that take a <name> argument + dispatch to an action RPC.
/// Tuple: (discord_cmd, description, list_rpc_for_autocomplete, action_rpc,
///         autocomplete_data_key, autocomplete_name_key)
const COPILOT_INTERACTIVE_COMMANDS: &[(&str, &str, &str, &str, &str, &str)] = &[
    ("agent",       "Select an agent by name",         "agents",     "agent-select",    "agents",  "name"),
    ("skill-on",    "Enable a skill by name",          "skills",     "skill-enable",    "skills",  "name"),
    ("skill-off",   "Disable a skill by name",         "skills",     "skill-disable",   "skills",  "name"),
    ("mcp-on",      "Enable an MCP server by name",    "mcp-list",   "mcp-enable",      "servers", "name"),
    ("mcp-off",     "Disable an MCP server by name",   "mcp-list",   "mcp-disable",     "servers", "name"),
    ("ext-on",      "Enable an extension by name",     "extensions", "extension-enable","extensions","name"),
    ("ext-off",     "Disable an extension by name",    "extensions", "extension-disable","extensions","name"),
];

/// Map interactive command name → (list_rpc, action_rpc, data_key, name_key).
fn copilot_interactive_spec(name: &str) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    for (cmd, _, list_rpc, action_rpc, data_key, name_key) in COPILOT_INTERACTIVE_COMMANDS {
        if *cmd == name {
            return Some((list_rpc, action_rpc, data_key, name_key));
        }
    }
    None
}

/// Static mode choices (Copilot has 6 fixed modes).
const COPILOT_MODES: &[(&str, &str)] = &[
    ("https://agentclientprotocol.com/protocol/session-modes#agent",     "Agent (default)"),
    ("https://agentclientprotocol.com/protocol/session-modes#plan",      "Plan Mode"),
    ("https://agentclientprotocol.com/protocol/session-modes#autopilot", "Autopilot"),
];

/// Static choices for /reload <kind>.
/// Each tuple: (discord_value, copilot_rpc_subcommand, label)
const COPILOT_RELOAD_KINDS: &[(&str, &str, &str)] = &[
    ("agents",     "agent-reload",     "Agents"),
    ("skills",     "skill-reload",     "Skills"),
    ("mcp",        "mcp-reload",       "MCP servers"),
    ("extensions", "extension-reload", "Extensions"),
];

/// Backend type inferred from the agent command in config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendType {
    Claude,
    Copilot,
    Gemini,
    Codex,
    Other,
}

impl BackendType {
    /// Infer backend from the agent command + args strings.
    pub fn from_agent_config(command: &str, args: &[String]) -> Self {
        let joined = format!("{} {}", command, args.join(" ")).to_lowercase();
        if joined.contains("copilot") {
            BackendType::Copilot
        } else if joined.contains("claude") {
            BackendType::Claude
        } else if joined.contains("gemini") {
            BackendType::Gemini
        } else if joined.contains("codex") {
            BackendType::Codex
        } else {
            BackendType::Other
        }
    }

    /// Does this backend support Copilot SDK RPCs?
    pub fn has_copilot_rpc(&self) -> bool {
        *self == BackendType::Copilot
    }
}

pub struct Handler {
    pub pool: Arc<SessionPool>,
    pub allowed_channels: HashSet<u64>,
    pub allowed_users: HashSet<u64>,
    pub reactions_config: ReactionsConfig,
    pub usage_config: Option<crate::config::UsageConfig>,
    pub backend: BackendType,
    /// Cache of Copilot SDK list RPCs keyed by rpc subcommand name
    /// (e.g. "agents", "skills", "mcp-list", "extensions"). Values are
    /// the item `name` strings for autocomplete. Refreshed every 5 min
    /// by a background task spawned at startup.
    pub copilot_list_cache: Arc<tokio::sync::RwLock<std::collections::HashMap<String, Vec<String>>>>,
}

#[async_trait]
impl EventHandler for Handler {
    async fn message(&self, ctx: Context, msg: Message) {
        if msg.author.bot {
            return;
        }

        let bot_id = ctx.cache.current_user().id;

        let channel_id = msg.channel_id.get();
        let in_allowed_channel =
            self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id);

        let is_mentioned = msg.mentions_user_id(bot_id)
            || msg.content.contains(&format!("<@{}>", bot_id))
            || msg.mention_roles.iter().any(|r| msg.content.contains(&format!("<@&{}>", r)));

        let in_thread = if !in_allowed_channel {
            match msg.channel_id.to_channel(&ctx.http).await {
                Ok(serenity::model::channel::Channel::Guild(gc)) => {
                    let result = gc
                        .parent_id
                        .is_some_and(|pid| self.allowed_channels.contains(&pid.get()));
                    tracing::debug!(channel_id = %msg.channel_id, parent_id = ?gc.parent_id, result, "thread check");
                    result
                }
                Ok(other) => {
                    tracing::debug!(channel_id = %msg.channel_id, kind = ?other, "not a guild channel");
                    false
                }
                Err(e) => {
                    tracing::debug!(channel_id = %msg.channel_id, error = %e, "to_channel failed");
                    false
                }
            }
        } else {
            false
        };

        if !in_allowed_channel && !in_thread {
            return;
        }
        if !in_thread && !is_mentioned {
            return;
        }

        if !self.allowed_users.is_empty() && !self.allowed_users.contains(&msg.author.id.get()) {
            tracing::info!(user_id = %msg.author.id, "denied user, ignoring");
            if let Err(e) = msg.react(&ctx.http, ReactionType::Unicode("🚫".into())).await {
                tracing::warn!(error = %e, "failed to react with 🚫");
            }
            return;
        }

        let prompt = if is_mentioned {
            strip_mention(&msg.content)
        } else {
            msg.content.trim().to_string()
        };

        // No text and no image attachments → skip to avoid wasting session slots
        if prompt.is_empty() && msg.attachments.is_empty() {
            return;
        }

        // Build content blocks: text + image attachments
        let mut content_blocks = vec![];

        // Inject structured sender context so the downstream CLI can identify who sent the message
        let display_name = msg.member.as_ref()
            .and_then(|m| m.nick.as_ref())
            .unwrap_or(&msg.author.name);
        let sender_ctx = serde_json::json!({
            "schema": "openab.sender.v1",
            "sender_id": msg.author.id.to_string(),
            "sender_name": msg.author.name,
            "display_name": display_name,
            "channel": "discord",
            "channel_id": msg.channel_id.to_string(),
            "is_bot": msg.author.bot,
        });
        let prompt_with_sender = format!(
            "<sender_context>\n{}\n</sender_context>\n\n{}",
            serde_json::to_string(&sender_ctx).unwrap(),
            prompt
        );

        // Add text block (always, even if empty, we still send for sender context)
        content_blocks.push(ContentBlock::Text {
            text: prompt_with_sender.clone(),
        });

        // Add image attachments
        if !msg.attachments.is_empty() {
            for attachment in &msg.attachments {
                if let Some(content_block) = download_and_encode_image(attachment).await {
                    debug!(url = %attachment.url, filename = %attachment.filename, "adding image attachment");
                    content_blocks.push(content_block);
                } else {
                    error!(
                        url = %attachment.url,
                        filename = %attachment.filename,
                        "failed to download image attachment"
                    );
                }
            }
        }

        tracing::debug!(
            text_len = prompt_with_sender.len(),
            num_attachments = msg.attachments.len(),
            in_thread,
            "processing"
        );

        // Note: image-only messages (no text) are intentionally allowed since
        // prompt_with_sender always includes the non-empty sender_context XML.
        // The guard above (prompt.is_empty() && no attachments) handles stickers/embeds.

        let thread_id = if in_thread {
            msg.channel_id.get()
        } else {
            match get_or_create_thread(&ctx, &msg, &prompt).await {
                Ok(id) => id,
                Err(e) => {
                    error!("failed to create thread: {e}");
                    return;
                }
            }
        };

        let thread_channel = ChannelId::new(thread_id);

        let thinking_msg = match thread_channel.say(&ctx.http, "...").await {
            Ok(m) => m,
            Err(e) => {
                error!("failed to post: {e}");
                return;
            }
        };

        let thread_key = thread_id.to_string();
        if let Err(e) = self.pool.get_or_create(&thread_key).await {
            let msg = format_user_error(&e.to_string());
            let _ = edit(&ctx, thread_channel, thinking_msg.id, &format!("⚠️ {}", msg)).await;
            error!("pool error: {e}");
            return;
        }

        // Create reaction controller on the user's original message
        let reactions = Arc::new(StatusReactionController::new(
            self.reactions_config.enabled,
            ctx.http.clone(),
            msg.channel_id,
            msg.id,
            self.reactions_config.emojis.clone(),
            self.reactions_config.timing.clone(),
        ));
        reactions.set_queued().await;

        // Stream prompt with live edits (pass content blocks instead of just text)
        let result = stream_prompt(
            &self.pool,
            &thread_key,
            content_blocks,
            &ctx,
            thread_channel,
            thinking_msg.id,
            reactions.clone(),
        )
        .await;

        match &result {
            Ok(()) => reactions.set_done().await,
            Err(_) => reactions.set_error().await,
        }

        // Hold emoji briefly then clear
        let hold_ms = if result.is_ok() {
            self.reactions_config.timing.done_hold_ms
        } else {
            self.reactions_config.timing.error_hold_ms
        };
        if self.reactions_config.remove_after_reply {
            let reactions = reactions;
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_millis(hold_ms)).await;
                reactions.clear().await;
            });
        }

        if let Err(e) = result {
            let _ = edit(&ctx, thread_channel, thinking_msg.id, &format!("⚠️ {e}")).await;
        }
    }

    async fn ready(&self, ctx: Context, ready: Ready) {
        info!(user = %ready.user.name, guilds = ready.guilds.len(), "discord bot connected");

        // Register guild commands in every guild we're in.
        // Guild commands appear instantly (vs. global commands which can take
        // up to 1 hour to propagate).
        let model_cmd = CreateCommand::new("model")
            .description("Switch or query the AI model used by this bot")
            .add_option(
                CreateCommandOption::new(
                    CommandOptionType::String,
                    "model",
                    "Model id — leave empty to view current",
                )
                .required(false)
                .set_autocomplete(true),
            );

        let mut commands = vec![model_cmd];

        // Copilot-only commands: read-only info, interactive, mode, reload
        if self.backend.has_copilot_rpc() {
            // Read-only info commands
            for (name, desc) in COPILOT_READONLY_COMMANDS {
                commands.push(CreateCommand::new(*name).description(*desc));
            }

            // Interactive commands with <name> arg + autocomplete
            for (name, desc, _list, _action, _dk, _nk) in COPILOT_INTERACTIVE_COMMANDS {
                commands.push(
                    CreateCommand::new(*name)
                        .description(*desc)
                        .add_option(
                            CreateCommandOption::new(
                                CommandOptionType::String,
                                "name",
                                "Name (autocomplete)",
                            )
                            .required(true)
                            .set_autocomplete(true),
                        ),
                );
            }

            // /mode with static choices (Discord renders as dropdown)
            let mode_cmd = {
                let mut opt = CreateCommandOption::new(
                    CommandOptionType::String,
                    "mode",
                    "Session mode",
                )
                .required(true);
                for (value, label) in COPILOT_MODES {
                    opt = opt.add_string_choice(*label, *value);
                }
                CreateCommand::new("mode")
                    .description("Switch Copilot session mode (agent / plan / autopilot)")
                    .add_option(opt)
            };
            commands.push(mode_cmd);
        } // end Copilot-only

        // /reload <kind> — reload agents/skills/mcp/extensions (Copilot-only)
        if self.backend.has_copilot_rpc() {
        let reload_cmd = {
            let mut opt = CreateCommandOption::new(
                CommandOptionType::String,
                "kind",
                "What to reload",
            )
            .required(true);
            for (value, _rpc, label) in COPILOT_RELOAD_KINDS {
                opt = opt.add_string_choice(*label, *value);
            }
            CreateCommand::new("reload")
                .description("Reload Copilot agents/skills/mcp/extensions without restarting the bot")
                .add_option(opt)
        };
        commands.push(reload_cmd);
        } // end Copilot-only reload

        // /compact — reset the current Discord thread's agent session
        commands.push(
            CreateCommand::new("compact")
                .description("Compact the current thread's agent session (frees tokens by starting fresh)"),
        );

        // /new-session — explicit reset (alias semantic for compact with different wording)
        commands.push(
            CreateCommand::new("new-session")
                .description("Reset the current thread's agent session completely"),
        );

        // /tokens — show current thread's session token usage (bridge-only)
        commands.push(
            CreateCommand::new("tokens")
                .description("Show current thread's context window token usage"),
        );

        // /permissions — recent tool permission audit log (bridge-only)
        commands.push(
            CreateCommand::new("permissions")
                .description("Show recent tool permission requests in this thread"),
        );

        // Only register /usage if the user has configured it.
        if self.usage_config.as_ref().is_some_and(|u| u.enabled && !u.runners.is_empty()) {
            commands.push(
                CreateCommand::new("usage")
                    .description("Show usage quotas for configured backends"),
            );
        }

        // /native — run an agent's built-in slash command (memory, extensions, etc.)
        commands.push(
            CreateCommand::new("native")
                .description("Run an agent-native slash command (e.g. /memory show, /compact)")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "command",
                        "Agent command name (autocomplete from agent's list)",
                    )
                    .required(true)
                    .set_autocomplete(true),
                )
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "args",
                        "Optional arguments",
                    )
                    .required(false),
                ),
        );

        // /plan-mode — enter plan mode (send /plan prompt to the agent)
        commands.push(
            CreateCommand::new("plan-mode")
                .description("Enter plan mode — agent plans before executing")
                .add_option(
                    CreateCommandOption::new(
                        CommandOptionType::String,
                        "description",
                        "What to plan (optional)",
                    )
                    .required(false),
                ),
        );

        // /mcp — list MCP servers connected to the agent
        commands.push(
            CreateCommand::new("mcp")
                .description("List MCP servers and tools connected to the agent"),
        );

        // /export — export conversation history from the current thread
        commands.push(
            CreateCommand::new("export")
                .description("Export the current thread's conversation as text"),
        );

        // /doctor — diagnose agent connection health
        commands.push(
            CreateCommand::new("doctor")
                .description("Diagnose agent connection health (ping, session, uptime)"),
        );

        // /stats — detailed session statistics
        commands.push(
            CreateCommand::new("stats")
                .description("Show detailed session statistics (uptime, messages, native commands)"),
        );

        for guild in &ready.guilds {
            match guild.id.set_commands(&ctx.http, commands.clone()).await {
                Ok(cmds) => info!(guild_id = %guild.id, count = cmds.len(), "registered slash commands"),
                Err(e) => error!(guild_id = %guild.id, error = %e, "failed to register slash commands"),
            }
        }
    }

    async fn interaction_create(&self, ctx: Context, interaction: Interaction) {
        match interaction {
            Interaction::Command(cmd) if cmd.data.name == "model" => {
                self.handle_model_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "usage" => {
                self.handle_usage_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if copilot_readonly_to_rpc(&cmd.data.name).is_some() => {
                self.handle_copilot_readonly(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "mode" => {
                self.handle_copilot_mode(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "reload" => {
                self.handle_copilot_reload(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "compact" || cmd.data.name == "new-session" => {
                self.handle_reset_session(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "tokens" => {
                self.handle_tokens_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "permissions" => {
                self.handle_permissions_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "native" => {
                self.handle_native_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "plan-mode" => {
                self.handle_plan_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "mcp" => {
                self.handle_mcp_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "export" => {
                self.handle_export_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "doctor" => {
                self.handle_doctor_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if cmd.data.name == "stats" => {
                self.handle_stats_command(&ctx, &cmd).await;
            }
            Interaction::Command(cmd) if copilot_interactive_spec(&cmd.data.name).is_some() => {
                self.handle_copilot_interactive(&ctx, &cmd).await;
            }
            Interaction::Autocomplete(ac) if ac.data.name == "model" => {
                self.handle_model_autocomplete(&ctx, &ac).await;
            }
            Interaction::Autocomplete(ac) if ac.data.name == "native" => {
                self.handle_native_autocomplete(&ctx, &ac).await;
            }
            Interaction::Autocomplete(ac) if copilot_interactive_spec(&ac.data.name).is_some() => {
                self.handle_copilot_interactive_autocomplete(&ctx, &ac).await;
            }
            _ => {}
        }
    }
}

impl Handler {
    /// Resolve `partial` (the user's typing in the autocomplete field) to up
    /// to 25 suggestions, drawing from cached aliases + cached model ids.
    async fn handle_model_autocomplete(&self, ctx: &Context, ac: &CommandInteraction) {
        let partial = ac
            .data
            .options
            .first()
            .and_then(|o| match &o.value {
                CommandDataOptionValue::Autocomplete { value, .. } => Some(value.as_str()),
                CommandDataOptionValue::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("")
            .to_lowercase();

        let models = self.pool.cached_models().await;
        let current = self.pool.cached_current_model().await;

        let mut choices: Vec<AutocompleteChoice> = Vec::new();

        // Aliases first — cheap shortcuts most users will reach for
        for (alias, target) in MODEL_ALIASES {
            if !partial.is_empty() && !alias.starts_with(&partial) {
                continue;
            }
            // Only surface an alias if its target is actually available
            // (or if it's "auto", which is always valid).
            if *target != "auto" && !models.iter().any(|m| m.model_id == *target) {
                continue;
            }
            let label = if *target == "auto" {
                "auto (smart routing)".to_string()
            } else {
                format!("{alias} → {target}")
            };
            choices.push(AutocompleteChoice::new(label, (*alias).to_string()));
            if choices.len() >= 25 {
                break;
            }
        }

        // Then real model ids
        for m in &models {
            if choices.len() >= 25 {
                break;
            }
            if !partial.is_empty() && !m.model_id.to_lowercase().contains(&partial) {
                continue;
            }
            let marker = if m.model_id == current { " (current)" } else { "" };
            let label = format!("{}{marker}", m.model_id);
            choices.push(AutocompleteChoice::new(label, m.model_id.clone()));
        }

        let response = CreateInteractionResponse::Autocomplete(
            CreateAutocompleteResponse::new().set_choices(choices),
        );
        if let Err(e) = ac.create_response(&ctx.http, response).await {
            warn!(error = %e, "failed to send autocomplete response");
        }
    }

    /// Autocomplete for /native — suggest agent-native commands.
    async fn handle_native_autocomplete(&self, ctx: &Context, ac: &CommandInteraction) {
        let partial = ac
            .data
            .options
            .first()
            .and_then(|o| match &o.value {
                CommandDataOptionValue::Autocomplete { value, .. } => Some(value.as_str()),
                CommandDataOptionValue::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("")
            .to_lowercase();

        let cmds = self.pool.cached_native_commands().await;
        let mut choices: Vec<AutocompleteChoice> = Vec::new();
        for c in &cmds {
            if choices.len() >= 25 {
                break;
            }
            if !partial.is_empty() && !c.name.to_lowercase().contains(&partial) {
                continue;
            }
            let label = if c.description.is_empty() {
                c.name.clone()
            } else {
                format!("{} — {}", c.name, c.description.chars().take(60).collect::<String>())
            };
            choices.push(AutocompleteChoice::new(label, c.name.clone()));
        }
        let response = CreateInteractionResponse::Autocomplete(
            CreateAutocompleteResponse::new().set_choices(choices),
        );
        if let Err(e) = ac.create_response(&ctx.http, response).await {
            warn!(error = %e, "failed to send native autocomplete");
        }
    }

    /// Handle /native <command> [args] — send as prompt to the agent which parses it.
    async fn handle_native_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        // Allowlist: user
        if !self.allowed_users.is_empty() {
            if !self.allowed_users.contains(&cmd.user.id.get()) {
                let _ = cmd
                    .create_response(
                        &ctx.http,
                        CreateInteractionResponse::Message(
                            CreateInteractionResponseMessage::new()
                                .content("⛔ Not authorized")
                                .ephemeral(true),
                        ),
                    )
                    .await;
                return;
            }
        }

        let command_name = cmd
            .data
            .options
            .iter()
            .find(|o| o.name == "command")
            .and_then(|o| o.value.as_str())
            .unwrap_or("");
        let args = cmd
            .data
            .options
            .iter()
            .find(|o| o.name == "args")
            .and_then(|o| o.value.as_str())
            .unwrap_or("");

        if command_name.is_empty() {
            let cmds = self.pool.cached_native_commands().await;
            let list = if cmds.is_empty() {
                "No native commands detected from this agent.".to_string()
            } else {
                cmds.iter()
                    .map(|c| format!("• `{}` — {}", c.name, c.description))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content(format!("**Agent Native Commands:**\n{list}"))
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

        // Build the prompt text — agent CLIs parse /command internally
        let prompt_text = if args.is_empty() {
            format!("/{command_name}")
        } else {
            format!("/{command_name} {args}")
        };

        let _ = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("⚡ `{prompt_text}`")),
                ),
            )
            .await;

        // Get the thread id from the channel
        let thread_id = cmd.channel_id.get().to_string();

        // Ensure session exists
        if let Err(e) = self.pool.get_or_create(&thread_id).await {
            let _ = cmd
                .channel_id
                .say(&ctx.http, format!("⚠️ Session error: {e}"))
                .await;
            return;
        }

        // Send as prompt
        use crate::acp::connection::ContentBlock;
        let result = self
            .pool
            .with_connection(&thread_id, |conn| {
                let prompt_text = prompt_text.clone();
                Box::pin(async move {
                    let (mut rx, _id) = conn
                        .session_prompt(vec![ContentBlock::Text { text: prompt_text }])
                        .await?;

                    let mut reply = String::new();
                    while let Some(msg) = rx.recv().await {
                        // Final response (has id)
                        if msg.id.is_some() {
                            if let Some(result) = &msg.result {
                                if let Some(content) = result.get("content") {
                                    if let Some(arr) = content.as_array() {
                                        for block in arr {
                                            if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                                                reply.push_str(text);
                                            }
                                        }
                                    }
                                }
                            }
                            break;
                        }
                        // Streaming text chunks
                        if let Some(params) = &msg.params {
                            if let Some(upd) = params.get("update") {
                                if upd.get("sessionUpdate").and_then(|v| v.as_str())
                                    == Some("agent_message_chunk")
                                {
                                    if let Some(text) = upd
                                        .get("content")
                                        .and_then(|c| c.get("text"))
                                        .and_then(|t| t.as_str())
                                    {
                                        reply.push_str(text);
                                    }
                                }
                            }
                        }
                    }
                    conn.prompt_done().await;
                    Ok(reply)
                })
            })
            .await;

        match result {
            Ok(reply) if reply.is_empty() => {
                let _ = cmd.channel_id.say(&ctx.http, "✅ Command executed (no output)").await;
            }
            Ok(reply) => {
                // Discord message limit is 2000 chars
                let truncated = if reply.len() > 1900 {
                    format!("{}…\n*(truncated)*", &reply[..1900])
                } else {
                    reply
                };
                let _ = cmd.channel_id.say(&ctx.http, &truncated).await;
            }
            Err(e) => {
                let _ = cmd
                    .channel_id
                    .say(&ctx.http, format!("⚠️ {e}"))
                    .await;
            }
        }
    }

    /// Handle /plan — send /plan to the agent as a prompt.
    async fn handle_plan_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        let desc = cmd
            .data
            .options
            .iter()
            .find(|o| o.name == "description")
            .and_then(|o| o.value.as_str())
            .unwrap_or("");
        let prompt_text = if desc.is_empty() {
            "/plan".to_string()
        } else {
            format!("/plan {desc}")
        };

        let _ = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content(format!("📋 `{prompt_text}`")),
                ),
            )
            .await;

        let thread_id = cmd.channel_id.get().to_string();
        if let Err(e) = self.pool.get_or_create(&thread_id).await {
            let _ = cmd.channel_id.say(&ctx.http, format!("⚠️ {e}")).await;
            return;
        }

        use crate::acp::connection::ContentBlock;
        let result = self
            .pool
            .with_connection(&thread_id, |conn| {
                let pt = prompt_text.clone();
                Box::pin(async move {
                    let (mut rx, _) = conn.session_prompt(vec![ContentBlock::Text { text: pt }]).await?;
                    let mut reply = String::new();
                    while let Some(msg) = rx.recv().await {
                        if msg.id.is_some() {
                            if let Some(r) = &msg.result {
                                if let Some(arr) = r.get("content").and_then(|c| c.as_array()) {
                                    for b in arr {
                                        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
                                            reply.push_str(t);
                                        }
                                    }
                                }
                            }
                            break;
                        }
                        if let Some(params) = &msg.params {
                            if let Some(upd) = params.get("update") {
                                if upd.get("sessionUpdate").and_then(|v| v.as_str()) == Some("agent_message_chunk") {
                                    if let Some(t) = upd.get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str()) {
                                        reply.push_str(t);
                                    }
                                }
                            }
                        }
                    }
                    conn.prompt_done().await;
                    Ok(reply)
                })
            })
            .await;

        match result {
            Ok(r) if r.is_empty() => { let _ = cmd.channel_id.say(&ctx.http, "✅ Plan mode activated (no output)").await; }
            Ok(r) => {
                let truncated = if r.len() > 1900 { format!("{}…\n*(truncated)*", &r[..1900]) } else { r };
                let _ = cmd.channel_id.say(&ctx.http, &truncated).await;
            }
            Err(e) => { let _ = cmd.channel_id.say(&ctx.http, format!("⚠️ {e}")).await; }
        }
    }

    /// Handle /mcp — send /mcp to the agent as a prompt.
    async fn handle_mcp_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        let _ = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new().content("🔌 Querying MCP servers…"),
                ),
            )
            .await;

        let thread_id = cmd.channel_id.get().to_string();
        if let Err(e) = self.pool.get_or_create(&thread_id).await {
            let _ = cmd.channel_id.say(&ctx.http, format!("⚠️ {e}")).await;
            return;
        }

        use crate::acp::connection::ContentBlock;
        let result = self
            .pool
            .with_connection(&thread_id, |conn| {
                Box::pin(async move {
                    let (mut rx, _) = conn.session_prompt(vec![ContentBlock::Text { text: "/mcp".to_string() }]).await?;
                    let mut reply = String::new();
                    while let Some(msg) = rx.recv().await {
                        if msg.id.is_some() {
                            if let Some(r) = &msg.result {
                                if let Some(arr) = r.get("content").and_then(|c| c.as_array()) {
                                    for b in arr { if let Some(t) = b.get("text").and_then(|t| t.as_str()) { reply.push_str(t); } }
                                }
                            }
                            break;
                        }
                        if let Some(params) = &msg.params {
                            if let Some(upd) = params.get("update") {
                                if upd.get("sessionUpdate").and_then(|v| v.as_str()) == Some("agent_message_chunk") {
                                    if let Some(t) = upd.get("content").and_then(|c| c.get("text")).and_then(|t| t.as_str()) { reply.push_str(t); }
                                }
                            }
                        }
                    }
                    conn.prompt_done().await;
                    Ok(reply)
                })
            })
            .await;

        match result {
            Ok(r) if r.is_empty() => { let _ = cmd.channel_id.say(&ctx.http, "ℹ️ No MCP information returned.").await; }
            Ok(r) => {
                let truncated = if r.len() > 1900 { format!("{}…\n*(truncated)*", &r[..1900]) } else { r };
                let _ = cmd.channel_id.say(&ctx.http, &truncated).await;
            }
            Err(e) => { let _ = cmd.channel_id.say(&ctx.http, format!("⚠️ {e}")).await; }
        }
    }

    /// Handle /export — fetch recent messages from the current thread and send as text.
    async fn handle_export_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /export");
            return;
        }

        // Fetch up to 100 recent messages from this channel
        use serenity::builder::GetMessages;
        let messages = match cmd
            .channel_id
            .messages(&ctx.http, GetMessages::new().limit(100))
            .await
        {
            Ok(msgs) => msgs,
            Err(e) => {
                let _ = cmd
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new().content(format!("⚠️ Failed to fetch messages: {e}")),
                    )
                    .await;
                return;
            }
        };

        let mut lines: Vec<String> = messages
            .iter()
            .rev()
            .map(|m| {
                let ts = m.timestamp.to_string();
                let author = &m.author.name;
                let content = if m.content.len() > 500 {
                    format!("{}…", &m.content[..500])
                } else {
                    m.content.clone()
                };
                format!("[{ts}] {author}: {content}")
            })
            .collect();

        if lines.is_empty() {
            lines.push("(no messages)".to_string());
        }

        let export = lines.join("\n");
        let truncated = if export.len() > 1900 {
            format!("```\n{}…\n```\n*(truncated to 100 messages)*", &export[..1800])
        } else {
            format!("```\n{export}\n```")
        };

        let embed = CreateEmbed::new()
            .title("📄 /export")
            .description(truncated)
            .color(0x5865F2);

        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await;
    }

    /// Handle /doctor — diagnose agent connection health.
    async fn handle_doctor_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /doctor");
            return;
        }

        let thread_key = cmd.channel_id.get().to_string();
        let mut report = String::new();

        // 1. Check if a session exists for this thread
        let session_exists = self.pool.get_or_create(&thread_key).await.is_ok();
        report.push_str(&format!("**Session:** {}\n", if session_exists { "✅ active" } else { "❌ failed to create" }));

        if session_exists {
            // 2. Ping the agent
            let ping_result = self
                .pool
                .with_connection(&thread_key, |conn| {
                    Box::pin(async move { conn.session_ping().await })
                })
                .await;
            match ping_result {
                Ok(_) => report.push_str("**Ping:** ✅ responsive\n"),
                Err(e) => report.push_str(&format!("**Ping:** ⚠️ {e}\n")),
            }

            // 3. Session info
            let session_info = self
                .pool
                .with_connection(&thread_key, |conn| {
                    Box::pin(async move {
                        let sid = conn.acp_session_id.as_deref().unwrap_or("none").to_string();
                        let model = conn.current_model.clone();
                        let alive = conn.alive();
                        let elapsed_secs = conn.last_active.elapsed().as_secs();
                        let native_count = conn.native_commands.lock().await.len();
                        Ok(format!(
                            "**Session ID:** `{sid}`\n**Model:** `{model}`\n**Process:** {}\n**Last active:** {elapsed_secs}s ago\n**Native commands:** {native_count}\n",
                            if alive { "✅ alive" } else { "❌ dead" }
                        ))
                    })
                })
                .await;
            if let Ok(info) = session_info {
                report.push_str(&info);
            }
        }

        // 4. Pool info
        let models = self.pool.cached_models().await;
        let native_cmds = self.pool.cached_native_commands().await;
        report.push_str(&format!("**Cached models:** {}\n", models.len()));
        report.push_str(&format!("**Cached native cmds:** {}\n", native_cmds.len()));

        let embed = CreateEmbed::new()
            .title("🩺 /doctor")
            .description(report)
            .color(0x2ECC71);

        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await;
    }

    /// Handle /stats — detailed session statistics.
    async fn handle_stats_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /stats");
            return;
        }

        let thread_key = cmd.channel_id.get().to_string();
        let mut report = String::new();

        // Try to get usage from bridge
        let has_session = self.pool.get_or_create(&thread_key).await.is_ok();
        if has_session {
            let usage = self
                .pool
                .with_connection(&thread_key, |conn| {
                    Box::pin(async move { conn.session_get_usage().await })
                })
                .await;

            match usage {
                Ok(v) => {
                    // Session-level token stats (top-level or nested under session_usage)
                    let session = v.get("session_usage").unwrap_or(&v);
                    if let Some(input) = session.get("inputTokens").and_then(|n| n.as_u64()) {
                        report.push_str(&format!("**Input tokens:** {input}\n"));
                    }
                    if let Some(output) = session.get("outputTokens").and_then(|n| n.as_u64()) {
                        report.push_str(&format!("**Output tokens:** {output}\n"));
                    }
                    if let Some(total) = session.get("totalTokens").and_then(|n| n.as_u64()) {
                        report.push_str(&format!("**Total tokens:** {total}\n"));
                    }
                    if let Some(turns) = session.get("turns").and_then(|n| n.as_u64()) {
                        report.push_str(&format!("**Turns:** {turns}\n"));
                    }
                    if let Some(cost) = v.get("cost").or_else(|| v.get("cost_totals")).and_then(|n| n.as_f64()) {
                        report.push_str(&format!("**Estimated cost:** ${cost:.4}\n"));
                    }

                    // Account-level quota (from copilot-agent-acp bridge)
                    if let Some(aq) = v.get("account_quota") {
                        for key in ["premium_interactions", "chat", "completions"] {
                            if let Some(q) = aq.pointer(&format!("/quotaSnapshots/{key}")) {
                                let pct = q.get("remainingPercentage").and_then(|v| v.as_f64()).unwrap_or(0.0);
                                let used = q.get("usedRequests").and_then(|v| v.as_u64()).unwrap_or(0);
                                let total = q.get("entitlementRequests").and_then(|v| v.as_u64()).unwrap_or(0);
                                if total > 0 {
                                    let label = match key {
                                        "premium_interactions" => "🔥 Premium",
                                        "chat" => "💬 Chat",
                                        "completions" => "⚡ Completions",
                                        _ => key,
                                    };
                                    report.push_str(&format!("**{label}:** {pct:.1}% remaining ({used}/{total})\n"));
                                }
                            }
                        }
                    }

                    if report.is_empty() {
                        report.push_str(&format!("```json\n{}\n```", serde_json::to_string_pretty(&v).unwrap_or_default()));
                    }
                }
                Err(_) => {
                    report.push_str("_Usage stats not available from this backend._\n");
                }
            }

            // Session metadata
            let meta = self
                .pool
                .with_connection(&thread_key, |conn| {
                    Box::pin(async move {
                        let elapsed_secs = conn.last_active.elapsed().as_secs();
                        let model = conn.current_model.clone();
                        let alive = conn.alive();
                        Ok(format!(
                            "\n**Model:** `{model}`\n**Session age:** {elapsed_secs}s\n**Process alive:** {}\n",
                            if alive { "✅" } else { "❌" }
                        ))
                    })
                })
                .await;
            if let Ok(m) = meta {
                report.push_str(&m);
            }
        } else {
            report.push_str("No active session in this thread.\n");
        }

        // Native commands summary
        let native = self.pool.cached_native_commands().await;
        if !native.is_empty() {
            report.push_str(&format!("\n**Native commands:** {} available via `/native`\n", native.len()));
        }

        let embed = CreateEmbed::new()
            .title("📊 /stats")
            .description(report)
            .color(0x5865F2);

        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await;
    }

    /// Handle the actual /model command submission.
    async fn handle_model_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        // Allowlist: channel
        let channel_id = cmd.channel_id.get();
        let in_allowed_channel =
            self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id);

        let in_thread = if !in_allowed_channel {
            match cmd.channel_id.to_channel(&ctx.http).await {
                Ok(serenity::model::channel::Channel::Guild(gc)) => gc
                    .parent_id
                    .map_or(false, |pid| self.allowed_channels.contains(&pid.get())),
                _ => false,
            }
        } else {
            false
        };

        if !in_allowed_channel && !in_thread {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("⚠️ This channel is not allowlisted.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

        // Allowlist: user
        if !self.allowed_users.is_empty() && !self.allowed_users.contains(&cmd.user.id.get()) {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("🚫 You are not authorized to use this command.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

        // Extract the model option (None → list current)
        let arg = cmd.data.options.first().and_then(|o| match &o.value {
            CommandDataOptionValue::String(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                }
            }
            _ => None,
        });

        // Defer the response — session spawn can take up to ~10s on cold pool
        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /model response");
            return;
        }

        let thread_key = cmd.channel_id.get().to_string();
        if let Err(e) = self.pool.get_or_create(&thread_key).await {
            let _ = cmd
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new()
                        .content(format!("⚠️ Failed to start agent: {e}")),
                )
                .await;
            return;
        }

        let reply = self
            .pool
            .with_connection(&thread_key, |conn| {
                let arg = arg.clone();
                Box::pin(async move {
                    match arg {
                        None => {
                            if conn.available_models.is_empty() {
                                return Ok::<String, anyhow::Error>(
                                    "_(no models reported by agent — backend may not support model switching)_"
                                        .to_string(),
                                );
                            }
                            let mut out = String::from("**Available models:**\n");
                            for m in &conn.available_models {
                                let marker = if m.model_id == conn.current_model {
                                    "**▶**"
                                } else {
                                    "•"
                                };
                                out.push_str(&format!(
                                    "{} `{}` — {}\n",
                                    marker, m.model_id, m.name
                                ));
                            }
                            out.push_str(&format!("\nCurrent: `{}`", conn.current_model));
                            Ok(out)
                        }
                        Some(input) => match conn.resolve_model_alias(&input) {
                            Some(model_id) => match conn.session_set_model(&model_id).await {
                                Ok(()) => Ok(format!("✅ Switched to `{model_id}`")),
                                Err(e) => Ok(format!("⚠️ Failed to set model: {e}")),
                            },
                            None => Ok(format!("❌ Unknown model: `{input}`")),
                        },
                    }
                })
            })
            .await;

        let text = match reply {
            Ok(t) => t,
            Err(e) => format!("⚠️ {e}"),
        };
        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().content(text))
            .await;
    }

    /// Handle the `/usage` slash command: run all configured usage runners
    /// in parallel and display the results as an embed.
    async fn handle_usage_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        // Channel allowlist
        let channel_id = cmd.channel_id.get();
        let in_allowed_channel =
            self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id);

        let in_thread = if !in_allowed_channel {
            match cmd.channel_id.to_channel(&ctx.http).await {
                Ok(serenity::model::channel::Channel::Guild(gc)) => gc
                    .parent_id
                    .map_or(false, |pid| self.allowed_channels.contains(&pid.get())),
                _ => false,
            }
        } else {
            false
        };

        if !in_allowed_channel && !in_thread {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("⚠️ This channel is not allowlisted.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

        // User allowlist
        if !self.allowed_users.is_empty() && !self.allowed_users.contains(&cmd.user.id.get()) {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("🚫 You are not authorized to use this command.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

        let Some(usage_cfg) = self.usage_config.as_ref() else {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("⚠️ `/usage` is not configured. See `[usage]` in config.toml.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        };

        if !usage_cfg.enabled || usage_cfg.runners.is_empty() {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("⚠️ No usage runners configured.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

        // Defer — runners may take up to `timeout_secs` each
        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /usage response");
            return;
        }

        let results = crate::usage::run_all(usage_cfg).await;
        let mut embed = build_usage_embed(&results);

        // Append session-level token stats if a session exists in this thread
        let thread_key = cmd.channel_id.get().to_string();
        if self.pool.get_or_create(&thread_key).await.is_ok() {
            let session_info = self
                .pool
                .with_connection(&thread_key, |conn| {
                    Box::pin(async move {
                        let model = conn.current_model.clone();
                        let elapsed = conn.last_active.elapsed().as_secs();
                        let usage = conn.session_get_usage().await.ok();
                        let mut lines = vec![format!("**Model:** `{model}` • **Age:** {elapsed}s")];
                        if let Some(v) = usage {
                            let session = v.get("session_usage").unwrap_or(&v);
                            if let Some(input) = session.get("inputTokens").and_then(|n| n.as_u64()) {
                                if let Some(output) = session.get("outputTokens").and_then(|n| n.as_u64()) {
                                    let total = input + output;
                                    lines.push(format!("**Tokens:** {total} (↑{input} ↓{output})"));
                                }
                            }
                            if let Some(turns) = session.get("turns").and_then(|n| n.as_u64()) {
                                lines.push(format!("**Turns:** {turns}"));
                            }
                            if let Some(cost) = v.get("cost").or_else(|| v.get("cost_totals")).and_then(|n| n.as_f64()) {
                                lines.push(format!("**Cost:** ${cost:.4}"));
                            }
                            // Account quota from copilot-agent-acp bridge
                            if let Some(premium) = v.pointer("/account_quota/quotaSnapshots/premium_interactions") {
                                if let Some(pct) = premium.get("remainingPercentage").and_then(|v| v.as_f64()) {
                                    let used = premium.get("usedRequests").and_then(|v| v.as_u64()).unwrap_or(0);
                                    let total = premium.get("entitlementRequests").and_then(|v| v.as_u64()).unwrap_or(0);
                                    lines.push(format!("🔥 **Premium:** {pct:.1}% ({used}/{total})"));
                                }
                            }
                        }
                        Ok(lines.join("\n"))
                    })
                })
                .await;
            if let Ok(info) = session_info {
                embed = embed.field("📡 Current Session", &info, false);
            }
        }

        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await;
    }

    /// Generic handler for top-level Copilot slash commands.
    /// Looks up the Discord command name in the dispatch table and runs copilot-rpc.js.
    async fn handle_copilot_readonly(&self, ctx: &Context, cmd: &CommandInteraction) {
        let Some(rpc_sub) = copilot_readonly_to_rpc(&cmd.data.name) else {
            return;
        };
        let display_name = cmd.data.name.clone();

        // Channel allowlist
        let channel_id = cmd.channel_id.get();
        let in_allowed_channel =
            self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id);
        let in_thread = if !in_allowed_channel {
            match cmd.channel_id.to_channel(&ctx.http).await {
                Ok(serenity::model::channel::Channel::Guild(gc)) => gc
                    .parent_id
                    .map_or(false, |pid| self.allowed_channels.contains(&pid.get())),
                _ => false,
            }
        } else {
            false
        };
        if !in_allowed_channel && !in_thread {
            let _ = cmd.create_response(&ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("⚠️ This channel is not allowlisted.")
                        .ephemeral(true))).await;
            return;
        }
        if !self.allowed_users.is_empty() && !self.allowed_users.contains(&cmd.user.id.get()) {
            let _ = cmd.create_response(&ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new()
                        .content("🚫 You are not authorized to use this command.")
                        .ephemeral(true))).await;
            return;
        }

        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, cmd = %display_name, "failed to defer response");
            return;
        }

        let script = r"C:\Users\Administrator\openab\scripts\copilot-rpc.js";
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            tokio::process::Command::new("node")
                .arg(script)
                .arg(rpc_sub)
                .output(),
        )
        .await;

        let embed = match output {
            Ok(Ok(out)) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let json_line = stdout
                    .lines()
                    .rev()
                    .find(|l| l.trim().starts_with('{'))
                    .unwrap_or("")
                    .trim();
                build_copilot_embed(&display_name, rpc_sub, json_line)
            }
            Ok(Ok(out)) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                CreateEmbed::new()
                    .title(format!("⚠️ /{display_name}"))
                    .description(format!("exit {}: ```{}```", out.status, stderr.chars().take(500).collect::<String>()))
                    .color(0xED4245)
            }
            Ok(Err(e)) => CreateEmbed::new()
                .title(format!("⚠️ /{display_name}"))
                .description(format!("spawn failed: {e}"))
                .color(0xED4245),
            Err(_) => CreateEmbed::new()
                .title(format!("⏱️ /{display_name}"))
                .description("timeout after 45s")
                .color(0xED4245),
        };

        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await;
    }

    /// Handle /mode — set Copilot session mode via static choices.
    async fn handle_copilot_mode(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        let mode_id = cmd
            .data
            .options
            .first()
            .and_then(|o| match &o.value {
                CommandDataOptionValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /mode");
            return;
        }

        let embed = run_copilot_rpc_action("mode", "mode-set", &mode_id).await;
        let _ = cmd.edit_response(&ctx.http, EditInteractionResponse::new().embed(embed)).await;
    }

    /// Handle /reload <kind> — reload a Copilot config category via SDK RPC.
    async fn handle_copilot_reload(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        let kind = cmd
            .data
            .options
            .first()
            .and_then(|o| match &o.value {
                CommandDataOptionValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        let rpc_sub = COPILOT_RELOAD_KINDS
            .iter()
            .find(|(v, _, _)| *v == kind)
            .map(|(_, rpc, _)| *rpc);

        let Some(rpc_sub) = rpc_sub else {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content(format!("❌ Unknown reload kind: `{kind}`"))
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        };

        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /reload");
            return;
        }

        // reload commands take no arg, but run_copilot_rpc_action expects one.
        // Pass an empty string; copilot-rpc.js ignores args for reload subcommands.
        let script = r"C:\Users\Administrator\openab\scripts\copilot-rpc.js";
        let output = tokio::time::timeout(
            std::time::Duration::from_secs(45),
            tokio::process::Command::new("node")
                .arg(script)
                .arg(rpc_sub)
                .output(),
        )
        .await;

        let embed = match output {
            Ok(Ok(out)) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let json_line = stdout
                    .lines()
                    .rev()
                    .find(|l| l.trim().starts_with('{'))
                    .unwrap_or("")
                    .trim();
                let parsed: Result<serde_json::Value, _> = serde_json::from_str(json_line);
                match parsed {
                    Ok(v) if v.get("ok").and_then(|b| b.as_bool()) == Some(true) => CreateEmbed::new()
                        .title("✅ /reload")
                        .description(format!("Reloaded: **{kind}**"))
                        .color(0x2ECC71),
                    Ok(v) => CreateEmbed::new()
                        .title("⚠️ /reload")
                        .description(format!(
                            "```{}```",
                            v.get("error").and_then(|s| s.as_str()).unwrap_or("unknown error")
                        ))
                        .color(0xED4245),
                    Err(e) => CreateEmbed::new()
                        .title("⚠️ /reload")
                        .description(format!("invalid JSON: {e}"))
                        .color(0xED4245),
                }
            }
            Ok(Ok(out)) => CreateEmbed::new()
                .title("⚠️ /reload")
                .description(format!(
                    "exit {}: ```{}```",
                    out.status,
                    String::from_utf8_lossy(&out.stderr).chars().take(400).collect::<String>()
                ))
                .color(0xED4245),
            Ok(Err(e)) => CreateEmbed::new()
                .title("⚠️ /reload")
                .description(format!("spawn failed: {e}"))
                .color(0xED4245),
            Err(_) => CreateEmbed::new()
                .title("⏱️ /reload")
                .description("timeout after 45s")
                .color(0xED4245),
        };

        let _ = cmd.edit_response(&ctx.http, EditInteractionResponse::new().embed(embed)).await;
    }

    /// Handle /compact and /new-session — drop the current thread's AcpConnection.
    /// Both commands have the same effect (fresh session on next message) but
    /// are presented with different wording to match user mental models.
    async fn handle_reset_session(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }

        let cmd_name = cmd.data.name.clone();
        let thread_key = cmd.channel_id.get().to_string();

        // For /compact: try the bridge's real LLM compaction first (preserves
        // summarized context). Fall back to drop-session if the bridge doesn't
        // support _meta/compactSession.
        if cmd_name == "compact" {
            if let Err(e) = cmd.defer(&ctx.http).await {
                error!(error = %e, "failed to defer /compact");
                return;
            }
            // Ensure a session exists before trying to compact
            if let Err(e) = self.pool.get_or_create(&thread_key).await {
                let _ = cmd
                    .edit_response(
                        &ctx.http,
                        EditInteractionResponse::new()
                            .content(format!("⚠️ Failed to start agent: {e}")),
                    )
                    .await;
                return;
            }

            let compact_res = self
                .pool
                .with_connection(&thread_key, |conn| {
                    Box::pin(async move { conn.session_compact().await })
                })
                .await;

            let embed = match compact_res {
                Ok(v) => {
                    let removed = v
                        .get("tokens_removed")
                        .and_then(|n| n.as_u64())
                        .map(|n| format!("{n} tokens freed"))
                        .unwrap_or_else(|| "compacted".to_string());
                    CreateEmbed::new()
                        .title("✅ /compact")
                        .description(format!("LLM-compacted conversation history — **{removed}**\n_Context preserved as summary._"))
                        .color(0x2ECC71)
                }
                Err(e) => {
                    // Fall back to drop-session
                    let dropped = self.pool.drop_session(&thread_key).await;
                    let note = if dropped { "Session dropped (history cleared)." } else { "No active session." };
                    CreateEmbed::new()
                        .title("ℹ️ /compact (fallback)")
                        .description(format!("Bridge compaction unavailable: {e}\n\n{note}"))
                        .color(0x5865F2)
                }
            };

            let _ = cmd
                .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
                .await;
            return;
        }

        // /new-session: hard reset, drop the pool entry.
        let dropped = self.pool.drop_session(&thread_key).await;

        let (title, body, color) = if dropped {
            (
                format!("✅ /{cmd_name}"),
                "Session reset — your next message in this thread will start a fresh agent session.".to_string(),
                0x2ECC71,
            )
        } else {
            (
                format!("ℹ️ /{cmd_name}"),
                "No active session to reset. Your next message will start fresh anyway.".to_string(),
                0x5865F2,
            )
        };

        let embed = CreateEmbed::new().title(title).description(body).color(color);
        let _ = cmd
            .create_response(
                &ctx.http,
                CreateInteractionResponse::Message(
                    CreateInteractionResponseMessage::new().add_embed(embed),
                ),
            )
            .await;
    }

    /// Handle /tokens — ask the bridge for current session token usage via `_meta/getUsage`.
    /// Only works if the agent backend is the copilot-agent-acp bridge.
    async fn handle_tokens_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }

        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /tokens");
            return;
        }

        let thread_key = cmd.channel_id.get().to_string();
        // Ensure a session exists so the bridge has something to report on.
        if let Err(e) = self.pool.get_or_create(&thread_key).await {
            let _ = cmd
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new()
                        .content(format!("⚠️ Failed to start agent: {e}")),
                )
                .await;
            return;
        }

        let usage_result = self
            .pool
            .with_connection(&thread_key, |conn| {
                Box::pin(async move { conn.session_get_usage().await })
            })
            .await;

        let embed = match usage_result {
            Ok(v) => render_session_tokens(&v),
            Err(e) => CreateEmbed::new()
                .title("⚠️ /tokens")
                .description(format!(
                    "This backend does not support `_meta/getUsage`.\n\nError: ```{e}```\n\n\
                     To enable: change `config-copilot.toml` agent command to `copilot-agent-acp`."
                ))
                .color(0xED4245),
        };

        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await;
    }

    /// Handle /permissions — show recent tool permission audit log.
    async fn handle_permissions_command(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, "failed to defer /permissions");
            return;
        }

        let thread_key = cmd.channel_id.get().to_string();
        if let Err(e) = self.pool.get_or_create(&thread_key).await {
            let _ = cmd
                .edit_response(
                    &ctx.http,
                    EditInteractionResponse::new()
                        .content(format!("⚠️ Failed to start agent: {e}")),
                )
                .await;
            return;
        }

        let result = self
            .pool
            .with_connection(&thread_key, |conn| {
                Box::pin(async move { conn.session_get_recent_permissions().await })
            })
            .await;

        let embed = match result {
            Ok(v) => render_permissions(&v),
            Err(e) => CreateEmbed::new()
                .title("⚠️ /permissions")
                .description(format!(
                    "Backend does not support `_meta/getRecentPermissions`.\n```{e}```"
                ))
                .color(0xED4245),
        };

        let _ = cmd
            .edit_response(&ctx.http, EditInteractionResponse::new().embed(embed))
            .await;
    }

    /// Handle interactive commands: /agent, /skill-on, /skill-off, /mcp-on, /mcp-off, /ext-on, /ext-off.
    async fn handle_copilot_interactive(&self, ctx: &Context, cmd: &CommandInteraction) {
        if !self.copilot_guard_ok(ctx, cmd).await {
            return;
        }
        let Some((_, action_rpc, _, _)) = copilot_interactive_spec(&cmd.data.name) else {
            return;
        };
        let cmd_name = cmd.data.name.clone();
        let name_arg = cmd
            .data
            .options
            .first()
            .and_then(|o| match &o.value {
                CommandDataOptionValue::String(s) => Some(s.clone()),
                _ => None,
            })
            .unwrap_or_default();

        if name_arg.is_empty() {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("⚠️ Missing name argument.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return;
        }

        if let Err(e) = cmd.defer(&ctx.http).await {
            error!(error = %e, cmd = %cmd_name, "failed to defer interactive");
            return;
        }

        let embed = run_copilot_rpc_action(&cmd_name, action_rpc, &name_arg).await;
        let _ = cmd.edit_response(&ctx.http, EditInteractionResponse::new().embed(embed)).await;
    }

    /// Autocomplete handler for interactive commands: fetches the list RPC
    /// and filters by the user's partial input. Falls back to empty on error.
    async fn handle_copilot_interactive_autocomplete(
        &self,
        ctx: &Context,
        ac: &CommandInteraction,
    ) {
        let Some((list_rpc, _, data_key, name_key)) = copilot_interactive_spec(&ac.data.name) else {
            return;
        };

        let partial = ac
            .data
            .options
            .first()
            .and_then(|o| match &o.value {
                CommandDataOptionValue::Autocomplete { value, .. } => Some(value.as_str()),
                CommandDataOptionValue::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("")
            .to_lowercase();

        // Read from the background-refreshed list cache — instant, no Node
        // subprocess spawn. The cache is populated by `refresh_copilot_list_cache`
        // in main.rs every 5 minutes.
        let _ = data_key;
        let _ = name_key;
        let mut choices: Vec<AutocompleteChoice> = Vec::new();
        let cache = self.copilot_list_cache.read().await;
        if let Some(names) = cache.get(list_rpc) {
            for name in names {
                if partial.is_empty() || name.to_lowercase().contains(&partial) {
                    let label = name.chars().take(100).collect::<String>();
                    choices.push(AutocompleteChoice::new(label.clone(), label));
                    if choices.len() >= 25 {
                        break;
                    }
                }
            }
        }
        drop(cache);

        let response = CreateInteractionResponse::Autocomplete(
            CreateAutocompleteResponse::new().set_choices(choices),
        );
        if let Err(e) = ac.create_response(&ctx.http, response).await {
            warn!(error = %e, "failed to send interactive autocomplete");
        }
    }

    /// Shared allowlist guard for all Copilot slash commands.
    /// Returns true if the interaction may proceed; sends an ephemeral error otherwise.
    async fn copilot_guard_ok(&self, ctx: &Context, cmd: &CommandInteraction) -> bool {
        let channel_id = cmd.channel_id.get();
        let in_allowed_channel =
            self.allowed_channels.is_empty() || self.allowed_channels.contains(&channel_id);
        let in_thread = if !in_allowed_channel {
            match cmd.channel_id.to_channel(&ctx.http).await {
                Ok(serenity::model::channel::Channel::Guild(gc)) => gc
                    .parent_id
                    .map_or(false, |pid| self.allowed_channels.contains(&pid.get())),
                _ => false,
            }
        } else {
            false
        };
        if !in_allowed_channel && !in_thread {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("⚠️ This channel is not allowlisted.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return false;
        }
        if !self.allowed_users.is_empty() && !self.allowed_users.contains(&cmd.user.id.get()) {
            let _ = cmd
                .create_response(
                    &ctx.http,
                    CreateInteractionResponse::Message(
                        CreateInteractionResponseMessage::new()
                            .content("🚫 You are not authorized to use this command.")
                            .ephemeral(true),
                    ),
                )
                .await;
            return false;
        }
        true
    }
}

/// Run a copilot-rpc.js action that takes one argument, return a rendered embed.
async fn run_copilot_rpc_action(display_name: &str, rpc_sub: &str, arg: &str) -> CreateEmbed {
    let script = r"C:\Users\Administrator\openab\scripts\copilot-rpc.js";
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        tokio::process::Command::new("node")
            .arg(script)
            .arg(rpc_sub)
            .arg(arg)
            .output(),
    )
    .await;

    match output {
        Ok(Ok(out)) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let json_line = stdout
                .lines()
                .rev()
                .find(|l| l.trim().starts_with('{'))
                .unwrap_or("")
                .trim();
            let parsed: Result<serde_json::Value, _> = serde_json::from_str(json_line);
            match parsed {
                Ok(v) if v.get("ok").and_then(|b| b.as_bool()) == Some(true) => {
                    CreateEmbed::new()
                        .title(format!("✅ /{display_name}"))
                        .description(format!("Applied: `{arg}`"))
                        .color(0x2ECC71)
                }
                Ok(v) => {
                    let err = v.get("error").and_then(|s| s.as_str()).unwrap_or("unknown error");
                    CreateEmbed::new()
                        .title(format!("⚠️ /{display_name}"))
                        .description(format!("```{err}```"))
                        .color(0xED4245)
                }
                Err(e) => CreateEmbed::new()
                    .title(format!("⚠️ /{display_name}"))
                    .description(format!("invalid JSON: {e}"))
                    .color(0xED4245),
            }
        }
        Ok(Ok(out)) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            CreateEmbed::new()
                .title(format!("⚠️ /{display_name}"))
                .description(format!("exit {}: ```{}```", out.status, stderr.chars().take(400).collect::<String>()))
                .color(0xED4245)
        }
        Ok(Err(e)) => CreateEmbed::new()
            .title(format!("⚠️ /{display_name}"))
            .description(format!("spawn failed: {e}"))
            .color(0xED4245),
        Err(_) => CreateEmbed::new()
            .title(format!("⏱️ /{display_name}"))
            .description("timeout after 45s")
            .color(0xED4245),
    }
}

/// Render an embed from the Node helper's JSON output.
///
/// `display_name` = the Discord command the user invoked (for error/title display)
/// `rpc_sub` = the subcommand passed to copilot-rpc.js (determines renderer)
fn build_copilot_embed(display_name: &str, rpc_sub: &str, json_line: &str) -> CreateEmbed {
    let parsed: Result<serde_json::Value, _> = serde_json::from_str(json_line);
    let v = match parsed {
        Ok(v) => v,
        Err(e) => {
            return CreateEmbed::new()
                .title(format!("⚠️ /{display_name}"))
                .description(format!(
                    "invalid JSON from helper: {e}\n\n```{}```",
                    json_line.chars().take(300).collect::<String>()
                ))
                .color(0xED4245);
        }
    };

    if v.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        let err = v.get("error").and_then(|s| s.as_str()).unwrap_or("unknown");
        return CreateEmbed::new()
            .title(format!("⚠️ /{display_name}"))
            .description(format!("```{err}```"))
            .color(0xED4245);
    }

    let data = v.get("data").cloned().unwrap_or(serde_json::Value::Null);
    let title = format!("⚡ /{display_name}");

    match rpc_sub {
        "usage" => render_usage(&data),
        "status" => render_status(&data),
        "models" => render_list(&title, &data, "models", "id"),
        "agents" => render_list(&title, &data, "agents", "name"),
        "skills" => render_list(&title, &data, "skills", "name"),
        "plugins" => render_list(&title, &data, "plugins", "name"),
        "extensions" => render_list(&title, &data, "extensions", "name"),
        "mcp-list" => render_list(&title, &data, "servers", "name"),
        "files" => render_list(&title, &data, "files", "path"),
        _ => {
            let pretty = serde_json::to_string_pretty(&data).unwrap_or_else(|_| format!("{data:?}"));
            let body = pretty.chars().take(3800).collect::<String>();
            CreateEmbed::new()
                .title(title)
                .description(format!("```json\n{body}\n```"))
                .color(0x24292F)
        }
    }
}

/// Render session token usage from the bridge's `_meta/getUsage` response.
/// Render the bridge's `_meta/getRecentPermissions` response as an embed.
fn render_permissions(data: &serde_json::Value) -> CreateEmbed {
    let perms = data.get("permissions").and_then(|v| v.as_array());
    let count = data.get("count").and_then(|v| v.as_u64()).unwrap_or(0);

    let mut embed = CreateEmbed::new()
        .title(format!("🔐 Recent Tool Permissions ({count})"))
        .color(0x24292F);

    let Some(arr) = perms else {
        return embed.description("_(no audit data)_");
    };

    if arr.is_empty() {
        return embed.description("_(no permissions requested yet — send a message that triggers a tool)_");
    }

    // Show the most recent 10 entries (bridge stores 50, embed space is limited)
    let lines: Vec<String> = arr
        .iter()
        .rev()
        .take(10)
        .enumerate()
        .map(|(i, p)| {
            let kind = p.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
            let cmd = p.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let intention = p.get("intention").and_then(|v| v.as_str()).unwrap_or("");
            let ts = p.get("ts").and_then(|v| v.as_str()).unwrap_or("");
            let cmd_short: String = cmd.chars().take(120).collect();
            let intention_short: String = intention.chars().take(80).collect();
            format!(
                "`{:>2}.` [{kind}] `{}`{}\n    {}",
                i + 1,
                cmd_short,
                if cmd.len() > 120 { "…" } else { "" },
                if intention.is_empty() {
                    format!("_{ts}_")
                } else {
                    format!("{intention_short} · _{ts}_")
                }
            )
        })
        .collect();

    embed.description(lines.join("\n\n"))
}

fn render_session_tokens(data: &serde_json::Value) -> CreateEmbed {
    let session_usage = data.get("session_usage");
    let account_quota = data.get("account_quota");
    let cost_totals = data.get("cost_totals");

    let mut embed = CreateEmbed::new()
        .title("🧮 Session Token Usage")
        .color(0x24292F);

    // Session-level token info
    if let Some(su) = session_usage.filter(|v| !v.is_null()) {
        let token_limit = su.get("tokenLimit").and_then(|v| v.as_u64()).unwrap_or(0);
        let current = su.get("currentTokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let system_t = su.get("systemTokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let conv_t = su.get("conversationTokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let tools_t = su
            .get("toolDefinitionsTokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let msgs = su.get("messagesLength").and_then(|v| v.as_u64()).unwrap_or(0);

        let pct = if token_limit > 0 {
            (current as f64 / token_limit as f64 * 100.0).round() as i64
        } else {
            0
        };
        let bar = progress_bar(pct as u32);

        let body = format!(
            "{bar} `{pct}%`\n\
             **Context:** {:.1}k / {:.0}k\n\
             • System: {:.1}k\n\
             • Tools defs: {:.1}k\n\
             • Conversation: {:.1}k\n\
             • Messages: {msgs}",
            current as f64 / 1000.0,
            token_limit as f64 / 1000.0,
            system_t as f64 / 1000.0,
            tools_t as f64 / 1000.0,
            conv_t as f64 / 1000.0,
        );
        embed = embed.field("📊 Current thread", body, false);
    } else {
        embed = embed.field(
            "📊 Current thread",
            "_(no usage captured yet — send a message first)_",
            false,
        );
    }

    // Cost totals (if bridge has been tracking assistant.usage events)
    if let Some(ct) = cost_totals.filter(|v| !v.is_null()) {
        let turns = ct.get("turns").and_then(|v| v.as_u64()).unwrap_or(0);
        let in_t = ct.get("inputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let out_t = ct.get("outputTokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_r = ct.get("cacheReadTokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cost = ct.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
        let model = ct.get("lastModel").and_then(|v| v.as_str()).unwrap_or("?");
        let body = format!(
            "**{turns}** turns · model: `{model}`\n\
             Input: {:.1}k · Output: {:.1}k · Cached: {:.1}k\n\
             💰 **{cost:.2}** premium requests",
            in_t as f64 / 1000.0,
            out_t as f64 / 1000.0,
            cache_r as f64 / 1000.0,
        );
        embed = embed.field("💸 This session", body, false);
    }

    // Account-level quota (if included)
    if let Some(aq) = account_quota.filter(|v| !v.is_null()) {
        if let Some(premium) = aq.pointer("/quotaSnapshots/premium_interactions") {
            let pct = premium
                .get("remainingPercentage")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let used = premium.get("usedRequests").and_then(|v| v.as_u64()).unwrap_or(0);
            let entitled = premium
                .get("entitlementRequests")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            let bar = progress_bar(pct as u32);
            embed = embed.field(
                "🔥 Premium monthly",
                format!("{bar} `{:>3}%`\n**{used}** / {entitled} used", pct.round() as i64),
                false,
            );
        }
    }

    embed
}

fn render_usage(data: &serde_json::Value) -> CreateEmbed {
    let snap = data.get("quotaSnapshots");
    let mut embed = CreateEmbed::new()
        .title("⚡ Copilot Usage Quota")
        .color(0x24292F);

    for key in ["premium_interactions", "chat", "completions"] {
        if let Some(q) = snap.and_then(|s| s.get(key)) {
            let pct = q.get("remainingPercentage").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let used = q.get("usedRequests").and_then(|v| v.as_u64()).unwrap_or(0);
            let entitled = q.get("entitlementRequests").and_then(|v| v.as_u64()).unwrap_or(0);
            let reset = q.get("resetDate").and_then(|v| v.as_str()).unwrap_or("?");
            let bar = progress_bar(pct as u32);
            let pretty_key = match key {
                "premium_interactions" => "🔥 Premium interactions",
                "chat" => "💬 Chat",
                "completions" => "✍️ Completions",
                _ => key,
            };
            let body = format!(
                "{bar} `{:>3}%`\nUsed: **{used}** / {entitled}\nResets: {reset}",
                pct.round() as i64
            );
            embed = embed.field(pretty_key, body, false);
        }
    }
    embed
}

fn render_status(data: &serde_json::Value) -> CreateEmbed {
    let cli_ver = data.pointer("/cli/version").and_then(|v| v.as_str()).unwrap_or("?");
    let proto_ver = data.pointer("/cli/protocolVersion").and_then(|v| v.as_u64()).unwrap_or(0);
    let auth_user = data.pointer("/auth/login").and_then(|v| v.as_str()).unwrap_or("?");
    let auth_type = data.pointer("/auth/authType").and_then(|v| v.as_str()).unwrap_or("?");
    let auth_ok = data.pointer("/auth/isAuthenticated").and_then(|v| v.as_bool()).unwrap_or(false);
    let model_count = data.pointer("/model_count").and_then(|v| v.as_u64()).unwrap_or(0);

    CreateEmbed::new()
        .title("⚡ Copilot Status")
        .color(0x24292F)
        .field("CLI", format!("v{cli_ver} (protocol {proto_ver})"), true)
        .field("Auth", if auth_ok { format!("✅ {auth_user} ({auth_type})") } else { "❌ not authenticated".into() }, true)
        .field("Models", format!("{model_count} available"), true)
}

fn render_list(title: &str, data: &serde_json::Value, array_key: &str, name_key: &str) -> CreateEmbed {
    let arr = data.get(array_key).and_then(|v| v.as_array());
    let count = arr.map(|a| a.len()).unwrap_or(0);
    let items: Vec<String> = arr
        .map(|a| a.iter().take(25).enumerate().map(|(i, item)| {
            let name = item.get(name_key).and_then(|v| v.as_str()).unwrap_or("?");
            format!("`{:>2}.` {name}", i + 1)
        }).collect())
        .unwrap_or_default();
    let body = if items.is_empty() {
        "_(empty)_".to_string()
    } else {
        let mut s = items.join("\n");
        if count > items.len() {
            s.push_str(&format!("\n… and {} more", count - items.len()));
        }
        s
    };
    CreateEmbed::new()
        .title(format!("{title} ({count})"))
        .description(body)
        .color(0x24292F)
}

fn progress_bar(pct: u32) -> String {
    let pct = pct.min(100);
    let filled = ((pct as f64 / 10.0).round() as usize).min(10);
    let empty = 10 - filled;
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

/// Compose an embed from usage runner results. Each runner becomes one field.
fn build_usage_embed(results: &[crate::usage::RunnerResult]) -> CreateEmbed {
    let mut embed = CreateEmbed::new()
        .title("📊 Agent Usage Report")
        .color(0x5865F2);

    if results.is_empty() {
        embed = embed.description("_(no runners)_");
        return embed;
    }

    // Pick the first successful runner's color for the embed bar
    if let Some(crate::usage::RunnerResult::Ok { color, .. }) =
        results.iter().find(|r| matches!(r, crate::usage::RunnerResult::Ok { .. }))
    {
        embed = embed.color(*color);
    }

    for r in results {
        match r {
            crate::usage::RunnerResult::Ok { label, rendered, .. } => {
                let body = rendered.trim();
                let body = if body.is_empty() { "_(empty)_" } else { body };
                embed = embed.field(label, body, false);
            }
            crate::usage::RunnerResult::Err { label, reason, .. } => {
                embed = embed.field(label, format!("⚠️ {reason}"), false);
            }
        }
    }

    embed
}

/// Maximum dimension (width or height) for resized images.
/// Matches OpenClaw's DEFAULT_IMAGE_MAX_DIMENSION_PX.
const IMAGE_MAX_DIMENSION_PX: u32 = 1200;

/// JPEG quality for compressed output (OpenClaw uses progressive 85→35;
/// we start at 75 which is a good balance of quality vs size).
const IMAGE_JPEG_QUALITY: u8 = 75;

/// Download a Discord image attachment, resize/compress it, then base64-encode
/// as an ACP image content block.
///
/// Large images are resized so the longest side is at most 1200px and
/// re-encoded as JPEG at quality 75. This keeps the base64 payload well
/// under typical JSON-RPC transport limits (~200-400KB after encoding).
async fn download_and_encode_image(attachment: &serenity::model::channel::Attachment) -> Option<ContentBlock> {
    const MAX_SIZE: u64 = 10 * 1024 * 1024; // 10 MB

    let url = &attachment.url;
    if url.is_empty() {
        return None;
    }

    // Determine media type — prefer content-type header, fallback to extension
    let media_type = attachment
        .content_type
        .as_deref()
        .or_else(|| {
            attachment
                .filename
                .rsplit('.')
                .next()
                .and_then(|ext| match ext.to_lowercase().as_str() {
                    "png" => Some("image/png"),
                    "jpg" | "jpeg" => Some("image/jpeg"),
                    "gif" => Some("image/gif"),
                    "webp" => Some("image/webp"),
                    _ => None,
                })
        });

    let Some(mime) = media_type else {
        debug!(filename = %attachment.filename, "skipping non-image attachment");
        return None;
    };
    let mime = mime.split(';').next().unwrap_or(mime).trim();
    if !mime.starts_with("image/") {
        debug!(filename = %attachment.filename, mime = %mime, "skipping non-image attachment");
        return None;
    }

    if u64::from(attachment.size) > MAX_SIZE {
        error!(filename = %attachment.filename, size = attachment.size, "image exceeds 10MB limit");
        return None;
    }

    let response = match HTTP_CLIENT.get(url).send().await {
        Ok(resp) => resp,
        Err(e) => { error!(url = %url, error = %e, "download failed"); return None; }
    };
    if !response.status().is_success() {
        error!(url = %url, status = %response.status(), "HTTP error downloading image");
        return None;
    }
    let bytes = match response.bytes().await {
        Ok(b) => b,
        Err(e) => { error!(url = %url, error = %e, "read failed"); return None; }
    };

    // Defense-in-depth: verify actual download size
    if bytes.len() as u64 > MAX_SIZE {
        error!(filename = %attachment.filename, size = bytes.len(), "downloaded image exceeds limit");
        return None;
    }

    // Resize and compress
    let (output_bytes, output_mime) = match resize_and_compress(&bytes) {
        Ok(result) => result,
        Err(e) => {
            // Fallback: use original bytes but reject if too large for transport
            if bytes.len() > 1024 * 1024 {
                error!(filename = %attachment.filename, error = %e, size = bytes.len(), "resize failed and original too large, skipping");
                return None;
            }
            debug!(filename = %attachment.filename, error = %e, "resize failed, using original");
            (bytes.to_vec(), mime.to_string())
        }
    };

    debug!(
        filename = %attachment.filename,
        original_size = bytes.len(),
        compressed_size = output_bytes.len(),
        "image processed"
    );

    let encoded = BASE64.encode(&output_bytes);
    Some(ContentBlock::Image {
        media_type: output_mime,
        data: encoded,
    })
}

/// Resize image so longest side ≤ IMAGE_MAX_DIMENSION_PX, then encode as JPEG.
/// Returns (compressed_bytes, mime_type). GIFs are passed through unchanged
/// to preserve animation.
fn resize_and_compress(raw: &[u8]) -> Result<(Vec<u8>, String), image::ImageError> {
    let reader = ImageReader::new(Cursor::new(raw))
        .with_guessed_format()?;

    let format = reader.format();

    // Pass through GIFs unchanged to preserve animation
    if format == Some(image::ImageFormat::Gif) {
        return Ok((raw.to_vec(), "image/gif".to_string()));
    }

    let img = reader.decode()?;
    let (w, h) = (img.width(), img.height());

    // Resize preserving aspect ratio: scale so longest side = 1200px
    let img = if w > IMAGE_MAX_DIMENSION_PX || h > IMAGE_MAX_DIMENSION_PX {
        let max_side = std::cmp::max(w, h);
        let ratio = f64::from(IMAGE_MAX_DIMENSION_PX) / f64::from(max_side);
        let new_w = (f64::from(w) * ratio) as u32;
        let new_h = (f64::from(h) * ratio) as u32;
        img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };

    // Encode as JPEG
    let mut buf = Cursor::new(Vec::new());
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, IMAGE_JPEG_QUALITY);
    img.write_with_encoder(encoder)?;

    Ok((buf.into_inner(), "image/jpeg".to_string()))
}

async fn edit(ctx: &Context, ch: ChannelId, msg_id: MessageId, content: &str) -> serenity::Result<Message> {
    ch.edit_message(&ctx.http, msg_id, serenity::builder::EditMessage::new().content(content)).await
}

async fn stream_prompt(
    pool: &SessionPool,
    thread_key: &str,
    content_blocks: Vec<ContentBlock>,
    ctx: &Context,
    channel: ChannelId,
    msg_id: MessageId,
    reactions: Arc<StatusReactionController>,
) -> anyhow::Result<()> {
    let reactions = reactions.clone();

    pool.with_connection(thread_key, |conn| {
        let content_blocks = content_blocks.clone();
        let ctx = ctx.clone();
        let reactions = reactions.clone();
        Box::pin(async move {
            let reset = conn.session_reset;
            conn.session_reset = false;

            let (mut rx, _): (_, _) = conn.session_prompt(content_blocks).await?;
            reactions.set_thinking().await;

            let initial = if reset {
                "⚠️ _Session expired, starting fresh..._\n\n...".to_string()
            } else {
                "...".to_string()
            };
            let (buf_tx, buf_rx) = watch::channel(initial);

            let mut text_buf = String::new();
            // Tool calls indexed by toolCallId. Vec preserves first-seen
            // order. We store id + title + state separately so a ToolDone
            // event that arrives without a refreshed title (claude-agent-acp's
            // update events don't always re-send the title field) can still
            // reuse the title we already learned from a prior
            // tool_call_update — only the icon flips 🔧 → ✅ / ❌. Rendering
            // happens on the fly in compose_display().
            let mut tool_lines: Vec<ToolEntry> = Vec::new();
            let current_msg_id = msg_id;

            if reset {
                text_buf.push_str("⚠️ _Session expired, starting fresh..._\n\n");
            }

            // Spawn edit-streaming task — only edits the single message, never sends new ones.
            // Long content is truncated during streaming; final multi-message split happens after.
            let edit_handle = {
                let ctx = ctx.clone();
                let mut buf_rx = buf_rx.clone();
                tokio::spawn(async move {
                    let mut last_content = String::new();
                    loop {
                        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
                        if buf_rx.has_changed().unwrap_or(false) {
                            let content = buf_rx.borrow_and_update().clone();
                            if content != last_content {
                                let display = if content.chars().count() > 1900 {
                                    let truncated = format::truncate_chars(&content, 1900);
                                    format!("{truncated}…")
                                } else {
                                    content.clone()
                                };
                                let _ = edit(&ctx, channel, msg_id, &display).await;
                                last_content = content;
                            }
                        }
                        if buf_rx.has_changed().is_err() {
                            break;
                        }
                    }
                })
            };

            // Process ACP notifications
            let mut got_first_text = false;
            let mut response_error: Option<String> = None;
            while let Some(notification) = rx.recv().await {
                if notification.id.is_some() {
                    // Capture error from ACP response to display in Discord
                    if let Some(ref err) = notification.error {
                        response_error = Some(format_coded_error(err.code, &err.message));
                    }
                    break;
                }

                if let Some(event) = classify_notification(&notification) {
                    match event {
                        AcpEvent::Text(t) => {
                            if !got_first_text {
                                got_first_text = true;
                                // Reaction: back to thinking after tools
                            }
                            text_buf.push_str(&t);
                            let _ = buf_tx.send(compose_display(&tool_lines, &text_buf));
                        }
                        AcpEvent::Thinking => {
                            reactions.set_thinking().await;
                        }
                        AcpEvent::ToolStart { id, title } if !title.is_empty() => {
                            reactions.set_tool(&title).await;
                            let title = sanitize_title(&title);
                            // Dedupe by toolCallId: replace if we've already
                            // seen this id, otherwise append a new entry.
                            // claude-agent-acp emits a placeholder title
                            // ("Terminal", "Edit", etc.) on the first event
                            // and refines it via tool_call_update; without
                            // dedup the placeholder and refined version
                            // appear as two separate orphaned lines.
                            if let Some(slot) = tool_lines.iter_mut().find(|e| e.id == id) {
                                slot.title = title;
                                slot.state = ToolState::Running;
                            } else {
                                tool_lines.push(ToolEntry {
                                    id,
                                    title,
                                    state: ToolState::Running,
                                });
                            }
                            let _ = buf_tx.send(compose_display(&tool_lines, &text_buf));
                        }
                        AcpEvent::ToolDone { id, title, status } => {
                            reactions.set_thinking().await;
                            let new_state = if status == "completed" {
                                ToolState::Completed
                            } else {
                                ToolState::Failed
                            };
                            // Find by id (the title is unreliable — substring
                            // match against the placeholder "Terminal" would
                            // never find the refined entry). Preserve the
                            // existing title if the Done event omits it.
                            if let Some(slot) = tool_lines.iter_mut().find(|e| e.id == id) {
                                if !title.is_empty() {
                                    slot.title = sanitize_title(&title);
                                }
                                slot.state = new_state;
                            } else if !title.is_empty() {
                                // Done arrived without a prior Start (rare
                                // race) — record it so we still show
                                // something.
                                tool_lines.push(ToolEntry {
                                    id,
                                    title: sanitize_title(&title),
                                    state: new_state,
                                });
                            }
                            let _ = buf_tx.send(compose_display(&tool_lines, &text_buf));
                        }
                        _ => {}
                    }
                }
            }

            conn.prompt_done().await;
            drop(buf_tx);
            let _ = edit_handle.await;

            // Final edit
            let final_content = compose_display(&tool_lines, &text_buf);
            // If ACP returned both an error and partial text, show both.
            // This can happen when the agent started producing content before hitting an error
            // (e.g. context length limit, rate limit mid-stream). Showing both gives users
            // full context rather than hiding the partial response.
            let final_content = if final_content.is_empty() {
                if let Some(err) = response_error {
                    format!("⚠️ {}", err)
                } else {
                    "_(no response)_".to_string()
                }
            } else if let Some(err) = response_error {
                format!("⚠️ {}\n\n{}", err, final_content)
            } else {
                final_content
            };

            let chunks = format::split_message(&final_content, 2000);
            for (i, chunk) in chunks.iter().enumerate() {
                if i == 0 {
                    let _ = edit(&ctx, channel, current_msg_id, chunk).await;
                } else {
                    let _ = channel.say(&ctx.http, chunk).await;
                }
            }

            Ok(())
        })
    })
    .await
}

/// Flatten a tool-call title into a single line that's safe to render
/// inside Discord inline-code spans. Discord renders single-backtick
/// code on a single line only, so multi-line shell commands (heredocs,
/// `&&`-chained commands split across lines) appear truncated; we
/// collapse newlines to ` ; ` and rewrite embedded backticks so they
/// don't break the wrapping span.
fn sanitize_title(title: &str) -> String {
    title.replace('\r', "").replace('\n', " ; ").replace('`', "'")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolState {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone)]
struct ToolEntry {
    id: String,
    title: String,
    state: ToolState,
}

impl ToolEntry {
    fn render(&self) -> String {
        let icon = match self.state {
            ToolState::Running => "🔧",
            ToolState::Completed => "✅",
            ToolState::Failed => "❌",
        };
        let suffix = if self.state == ToolState::Running { "..." } else { "" };
        format!("{icon} `{}`{}", self.title, suffix)
    }
}

fn compose_display(tool_lines: &[ToolEntry], text: &str) -> String {
    let mut out = String::new();
    if !tool_lines.is_empty() {
        for entry in tool_lines {
            out.push_str(&entry.render());
            out.push('\n');
        }
        out.push('\n');
    }
    out.push_str(text.trim_end());
    out
}

static MENTION_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"<@[!&]?\d+>").unwrap()
});

fn strip_mention(content: &str) -> String {
    MENTION_RE.replace_all(content, "").trim().to_string()
}

fn shorten_thread_name(prompt: &str) -> String {
    // Shorten GitHub URLs: https://github.com/owner/repo/issues/123 → owner/repo#123
    let re = regex::Regex::new(r"https?://github\.com/([^/]+/[^/]+)/(issues|pull)/(\d+)").unwrap();
    let shortened = re.replace_all(prompt, "$1#$3");
    let name: String = shortened.chars().take(40).collect();
    if name.len() < shortened.len() {
        format!("{name}...")
    } else {
        name
    }
}

async fn get_or_create_thread(ctx: &Context, msg: &Message, prompt: &str) -> anyhow::Result<u64> {
    let channel = msg.channel_id.to_channel(&ctx.http).await?;
    if let serenity::model::channel::Channel::Guild(ref gc) = channel {
        if gc.thread_metadata.is_some() {
            return Ok(msg.channel_id.get());
        }
    }

    let thread_name = shorten_thread_name(prompt);

    let thread = msg
        .channel_id
        .create_thread_from_message(
            &ctx.http,
            msg.id,
            serenity::builder::CreateThread::new(thread_name)
                .auto_archive_duration(serenity::model::channel::AutoArchiveDuration::OneDay),
        )
        .await?;

    Ok(thread.id.get())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let img = image::RgbImage::new(width, height);
        let mut buf = Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png).unwrap();
        buf.into_inner()
    }

    #[test]
    fn large_image_resized_to_max_dimension() {
        let png = make_png(3000, 2000);
        let (compressed, mime) = resize_and_compress(&png).unwrap();

        assert_eq!(mime, "image/jpeg");
        let result = image::load_from_memory(&compressed).unwrap();
        assert!(result.width() <= IMAGE_MAX_DIMENSION_PX);
        assert!(result.height() <= IMAGE_MAX_DIMENSION_PX);
    }

    #[test]
    fn small_image_keeps_original_dimensions() {
        let png = make_png(800, 600);
        let (compressed, mime) = resize_and_compress(&png).unwrap();

        assert_eq!(mime, "image/jpeg");
        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 800);
        assert_eq!(result.height(), 600);
    }

    #[test]
    fn landscape_image_respects_aspect_ratio() {
        let png = make_png(4000, 2000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 1200);
        assert_eq!(result.height(), 600);
    }

    #[test]
    fn portrait_image_respects_aspect_ratio() {
        let png = make_png(2000, 4000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        let result = image::load_from_memory(&compressed).unwrap();
        assert_eq!(result.width(), 600);
        assert_eq!(result.height(), 1200);
    }

    #[test]
    fn compressed_output_is_smaller_than_original() {
        let png = make_png(3000, 2000);
        let (compressed, _) = resize_and_compress(&png).unwrap();

        assert!(compressed.len() < png.len(), "compressed {} should be < original {}", compressed.len(), png.len());
    }

    #[test]
    fn gif_passes_through_unchanged() {
        // Minimal valid GIF89a (1x1 pixel)
        let gif: Vec<u8> = vec![
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, // GIF89a
            0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, // logical screen descriptor
            0x2C, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, // image descriptor
            0x02, 0x02, 0x44, 0x01, 0x00, // image data
            0x3B, // trailer
        ];
        let (output, mime) = resize_and_compress(&gif).unwrap();

        assert_eq!(mime, "image/gif");
        assert_eq!(output, gif);
    }

    #[test]
    fn invalid_data_returns_error() {
        let garbage = vec![0x00, 0x01, 0x02, 0x03];
        assert!(resize_and_compress(&garbage).is_err());
    }
}
