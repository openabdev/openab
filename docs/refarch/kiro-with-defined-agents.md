# Reference Architecture: OpenAB with Defined Kiro Agents

> **Agent-friendly prompt:**
>
> ```text
> Configure four global Kiro agents named sol, terra, luna, and auto from
> https://github.com/openabdev/openab/blob/main/docs/refarch/kiro-with-defined-agents.md,
> then configure this OpenAB instance to launch the agent I select.
> ```

Use named Kiro agents to give each OpenAB deployment an explicit model policy. This example defines four reusable agents:

| Agent name | Agent `model` value | Intended policy |
|---|---|---|
| `sol` | `gpt-5.6-sol` | Hardest multi-step development and long-horizon work |
| `terra` | `gpt-5.6-terra` | Balanced, routine multi-step development |
| `luna` | `gpt-5.6-luna` | Fast, high-frequency agentic work |
| `auto` | `auto` | Kiro routing for general development (recommended default) |

GPT-5.6 Sol, Terra, and Luna were announced for Kiro on July 14, 2026. All three have a 272K context window and are currently Experimental; their Kiro credit multipliers are 2.4x, 1.2x, and 0.6x respectively, compared with Auto's 1.0x baseline. See the [GPT-5.6 announcement](https://kiro.dev/changelog/models/gpt-5-6/) and [Kiro model guide](https://kiro.dev/docs/models/) for current availability and lifecycle details.

The agent names are aliases. OpenAB selects an alias when it launches Kiro ACP; Kiro then loads that agent's configuration and uses its configured model.

## Why This Architecture

- **Deterministic startup** — each OpenAB instance explicitly selects a named agent instead of relying on the ambient Kiro default.
- **Separation of concerns** — Kiro agent JSON owns the model and tool policy, while OpenAB `config.toml` only selects the agent.
- **Easy model changes** — switch the agent name in `config.toml`, restart the instance, and new ACP sessions use the selected policy.
- **Reusable configuration** — the same named agents can be used by Kiro CLI directly and by multiple OpenAB deployments.

## Architecture

```text
+--------------------------- OpenAB instance ---------------------------+
|                                                                       |
|  config.toml                                                          |
|  [agent]                                                              |
|  command = "kiro-cli"                                                 |
|  args = ["acp", "--agent", "sol", "--trust-all-tools"]                |
|                         |                                             |
+-------------------------|---------------------------------------------+
                          | starts ACP over stdio
                          v
                 +--------------------+
                 | kiro-cli acp       |
                 | --agent sol        |
                 +---------+----------+
                           | loads
                           v
                $HOME/.kiro/agents/sol.json
                           |
                           | "model": "gpt-5.6-sol"
                           v
                     GPT-5.6 Sol

Change only the selected alias for another deployment:
  --agent terra  ->  gpt-5.6-terra
  --agent luna   ->  gpt-5.6-luna
  --agent auto   ->  auto
```

No additional OpenAB service or infrastructure is required. The model used by each agent is billed according to the user's Kiro plan.

## Multi-OAB Delegation by Complexity

The same pattern can back multiple **named OpenAB agents** with different model policies. A common two-tier design uses:

| Named OAB agent | Kiro agent | Model policy | Workload |
|---|---|---|---|
| `generic` | `auto` | `auto` | Routine questions, summaries, small changes, and general operations |
| `complex` | `sol` | `gpt-5.6-sol` | Architecture, difficult debugging, long-horizon refactors, and high-complexity implementation |

Use `terra` for an additional fixed, balanced tier or `luna` for a fixed high-throughput tier. The diagram below keeps two deployments for clarity; every added policy still requires its own OpenAB deployment.

Each named OAB agent is an independent deployment with its own `config.toml`,
bot identity, process, and state. OpenAB does not infer task complexity in this
pattern: a human can select the target bot directly, or an existing coordinator
bot can route a request through an explicit mention of the target bot. That
Discord path is message routing, not the formal delegation adapter: it has no
stable remote task identity, replay, or signed provenance. For bounded
inter-OAB delegation, use the [OAB Delegation Adapter ADR](../adr/oab-to-oab-delegation.md).

### Delegation architecture

