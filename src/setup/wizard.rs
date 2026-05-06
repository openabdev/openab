//! Interactive setup wizard TUI and Discord API client.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use crate::setup::config::{generate_config, mask_bot_token};
use crate::setup::validate::{validate_bot_token, validate_channel_id};

// ---------------------------------------------------------------------------
// Color codes (ANSI)
// ---------------------------------------------------------------------------

const C: Colors = Colors {
    reset: "\x1b[0m",
    bold: "\x1b[1m",
    cyan: "\x1b[36m",
    green: "\x1b[32m",
    red: "\x1b[31m",
    yellow: "\x1b[33m",
    magenta: "\x1b[35m",
};

struct Colors {
    reset: &'static str,
    bold: &'static str,
    cyan: &'static str,
    green: &'static str,
    red: &'static str,
    yellow: &'static str,
    magenta: &'static str,
}

const BORDER: char = '═';

macro_rules! cprintln {
    ($color:expr, $fmt:expr) => {{
        println!("{}{}{}", $color, $fmt, C.reset);
    }};
    ($color:expr, $fmt:expr, $($arg:tt)*) => {{
        println!("{}{}{}", $color, format!($fmt, $($arg)*), C.reset);
    }};
}

// ---------------------------------------------------------------------------
// Input helpers
// ---------------------------------------------------------------------------

fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn prompt(prompt_text: &str) -> String {
    print!("{}{}: {}", C.yellow, prompt_text, C.reset);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    input.trim().to_string()
}

fn prompt_default(prompt_text: &str, default: &str) -> String {
    print!("{}{} [{}]: {}", C.yellow, prompt_text, default, C.reset);
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let input = input.trim();
    if input.is_empty() {
        default.to_string()
    } else {
        input.to_string()
    }
}

fn prompt_password(prompt_text: &str) -> String {
    print!("{}{}: ", C.yellow, prompt_text);
    io::stdout().flush().ok();
    rpassword::read_password().unwrap_or_default()
}

fn prompt_yes_no(prompt_text: &str, default: bool) -> bool {
    let default_str = if default { "Y/n" } else { "y/N" };
    loop {
        print!("{}{} [{}]: ", C.yellow, prompt_text, default_str,);
        io::stdout().flush().ok();
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        let input = input.trim().to_lowercase();
        if input.is_empty() {
            return default;
        }
        match input.as_str() {
            "y" | "yes" => return true,
            "n" | "no" => return false,
            _ => cprintln!(C.red, "Please enter 'y' or 'n'"),
        }
    }
}

fn prompt_choice(prompt_text: &str, choices: &[&str]) -> usize {
    println!();
    cprintln!(C.cyan, "{}", prompt_text);
    for (i, choice) in choices.iter().enumerate() {
        println!("  {}. {}", i + 1, choice);
    }
    print!("{}Select [1-{}]: {}", C.yellow, choices.len(), C.reset);
    io::stdout().flush().ok();
    loop {
        let mut input = String::new();
        io::stdin().read_line(&mut input).ok();
        match input.trim().parse::<usize>() {
            Ok(n) if n >= 1 && n <= choices.len() => return n - 1,
            _ => {
                print!("{}Select [1-{}]: {}", C.yellow, choices.len(), C.reset);
                io::stdout().flush().ok();
            }
        }
    }
}

fn prompt_checklist(prompt_text: &str, items: &[&str]) -> Vec<usize> {
    println!();
    cprintln!(C.cyan, "{}", prompt_text);
    for (i, item) in items.iter().enumerate() {
        println!("  [{}] {}", i + 1, item);
    }
    println!();
    print!(
        "{}Enter numbers separated by commas (e.g. 1,3,5) or press Enter for all: {}",
        C.yellow, C.reset
    );
    io::stdout().flush().ok();
    let mut input = String::new();
    io::stdin().read_line(&mut input).ok();
    let input = input.trim();
    if input.is_empty() {
        return (0..items.len()).collect();
    }
    input
        .split(',')
        .filter_map(|s| s.trim().parse::<usize>().ok())
        .filter(|n| *n >= 1 && *n <= items.len())
        .map(|n| n - 1)
        .collect()
}

