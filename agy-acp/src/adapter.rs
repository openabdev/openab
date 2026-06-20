use fs2::FileExt;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

use crate::types::*;

pub struct Adapter {
    pub sessions: HashMap<String, Session>,
    pub working_dir: String,
    pub conversations_dir: PathBuf,
    pub state_file: PathBuf,
    pub available_models: Option<Vec<String>>,
}

impl Adapter {
    pub fn new() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let state_dir = PathBuf::from(&home).join(".openab/agy-acp");
        Self {
            sessions: HashMap::new(),
            working_dir: std::env::current_dir()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|_| "/tmp".to_string()),
            conversations_dir: PathBuf::from(&home).join(".gemini/antigravity-cli/conversations"),
            state_file: state_dir.join("sessions.json"),
            available_models: None,
        }
    }

    // --- Model cache ---

    pub fn models_cache_path(&self) -> PathBuf {
        self.state_file.with_file_name("models_cache.json")
    }

    pub fn load_cached_models(&self) -> Option<Vec<String>> {
        let path = self.models_cache_path();
        let content = fs::read_to_string(&path).ok()?;
        serde_json::from_str::<Vec<String>>(&content).ok().filter(|v| !v.is_empty())
    }

    pub fn save_models_cache(&self, models: &[String]) {
        if let Some(parent) = self.models_cache_path().parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string(models) {
            let tmp = self.models_cache_path().with_extension("tmp");
            if fs::write(&tmp, &json).is_ok() {
                let _ = fs::rename(&tmp, self.models_cache_path());
            }
        }
    }

    pub fn static_fallback_models() -> Vec<String> {
        vec![
            "Gemini 3.5 Flash (Medium)".to_string(),
            "Gemini 3.5 Flash (High)".to_string(),
            "Gemini 3.5 Flash (Low)".to_string(),
            "Gemini 3.1 Pro (Low)".to_string(),
            "Gemini 3.1 Pro (High)".to_string(),
        ]
    }

    /// Resolve the `agy` binary path.
    ///
    /// Order: `AGY_BIN` env override → first existing common install path
    /// (`~/.local/bin`, `/usr/local/bin`, `/opt/homebrew/bin`) → bare `agy`
    /// (PATH lookup). The previous hardcoded `/usr/local/bin/agy` broke local
    /// installs where the binary lives under `~/.local/bin` — the spawn failed
    /// with exit 127 and surfaced in Zed as a broken/auth error.
    pub fn agy_bin() -> String {
        if let Ok(custom) = std::env::var("AGY_BIN") {
            if !custom.is_empty() {
                return custom;
            }
        }
        let home = std::env::var("HOME").unwrap_or_default();
        let candidates = [
            format!("{home}/.local/bin/agy"),
            "/usr/local/bin/agy".to_string(),
            "/opt/homebrew/bin/agy".to_string(),
        ];
        for c in candidates {
            if std::path::Path::new(&c).is_file() {
                return c;
            }
        }
        "agy".to_string()
    }

    /// Build PATH with common agent binary locations prepended.
    pub fn augmented_path() -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/agent".to_string());
        let base = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
        format!("{home}/bin:{home}/.local/bin:{home}/.local/share/fnm/aliases/default/bin:{base}")
    }

    pub fn fetch_available_models() -> Vec<String> {
        std::process::Command::new(Self::agy_bin())
            .arg("models")
            .env("PATH", Self::augmented_path())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_available_models(&mut self) -> &[String] {
        if self.available_models.is_none() {
            let models = Self::fetch_available_models();
            if !models.is_empty() {
                eprintln!("[agy-acp] fetched {} models from `agy models`, updating cache", models.len());
                self.save_models_cache(&models);
                self.available_models = Some(models);
            } else if let Some(cached) = self.load_cached_models() {
                eprintln!("[agy-acp] `agy models` failed, using cached model list ({} models)", cached.len());
                self.available_models = Some(cached);
            } else {
                eprintln!("[agy-acp] `agy models` failed and no cache found, using hardcoded fallback");
                self.available_models = Some(Self::static_fallback_models());
            }
        }
        self.available_models.as_ref().unwrap()
    }

    pub fn config_options_json(&mut self, model_id: Option<&str>) -> Value {
        let models = self.get_available_models();
        if models.is_empty() {
            return json!([]);
        }
        let current = model_id
            .or_else(|| models.first().map(|s| s.as_str()))
            .unwrap_or("");
        let options: Vec<Value> = models
            .iter()
            .map(|name| json!({ "value": name, "name": name }))
            .collect();
        json!([{
            "id": "model",
            "name": "Model",
            "category": "model",
            "type": "select",
            "currentValue": current,
            "options": options,
        }])
    }

    // --- State persistence ---

    fn lock_state_file(&self) -> Option<fs::File> {
        if let Some(parent) = self.state_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let lock_path = self.state_file.with_extension("lock");
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .ok()?;
        lock_file.lock_exclusive().ok()?;
        Some(lock_file)
    }

    fn load_store_inner(&self) -> SessionStore {
        let Some(file) = fs::File::open(&self.state_file).ok() else {
            return SessionStore::default();
        };
        serde_json::from_reader(&file).unwrap_or_default()
    }

    pub fn load_store(&self) -> SessionStore {
        let _lock = self.lock_state_file();
        self.load_store_inner()
    }

    pub fn restore_session(&self, session_id: &str) -> Option<(String, i64, Option<String>)> {
        let store = self.load_store();
        store.sessions.get(session_id).and_then(|s| {
            s.conversation_id.clone().map(|cid| (cid, s.last_step_idx, s.model_id.clone()))
        })
    }

    pub fn persist_session(&self, session_id: &str, conversation_id: Option<&str>, last_step_idx: i64, model_id: Option<&str>) {
        let Some(_lock) = self.lock_state_file() else { return; };
        let mut store = self.load_store_inner();
        store.sessions.insert(
            session_id.to_string(),
            StoredSession {
                conversation_id: conversation_id.map(String::from),
                last_step_idx,
                model_id: model_id.map(String::from),
            },
        );
        let tmp = self.state_file.with_extension("tmp");
        if let Ok(file) = fs::File::create(&tmp) {
            if serde_json::to_writer_pretty(&file, &store).is_ok() {
                let _ = fs::rename(&tmp, &self.state_file);
            }
        }
    }

    // --- Conversation snapshot ---

    pub fn conversation_snapshot(&self) -> HashSet<String> {
        let Ok(entries) = fs::read_dir(&self.conversations_dir) else {
            return HashSet::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let path = e.path();
                if path.extension().map(|x| x == "db").unwrap_or(false) {
                    path.file_stem().map(|s| s.to_string_lossy().to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    #[allow(dead_code)]
    pub fn new_conversation_id(&self, before: &HashSet<String>) -> Option<String> {
        let after = self.conversation_snapshot();
        let mut created: Vec<_> = after.difference(before).collect();
        if created.is_empty() { return None; }
        if created.len() > 1 {
            eprintln!("[agy-acp] WARN: multiple new agy conversation files appeared; refusing to bind");
            return None;
        }
        Some(created.remove(0).clone())
    }

    // --- Session management ---

    pub fn evict_if_needed(&mut self) {
        const MAX_SESSIONS: usize = 64;
        while self.sessions.len() >= MAX_SESSIONS {
            if let Some(key) = self.sessions.keys().next().cloned() {
                self.sessions.remove(&key);
            }
        }
    }

    pub fn restore_session_state(&mut self, session_id: &str) -> bool {
        let Some((conversation_id, last_step_idx, model_id)) = self.restore_session(session_id) else {
            return false;
        };
        if !self.sessions.contains_key(session_id) {
            self.evict_if_needed();
        }
        self.sessions.insert(
            session_id.to_string(),
            Session { conversation_id: Some(conversation_id), last_step_idx, model_id },
        );
        true
    }

    // --- Slash commands (ACP availableCommands) ---

    /// Commands advertised to the client via `available_commands_update`.
    ///
    /// Only commands the bridge can actually fulfil in non-interactive (`-p`)
    /// mode are listed. agy's TUI-only commands (`/compact`, `/btw`…)
    /// require a real terminal (bubbletea TTY) and cannot be bridged here.
    pub fn available_commands_json() -> Value {
        json!([
            { "name": "help",      "description": "Köprü komutları hakkında yardım" },
            { "name": "models",    "description": "Kullanılabilir modelleri listele" },
            { "name": "changelog", "description": "agy sürüm notlarını göster" },
            { "name": "plugins",   "description": "Yüklü agy eklentilerini listele" },
            { "name": "usage",     "description": "Modellerin kullanım istatistiklerini göster" },
            { "name": "new",       "description": "Yeni konuşma başlat (bağlamı temizle)" }
        ])
    }

    /// Build an `available_commands_update` session notification for a session.
    pub fn available_commands_notification(session_id: &str) -> JsonRpcNotification {
        JsonRpcNotification {
            jsonrpc: "2.0",
            method: "session/update".to_string(),
            params: json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "available_commands_update",
                    "availableCommands": Self::available_commands_json(),
                },
            }),
        }
    }

    /// Run an `agy` subcommand non-interactively and capture its output.
    fn run_agy_subcommand(args: &[&str]) -> String {
        match std::process::Command::new(Self::agy_bin())
            .args(args)
            .env("PATH", Self::augmented_path())
            .output()
        {
            Ok(o) => {
                let mut s = String::from_utf8_lossy(&o.stdout).trim_end().to_string();
                if s.is_empty() {
                    let err = String::from_utf8_lossy(&o.stderr);
                    if !err.trim().is_empty() {
                        s = err.trim_end().to_string();
                    }
                }
                if s.is_empty() {
                    s = format!("(çıktı yok — exit: {})", o.status);
                }
                s
            }
            Err(e) => format!("Komut çalıştırılamadı: {e}"),
        }
    }

    fn help_text() -> String {
        "Kullanılabilir komutlar (agy-acp köprüsü):\n\
         • /help — bu yardım\n\
         • /models — kullanılabilir modeller\n\
         • /changelog — agy sürüm notları\n\
         • /plugins — yüklü eklentiler\n\
         • /usage — model kullanım istatistikleri\n\
         • /new — yeni konuşma başlat (bağlamı temizle)\n\n\
         Not: agy'nin /compact, /btw gibi komutları gerçek bir terminal \
         (TTY) gerektirir ve bu köprü üzerinden kullanılamaz."
            .to_string()
    }

    /// Build a simple usage report from persisted session state.
    pub fn usage_report(&self, on_status: Option<&dyn Fn(&str)>) -> String {
        use std::process::Command;

        let make_progress_bar = |percentage: f64| -> String {
            let filled = (percentage / 10.0).round().clamp(0.0, 10.0) as usize;
            let empty = 10 - filled;
            format!("{}{}", "█".repeat(filled), "░".repeat(empty))
        };

        if let Some(cb) = on_status { cb("Lokal agy portları taranıyor..."); }

        // Find active agy ports using lsof
        let lsof_cmd = "lsof -i -P -n 2>/dev/null | grep \"agy.*LISTEN\" | awk '{print $9}' | grep -oE '[0-9]+$' | sort -u";
        let output = Command::new("sh")
            .arg("-c")
            .arg(lsof_cmd)
            .output();

        let mut ports = Vec::new();
        if let Ok(out) = output {
            let stdout = String::from_utf8_lossy(&out.stdout);
            for line in stdout.lines() {
                if let Ok(port) = line.trim().parse::<u16>() {
                    ports.push(port);
                }
            }
        }

        // Add some default fallback ports if lsof fails or returns empty
        if ports.is_empty() {
            ports = vec![54644, 54724, 54808, 54883, 54933];
        }

        if let Some(cb) = on_status { cb("Modellerin kota bilgileri canlı API üzerinden alınıyor..."); }

        let mut quota_json = None;
        for &port in &ports {
            let url = format!("http://127.0.0.1:{}/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary", port);
            let out = Command::new("curl")
                .arg("-s")
                .arg("--connect-timeout")
                .arg("1")
                .arg("--max-time")
                .arg("2")
                .arg("-X")
                .arg("POST")
                .arg(&url)
                .arg("-H")
                .arg("Content-Type: application/json")
                .arg("-d")
                .arg("{}")
                .output();

            if let Ok(res) = out {
                if res.status.success() {
                    if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&res.stdout) {
                        if json.pointer("/response/groups").is_some() {
                            quota_json = Some(json);
                            break;
                        }
                    }
                }
            }
        }

        if let Some(json) = quota_json {
            if let Some(cb) = on_status { cb("Kota bilgileri çözümleniyor..."); }
            
            let mut report = String::from("### Modellerin Kullanım Bilgileri (Canlı API)\n\n");
            report.push_str("| Grup / Model | Limit Tipi | Kalan Kota | Durum |\n");
            report.push_str("| :--- | :--- | :---: | :--- |\n");
            
            if let Some(groups) = json.pointer("/response/groups").and_then(|g| g.as_array()) {
                for group in groups {
                    let group_name = group.get("displayName").and_then(|v| v.as_str()).unwrap_or("Bilinmeyen Grup");
                    
                    if let Some(buckets) = group.get("buckets").and_then(|b| b.as_array()) {
                        for bucket in buckets {
                            let bucket_name = bucket.get("displayName").and_then(|v| v.as_str()).unwrap_or("Limit");
                            let desc = bucket.get("description").and_then(|v| v.as_str()).unwrap_or("");
                            let remaining = bucket.get("remainingFraction").and_then(|v| v.as_f64()).unwrap_or(0.0);
                            
                            let percentage = remaining * 100.0;
                            let bar = make_progress_bar(percentage);
                            
                            let limit_name = if !desc.is_empty() {
                                format!("{} ({})", bucket_name, desc)
                            } else {
                                bucket_name.to_string()
                            };
                            
                            report.push_str(&format!(
                                "| **{}** | {} | %{:.2} | `{}` |\n",
                                group_name.to_uppercase(),
                                limit_name,
                                percentage,
                                bar
                            ));
                        }
                    }
                }
                return report;
            }
        }

        // Fallback to the old screen-based scraper method if local API fails
        if let Some(cb) = on_status { cb("Canlı API başarısız oldu, eski screen-based yöntemi deneniyor..."); }
        self.fallback_screen_usage_report(on_status)
    }

    /// Fallback method to build a usage report using screen-based scraping if the API fails.
    pub fn fallback_screen_usage_report(&self, on_status: Option<&dyn Fn(&str)>) -> String {
        use std::process::Command;
        use std::fs;

        let sess_id = format!("agy_usage_{}", uuid::Uuid::new_v4().to_string().chars().take(8).collect::<String>());
        let screenrc_file = format!("/tmp/screenrc_{}", sess_id);
        let out_file_1 = format!("/tmp/page1_{}.txt", sess_id);
        let out_file_2 = format!("/tmp/page2_{}.txt", sess_id);

        // Write temporary screenrc file to enforce a terminal size (necessary for TUI rendering)
        let screenrc_content = "width 80\nheight 40\n";
        if fs::write(&screenrc_file, screenrc_content).is_err() {
            return String::from("Hata: geçici screenrc dosyası oluşturulamadı.");
        }

        let agy_bin = Self::agy_bin();
        let path = Self::augmented_path();

        let run_sh = |cmd: &str| -> bool {
            Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .env("PATH", &path)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        };

        if let Some(cb) = on_status { cb("Kota oturumu arka planda başlatılıyor..."); }
        let init_cmd = format!("screen -c {} -d -m -S {} {}", screenrc_file, sess_id, agy_bin);
        if !run_sh(&init_cmd) {
            let _ = fs::remove_file(&screenrc_file);
            return String::from("Hata: screen oturumu başlatılamadı.");
        }

        std::thread::sleep(std::time::Duration::from_secs(6));

        if let Some(cb) = on_status { cb("Kota ekranına geçmek için /usage komutu gönderiliyor..."); }
        let usage_cmd = format!("screen -S {} -p 0 -X stuff \"/usage\"$'\\r'", sess_id);
        let _ = run_sh(&usage_cmd);

        std::thread::sleep(std::time::Duration::from_secs(4));

        if let Some(cb) = on_status { cb("Gemini kota bilgileri alınıyor (Sayfa 1)..."); }
        let hardcopy1_cmd = format!("screen -S {} -p 0 -X hardcopy {}", sess_id, out_file_1);
        let _ = run_sh(&hardcopy1_cmd);

        let pgdown_cmd = format!("screen -S {} -p 0 -X stuff $'\\033[6~'", sess_id);
        let _ = run_sh(&pgdown_cmd);

        std::thread::sleep(std::time::Duration::from_secs(2));

        if let Some(cb) = on_status { cb("Claude/GPT kota bilgileri alınıyor (Sayfa 2)..."); }
        let hardcopy2_cmd = format!("screen -S {} -p 0 -X hardcopy {}", sess_id, out_file_2);
        let _ = run_sh(&hardcopy2_cmd);

        let quit_cmd = format!("screen -S {} -p 0 -X quit", sess_id);
        let _ = run_sh(&quit_cmd);

        if let Some(cb) = on_status { cb("Kota bilgileri çözümleniyor (parse ediliyor)..."); }

        // Helper function to clean screen bytes
        let clean_screen_output = |bytes: &[u8]| -> String {
            let mut result = String::new();
            for &b in bytes {
                match b {
                    0x0 => {} // skip null bytes
                    0x88 => result.push_str("█"), // filled block
                    0x91 => result.push_str("░"), // empty block
                    0xb7 => result.push_str("·"), // bullet dot
                    _ => {
                        if b.is_ascii() {
                            result.push(b as char);
                        } else {
                            result.push_str(&String::from_utf8_lossy(&[b]));
                        }
                    }
                }
            }
            result
        };

        // Read files
        let page1_bytes = fs::read(&out_file_1).unwrap_or_default();
        let page2_bytes = fs::read(&out_file_2).unwrap_or_default();

        let page1_str = clean_screen_output(&page1_bytes);
        let page2_str = clean_screen_output(&page2_bytes);

        // Cleanup temp files
        let _ = fs::remove_file(&screenrc_file);
        let _ = fs::remove_file(&out_file_1);
        let _ = fs::remove_file(&out_file_2);

        // Helper functions for parsing
        let extract_percentage = |line: &str| -> Option<String> {
            let bytes = line.as_bytes();
            if let Some(pos) = line.find('%') {
                let mut start = pos;
                while start > 0 {
                    let prev = bytes[start - 1];
                    if prev.is_ascii_digit() || prev == b'.' {
                        start -= 1;
                    } else {
                        break;
                    }
                }
                if start < pos {
                    return Some(String::from_utf8_lossy(&bytes[start..=pos]).to_string());
                }
            }
            None
        };

        let extract_refresh_time = |line: &str| -> Option<String> {
            if let Some(pos) = line.find("Refreshes in ") {
                return Some(line[pos + "Refreshes in ".len()..].trim().to_string());
            }
            if line.contains("Quota available") {
                return Some("Mevcut".to_string());
            }
            None
        };

        let parse_limits = |content: &str| -> Option<(String, String, String, String)> {
            let mut weekly_pct = None;
            let mut weekly_ref = None;
            let mut five_hour_pct = None;
            let mut five_hour_ref = None;

            let lines: Vec<&str> = content.lines().map(|l| l.trim()).collect();
            
            for i in 0..lines.len() {
                if lines[i] == "Weekly Limit" {
                    for j in (i + 1)..std::cmp::min(i + 4, lines.len()) {
                        if weekly_pct.is_none() {
                            if let Some(pct) = extract_percentage(lines[j]) {
                                weekly_pct = Some(pct);
                            }
                        }
                        if weekly_ref.is_none() {
                            if let Some(ref_time) = extract_refresh_time(lines[j]) {
                                weekly_ref = Some(ref_time);
                            }
                        }
                    }
                }
                if lines[i] == "Five Hour Limit" {
                    for j in (i + 1)..std::cmp::min(i + 4, lines.len()) {
                        if five_hour_pct.is_none() {
                            if let Some(pct) = extract_percentage(lines[j]) {
                                five_hour_pct = Some(pct);
                            }
                        }
                        if five_hour_ref.is_none() {
                            if let Some(ref_time) = extract_refresh_time(lines[j]) {
                                five_hour_ref = Some(ref_time);
                            }
                        }
                    }
                }
            }
            
            if let (Some(w_pct), Some(w_ref), Some(f_pct), Some(f_ref)) = (weekly_pct, weekly_ref, five_hour_pct, five_hour_ref) {
                Some((w_pct, w_ref, f_pct, f_ref))
            } else {
                None
            }
        };

        // Try to parse both pages
        let gemini_parsed = parse_limits(&page1_str);
        let claude_parsed = parse_limits(&page2_str);

        match (gemini_parsed, claude_parsed) {
            (Some(g), Some(c)) => {
                let make_progress_bar = |percentage: f64| -> String {
                    let filled = (percentage / 10.0).round().clamp(0.0, 10.0) as usize;
                    let empty = 10 - filled;
                    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
                };

                let to_pct = |s: &str| -> f64 {
                    s.replace("%", "").trim().parse::<f64>().unwrap_or(0.0)
                };

                let g_w_pct = to_pct(&g.0);
                let g_f_pct = to_pct(&g.2);
                let c_w_pct = to_pct(&c.0);
                let c_f_pct = to_pct(&c.2);
                
                let g_w_bar = make_progress_bar(g_w_pct);
                let g_f_bar = make_progress_bar(g_f_pct);
                let c_w_bar = make_progress_bar(c_w_pct);
                let c_f_bar = make_progress_bar(c_f_pct);
                
                format!(
                    "### Modellerin Kullanım Bilgileri (Canlı API fallback)\n\n\
                     | Grup / Model | Limit Tipi | Kalan Kota | Durum | Yenilenme |\n\
                     | :--- | :--- | :---: | :--- | :--- |\n\
                     | **GEMINI** | Haftalık Limit | %{:.2} | `{}` | {} |\n\
                     | **GEMINI** | 5 Saatlik Limit | %{:.2} | `{}` | {} |\n\
                     | **CLAUDE/GPT** | Haftalık Limit | %{:.2} | `{}` | {} |\n\
                     | **CLAUDE/GPT** | 5 Saatlik Limit | %{:.2} | `{}` | {} |",
                    g_w_pct, g_w_bar, g.1,
                    g_f_pct, g_f_bar, g.3,
                    c_w_pct, c_w_bar, c.1,
                    c_f_pct, c_f_bar, c.3
                )
            }
            _ => {
                if page1_str.trim().is_empty() && page2_str.trim().is_empty() {
                    String::from("Hata: Kota bilgisi boş döndü. agy henüz giriş yapmamış olabilir veya API yanıt vermedi.")
                } else {
                    format!(
                        "Kota bilgileri parse edilemedi.\n\n\
                         --- SAYFA 1 HAM ÇIKTI ---\n{}\n\n\
                         --- SAYFA 2 HAM ÇIKTI ---\n{}",
                        page1_str, page2_str
                    )
                }
            }
        }
    }

    /// If `prompt_text` is a recognised slash command, execute it and return
    /// its output. Returns `None` for normal prompts (forwarded to agy as usual).
    pub fn try_handle_command(&mut self, session_id: &str, prompt_text: &str, on_status: Option<&dyn Fn(&str)>) -> Option<String> {
        let rest = prompt_text.trim().strip_prefix('/')?;
        let mut parts = rest.splitn(2, char::is_whitespace);
        let cmd = parts.next().unwrap_or("").to_lowercase();
        match cmd.as_str() {
            "help" => Some(Self::help_text()),
            "models" => Some(Self::run_agy_subcommand(&["models"])),
            "changelog" => Some(Self::run_agy_subcommand(&["changelog"])),
            "plugins" | "plugin" => Some(Self::run_agy_subcommand(&["plugin", "list"])),
            "usage" => Some(self.usage_report(on_status)),
            "new" | "clear" => {
                if !self.sessions.contains_key(session_id) {
                    let _ = self.restore_session_state(session_id);
                }
                if let Some(s) = self.sessions.get_mut(session_id) {
                    s.conversation_id = None;
                    s.last_step_idx = -1;
                }
                let model_id = self.sessions.get(session_id).and_then(|s| s.model_id.clone());
                self.persist_session(session_id, None, -1, model_id.as_deref());
                Some("✓ Yeni konuşma başlatıldı. Önceki bağlam temizlendi.".to_string())
            }
            _ => None,
        }
    }

    // --- JSON-RPC handlers ---

    pub fn handle_initialize(&self, id: Value) -> JsonRpcResponse {
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({
                "protocolVersion": 1,
                "agentInfo": { "name": "agy", "version": env!("CARGO_PKG_VERSION") },
                "agentCapabilities": {
                    "streaming": true,
                    "loadSession": true,
                    "promptCapabilities": {
                        "image": true,
                        "embeddedContext": true
                    }
                },
            })),
            error: None,
        }
    }

    pub fn handle_session_new(&mut self, id: Value) -> JsonRpcResponse {
        let session_id = Uuid::new_v4().to_string();
        self.evict_if_needed();
        self.sessions.insert(session_id.clone(), Session {
            conversation_id: None, last_step_idx: -1, model_id: None,
        });
        let config_options = self.config_options_json(None);
        JsonRpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(json!({ "sessionId": session_id, "configOptions": config_options })),
            error: None,
        }
    }

    pub fn handle_session_load(&mut self, id: Value, params: &Value) -> JsonRpcResponse {
        let session_id = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        if session_id.is_empty() {
            return JsonRpcResponse { jsonrpc: "2.0", id, result: None,
                error: Some(json!({"code":-32602,"message":"missing sessionId"})) };
        }
        if self.restore_session_state(session_id) {
            let model_id = self.sessions.get(session_id).and_then(|s| s.model_id.clone());
            let config_options = self.config_options_json(model_id.as_deref());
            return JsonRpcResponse { jsonrpc: "2.0", id,
                result: Some(json!({ "sessionId": session_id, "configOptions": config_options })), error: None };
        }
        JsonRpcResponse { jsonrpc: "2.0", id, result: None,
            error: Some(json!({"code":-32000,"message":format!("unknown sessionId: {session_id}")})) }
    }

    pub fn handle_session_set_config_option(&mut self, id: Value, params: &Value) -> JsonRpcResponse {
        let session_id = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("");
        let config_id = params.get("configId").and_then(|v| v.as_str()).unwrap_or("");
        let value = params.get("value").and_then(|v| v.as_str()).unwrap_or("");

        if session_id.is_empty() || config_id != "model" || value.is_empty() {
            return JsonRpcResponse { jsonrpc: "2.0", id, result: None,
                error: Some(json!({"code":-32602,"message":"missing sessionId, configId, or value"})) };
        }
        if !self.sessions.contains_key(session_id) {
            let _ = self.restore_session_state(session_id);
        }
        let Some(session) = self.sessions.get_mut(session_id) else {
            return JsonRpcResponse { jsonrpc: "2.0", id, result: None,
                error: Some(json!({"code":-32000,"message":format!("unknown sessionId: {session_id}")})) };
        };
        session.model_id = Some(value.to_string());
        let conv_id = session.conversation_id.clone();
        let last_step_idx = session.last_step_idx;
        self.persist_session(session_id, conv_id.as_deref(), last_step_idx, Some(value));
        let config_options = self.config_options_json(Some(value));
        JsonRpcResponse { jsonrpc: "2.0", id, result: Some(json!({ "configOptions": config_options })), error: None }
    }

    /// Gather session state needed for prompt execution (under lock).
    pub fn prepare_prompt_state(
        &mut self,
        params: &Value,
    ) -> (String, String, Vec<String>, Option<HashSet<String>>, Option<String>, i64, Vec<String>) {
        let session_id = params.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();

        if !session_id.is_empty() && !self.sessions.contains_key(&session_id) {
            let _ = self.restore_session_state(&session_id);
        }

        let (prompt_text, temp_files) = parse_prompt_and_extract_media(params.get("prompt"));
        let clean_prompt = prompt_text.trim().to_string();

        let snapshot = if self.sessions.get(&session_id).map(|s| s.conversation_id.is_none()).unwrap_or(false) {
            Some(self.conversation_snapshot())
        } else {
            None
        };

        let mut args: Vec<String> = Vec::new();
        args.push("--add-dir".to_string());
        args.push(self.working_dir.clone());
        if let Ok(extra) = std::env::var("AGY_EXTRA_ARGS") {
            if let Ok(parsed) = shell_words::split(&extra) {
                args.extend(parsed);
            } else {
                eprintln!("[agy-acp] WARN: failed to parse AGY_EXTRA_ARGS, ignoring");
            }
        }
        if let Some(session) = self.sessions.get(&session_id) {
            if let Some(conv_id) = &session.conversation_id {
                args.push("--conversation".to_string());
                args.push(conv_id.clone());
            }
            if let Some(model_id) = &session.model_id {
                args.push("--model".to_string());
                args.push(model_id.clone());
            }
        }
        args.push("-p".to_string());
        args.push(clean_prompt.clone());

        let initial_conv_id = self.sessions.get(&session_id).and_then(|s| s.conversation_id.clone());
        let initial_step_idx = self.sessions.get(&session_id).map(|s| s.last_step_idx).unwrap_or(-1);

        (session_id, clean_prompt, args, snapshot, initial_conv_id, initial_step_idx, temp_files)
    }
}
