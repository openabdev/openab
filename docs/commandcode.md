# Command Code — Agent Backend Guide

[Command Code](https://commandcode.ai/docs/acp) provides native ACP over stdio.
OpenAB runs `command-code acp` directly using its existing ACP connection.
The full executable name is used because the `cmd` alias conflicts with the
Windows command shell.

## Local setup

Install [Node.js 22 or newer and Command Code](https://commandcode.ai/docs/quickstart):

```bash
npm install -g command-code
command-code --version
command-code login
```

Run login as the same user, with the same `HOME`, that will run OpenAB. Add this
to your OpenAB configuration, replacing the working directory with your project:

```toml
[agent]
command = "command-code"
args = ["acp"]
working_dir = "/path/to/project"
```

Use an absolute `working_dir`: Command Code rejects relative ACP project paths.
The interactive `openab setup` wizard also offers `commandcode` and resolves
local relative directories to absolute paths when generating the configuration.

## Authentication and persistence

For a server or container, set `COMMAND_CODE_API_KEY` using a secret and explicitly
forward it to the child:

```toml
[agent]
command = "command-code"
args = ["acp"]
working_dir = "/home/node"
inherit_env = ["COMMAND_CODE_API_KEY"]
```

Alternatively, use
`env = { COMMAND_CODE_API_KEY = "${COMMAND_CODE_API_KEY}" }` in `[agent]`.
OpenAB clears the agent's environment before spawning it, so setting a variable
on the OpenAB process alone does not pass it to Command Code. Keep Discord and
Slack tokens out of `[agent].env` and `inherit_env`.

Interactive login stores credentials in `~/.commandcode/auth.json`. Command Code
also stores settings, provider configuration, and session history under
[`~/.commandcode/`](https://commandcode.ai/docs/settings). Persist that directory
or the entire home directory. The OpenAB image runs as `node`, with
`HOME=/home/node`.

## Docker

Build the standalone image:

```bash
docker build -f Dockerfile.commandcode -t openab-commandcode .
```

Or use the unified Dockerfile target:

```bash
docker build --target commandcode -f Dockerfile.unified -t openab-commandcode .
```

The `commandcode` target is also available in `Dockerfile.package` for packaging
prebuilt binaries. All three Dockerfiles pin `COMMAND_CODE_VERSION`. They set:

```text
OPENAB_AGENT_COMMAND=command-code acp
OPENAB_AGENT_AUTH_COMMAND=command-code login
```

For API key authentication, pass `COMMAND_CODE_API_KEY` to the container and
include `inherit_env` as shown above. Mount persistent storage at `/home/node`.

## Helm

The chart accepts `commandcode` as an agent key. Use a built image and inject
credentials from an existing Secret with `DISCORD_BOT_TOKEN` and
`COMMAND_CODE_API_KEY` keys:

```yaml
agents:
  kiro:
    enabled: false
  commandcode:
    enabled: true
    image: openab-commandcode:local # replace with your pushed image reference
    workingDir: /home/node
    secretEnv:
      - name: DISCORD_BOT_TOKEN
        secretName: commandcode-secrets
        secretKey: DISCORD_BOT_TOKEN
      - name: COMMAND_CODE_API_KEY
        secretName: commandcode-secrets
        secretKey: COMMAND_CODE_API_KEY
    persistence:
      enabled: true
      size: 1Gi
    configToml: |
      [discord]
      bot_token = "${DISCORD_BOT_TOKEN}"
      allowed_channels = ["YOUR_CHANNEL_ID"]

      [agent]
      command = "command-code"
      args = ["acp"]
      working_dir = "/home/node"
      inherit_env = ["COMMAND_CODE_API_KEY"]
```

Only the API key is forwarded to the agent. Raw `configToml` must include its own
`inherit_env`; the chart mounts that TOML verbatim.

## ACP behavior and troubleshooting

Command Code supports streaming, tool calls, permission requests, images, and
session loading. Audio is unsupported. Each `command-code acp` process serves
one project directory, matching OpenAB's per-session process lifecycle.

OpenAB automatically selects an allow option for ACP tool permission requests,
as it does for other backends. Command Code's own configured permission rules
still apply. Agent questions and plan approval choices do not have a dedicated
OpenAB chat interaction in this integration.

- **Not authenticated:** verify the login used OpenAB's `HOME`, or explicitly
  forward `COMMAND_CODE_API_KEY`.
- **ACP mod disabled:** remove `acp` from the user-level `mods.disabled` settings.
- **Process exits:** verify Node.js is at least version 22 and `command-code`
  is available on the agent's `PATH`.
- **Missing history after restart:** verify persistent storage covers
  `/home/node/.commandcode` and is writable by UID 1000.

CI runs `.github/scripts/commandcode_acp_smoke.mjs` against the real CLI with an
isolated home directory. It verifies initialization, absolute project directory
validation, and the structured authentication error from `session/new` without
credentials.

With an existing Command Code login, run the opt-in live integration test:

```bash
cargo test -p openab-core --no-default-features --test commandcode_acp -- --ignored --nocapture
```

This uses OpenAB's `AcpConnection` and the current user's login. It makes two real
model calls, verifies streamed text and a tool-written file in a disposable
project, then restarts the agent and checks that `session/load` restores a token
present only in the previous conversation. The test is ignored by default so
normal test runs do not require credentials or make model calls. Set
`COMMANDCODE_ACP_COMMAND` to override the CLI executable path if needed.