// ---------------------------------------------------------------------------
// Box drawing helpers
// ---------------------------------------------------------------------------

fn print_box(lines: &[&str]) {
    let width = lines
        .iter()
        .map(|l| unicode_width::UnicodeWidthStr::width(&**l))
        .max()
        .unwrap_or(60);
    let width = width.clamp(60, 76);
    println!();
    cprintln!(
        C.cyan,
        "{}",
        "╔".to_string() + &BORDER.to_string().repeat(width + 2) + "╗"
    );
    for line in lines {
        let padded = format!(" {:<width$} ", format!("{}", line), width = width);
        print!("{}", C.cyan);
        print!("║");
        print!("{}{}", C.reset, padded);
        print!("{}", C.cyan);
        println!("║");
    }
    cprintln!(
        C.cyan,
        "{}",
        "╚".to_string() + &BORDER.to_string().repeat(width + 2) + "╝"
    );
    println!();
}

// ---------------------------------------------------------------------------
// Discord API client (uses reqwest — no ureq dependency)
// ---------------------------------------------------------------------------

struct DiscordClient {
    token: String,
    http: reqwest::blocking::Client,
}

impl DiscordClient {
    fn new(token: &str) -> Self {
        Self {
            token: token.to_string(),
            http: reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("static HTTP client must build"),
        }
    }

    /// Verify token by fetching bot info
    fn verify_token(&self) -> anyhow::Result<(String, String)> {
        let resp = self
            .http
            .get("https://discord.com/api/v10/users/@me")
            .header("Authorization", format!("Bot {}", self.token))
            .header("User-Agent", "OpenAB setup wizard")
            .send()?;
        if !resp.status().is_success() {
            anyhow::bail!("Token verification failed: HTTP {}", resp.status());
        }
        #[derive(serde::Deserialize)]
        struct MeResponse {
            id: String,
            username: String,
        }
        let me: MeResponse = resp.json()?;
        Ok((me.id, me.username))
    }

    /// Fetch guilds the bot is in
    fn fetch_guilds(&self) -> anyhow::Result<Vec<(String, String)>> {
        let resp = self
            .http
            .get("https://discord.com/api/v10/users/@me/guilds")
            .header("Authorization", format!("Bot {}", self.token))
            .header("User-Agent", "OpenAB setup wizard")
            .send()?;
        if !resp.status().is_success() {
            anyhow::bail!("Failed to fetch guilds: HTTP {}", resp.status());
        }
        #[derive(serde::Deserialize)]
        struct Guild {
            id: String,
            name: String,
        }
        let guilds: Vec<Guild> = resp.json()?;
        Ok(guilds.into_iter().map(|g| (g.id, g.name)).collect())
    }

    /// Fetch channels in a guild
    fn fetch_channels(&self, guild_id: &str) -> anyhow::Result<Vec<(String, String, String)>> {
        let url = format!("https://discord.com/api/v10/guilds/{}/channels", guild_id);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bot {}", self.token))
            .header("User-Agent", "OpenAB setup wizard")
            .send()?;
        if !resp.status().is_success() {
            anyhow::bail!("Failed to fetch channels: HTTP {}", resp.status());
        }
        #[derive(serde::Deserialize)]
        struct Channel {
            id: String,
            #[serde(rename = "type")]
            kind: u8,
            name: String,
        }
        let channels: Vec<Channel> = resp.json()?;
        // type 0 = text channel
        Ok(channels
            .into_iter()
            .filter(|c| c.kind == 0)
            .map(|c| (c.id, c.name, guild_id.to_string()))
            .collect())
    }
}

// ---------------------------------------------------------------------------
// Section 1: Discord Bot Setup Guide
// ---------------------------------------------------------------------------

