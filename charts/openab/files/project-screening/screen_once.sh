#!/usr/bin/env bash
set -euo pipefail

PROJECT_OWNER="${PROJECT_OWNER:-openabdev}"
PROJECT_NUMBER="${PROJECT_NUMBER:-1}"
INCOMING_STATUS_NAME="${INCOMING_STATUS_NAME:-Incoming}"
SCREENING_STATUS_NAME="${SCREENING_STATUS_NAME:-PR-Screening}"
REPORT_TO_STDOUT="${REPORT_TO_STDOUT:-true}"
PROJECT_QUERY_EXTRA="${PROJECT_QUERY_EXTRA:-}"
SENDER_CONTEXT_JSON="${SENDER_CONTEXT_JSON:-}"
PROMPT_TEMPLATE="${PROMPT_TEMPLATE:-/opt/openab-project-screening/screening_prompt.md}"
CODEX_AUTH_JSON_SOURCE="${CODEX_AUTH_JSON_SOURCE:-/opt/openab-project-screening-auth/auth.json}"
DISCORD_BOT_TOKEN="${DISCORD_BOT_TOKEN:-}"
DISCORD_REPORT_CHANNEL_ID="${DISCORD_REPORT_CHANNEL_ID:-}"
WORK_DIR="${WORK_DIR:-/tmp/openab-project-screening}"
HOME_DIR="${HOME:-/tmp/openab-project-screening-home}"

timestamp() {
  date -u +"%Y-%m-%dT%H:%M:%SZ"
}

log() {
  printf '[%s] %s\n' "$(timestamp)" "$*"
}

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    log "missing required command: $1"
    exit 1
  fi
}

require_env() {
  local name="$1"
  if [[ -z "${!name:-}" ]]; then
    log "missing required environment variable: $name"
    exit 1
  fi
}

project_query() {
  if [[ -n "$PROJECT_QUERY_EXTRA" ]]; then
    printf 'status:"%s" %s' "$INCOMING_STATUS_NAME" "$PROJECT_QUERY_EXTRA"
  else
    printf 'status:"%s"' "$INCOMING_STATUS_NAME"
  fi
}

project_view_jq() {
  local jq_expr="$1"
  gh project view "$PROJECT_NUMBER" \
    --owner "$PROJECT_OWNER" \
    --format json \
    --jq "$jq_expr"
}

field_list_jq() {
  local jq_expr="$1"
  gh project field-list "$PROJECT_NUMBER" \
    --owner "$PROJECT_OWNER" \
    --format json \
    --jq "$jq_expr"
}

incoming_item_jq() {
  local jq_expr="$1"
  gh project item-list "$PROJECT_NUMBER" \
    --owner "$PROJECT_OWNER" \
    --query "$(project_query)" \
    --limit 1 \
    --format json \
    --jq "$jq_expr"
}

fetch_content_json() {
  local item_type="$1"
  local item_number="$2"
  local repo="$3"

  case "$item_type" in
    PullRequest)
      gh pr view "$item_number" \
        --repo "$repo" \
        --json title,number,body,author,files,headRefName,baseRefName,url
      ;;
    Issue)
      gh issue view "$item_number" \
        --repo "$repo" \
        --json title,number,body,author,labels,url
      ;;
    *)
      printf '{"type":"%s","number":"%s","repository":"%s"}\n' \
        "$item_type" "$item_number" "$repo"
      ;;
  esac
}

sender_context_field() {
  local field="$1"
  FIELD_NAME="$field" SENDER_CONTEXT_JSON="$SENDER_CONTEXT_JSON" node <<'EOF'
const field = process.env.FIELD_NAME;
const raw = process.env.SENDER_CONTEXT_JSON || "";
if (!raw || !field) process.exit(0);
const obj = JSON.parse(raw);
const value = obj[field];
if (value !== undefined && value !== null) process.stdout.write(String(value));
EOF
}

discord_post_json() {
  local url="$1"
  local payload="$2"
  local response_file status retry_after
  response_file="$(mktemp)"

  while true; do
    status="$(
      curl -sS \
        -o "$response_file" \
        -w '%{http_code}' \
        -X POST \
        -H "Authorization: Bot $DISCORD_BOT_TOKEN" \
        -H "Content-Type: application/json" \
        --data "$payload" \
        "$url"
    )"

    if [[ "$status" == "429" ]]; then
      retry_after="$(
        RESPONSE_FILE="$response_file" node <<'EOF'
const fs = require('fs');
const raw = fs.readFileSync(process.env.RESPONSE_FILE, 'utf8');
const obj = JSON.parse(raw || '{}');
process.stdout.write(String(obj.retry_after ?? '1'));
EOF
      )"
      log "Discord API rate limited; retrying after ${retry_after}s"
      sleep "$retry_after"
      continue
    fi

    if [[ "$status" != 2* ]]; then
      log "Discord API request failed status=$status"
      cat "$response_file" >&2
      rm -f "$response_file"
      return 1
    fi

    cat "$response_file"
    rm -f "$response_file"
    return 0
  done
}

