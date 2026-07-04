# <Platform> — platform notes

**Schema version:** 2026-07-04

Engineering-facing capability & quirks reference for the <Platform> adapter. For operator setup see `docs/<platform>.md`. Follows the schemas in [`README.md`](./README.md).

## 1. Platform capability (`platform-capability`)

| Field | Value | Source |
|---|---|---|
| transport | | |
| inbound_auth | | |
| threads | | |
| slash_commands | | |
| mentions | | |
| emoji_reactions | | |
| edit_message | | |
| delete_message | | |
| rich_content | | |
| attachments | | |
| message_length_limit | | |
| dm_support | | |
| group_model | | |
| group_sender_identity | | |
| send_model | | |
| proactive_push | | |
| bot_to_bot | | |
| typing_indicator | | |

## 2. OpenAB feature support (`openab-feature-support`)

| Feature | Status | Note | Ref |
|---|---|---|---|
| send_message | | | |
| message_split/chunking | | | |
| streaming | | | |
| reply/quote | | | |
| edit_message | | | |
| delete_message | | | |
| emoji_reactions | | | |
| threads/topics | | | |
| media_inbound | | | |
| voice_stt | | | |
| trust_gate | | | |
| deny_echo | | | |
| mention_gating | | | |
| slash_commands | | | |
| multibot | | | |
| group_routing | | | |

## 3. Platform quirks (`platform-quirks`)

### <Quirk title>
<free-form>

### Findings log
- YYYY-MM-DD (A|B) <finding>. [source]