fn section_discord_guide() {
    print_box(&[
        "Discord Bot Setup Guide",
        "",
        "1. Go to: https://discord.com/developers/applications",
        "2. Click 'New Application' -> name it (e.g. OpenAB)",
        "3. Bot -> Reset Token -> COPY the token",
        "",
        "4. Enable Privileged Gateway Intents:",
        "   - Message Content Intent",
        "   - Guild Members Intent",
        "",
        "5. OAuth2 -> URL Generator:",
        "   - SCOPES: bot",
        "   - BOT PERMISSIONS:",
        "     Send Messages | Embed Links | Attach Files",
        "     Read Message History | Add Reactions",
        "     Use Slash Commands",
        "",
        "6. Visit the generated URL -> add bot to your server",
    ]);
}

// ---------------------------------------------------------------------------
// Section 2: Channel Selection
// ---------------------------------------------------------------------------

fn section_channels(client: &DiscordClient) -> anyhow::Result<Vec<String>> {
    println!();
    cprintln!(C.bold, "--- Step 2: Allowed Channels ---");
    println!();

    print!("  Fetching servers... ");
    io::stdout().flush().ok();
    let guilds = client.fetch_guilds()?;
    cprintln!(C.green, "OK Found {} server(s)", guilds.len());
    println!();

    if guilds.is_empty() {
        cprintln!(C.yellow, "  No servers found. Enter channel IDs manually.");
        let input = prompt("  Channel ID(s), comma-separated");
        let ids: Vec<String> = input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for id in &ids {
            validate_channel_id(id)?;
        }
        return Ok(ids);
    }

    let guild_names: Vec<&str> = guilds.iter().map(|(_, n)| n.as_str()).collect();
    let guild_idx = prompt_choice("  Select server:", &guild_names);
    let (guild_id, guild_name) = &guilds[guild_idx];

    print!("  Fetching channels in '{}'... ", guild_name);
    io::stdout().flush().ok();
    let channels = client.fetch_channels(guild_id)?;
    cprintln!(C.green, "OK Found {} channel(s)", channels.len());
    println!();

    if channels.is_empty() {
        cprintln!(
            C.yellow,
            "  No text channels found. Enter channel IDs manually."
        );
        let input = prompt("  Channel ID(s), comma-separated");
        let ids: Vec<String> = input
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        for id in &ids {
            validate_channel_id(id)?;
        }
        return Ok(ids);
    }

    let channel_names: Vec<String> = channels.iter().map(|(_, n, _)| format!("#{}", n)).collect();
    let channel_names_refs: Vec<&str> = channel_names.iter().map(|s| s.as_str()).collect();

    let selected = prompt_checklist("  Select channels (by number):", &channel_names_refs);
    let selected_ids: Vec<String> = selected.iter().map(|&i| channels[i].0.clone()).collect();

    println!();
    cprintln!(C.green, "  Selected {} channel(s)", selected_ids.len());
    for id in &selected_ids {
        if let Some((_, name, _)) = channels.iter().find(|(cid, _, _)| cid == id) {
            println!("    * #{}", name);
        } else {
            println!("    * {}", id);
        }
    }
    println!();

    Ok(selected_ids)
}

// ---------------------------------------------------------------------------
// Section 3: Agent Configuration
// ---------------------------------------------------------------------------

fn section_agent() -> (String, String, bool) {
    println!();
    cprintln!(C.bold, "--- Step 3: Agent Configuration ---");
    println!();

    print_box(&[
        "Agent Installation Guide",
        "",
        "claude:  npm install -g @anthropic-ai/claude-code",
        "kiro:    npm install -g @koryhutchison/kiro-cli",
        "codex:   npm install -g openai-codex (requires OpenAI API key)",
        "gemini:  npm install -g @google/gemini-cli",
        "",
        "Make sure the agent is in your PATH before continuing.",
    ]);
    println!();

    let choices = ["claude", "kiro", "codex", "gemini"];
    let idx = prompt_choice("  Select agent:", &choices);
    let agent = choices[idx];

    let deploy_choices = ["Local (current directory)", "Docker / k8s"];
    let deploy_idx = prompt_choice("  Deployment target:", &deploy_choices);
    let is_local = deploy_idx == 0;
    let default_dir = match (is_local, agent) {
        (true, _) => ".",
        (false, "kiro") => "/home/agent",
        (false, _) => "/home/node",
    };

    let working_dir = prompt_default("  Working directory", default_dir);

    cprintln!(C.green, "  Agent: {} | Working dir: {}", agent, working_dir);
    println!();

    (agent.to_string(), working_dir, is_local)
}

