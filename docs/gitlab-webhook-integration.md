# GitLab Webhook to Discord — Agent Trigger Pattern

> **Note:** This documents a v1 workaround using GitLab webhooks + Discord webhooks.
> The target architecture (v2+) is the [Custom Gateway](adr/custom-gateway.md) with a
> native GitLab adapter, which provides direct webhook reception, HMAC validation,
> and richer event context. See [ADR: Custom Gateway — Section 5](adr/custom-gateway.md#5-what-this-enables-beyond-chat) for the GitLab integration example.

## Overview

OpenAB only listens to Discord events. It does not accept external webhooks directly. To trigger agents from GitLab events (MR, Issue, etc.), we route through Discord as the single entry point.

## Architecture

```
GitLab (MR/Issue event)
  → GitLab Webhook
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

### 2. GitLab Webhook

Add a webhook to your GitLab project:

1. Go to your GitLab project → Settings → Webhooks
2. Click **Add webhook**
3. Configure:
   - **URL**: your Discord webhook URL (from step 1)
   - **Trigger events**: select the events you want to forward:
     - Issues events
     - Merge request events
     - Push events
     - Comments
     - etc.
   - **SSL verification**: enable (recommended)
4. Click **Add webhook**

### 3. Transform GitLab Events to Discord Format

GitLab webhooks send JSON payloads. To format them for Discord, you have two options:

#### Option A: Use a Webhook Transformer Service

Services like [Zapier](https://zapier.com), [Make](https://make.com), or [Webhook.cool](https://webhook.cool) can transform GitLab payloads to Discord format.

#### Option B: Self-Hosted Transformer

Create a simple Node.js/Python service that:
1. Receives GitLab webhook POST
2. Extracts event data
3. Formats as Discord message
4. POSTs to Discord webhook

Example Node.js transformer:

```javascript
const express = require('express');
const axios = require('axios');
const app = express();

app.use(express.json());

app.post('/gitlab-webhook', async (req, res) => {
  const event = req.body;
  const discordWebhookUrl = process.env.DISCORD_WEBHOOK_URL;

  let message = '';

  if (event.object_kind === 'merge_request') {
    const mr = event.object_attributes;
    message = `[GL-EVENT] repo:${event.project.path_with_namespace} action:mr_${mr.action} MR !${mr.iid}\n`;
    message += `**${mr.title}**\n`;
    message += `by ${event.user.username}\n`;
    message += `${mr.url}`;
  } else if (event.object_kind === 'issue') {
    const issue = event.object_attributes;
    message = `[GL-EVENT] repo:${event.project.path_with_namespace} action:issue_${issue.action} Issue #${issue.iid}\n`;
    message += `**${issue.title}**\n`;
    message += `by ${event.user.username}\n`;
    message += `${issue.url}`;
  } else if (event.object_kind === 'push') {
    message = `[GL-EVENT] repo:${event.project.path_with_namespace} action:push\n`;
    message += `Branch: ${event.ref.split('/').pop()}\n`;
    message += `Commits: ${event.total_commits_count}\n`;
    message += `by ${event.user_username}`;
  }

  if (message) {
    try {
      await axios.post(discordWebhookUrl, {
        content: message
      });
      res.status(200).json({ status: 'ok' });
    } catch (error) {
      console.error('Discord webhook error:', error);
      res.status(500).json({ error: 'Failed to post to Discord' });
    }
  } else {
    res.status(200).json({ status: 'ignored' });
  }
});

app.listen(3000, () => console.log('Listening on port 3000'));
```

### 4. Message Format Convention

Messages use a structured prefix so OpenAB can identify GitLab events:

```
[GL-EVENT] repo:{namespace/project} action:{event_type} {MR/Issue} {identifier}
**{title}**
by {author}
{url}
```

Example:
```
[GL-EVENT] repo:openabdev/openab action:mr_opened MR !42
**Add webhook integration docs**
by obrutjack
https://gitlab.com/openabdev/openab/-/merge_requests/42
```

## Message Flow

```
GitLab Event (MR opened)
  ↓
GitLab Webhook POST
  ↓
Transformer Service (formats to Discord)
  ↓
Discord Webhook POST
  ↓
Discord Channel receives message
  ↓
OpenAB detects [GL-EVENT] prefix
  ↓
Routes to configured agent
  ↓
Agent performs action (review, comment, notify)
```

## Configuration in OpenAB

To handle GitLab events in your OpenAB config:

```toml
[discord]
token = "YOUR_DISCORD_BOT_TOKEN"
allowed_channels = ["1234567890"]  # Channel where webhooks post

[[agents]]
name = "gitlab-reviewer"
enabled = true
# Agent will receive messages with [GL-EVENT] prefix
```

## Open Questions

- **Bot message handling**: Does OpenAB currently ignore messages from bots/webhooks? If so, webhook sources need to be allowlisted. Note: OpenAB uses `allowed_channels` and `allowed_users` in `config.toml` for filtering — webhook messages come from a bot user, so the bot's user ID may need to be added to `allowed_users`, or the filtering logic would need a `[GL-EVENT]` prefix check.
- **Routing**: How does OpenAB determine which agent handles a `[GL-EVENT]` message?
- **Loop prevention**: If an agent replies in the same channel, could it re-trigger events? Recommend using a dedicated channel and filtering by `[GL-EVENT]` prefix only.

## Best Practices

- Use a dedicated channel or thread for webhook events
- Stick to the `[GL-EVENT]` prefix convention for all GitLab-sourced messages
- Validate webhook sources on the Discord side (restrict channel permissions)
- Avoid agents posting back to the same webhook channel to prevent loops
- Start minimal (MR + Issue notifications), expand as needed
- Use GitLab's webhook secret to validate incoming requests (if using a custom transformer)

## GitLab Webhook Secret Validation

To prevent unauthorized webhook calls, GitLab supports a secret token:

1. In GitLab webhook settings, set a **Secret token**
2. In your transformer, validate the `X-Gitlab-Token` header:

```javascript
app.post('/gitlab-webhook', (req, res) => {
  const token = req.headers['x-gitlab-token'];
  if (token !== process.env.GITLAB_WEBHOOK_SECRET) {
    return res.status(401).json({ error: 'Unauthorized' });
  }
  // ... process webhook
});
```

## Supported GitLab Events

| Event | Payload Key | Example |
|---|---|---|
| Merge Request | `object_kind: merge_request` | `action: opened, merged, closed` |
| Issue | `object_kind: issue` | `action: opened, closed, reopened` |
| Push | `object_kind: push` | Branch name, commit count |
| Comment | `object_kind: note` | `noteable_type: MergeRequest, Issue` |
| Pipeline | `object_kind: pipeline` | `status: success, failed` |
| Release | `object_kind: release` | Release notes, tag |

## Future Considerations

- Extend pattern to other sources: Jira, GitHub, PagerDuty, etc.
- Agent-to-agent invocation during review workflows
- Event filtering and deduplication at the OpenAB level
- Richer payloads using Discord embeds instead of plain text
- Native GitLab adapter in Custom Gateway (v2+)

## Troubleshooting

- **Webhook not firing**: Check GitLab webhook logs (Settings → Webhooks → Recent deliveries)
- **Discord message not appearing**: Verify Discord webhook URL is correct and the channel is accessible
- **Transformer service errors**: Check transformer logs and ensure it's accessible from GitLab
- **Agent not responding**: Verify `[GL-EVENT]` prefix is present and agent is configured to handle it
- **Duplicate messages**: Check for multiple webhooks pointing to the same Discord channel
