# GitHub Webhook to Discord — Agent Trigger Pattern

> **Note:** This documents a v1 workaround using GitHub Actions + Discord webhooks.
> The target architecture (v2+) is the [Custom Gateway](adr/custom-gateway.md) with a
> native GitHub adapter, which provides direct webhook reception, HMAC validation,
> and richer event context. See [ADR: Custom Gateway — Section 5](adr/custom-gateway.md#5-what-this-enables-beyond-chat) for the GitHub integration example.

## Overview

OpenAB only listens to Discord events. It does not accept external webhooks directly. To trigger agents from GitHub events (PR, Issue, etc.), we route through Discord as the single entry point.

## Architecture

```
GitHub (PR/Issue event)
  → GitHub Actions workflow
    → Discord Webhook (formatted message to channel)
      → OpenAB detects message
        → Routes to target agent
          → Agent performs action (review, comment, notify)
```

## Setup

### 1. Discord Webhook

Create a webhook in your Discord server for the target channel/topic:
- Server Settings → Integrations → Webhooks → New Webhook
- Copy the webhook URL

### 2. GitHub Secret

Add the webhook URL as a repository secret:
- Repo → Settings → Secrets and variables → Actions
- Name: `DISCORD_WEBHOOK_URL`
- Value: the webhook URL from step 1

### 3. GitHub Actions Workflow

Add `.github/workflows/notify-discord.yml` to your repo:

```yaml
name: Notify Discord

on:
  pull_request:
    types: [opened, reopened]
  issues:
    types: [opened]

jobs:
  notify:
    runs-on: ubuntu-latest
    steps:
      - name: Send to Discord
        env:
          DISCORD_WEBHOOK_URL: ${{ secrets.DISCORD_WEBHOOK_URL }}
        run: |
          if [ "${{ github.event_name }}" = "pull_request" ]; then
            TITLE="${{ github.event.pull_request.title }}"
            URL="${{ github.event.pull_request.html_url }}"
            AUTHOR="${{ github.event.pull_request.user.login }}"
            NUM="${{ github.event.pull_request.number }}"
            TYPE="pr_opened"
            LABEL="PR #${NUM}"
          else
            TITLE="${{ github.event.issue.title }}"
            URL="${{ github.event.issue.html_url }}"
            AUTHOR="${{ github.event.issue.user.login }}"
            NUM="${{ github.event.issue.number }}"
            TYPE="issue_opened"
            LABEL="Issue #${NUM}"
          fi

          MSG="[GH-EVENT] repo:${{ github.repository }} action:${TYPE} ${LABEL}"
          MSG="${MSG}\n**${TITLE}**\nby ${AUTHOR}\n${URL}"
          PAYLOAD=$(printf '%s' "$MSG" | jq -Rs '{content: .}')
          curl -s -H "Content-Type: application/json" -d "$PAYLOAD" "$DISCORD_WEBHOOK_URL"
```

## Message Format Convention

Messages use a structured prefix so OpenAB can identify GitHub events:

```
[GH-EVENT] repo:{owner/repo} action:{event_type} {PR/Issue} #{number}
**{title}**
by {author}
{url}
```

Example:
```
[GH-EVENT] repo:openabdev/openab action:pr_opened PR #42
**Add webhook integration docs**
by obrutjack
https://github.com/openabdev/openab/pull/42
```

## Open Questions

- **Bot message handling**: Does OpenAB currently ignore messages from bots/webhooks? If so, webhook sources need to be allowlisted. Note: OpenAB uses `allowed_channels` and `allowed_users` in `config.toml` for filtering — webhook messages come from a bot user, so the bot's user ID may need to be added to `allowed_users`, or the filtering logic would need a `[GH-EVENT]` prefix check.
- **Routing**: How does OpenAB determine which agent handles a `[GH-EVENT]` message?
- **Loop prevention**: If an agent replies in the same channel, could it re-trigger events? Recommend using a dedicated channel and filtering by `[GH-EVENT]` prefix only.

## Best Practices

- Use a dedicated channel or thread for webhook events
- Stick to the `[GH-EVENT]` prefix convention for all GitHub-sourced messages
- Validate webhook sources on the Discord side (restrict channel permissions)
- Avoid agents posting back to the same webhook channel to prevent loops
- Start minimal (PR + Issue notifications), expand as needed

## Future Considerations

- Extend pattern to other sources: Jira, Slack, PagerDuty, etc.
- Agent-to-agent invocation during review workflows
- Event filtering and deduplication at the OpenAB level
- Richer payloads using Discord embeds instead of plain text
