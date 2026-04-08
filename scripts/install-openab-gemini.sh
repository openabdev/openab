#!/usr/bin/env bash
set -euo pipefail

DISCORD_BOT_TOKEN="${DISCORD_BOT_TOKEN:-replace_with_your_discord_bot_token}"
DISCORD_CHANNEL_ID="${DISCORD_CHANNEL_ID:-replace_with_your_discord_channel_id}"
GEMINI_API_KEY="${GEMINI_API_KEY:-replace_with_your_gemini_api_key}"
OPENAB_REF="${OPENAB_REF:-main}"

APP_USER="openab"
APP_GROUP="openab"
APP_HOME="/home/${APP_USER}"
APP_DIR="/opt/openab"
ETC_DIR="/etc/openab"
SRC_DIR="/tmp/openab-src"

if [[ "$DISCORD_BOT_TOKEN" == "replace_with_your_discord_bot_token" ]]; then
  echo "DISCORD_BOT_TOKEN is not set"
  exit 1
fi

if [[ "$DISCORD_CHANNEL_ID" == "replace_with_your_discord_channel_id" ]]; then
  echo "DISCORD_CHANNEL_ID is not set"
  exit 1
fi

if [[ "$GEMINI_API_KEY" == "replace_with_your_gemini_api_key" ]]; then
  echo "GEMINI_API_KEY is not set"
  exit 1
fi

if [[ $EUID -ne 0 ]]; then
  echo "Please run this script as root or with sudo"
  exit 1
fi

export DEBIAN_FRONTEND=noninteractive

apt update
apt install -y build-essential pkg-config libssl-dev curl git ca-certificates

if ! command -v node >/dev/null 2>&1; then
  curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
  apt install -y nodejs
fi

if [[ ! -x /root/.cargo/bin/cargo ]]; then
  curl https://sh.rustup.rs -sSf | sh -s -- -y
fi

source /root/.cargo/env

if ! id -u "${APP_USER}" >/dev/null 2>&1; then
  useradd -m -s /bin/bash "${APP_USER}"
fi

mkdir -p "${APP_DIR}" "${ETC_DIR}"
chown -R "${APP_USER}:${APP_GROUP}" "${APP_DIR}" "${APP_HOME}"

if [[ -d "${SRC_DIR}/.git" ]]; then
  git -C "${SRC_DIR}" fetch --depth=1 origin "${OPENAB_REF}"
else
  rm -rf "${SRC_DIR}"
  git clone --depth=1 --branch "${OPENAB_REF}" https://github.com/Joseph19820124/openab "${SRC_DIR}"
fi

git -C "${SRC_DIR}" checkout -f FETCH_HEAD 2>/dev/null || git -C "${SRC_DIR}" checkout -f "${OPENAB_REF}"

cd "${SRC_DIR}"
cargo build --release

install -o "${APP_USER}" -g "${APP_GROUP}" -m 0755 target/release/openab "${APP_DIR}/openab"

if ! command -v gemini >/dev/null 2>&1; then
  npm install -g @google/gemini-cli
fi

cat > "${ETC_DIR}/config.toml" <<CFG
[discord]
bot_token = "\${DISCORD_BOT_TOKEN}"
allowed_channels = ["${DISCORD_CHANNEL_ID}"]

[agent]
command = "gemini"
args = ["--acp"]
working_dir = "${APP_HOME}"
env = { GEMINI_API_KEY = "\${GEMINI_API_KEY}" }

[pool]
max_sessions = 10
session_ttl_hours = 24

[reactions]
enabled = true
remove_after_reply = false
CFG

cat > "${ETC_DIR}/openab.env" <<ENV
DISCORD_BOT_TOKEN=${DISCORD_BOT_TOKEN}
GEMINI_API_KEY=${GEMINI_API_KEY}
HOME=${APP_HOME}
PATH=/usr/local/bin:/usr/bin:/bin
ENV

chmod 600 "${ETC_DIR}/openab.env"

cat > /etc/systemd/system/openab.service <<SERVICE
[Unit]
Description=OpenAB Discord Agent Broker
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=${APP_USER}
Group=${APP_GROUP}
WorkingDirectory=${APP_DIR}
EnvironmentFile=${ETC_DIR}/openab.env
ExecStart=${APP_DIR}/openab ${ETC_DIR}/config.toml
Restart=always
RestartSec=5

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=false

[Install]
WantedBy=multi-user.target
SERVICE

systemctl daemon-reload
systemctl enable --now openab

echo
echo "Installation completed"
echo "Installed ref: ${OPENAB_REF}"
echo "Check status: systemctl status openab"
echo "View logs: journalctl -u openab -f"
echo "Test Gemini CLI: sudo -u ${APP_USER} -H gemini --help"