discord_get_json() {
  local url="$1"
  curl -sS \
    -H "Authorization: Bot $DISCORD_BOT_TOKEN" \
    -H "Content-Type: application/json" \
    "$url"
}

discord_message_payload() {
  local content="$1"
  CONTENT="$content" node <<'EOF'
const content = process.env.CONTENT || "";
process.stdout.write(JSON.stringify({
  content,
  allowed_mentions: { parse: [] }
}));
EOF
}

discord_thread_payload() {
  local name="$1"
  THREAD_NAME="$name" node <<'EOF'
const name = process.env.THREAD_NAME || "project-screening-report";
process.stdout.write(JSON.stringify({
  name: name.slice(0, 100),
  auto_archive_duration: 1440
}));
EOF
}

discord_extract_id() {
  local response="$1"
  RESPONSE_JSON="$response" node <<'EOF'
const raw = process.env.RESPONSE_JSON || "{}";
const obj = JSON.parse(raw);
if (obj.id) process.stdout.write(String(obj.id));
EOF
}

discord_resolve_parent_channel_id() {
  local channel_id="$1"
  local response
  response="$(discord_get_json "https://discord.com/api/v10/channels/${channel_id}")"
  RESPONSE_JSON="$response" node <<'EOF'
const obj = JSON.parse(process.env.RESPONSE_JSON || "{}");
const threadTypes = new Set([10, 11, 12]);
if (threadTypes.has(obj.type) && obj.parent_id) {
  process.stdout.write(String(obj.parent_id));
} else if (obj.id) {
  process.stdout.write(String(obj.id));
}
EOF
}

discord_thread_name() {
  local item_number="$1"
  local item_title="$2"
  ITEM_NUMBER="$item_number" ITEM_TITLE="$item_title" node <<'EOF'
const number = process.env.ITEM_NUMBER || "item";
const title = (process.env.ITEM_TITLE || "")
  .replace(/\s+/g, " ")
  .trim();
const base = `Screening: #${number}${title ? ` ${title}` : ""}`.trim();
process.stdout.write(base.slice(0, 100) || `Screening: #${number}`);
EOF
}

post_report_to_discord() {
  local item_number="$1"
  local item_title="$2"
  local item_url="$3"
  local report_file="$4"
  local channel_id starter_content starter_response starter_message_id thread_name thread_response thread_id

  if [[ -z "$DISCORD_BOT_TOKEN" ]]; then
    log "Discord report delivery skipped: DISCORD_BOT_TOKEN not set"
    return 0
  fi

  channel_id="$DISCORD_REPORT_CHANNEL_ID"
  if [[ -z "$channel_id" ]]; then
    channel_id="$(sender_context_field channel_id)"
  fi

  if [[ -z "$channel_id" ]]; then
    log "Discord report delivery skipped: no report channel id available"
    return 0
  fi

  channel_id="$(discord_resolve_parent_channel_id "$channel_id")"
  if [[ -z "$channel_id" ]]; then
    log "Discord report delivery skipped: failed to resolve parent report channel id"
    return 0
  fi

  starter_content="🔍 **PR Screening** — [#${item_number}](${item_url})
${item_title}
Status: moved to ${SCREENING_STATUS_NAME}"
  starter_response="$(
    discord_post_json \
      "https://discord.com/api/v10/channels/${channel_id}/messages" \
      "$(discord_message_payload "$starter_content")"
  )"
  starter_message_id="$(discord_extract_id "$starter_response")"

  if [[ -z "$starter_message_id" ]]; then
    log "Discord report delivery failed: no starter message id returned"
    return 1
  fi

  thread_name="$(discord_thread_name "$item_number" "$item_title")"
  thread_response="$(
    discord_post_json \
      "https://discord.com/api/v10/channels/${channel_id}/messages/${starter_message_id}/threads" \
      "$(discord_thread_payload "$thread_name")"
  )"
  thread_id="$(discord_extract_id "$thread_response")"

  if [[ -z "$thread_id" ]]; then
    log "Discord report delivery failed: no thread id returned"
    return 1
  fi

  while IFS= read -r chunk || [[ -n "$chunk" ]]; do
    [[ -z "$chunk" ]] && continue
    discord_post_json \
      "https://discord.com/api/v10/channels/${thread_id}/messages" \
      "$(discord_message_payload "$chunk")" >/dev/null
  done < <(fold -s -w 1800 "$report_file")

  log "report delivered to Discord thread ${thread_id}"
}

