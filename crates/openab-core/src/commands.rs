//! Platform-neutral command parsing and execution.
//!
//! Ingress admission and presentation remain platform responsibilities. Callers
//! must invoke this service only after their structural, scope, and identity
//! gates have admitted the event.

use std::sync::Arc;

use async_trait::async_trait;

use crate::acp::protocol::{ConfigOption, UsageReport};
use crate::acp::SessionPool;
use crate::dispatch::Dispatcher;

const COMMAND_EXECUTION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(45);
const TEXT_OPTION_LIMIT: usize = 25;
const TEXT_RESPONSE_CHAR_LIMIT: usize = 3_500;
const TEXT_VALUE_CHAR_LIMIT: usize = 120;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConfigCategory {
    Model,
    Agent,
}

impl ConfigCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Model => "model",
            Self::Agent => "agent",
        }
    }

    fn matches(self, category: Option<&str>) -> bool {
        match self {
            Self::Model => category == Some("model"),
            Self::Agent => matches!(category, Some("agent" | "mode")),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandName {
    Models,
    Agents,
    Cancel,
    CancelAll,
    Reset,
    Usage,
}

impl CommandName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Models => "models",
            Self::Agents => "agents",
            Self::Cancel => "cancel",
            Self::CancelAll => "cancel-all",
            Self::Reset => "reset",
            Self::Usage => "usage",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    ListConfig(ConfigCategory),
    SetConfig {
        category: ConfigCategory,
        selector: String,
    },
    Cancel,
    CancelAll,
    Reset,
    Usage,
    InvalidArguments {
        name: CommandName,
    },
}

impl Command {
    pub fn name(&self) -> CommandName {
        match self {
            Self::ListConfig(ConfigCategory::Model)
            | Self::SetConfig {
                category: ConfigCategory::Model,
                ..
            } => CommandName::Models,
            Self::ListConfig(ConfigCategory::Agent)
            | Self::SetConfig {
                category: ConfigCategory::Agent,
                ..
            } => CommandName::Agents,
            Self::Cancel => CommandName::Cancel,
            Self::CancelAll => CommandName::CancelAll,
            Self::Reset => CommandName::Reset,
            Self::Usage => CommandName::Usage,
            Self::InvalidArguments { name } => *name,
        }
    }
}

/// Parse only broker-owned commands. Prefix collisions and unknown slash text
/// return `None` so agent-native commands continue through the ordinary prompt
/// path.
pub fn parse_command(input: &str) -> Option<Command> {
    let trimmed = input.trim();
    match trimmed {
        "/models" => return Some(Command::ListConfig(ConfigCategory::Model)),
        "/agents" => return Some(Command::ListConfig(ConfigCategory::Agent)),
        "/cancel" => return Some(Command::Cancel),
        "/cancel-all" => return Some(Command::CancelAll),
        "/reset" => return Some(Command::Reset),
        "/usage" => return Some(Command::Usage),
        "/model" => return Some(Command::ListConfig(ConfigCategory::Model)),
        "/agent" => return Some(Command::ListConfig(ConfigCategory::Agent)),
        _ => {}
    }

    for (prefix, name) in [
        ("/models", CommandName::Models),
        ("/agents", CommandName::Agents),
        ("/cancel-all", CommandName::CancelAll),
        ("/cancel", CommandName::Cancel),
        ("/reset", CommandName::Reset),
        ("/usage", CommandName::Usage),
    ] {
        if has_whitespace_suffix(trimmed, prefix) {
            return Some(Command::InvalidArguments { name });
        }
    }

    parse_config_compatibility(trimmed, "/model", ConfigCategory::Model)
        .or_else(|| parse_config_compatibility(trimmed, "/agent", ConfigCategory::Agent))
}

fn has_whitespace_suffix(input: &str, prefix: &str) -> bool {
    input
        .strip_prefix(prefix)
        .is_some_and(|suffix| suffix.chars().next().is_some_and(char::is_whitespace))
}

fn parse_config_compatibility(
    input: &str,
    prefix: &str,
    category: ConfigCategory,
) -> Option<Command> {
    let suffix = input.strip_prefix(prefix)?;
    if suffix.is_empty() {
        return Some(Command::ListConfig(category));
    }
    if !suffix.chars().next().is_some_and(char::is_whitespace) {
        return None;
    }

    let mut parts = suffix.split_whitespace();
    match parts.next() {
        Some("list") if parts.next().is_none() => Some(Command::ListConfig(category)),
        Some("set") => {
            let selector = parts.collect::<Vec<_>>().join(" ");
            if selector.is_empty() {
                Some(Command::InvalidArguments {
                    name: command_name(category),
                })
            } else {
                Some(Command::SetConfig { category, selector })
            }
        }
        _ => Some(Command::InvalidArguments {
            name: command_name(category),
        }),
    }
}

fn command_name(category: ConfigCategory) -> CommandName {
    match category {
        ConfigCategory::Model => CommandName::Models,
        ConfigCategory::Agent => CommandName::Agents,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandContext {
    pub platform: String,
    pub logical_thread_id: String,
    pub response_is_private: bool,
}

impl CommandContext {
    pub fn new(
        platform: impl Into<String>,
        logical_thread_id: impl Into<String>,
        response_is_private: bool,
    ) -> Self {
        Self {
            platform: platform.into(),
            logical_thread_id: logical_thread_id.into(),
            response_is_private,
        }
    }

    pub fn session_key(&self) -> String {
        format!("{}:{}", self.platform, self.logical_thread_id)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CommandError {
    InvalidArguments(CommandName),
    NoConfigOptions(ConfigCategory),
    InvalidConfigSelection(Option<ConfigCategory>),
    ConfigUpdateUnavailable,
    OperationUnavailable,
    NoActiveSession,
    UsagePrivateOnly,
    UsageUnsupported,
    UsageUnavailable,
}

#[derive(Clone, Debug)]
pub enum CommandResult {
    ConfigOptions {
        category: ConfigCategory,
        options: Vec<ConfigOption>,
    },
    ConfigUpdated {
        display_name: String,
    },
    Cancel {
        signalled: bool,
    },
    CancelAll {
        signalled: bool,
        buffers_cleared: bool,
    },
    Reset {
        session_reset: bool,
        buffers_cleared: bool,
    },
    Usage(UsageReport),
    Error(CommandError),
}

impl CommandResult {
    pub fn outcome_class(&self) -> &'static str {
        match self {
            Self::ConfigOptions { .. }
            | Self::ConfigUpdated { .. }
            | Self::Cancel { signalled: true }
            | Self::CancelAll {
                signalled: true, ..
            }
            | Self::CancelAll {
                buffers_cleared: true,
                ..
            }
            | Self::Reset {
                session_reset: true,
                ..
            }
            | Self::Reset {
                buffers_cleared: true,
                ..
            }
            | Self::Usage(_) => "completed",
            Self::Cancel { signalled: false }
            | Self::CancelAll {
                signalled: false,
                buffers_cleared: false,
            }
            | Self::Reset {
                session_reset: false,
                buffers_cleared: false,
            } => "no_active_session",
            Self::Error(CommandError::UsagePrivateOnly) => "denied_private_surface",
            Self::Error(CommandError::InvalidArguments(_))
            | Self::Error(CommandError::InvalidConfigSelection(_)) => "invalid",
            Self::Error(_) => "unavailable",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UsageFailure {
    Unsupported,
    Unavailable,
}

#[async_trait]
trait CommandBackend: Send + Sync {
    async fn has_live_session(&self, session_key: &str) -> bool;
    async fn get_config_options(&self, session_key: &str) -> Vec<ConfigOption>;
    async fn set_config_option(
        &self,
        session_key: &str,
        config_id: &str,
        value: &str,
    ) -> anyhow::Result<()>;
    async fn get_usage(&self, session_key: &str) -> Result<UsageReport, UsageFailure>;
    async fn cancel_session(&self, session_key: &str) -> bool;
    async fn reset_session(&self, session_key: &str) -> bool;
    fn clear_buffered_thread(&self, platform: &str, logical_thread_id: &str) -> bool;
}

struct CoreCommandBackend {
    pool: Arc<SessionPool>,
    dispatcher: Arc<Dispatcher>,
}

#[async_trait]
impl CommandBackend for CoreCommandBackend {
    async fn has_live_session(&self, session_key: &str) -> bool {
        self.pool.has_live_session(session_key).await
    }

    async fn get_config_options(&self, session_key: &str) -> Vec<ConfigOption> {
        self.pool.get_config_options(session_key).await
    }

    async fn set_config_option(
        &self,
        session_key: &str,
        config_id: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        self.pool
            .set_config_option_strict(session_key, config_id, value)
            .await
            .map(|_| ())
    }

    async fn get_usage(&self, session_key: &str) -> Result<UsageReport, UsageFailure> {
        self.pool.get_usage(session_key).await.map_err(|error| {
            if error.to_string().contains("usage query is not supported") {
                UsageFailure::Unsupported
            } else {
                UsageFailure::Unavailable
            }
        })
    }

    async fn cancel_session(&self, session_key: &str) -> bool {
        self.pool.cancel_session(session_key).await.is_ok()
    }

    async fn reset_session(&self, session_key: &str) -> bool {
        self.pool.reset_session(session_key).await.is_ok()
    }

    fn clear_buffered_thread(&self, platform: &str, logical_thread_id: &str) -> bool {
        self.dispatcher
            .cancel_buffered_thread(platform, logical_thread_id)
            > 0
    }
}

#[derive(Clone)]
pub struct CommandService {
    backend: Arc<dyn CommandBackend>,
}

impl CommandService {
    pub fn new(pool: Arc<SessionPool>, dispatcher: Arc<Dispatcher>) -> Self {
        Self {
            backend: Arc::new(CoreCommandBackend { pool, dispatcher }),
        }
    }

    pub async fn execute(&self, command: Command, context: &CommandContext) -> CommandResult {
        tokio::time::timeout(
            COMMAND_EXECUTION_TIMEOUT,
            self.execute_inner(command, context),
        )
        .await
        .unwrap_or(CommandResult::Error(CommandError::OperationUnavailable))
    }

    async fn execute_inner(&self, command: Command, context: &CommandContext) -> CommandResult {
        match command {
            Command::ListConfig(category) => self.list_config(context, category).await,
            Command::SetConfig { category, selector } => {
                self.set_config_by_selector(context, category, &selector)
                    .await
            }
            Command::Cancel => CommandResult::Cancel {
                signalled: self.backend.cancel_session(&context.session_key()).await,
            },
            Command::CancelAll => {
                let buffers_cleared = self
                    .backend
                    .clear_buffered_thread(&context.platform, &context.logical_thread_id);
                let signalled = self.backend.cancel_session(&context.session_key()).await;
                CommandResult::CancelAll {
                    signalled,
                    buffers_cleared,
                }
            }
            Command::Reset => {
                let buffers_cleared = self
                    .backend
                    .clear_buffered_thread(&context.platform, &context.logical_thread_id);
                let session_reset = self.backend.reset_session(&context.session_key()).await;
                CommandResult::Reset {
                    session_reset,
                    buffers_cleared,
                }
            }
            Command::Usage => self.usage(context).await,
            Command::InvalidArguments { name } => {
                CommandResult::Error(CommandError::InvalidArguments(name))
            }
        }
    }

    pub async fn set_config_value(
        &self,
        context: &CommandContext,
        config_id: &str,
        value: &str,
    ) -> CommandResult {
        tokio::time::timeout(
            COMMAND_EXECUTION_TIMEOUT,
            self.set_config_value_inner(context, config_id, value),
        )
        .await
        .unwrap_or(CommandResult::Error(CommandError::OperationUnavailable))
    }

    async fn set_config_value_inner(
        &self,
        context: &CommandContext,
        config_id: &str,
        value: &str,
    ) -> CommandResult {
        let options = self
            .backend
            .get_config_options(&context.session_key())
            .await;
        let Some(display_name) = options.iter().find_map(|option| {
            if option.id != config_id
                || (!ConfigCategory::Model.matches(option.category.as_deref())
                    && !ConfigCategory::Agent.matches(option.category.as_deref()))
            {
                return None;
            }
            option
                .options
                .iter()
                .find(|choice| choice.value == value)
                .map(|choice| choice.name.clone())
        }) else {
            return CommandResult::Error(CommandError::InvalidConfigSelection(None));
        };

        match self
            .backend
            .set_config_option(&context.session_key(), config_id, value)
            .await
        {
            Ok(()) => CommandResult::ConfigUpdated { display_name },
            Err(_) => CommandResult::Error(CommandError::ConfigUpdateUnavailable),
        }
    }

    async fn list_config(
        &self,
        context: &CommandContext,
        category: ConfigCategory,
    ) -> CommandResult {
        let options = matching_options(
            self.backend
                .get_config_options(&context.session_key())
                .await,
            category,
        );
        if options.is_empty() {
            CommandResult::Error(CommandError::NoConfigOptions(category))
        } else {
            CommandResult::ConfigOptions { category, options }
        }
    }

    async fn set_config_by_selector(
        &self,
        context: &CommandContext,
        category: ConfigCategory,
        selector: &str,
    ) -> CommandResult {
        let options = matching_options(
            self.backend
                .get_config_options(&context.session_key())
                .await,
            category,
        );
        if options.is_empty() {
            return CommandResult::Error(CommandError::NoConfigOptions(category));
        }

        let choices = ordered_choices(&options);
        let selected = selector
            .parse::<usize>()
            .ok()
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| choices.get(index).copied())
            .or_else(|| {
                let folded = selector.to_lowercase();
                choices.iter().copied().find(|(_, choice)| {
                    choice.value.to_lowercase() == folded || choice.name.to_lowercase() == folded
                })
            });
        let Some((config_id, choice)) = selected else {
            return CommandResult::Error(CommandError::InvalidConfigSelection(Some(category)));
        };

        match self
            .backend
            .set_config_option(&context.session_key(), config_id, &choice.value)
            .await
        {
            Ok(()) => CommandResult::ConfigUpdated {
                display_name: choice.name.clone(),
            },
            Err(_) => CommandResult::Error(CommandError::ConfigUpdateUnavailable),
        }
    }

    async fn usage(&self, context: &CommandContext) -> CommandResult {
        if !context.response_is_private {
            return CommandResult::Error(CommandError::UsagePrivateOnly);
        }
        if !self.backend.has_live_session(&context.session_key()).await {
            return CommandResult::Error(CommandError::NoActiveSession);
        }
        match self.backend.get_usage(&context.session_key()).await {
            Ok(report) => CommandResult::Usage(report),
            Err(UsageFailure::Unsupported) => CommandResult::Error(CommandError::UsageUnsupported),
            Err(UsageFailure::Unavailable) => CommandResult::Error(CommandError::UsageUnavailable),
        }
    }
}

fn matching_options(options: Vec<ConfigOption>, category: ConfigCategory) -> Vec<ConfigOption> {
    options
        .into_iter()
        .filter(|option| category.matches(option.category.as_deref()))
        .map(|mut option| {
            option.category = Some(category.as_str().to_string());
            option
        })
        .collect()
}

fn ordered_choices(
    options: &[ConfigOption],
) -> Vec<(&str, &crate::acp::protocol::ConfigOptionValue)> {
    let mut choices = Vec::new();
    for option in options {
        choices.extend(
            option
                .options
                .iter()
                .filter(|choice| choice.value == option.current_value)
                .map(|choice| (option.id.as_str(), choice)),
        );
        choices.extend(
            option
                .options
                .iter()
                .filter(|choice| choice.value != option.current_value)
                .map(|choice| (option.id.as_str(), choice)),
        );
    }
    choices
}

pub fn render_text_result(result: &CommandResult) -> String {
    let text = match result {
        CommandResult::ConfigOptions { category, options } => {
            let choices = ordered_choices(options);
            let shown = choices.len().min(TEXT_OPTION_LIMIT);
            let mut lines = vec![format!("🔧 Available {}s:", category.as_str())];
            for (index, (_, choice)) in choices.iter().take(shown).enumerate() {
                let is_current = options.iter().any(|option| {
                    option.current_value == choice.value
                        && option
                            .options
                            .iter()
                            .any(|candidate| std::ptr::eq(candidate, *choice))
                });
                lines.push(format!(
                    "  {}. {}{}",
                    index + 1,
                    truncate_chars(&choice.name, TEXT_VALUE_CHAR_LIMIT),
                    if is_current { " ✅" } else { "" }
                ));
            }
            if choices.len() > shown {
                lines.push(format!(
                    "… {} more option(s) omitted.",
                    choices.len() - shown
                ));
            }
            lines.push(format!(
                "\nUsage: /{} set <number or exact name>",
                category.as_str()
            ));
            lines.join("\n")
        }
        CommandResult::ConfigUpdated { display_name } => format!(
            "✅ Switched to **{}**",
            truncate_chars(display_name, TEXT_VALUE_CHAR_LIMIT)
        ),
        CommandResult::Cancel { signalled: true } => "🛑 Cancel signal sent.".to_string(),
        CommandResult::Cancel { signalled: false } => {
            "⚠️ Nothing to cancel — no active session.".to_string()
        }
        CommandResult::CancelAll {
            signalled: true,
            buffers_cleared: true,
        } => "🛑 Cancel signal sent. Buffered messages cleared.".to_string(),
        CommandResult::CancelAll {
            signalled: true,
            buffers_cleared: false,
        } => "🛑 Cancel signal sent.".to_string(),
        CommandResult::CancelAll {
            signalled: false,
            buffers_cleared: true,
        } => "🛑 Buffered messages cleared. No active session to cancel.".to_string(),
        CommandResult::CancelAll {
            signalled: false,
            buffers_cleared: false,
        } => "⚠️ Nothing to cancel — no active session and no buffered messages.".to_string(),
        CommandResult::Reset {
            session_reset: true,
            buffers_cleared: true,
        } => "🔄 Session reset. Buffered messages cleared. Start a new conversation!".to_string(),
        CommandResult::Reset {
            session_reset: true,
            buffers_cleared: false,
        } => "🔄 Session reset. Start a new conversation!".to_string(),
        CommandResult::Reset {
            session_reset: false,
            buffers_cleared: true,
        } => "🔄 Buffered messages cleared. No active session to reset.".to_string(),
        CommandResult::Reset {
            session_reset: false,
            buffers_cleared: false,
        } => "⚠️ No active session to reset.".to_string(),
        CommandResult::Usage(report) => render_usage(report),
        CommandResult::Error(error) => render_error(*error),
    };
    truncate_chars(&text, TEXT_RESPONSE_CHAR_LIMIT)
}

fn render_usage(report: &UsageReport) -> String {
    let mut lines = vec![format!(
        "📊 **Usage — {}**",
        truncate_chars(&report.plan_name, TEXT_VALUE_CHAR_LIMIT)
    )];
    for breakdown in &report.breakdowns {
        let name = truncate_chars(&breakdown.display_name, TEXT_VALUE_CHAR_LIMIT);
        match breakdown.limit {
            Some(limit) => {
                let percentage = breakdown.percentage.unwrap_or_else(|| {
                    if limit > 0.0 {
                        (breakdown.used / limit * 100.0).round() as u64
                    } else {
                        0
                    }
                });
                let filled = percentage.min(100) as usize / 10;
                let bar = "█".repeat(filled) + &"░".repeat(10 - filled);
                lines.push(format!(
                    "{name}: {:.2} / {:.0} `{bar}` {percentage}%{}",
                    breakdown.used,
                    limit,
                    if percentage > 100 { " ⚠️" } else { "" }
                ));
            }
            None => lines.push(format!("{name}: {:.2} used", breakdown.used)),
        }
        if let Some(charges) = breakdown.overage_charges.filter(|charges| *charges > 0.0) {
            lines.push(format!(
                "Overage charges: {:.2} {}",
                charges,
                truncate_chars(
                    breakdown.currency.as_deref().unwrap_or("USD"),
                    TEXT_VALUE_CHAR_LIMIT,
                )
            ));
        }
    }
    if let Some(reset) = &report.billing_cycle_reset {
        lines.push(format!(
            "Billing cycle resets {}",
            truncate_chars(reset, TEXT_VALUE_CHAR_LIMIT)
        ));
    }
    lines.join("\n")
}

fn render_error(error: CommandError) -> String {
    match error {
        CommandError::InvalidArguments(name) => format!(
            "⚠️ Invalid arguments. Usage: {}",
            match name {
                CommandName::Models => "/models or /model list | /model set <number or exact name>",
                CommandName::Agents => "/agents or /agent list | /agent set <number or exact name>",
                CommandName::Cancel => "/cancel",
                CommandName::CancelAll => "/cancel-all",
                CommandName::Reset => "/reset",
                CommandName::Usage => "/usage",
            }
        ),
        CommandError::NoConfigOptions(category) => format!(
            "⚠️ No {} options available. Start a conversation first.",
            category.as_str()
        ),
        CommandError::InvalidConfigSelection(Some(category)) => format!(
            "⚠️ No matching {}. Use /{} list to see options.",
            category.as_str(),
            category.as_str()
        ),
        CommandError::InvalidConfigSelection(None) => {
            "⚠️ That configuration selection is no longer available.".to_string()
        }
        CommandError::ConfigUpdateUnavailable => {
            "❌ The configuration change could not be completed.".to_string()
        }
        CommandError::OperationUnavailable => "⚠️ The command could not be completed.".to_string(),
        CommandError::NoActiveSession => {
            "⚠️ No active session. Start a conversation first.".to_string()
        }
        CommandError::UsagePrivateOnly => {
            "🔒 `/usage` is only available in a private chat.".to_string()
        }
        CommandError::UsageUnsupported => {
            "⚠️ Usage reporting is not supported by this backend.".to_string()
        }
        CommandError::UsageUnavailable => {
            "⚠️ Usage information is temporarily unavailable.".to_string()
        }
    }
}

fn truncate_chars(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        input.to_string()
    } else if max == 0 {
        String::new()
    } else {
        let mut output: String = input.chars().take(max - 1).collect();
        output.push('…');
        output
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::acp::protocol::{ConfigOptionValue, UsageBreakdown};

    #[derive(Default)]
    struct FakeState {
        active: bool,
        options: Vec<ConfigOption>,
        usage: Option<Result<UsageReport, UsageFailure>>,
        usage_delay: Option<std::time::Duration>,
        cancel_succeeds: bool,
        reset_succeeds: bool,
        buffers_cleared: bool,
        set_calls: Vec<(String, String, String)>,
        usage_calls: usize,
        cancel_calls: usize,
        reset_calls: usize,
        clear_calls: Vec<(String, String)>,
    }

    #[derive(Default)]
    struct FakeBackend {
        state: Mutex<FakeState>,
    }

    impl FakeBackend {
        fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }
    }

    #[async_trait]
    impl CommandBackend for FakeBackend {
        async fn has_live_session(&self, _session_key: &str) -> bool {
            self.state().active
        }

        async fn get_config_options(&self, _session_key: &str) -> Vec<ConfigOption> {
            self.state().options.clone()
        }

        async fn set_config_option(
            &self,
            session_key: &str,
            config_id: &str,
            value: &str,
        ) -> anyhow::Result<()> {
            self.state().set_calls.push((
                session_key.to_string(),
                config_id.to_string(),
                value.to_string(),
            ));
            Ok(())
        }

        async fn get_usage(&self, _session_key: &str) -> Result<UsageReport, UsageFailure> {
            let (delay, result) = {
                let mut state = self.state();
                state.usage_calls += 1;
                (
                    state.usage_delay,
                    state
                        .usage
                        .clone()
                        .unwrap_or(Err(UsageFailure::Unavailable)),
                )
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            result
        }

        async fn cancel_session(&self, _session_key: &str) -> bool {
            let mut state = self.state();
            state.cancel_calls += 1;
            state.cancel_succeeds
        }

        async fn reset_session(&self, _session_key: &str) -> bool {
            let mut state = self.state();
            state.reset_calls += 1;
            state.reset_succeeds
        }

        fn clear_buffered_thread(&self, platform: &str, logical_thread_id: &str) -> bool {
            let mut state = self.state();
            state
                .clear_calls
                .push((platform.to_string(), logical_thread_id.to_string()));
            state.buffers_cleared
        }
    }

    fn service(backend: Arc<FakeBackend>) -> CommandService {
        CommandService { backend }
    }

    fn context(private: bool) -> CommandContext {
        CommandContext::new("teams", "conversation", private)
    }

    fn option(category: &str, count: usize, current: usize) -> ConfigOption {
        ConfigOption {
            id: category.to_string(),
            name: category.to_string(),
            description: None,
            category: Some(category.to_string()),
            option_type: "enum".to_string(),
            current_value: format!("value-{current}"),
            options: (0..count)
                .map(|index| ConfigOptionValue {
                    value: format!("value-{index}"),
                    name: format!("Choice {index}"),
                    description: None,
                })
                .collect(),
        }
    }

    fn usage_report(limit: Option<f64>, percentage: Option<u64>) -> UsageReport {
        UsageReport {
            plan_name: "Plan".to_string(),
            billing_cycle_reset: Some("2026-09-01".to_string()),
            breakdowns: vec![UsageBreakdown {
                display_name: "Credits".to_string(),
                used: 12.5,
                limit,
                percentage,
                overage_charges: Some(1.25),
                currency: Some("USD".to_string()),
            }],
        }
    }

    fn configure_usage(backend: &FakeBackend, usage: Result<UsageReport, UsageFailure>) {
        let mut state = backend.state();
        state.active = true;
        state.usage = Some(usage);
    }

    #[test]
    fn parser_requires_exact_boundaries_and_preserves_unknown_slash_text() {
        assert_eq!(
            parse_command(" /models \n"),
            Some(Command::ListConfig(ConfigCategory::Model))
        );
        assert_eq!(parse_command("/cancel-all"), Some(Command::CancelAll));
        assert_eq!(
            parse_command("/reset now"),
            Some(Command::InvalidArguments {
                name: CommandName::Reset
            })
        );
        assert_eq!(
            parse_command("/model set Choice 1"),
            Some(Command::SetConfig {
                category: ConfigCategory::Model,
                selector: "Choice 1".to_string()
            })
        );
        assert_eq!(
            parse_command("/agent list extra"),
            Some(Command::InvalidArguments {
                name: CommandName::Agents
            })
        );
        assert_eq!(parse_command("/reset-now"), None);
        assert_eq!(parse_command("/cancel-all-now"), None);
        assert_eq!(parse_command("/usage-report"), None);
        assert_eq!(parse_command("/compact"), None);
        assert_eq!(parse_command("/Models"), None);
    }

    #[tokio::test]
    async fn text_config_list_is_current_first_and_bounded_to_25() {
        let backend = Arc::new(FakeBackend::default());
        backend.state().options = vec![option("model", 28, 27)];
        let result = service(backend)
            .execute(Command::ListConfig(ConfigCategory::Model), &context(true))
            .await;
        let text = render_text_result(&result);
        let Some(first_choice) = text.lines().nth(1) else {
            panic!("rendered config list has no first choice");
        };
        assert!(first_choice.contains("Choice 27 ✅"));
        assert!(text.contains("… 3 more option(s) omitted."));
        assert!(!text.contains("Choice 26"));
    }

    #[tokio::test]
    async fn agent_category_accepts_mode_and_selection_uses_full_option_set() {
        let backend = Arc::new(FakeBackend::default());
        backend.state().options = vec![option("mode", 30, 0)];
        let result = service(backend.clone())
            .execute(
                Command::SetConfig {
                    category: ConfigCategory::Agent,
                    selector: "Choice 29".to_string(),
                },
                &context(true),
            )
            .await;
        assert!(matches!(result, CommandResult::ConfigUpdated { .. }));
        assert_eq!(backend.state().set_calls.len(), 1);
    }

    #[tokio::test]
    async fn forged_config_payload_is_rejected_before_backend_mutation() {
        let backend = Arc::new(FakeBackend::default());
        backend.state().options = vec![option("model", 2, 0)];
        let result = service(backend.clone())
            .set_config_value(&context(true), "forged", "value-1")
            .await;
        assert!(matches!(
            result,
            CommandResult::Error(CommandError::InvalidConfigSelection(_))
        ));
        assert!(backend.state().set_calls.is_empty());
    }

    #[tokio::test]
    async fn cancel_preserves_buffers_while_cancel_all_and_reset_clear_only_context_thread() {
        let backend = Arc::new(FakeBackend::default());
        {
            let mut state = backend.state();
            state.cancel_succeeds = true;
            state.reset_succeeds = true;
            state.buffers_cleared = true;
        }
        let service = service(backend.clone());
        service.execute(Command::Cancel, &context(true)).await;
        assert!(backend.state().clear_calls.is_empty());

        service.execute(Command::CancelAll, &context(true)).await;
        service.execute(Command::Reset, &context(true)).await;
        let state = backend.state();
        assert_eq!(
            state.clear_calls,
            vec![("teams".to_string(), "conversation".to_string()); 2]
        );
        assert_eq!(state.cancel_calls, 2);
        assert_eq!(state.reset_calls, 1);
    }

    #[tokio::test]
    async fn public_usage_is_denied_before_session_or_backend_access() {
        let backend = Arc::new(FakeBackend::default());
        backend.state().active = true;
        let result = service(backend.clone())
            .execute(Command::Usage, &context(false))
            .await;
        assert!(matches!(
            result,
            CommandResult::Error(CommandError::UsagePrivateOnly)
        ));
        assert_eq!(backend.state().usage_calls, 0);
    }

    #[tokio::test]
    async fn usage_classifies_absent_unsupported_and_malformed_without_raw_errors() {
        let backend = Arc::new(FakeBackend::default());
        let service = service(backend.clone());
        let absent = service.execute(Command::Usage, &context(true)).await;
        assert!(matches!(
            absent,
            CommandResult::Error(CommandError::NoActiveSession)
        ));

        configure_usage(&backend, Err(UsageFailure::Unsupported));
        let unsupported = service.execute(Command::Usage, &context(true)).await;
        assert!(matches!(
            unsupported,
            CommandResult::Error(CommandError::UsageUnsupported)
        ));

        backend.state().usage = Some(Err(UsageFailure::Unavailable));
        let malformed = service.execute(Command::Usage, &context(true)).await;
        assert!(matches!(
            malformed,
            CommandResult::Error(CommandError::UsageUnavailable)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn command_execution_timeout_returns_bounded_error() {
        let backend = Arc::new(FakeBackend::default());
        configure_usage(&backend, Ok(usage_report(Some(10.0), Some(50))));
        backend.state().usage_delay = Some(std::time::Duration::from_secs(60));
        let result = service(backend)
            .execute(Command::Usage, &context(true))
            .await;
        assert!(matches!(
            result,
            CommandResult::Error(CommandError::OperationUnavailable)
        ));
        assert_eq!(
            render_text_result(&result),
            "⚠️ The command could not be completed."
        );
    }

    #[tokio::test]
    async fn usage_renderer_handles_over_limit_and_no_cap_reports() {
        let backend = Arc::new(FakeBackend::default());
        configure_usage(&backend, Ok(usage_report(Some(10.0), Some(125))));
        let service = service(backend.clone());
        let over = service.execute(Command::Usage, &context(true)).await;
        let over_text = render_text_result(&over);
        assert!(over_text.contains("125% ⚠️"));
        assert!(over_text.contains("Overage charges: 1.25 USD"));

        backend.state().usage = Some(Ok(usage_report(None, None)));
        let no_cap = service.execute(Command::Usage, &context(true)).await;
        assert!(render_text_result(&no_cap).contains("Credits: 12.50 used"));
    }

    #[test]
    fn renderer_bounds_untrusted_backend_strings() {
        let text = render_text_result(&CommandResult::Usage(UsageReport {
            plan_name: "x".repeat(10_000),
            billing_cycle_reset: None,
            breakdowns: vec![],
        }));
        assert!(text.chars().count() <= TEXT_RESPONSE_CHAR_LIMIT);
        assert!(!text.contains(&"x".repeat(TEXT_VALUE_CHAR_LIMIT + 1)));
    }

    #[test]
    fn session_key_is_namespaced_by_platform() {
        assert_eq!(context(true).session_key(), "teams:conversation");
        assert_ne!(
            context(true).session_key(),
            CommandContext::new("discord", "conversation", true).session_key()
        );
    }

    #[test]
    fn error_rendering_never_contains_backend_details() {
        let expected = [
            (
                CommandError::ConfigUpdateUnavailable,
                "❌ The configuration change could not be completed.",
            ),
            (
                CommandError::UsageUnavailable,
                "⚠️ Usage information is temporarily unavailable.",
            ),
        ];
        for (error, message) in expected {
            assert_eq!(render_text_result(&CommandResult::Error(error)), message);
        }
    }
}