```text
                              TASK INTAKE
                    +---------------------------+
                    | Human or coordinator bot  |
                    | classifies/selects target |
                    +-------------+-------------+
                                  |
                    +-------------+-------------+
                    |                           |
             routine/generic             high complexity
                    |                           |
                    v                           v
       +--------------------------+  +--------------------------+
       | Named OAB agent: generic |  | Named OAB agent: complex |
       |                          |  |                          |
       | config.toml              |  | config.toml              |
       | --agent auto             |  | --agent sol              |
       |          |               |  |          |               |
       |          v               |  |          v               |
       | kiro-cli acp             |  | kiro-cli acp             |
       |          |               |  |          |               |
       |          v               |  |          v               |
       | $HOME/.kiro/agents/      |  | $HOME/.kiro/agents/      |
       |   auto.json              |  |   sol.json               |
       | model: auto              |  | model: gpt-5.6-sol       |
       +------------+-------------+  +-------------+------------+
                    ^                              ^
                    | hooks.pre_seed               | hooks.pre_seed
                    +---------------+--------------+
                                    |
                    +---------------+----------------------------+
                    | Private S3 HOME seed                       |
                    | kiro-named-agents-home.tar.gz              |
                    |                                            |
                    | .kiro/agents/sol.json                      |
                    | .kiro/agents/terra.json                    |
                    | .kiro/agents/luna.json                     |
                    | .kiro/agents/auto.json                     |
                    | (optionally auth, steering, skills, tools) |
                    +--------------------------------------------+
```

The OAB deployments can restore the same base HOME archive because it contains all four named Kiro profiles. Each deployment activates only the profile selected by its own `--agent` argument.

Configure the two OpenAB backends independently:

```toml
# generic OAB agent config.toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent", "auto", "--trust-all-tools"]
```

```toml
# complex OAB agent config.toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent", "sol", "--trust-all-tools"]
```

For manual routing, users mention the appropriate bot. Existing
coordinator-to-worker message routing over Discord allows only explicit bot
mentions on the receiving worker; it is not the formal delegation adapter
specified by the [OAB Delegation Adapter ADR](../adr/oab-to-oab-delegation.md):

```toml
[discord]
allow_bot_messages = "mentions"
# Optional: restrict delegation to known coordinator bot IDs.
trusted_bot_ids = ["COORDINATOR_BOT_ID"]
```

`"mentions"` is the recommended loop breaker. Avoid `allow_bot_messages = "all"` unless every bot message must trigger work. See [Multi-Agent Setup](../multi-agent.md) and [Messaging](../messaging.md) for the complete routing and trust model.

## Persist Named Kiro Agents in Stateless Environments

Kiro discovers global named agents from `$HOME/.kiro/agents/`. For stateless runtimes such as ECS Fargate or Fargate Spot, keep the profiles under HOME, bundle the required HOME content into a private S3 tarball, and restore it before Kiro ACP starts. At minimum, the archive must recreate `.kiro/agents/...` relative to `$HOME`; it can also carry authentication, steering, skills, and tools when those need to persist.

Use OpenAB's built-in `pre_seed` lifecycle hook for the restore. It extracts the archive into `$HOME` by default before `pre_boot` and before the agent pool starts, so all four named profiles exist when OpenAB launches Kiro with the selected alias. If runtime changes to HOME must survive task replacement, pair the restore with a `pre_shutdown` backup.