// ---------------------------------------------------------------------------
// Section 4: Pool Settings
// ---------------------------------------------------------------------------

fn section_pool() -> (usize, u64) {
    println!();
    cprintln!(C.bold, "--- Step 4: Session Pool ---");
    println!();

    let max_sessions: usize = prompt_default("  Max sessions", "10").parse().unwrap_or(10);
    let ttl_hours: u64 = prompt_default("  Session TTL (hours)", "24")
        .parse()
        .unwrap_or(24);

    cprintln!(
        C.green,
        "  Max sessions: {} | TTL: {}h",
        max_sessions,
        ttl_hours
    );
    println!();

    (max_sessions, ttl_hours)
}

// ---------------------------------------------------------------------------
// Preview & Save
// ---------------------------------------------------------------------------

fn section_preview_and_save(config_content: &str, output_path: &PathBuf) -> anyhow::Result<()> {
    println!();
    cprintln!(C.bold, "--- Preview ---");
    println!();
    println!("{}", mask_bot_token(config_content));
    println!();

    if output_path.exists() && !prompt_yes_no("  File exists. Overwrite?", false) {
        println!("  Saving cancelled.");
        return Ok(());
    }

    std::fs::write(output_path, config_content)?;
    cprintln!(C.green, "OK config.toml saved to {}", output_path.display());
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Non-interactive guidance
// ---------------------------------------------------------------------------

fn print_noninteractive_guide() {
    print_box(&[
        "Non-Interactive Mode",
        "",
        "The interactive wizard requires a terminal.",
        "Create config.toml manually, then run:",
        "",
        "  openab run config.toml",
        "",
        "Config format reference:",
        "  [discord]",
        "  bot_token = \"YOUR_BOT_TOKEN\"",
        "  allowed_channels = [\"CHANNEL_ID\"]",
        "",
        "  [agent]",
        "  command = \"kiro-cli\"",
        "  args = [\"acp\", \"--trust-all-tools\"]",
        "  working_dir = \"/home/agent\"",
        "",
        "  [pool]",
        "  max_sessions = 10",
        "  session_ttl_hours = 24",
        "",
        "  [reactions]",
        "  enabled = true",
        "  remove_after_reply = false",
        "  ...",
    ]);
}

// ---------------------------------------------------------------------------
// Next steps printer
// ---------------------------------------------------------------------------

fn print_next_steps(agent: &str, output_path: &Path, is_local: bool) {
    println!();
    cprintln!(C.bold, "--- Next Steps ---");
    println!();

    if is_local {
        match agent {
            "kiro" => {
                cprintln!(
                    C.cyan,
                    "  1. Install kiro-cli (see https://kiro.dev for installer)"
                );
                cprintln!(C.cyan, "  2. Authenticate:");
                println!("       kiro-cli login --use-device-flow");
            }
            "claude" => {
                cprintln!(C.cyan, "  1. Install Claude Code + ACP adapter:");
                println!("       npm install -g @anthropic-ai/claude-code @agentclientprotocol/claude-agent-acp");
                cprintln!(C.cyan, "  2. Authenticate:");
                println!("       claude auth login");
            }
            "codex" => {
                cprintln!(C.cyan, "  1. Install Codex CLI + ACP adapter:");
                println!("       npm install -g @openai/codex @zed-industries/codex-acp");
                cprintln!(C.cyan, "  2. Authenticate:");
                println!("       codex login --device-auth");
            }
            "gemini" => {
                cprintln!(C.cyan, "  1. Install Gemini CLI:");
                println!("       npm install -g @google/gemini-cli");
                cprintln!(
                    C.cyan,
                    "  2. Authenticate via Google OAuth, or set GEMINI_API_KEY in config.toml"
                );
            }
            _ => {}
        }

        println!();
        cprintln!(C.green, "  3. Run the bot:");
        println!("       cargo run -- run {}", output_path.display());
    } else {
        cprintln!(
            C.cyan,
            "  Docker image already bundles the agent CLI and ACP adapter."
        );
        println!();
        cprintln!(C.cyan, "  1. Deploy with Helm (or your preferred method):");
        println!("       helm install openab openab/openab \\");
        println!(
            "         --set agents.{}.discord.botToken=\"$BOT_TOKEN\"",
            agent
        );
        println!();
        cprintln!(
            C.cyan,
            "  2. Authenticate inside the pod (first time only):"
        );
        match agent {
            "kiro" => println!(
                "       kubectl exec -it deployment/openab-kiro -- kiro-cli login --use-device-flow"
            ),
            "claude" => {
                println!("       kubectl exec -it deployment/openab-claude -- claude auth login")
            }
            "codex" => println!(
                "       kubectl exec -it deployment/openab-codex -- codex login --device-auth"
            ),
            "gemini" => {
                println!("       Set GEMINI_API_KEY via secret, or exec into the pod for OAuth")
            }
            _ => {}
        }
        println!();
        cprintln!(C.green, "  See README for full Helm options.");
    }
    println!();
}

// ---------------------------------------------------------------------------
// Main wizard entry point
// ---------------------------------------------------------------------------

pub fn run_setup(output_path: Option<PathBuf>) -> anyhow::Result<()> {
    if !is_interactive() {
        print_noninteractive_guide();
        return Ok(());
    }

    println!();
    cprintln!(
        C.magenta,
        "============================================================"
    );
    cprintln!(
        C.magenta,
        "           OpenAB Interactive Setup Wizard                  "
    );
    cprintln!(
        C.magenta,
        "============================================================"
    );

    // Step 1: Discord Guide + Token
    section_discord_guide();
    println!();
    let bot_token = prompt_password("  Bot Token (or press Enter to skip)");
    if bot_token.is_empty() {
        cprintln!(C.yellow, "  Skipped. Set bot_token manually in config.toml");
        println!();
        cprintln!(
            C.green,
            "  Setup complete! Edit config.toml to add your bot token."
        );
        return Ok(());
    }
    validate_bot_token(&bot_token)?;

    let client = DiscordClient::new(&bot_token);
    print!("  Verifying token with Discord API... ");
    io::stdout().flush().ok();
    let (_bot_id, bot_username) = client.verify_token()?;
    cprintln!(C.green, "OK Logged in as {}", bot_username);

    // Step 2: Channels
    let channel_ids = match section_channels(&client) {
        Ok(ids) if !ids.is_empty() => ids,
        Ok(_) => {
            cprintln!(C.yellow, "  No channels selected.");
            vec![]
        }
        Err(e) => {
            cprintln!(C.yellow, "  Channel fetch failed: {}. Enter manually.", e);
            let input = prompt("  Channel ID(s), comma-separated");
            let ids: Vec<String> = input
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            for id in &ids {
                validate_channel_id(id).map_err(|e| anyhow::anyhow!("{}", e))?;
            }
            ids
        }
    };

    // Step 3: Agent
    let (agent, working_dir, is_local) = section_agent();

    // Step 4: Pool
    let (max_sessions, ttl_hours) = section_pool();

    // Generate
    let config_content = generate_config(
        &bot_token,
        &agent,
        channel_ids,
        &working_dir,
        max_sessions,
        ttl_hours,
    );

    // Output
    let output_path = output_path.unwrap_or_else(|| PathBuf::from("config.toml"));
    section_preview_and_save(&config_content, &output_path)?;

    print_next_steps(&agent, &output_path, is_local);

    Ok(())
}