build_prompt() {
  local item_id="$1"
  local item_type="$2"
  local item_number="$3"
  local repo="$4"
  local item_title="$5"
  local item_url="$6"
  local detail_json="$7"
  local prompt_file="$8"

  {
    printf '<sender_context>\n'
    printf '%s\n' "$SENDER_CONTEXT_JSON"
    printf '</sender_context>\n\n'
    cat "$PROMPT_TEMPLATE"
    printf '\n## Board Context\n\n'
    printf -- '- Claimed project item ID: `%s`\n' "$item_id"
    printf -- '- Status transition: `%s` -> `%s`\n' "$INCOMING_STATUS_NAME" "$SCREENING_STATUS_NAME"
    printf -- '- Project owner: `%s`\n' "$PROJECT_OWNER"
    printf -- '- Project number: `%s`\n' "$PROJECT_NUMBER"
    printf -- '- Current expectation: clarify intent, rewrite the implementation prompt, and prepare the item for Masami or Pahud follow-up\n'
    printf '\n## Item Summary\n\n'
    printf -- '- Type: `%s`\n' "$item_type"
    printf -- '- Repository: `%s`\n' "$repo"
    printf -- '- Number: `%s`\n' "$item_number"
    printf -- '- Title: `%s`\n' "$item_title"
    printf -- '- URL: %s\n' "$item_url"
    printf '\n## Source Data\n\n'
    printf '```json\n'
    printf '%s\n' "$detail_json"
    printf '```\n'
  } >"$prompt_file"
}

generate_report() {
  local prompt_file="$1"
  local report_file="$2"

  codex exec \
    --skip-git-repo-check \
    --cd "$WORK_DIR" \
    --sandbox read-only \
    --ephemeral \
    --color never \
    --output-last-message "$report_file" \
    - <"$prompt_file" >/dev/null
}

main() {
  require_cmd bash
  require_cmd gh
  require_cmd codex
  require_cmd curl
  require_cmd node
  require_env GH_TOKEN
  require_env SENDER_CONTEXT_JSON
  if [[ ! -f "$CODEX_AUTH_JSON_SOURCE" ]]; then
    log "missing Codex auth source file: $CODEX_AUTH_JSON_SOURCE"
    exit 1
  fi

  mkdir -p "$WORK_DIR" "$HOME_DIR/.codex"
  export HOME="$HOME_DIR"
  cp "$CODEX_AUTH_JSON_SOURCE" "$HOME/.codex/auth.json"

  local item_id
  item_id="$(incoming_item_jq '.items[0].id // empty')"

  if [[ -z "$item_id" ]]; then
    log "no Incoming items found"
    exit 0
  fi

  local item_type item_number repo item_title item_url
  item_type="$(incoming_item_jq '.items[0].content.type // empty')"
  item_number="$(incoming_item_jq '.items[0].content.number // empty')"
  repo="$(incoming_item_jq '.items[0].content.repository // empty')"
  item_title="$(incoming_item_jq '.items[0].content.title // empty')"
  item_url="$(incoming_item_jq '.items[0].content.url // empty')"

  if [[ -z "$item_type" || -z "$item_number" || -z "$repo" ]]; then
    log "Incoming item is missing required metadata; refusing to claim"
    exit 1
  fi

  local project_id status_field_id screening_option_id
  project_id="$(project_view_jq '.id')"
  status_field_id="$(field_list_jq '.fields[] | select(.name=="Status") | .id')"
  screening_option_id="$(
    field_list_jq ".fields[] | select(.name==\"Status\") | .options[] | select(.name==\"$SCREENING_STATUS_NAME\") | .id"
  )"

  if [[ -z "$project_id" || -z "$status_field_id" || -z "$screening_option_id" ]]; then
    log "failed to resolve project metadata for claim operation"
    exit 1
  fi

  gh project item-edit \
    --id "$item_id" \
    --project-id "$project_id" \
    --field-id "$status_field_id" \
    --single-select-option-id "$screening_option_id" >/dev/null
  log "claimed item $item_id into $SCREENING_STATUS_NAME"

  local detail_json
  detail_json="$(fetch_content_json "$item_type" "$item_number" "$repo")"

  local stamp prompt_file report_file
  stamp="$(date -u +%Y%m%dT%H%M%SZ)"
  prompt_file="$WORK_DIR/${stamp}-prompt.md"
  report_file="$WORK_DIR/${stamp}-report.md"

  build_prompt \
    "$item_id" \
    "$item_type" \
    "$item_number" \
    "$repo" \
    "$item_title" \
    "$item_url" \
    "$detail_json" \
    "$prompt_file"

  generate_report "$prompt_file" "$report_file"
  log "report generated for $repo#$item_number"

  post_report_to_discord "$item_number" "$item_title" "$item_url" "$report_file"

  if [[ "$REPORT_TO_STDOUT" == "true" ]]; then
    printf '%s\n' '--- BEGIN OPENAB PROJECT SCREENING REPORT ---'
    cat "$report_file"
    printf '\n%s\n' '--- END OPENAB PROJECT SCREENING REPORT ---'
  fi
}

main "$@"