See [Hooks: Pre-Seed and HOME Backup/Restore](../hooks.md#real-world-example-s3-restore--backup-round-trip) for the canonical archive layout, S3 configuration, checksum verification, IAM permissions, size limits, and complete `pre_seed`/`pre_shutdown` round-trip. Treat full HOME archives as sensitive because they may contain authentication material.

## Prerequisites

- OpenAB with the Kiro CLI backend installed and authenticated. See [Kiro CLI (Default Agent)](../kiro.md).
- A Kiro CLI release whose `kiro-cli acp --help` output includes `--agent <AGENT>`.
- Access to the requested models in your Kiro account and region. GPT-5.6 support is Experimental and may require an eligible paid plan and cross-region inference; run `/model` in Kiro CLI to inspect currently available models.
- Write access to the Kiro home directory used by the OpenAB process.

## Create the Four Kiro Agents

Global agents live in `$HOME/.kiro/agents/` and are available across workspaces. Create the directory first:

```bash
mkdir -p "$HOME/.kiro/agents"
```

### `sol`

Create `$HOME/.kiro/agents/sol.json`:

```json
{
  "name": "sol",
  "description": "Flagship agent for the hardest multi-step work using GPT-5.6 Sol",
  "model": "gpt-5.6-sol",
  "tools": ["*"],
  "allowedTools": ["@builtin"]
}
```

### `terra`

Create `$HOME/.kiro/agents/terra.json`:

```json
{
  "name": "terra",
  "description": "Balanced agent for routine multi-step work using GPT-5.6 Terra",
  "model": "gpt-5.6-terra",
  "tools": ["*"],
  "allowedTools": ["@builtin"]
}
```

### `luna`

Create `$HOME/.kiro/agents/luna.json`:

```json
{
  "name": "luna",
  "description": "Fast agent for high-frequency work using GPT-5.6 Luna",
  "model": "gpt-5.6-luna",
  "tools": ["*"],
  "allowedTools": ["@builtin"]
}
```

### `auto`

Create `$HOME/.kiro/agents/auto.json`:

```json
{
  "name": "auto",
  "description": "General-purpose agent using Kiro automatic model selection",
  "model": "auto",
  "tools": ["*"],
  "allowedTools": ["@builtin"]
}
```

`"tools": ["*"]` makes every available tool visible to the agent. `"allowedTools": ["@builtin"]` auto-approves all built-in tools; `"*"` is not supported in `allowedTools`. The OpenAB launch examples below also use `--trust-all-tools`, which auto-approves tool permission requests for the non-interactive ACP process.

> **Why `--trust-all-tools` is used:** OpenAB normally runs Kiro ACP non-interactively inside a pod or container. There is no terminal operator available to approve individual tool requests, so an untrusted tool call can block the ACP session indefinitely. Keep `--trust-all-tools` on OAB-managed Kiro commands unless every required tool is covered by an explicit non-interactive trust policy. Treat the pod boundary, OpenAB channel/user/bot allowlists, container permissions, and IAM role as the security controls around that authority.

## Validate the Agent Files

Validate all four files against the installed Kiro agent schema:

```bash
for agent in sol terra luna auto; do
  kiro-cli agent validate --path "$HOME/.kiro/agents/${agent}.json"
done

kiro-cli agent list
```

Kiro also supports workspace agents under `.kiro/agents/`. A workspace agent takes precedence over a global agent with the same name. For containers, make sure the files are created or mounted under the `$HOME` used by the OpenAB process; a different user's home directory will not be discovered.

## Select an Agent in OpenAB `config.toml`

An OpenAB config has one `[agent]` backend. Set `command` to `kiro-cli` and pass the selected agent name as separate entries in `args`.

### Select `sol`

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent", "sol", "--trust-all-tools"]
```

### Select `terra`

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent", "terra", "--trust-all-tools"]
```

### Select `luna`

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent", "luna", "--trust-all-tools"]
```

### Select `auto`

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent", "auto", "--trust-all-tools"]
```

The resulting command is, for example:

```bash
kiro-cli acp --agent sol --trust-all-tools
```

`--agent <AGENT>` tells Kiro which agent to use when starting the first ACP session. Because the selected agent JSON contains `model`, the session starts with that agent's configured model. This avoids depending on the global `chat.defaultAgent` or `chat.defaultModel` settings.

To run all four policies concurrently, deploy four OpenAB instances and give each instance its own `config.toml` with a different `--agent` value. Do not add four `[agent]` sections to one config.

## Deploy and Verify

1. Restart or redeploy OpenAB after changing `config.toml` so it starts a new Kiro ACP process.
2. Confirm that the expected argument is present:

   ```bash
   ps -ef | grep '[k]iro-cli acp'
   ```

3. Send a test message through the configured OpenAB adapter and confirm the ACP session responds.
4. If Kiro reports that a model is unavailable, run `/model` in an interactive Kiro session to check the model IDs available to the account and region, then update the corresponding agent JSON.

## Day-2 Operations

### Change the model behind an alias

Edit the alias's JSON file, validate it, and restart OpenAB. The OpenAB `config.toml` does not need to change:

```bash
kiro-cli agent validate --path "$HOME/.kiro/agents/sol.json"
```

### Change the selected alias

Change only the value after `--agent` in `config.toml`, then restart OpenAB:

```toml
[agent]
command = "kiro-cli"
args = ["acp", "--agent", "terra", "--trust-all-tools"]
```

### Set a Kiro-wide fallback

Agent-specific `model` values are preferred for this architecture. Kiro's global fallback can still be configured separately:

```bash
kiro-cli settings chat.defaultModel <model-id>
```

If an agent does not specify `model`, Kiro uses the default model. If its configured model is unavailable, Kiro falls back to the default model.

## Troubleshooting

| Symptom | Check |
|---|---|
| `Agent not found` | Confirm `$HOME/.kiro/agents/<name>.json` exists for the same user running OpenAB and that the filename exactly matches `--agent`. |
| Agent file is rejected | Run `kiro-cli agent validate --path <file>` and fix the reported JSON or schema error. |
| Wrong agent starts | Inspect the running process arguments and check for a workspace agent with the same name overriding the global file. |
| Wrong or unavailable model | Check the exact `model` value and use `/model` to see models available in the current account and region. |
| Tool approval blocks ACP | Include `--trust-all-tools`, or configure the precise trusted tools required by the deployment. |
| Config change has no effect | Restart OpenAB so it launches a new ACP process and starts a new session with the updated `--agent` value. |

## Important Notes

- The Kiro agent field is named `model`, not `default_model`.
- Agent names and filenames are case-sensitive.
- `--agent` selects the agent for the first ACP session; it is not an OpenAB model-setting flag.
- Keep agent files on persistent storage in containerized deployments so they survive restarts.
- GPT-5.6 Sol, Terra, and Luna are Experimental at the time of writing; review Kiro's model lifecycle and data-protection guidance before production use.
- Model availability is account- and region-dependent and may change over time.
