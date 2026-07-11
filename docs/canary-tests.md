# Canary Testing Pull Requests

Use this guide to validate a pull request that changes an OpenAB container image, agent adapter, ACP behavior, authentication flow, session lifecycle, or gateway integration. A canary complements unit tests and Docker smoke tests; it does not replace them.

The goal is to produce evidence that another contributor or maintainer can review and repeat without sharing production credentials.

## Table of Contents

- [Prerequisites](#prerequisites)
- [Build the Preview Image](#build-the-preview-image)
- [Layer 1: Image Inspection](#layer-1-image-inspection)
- [Layer 2: ACP Protocol Smoke](#layer-2-acp-protocol-smoke)
  - [Transport Method](#transport-method)
- [Layer 3: Runtime Isolation Probe](#layer-3-runtime-isolation-probe)
- [Layer 4: Interactive Validation](#layer-4-interactive-validation)
- [Layer 5: Discord Gateway E2E](#layer-5-discord-gateway-e2e)
  - [Local Docker Example](#local-docker-example)
- [Acceptance Checklist by Agent and Change Type](#acceptance-checklist-by-agent-and-change-type)
- [Report Results](#report-results)
- [Clean Up](#clean-up)
- [Post-Merge Canary](#post-merge-canary)
- [Worked Example](#worked-example)

## Prerequisites

Before testing, prepare:

- the pull request number and full head commit SHA;
- the affected image variant, such as `codex`, `claude`, or `default`;
- a non-production bot or gateway identity and test channel;
- isolated credential and workspace volumes;
- the previous working image tag for rollback;
- the expected model, agent mode, tools, and session behavior.

Never run two OpenAB instances with the same bot token. Both instances can receive and reply to the same message.

Do not paste tokens, authentication files, or unredacted environment dumps into logs, issues, pull requests, or chat. Treat mounted files, credentials, service accounts, and network access as part of the canary's security boundary.

Set reusable shell variables before starting. The example below uses PR `1353` and the `codex` variant; replace both values for the pull request under test:

```bash
PR_NUMBER=1353
VARIANT=codex
PR_HEAD="$(gh pr view "$PR_NUMBER" --repo openabdev/openab --json headRefOid --jq .headRefOid)"
IMAGE="ghcr.io/openabdev/openab:pr${PR_NUMBER}-${VARIANT}"
PLATFORM=linux/arm64

printf 'PR head: %s\nImage: %s\nPlatform: %s\n' "$PR_HEAD" "$IMAGE" "$PLATFORM"
```

For the `default` variant, use `IMAGE="ghcr.io/openabdev/openab:pr${PR_NUMBER}"` because its tag has no variant suffix.

## Build the Preview Image

Run the repository checks that apply to the change. For an image or adapter change, confirm both `Docker Smoke Test` and `Docker Smoke Test (Unified)` run against the exact PR head.

[`PR Preview Build`](../.github/workflows/pr-preview.yml) is a manual upstream workflow. Dispatching it requires GitHub Actions write permission on `openabdev/openab`. A contributor without that permission should ask a maintainer to run it and attach the workflow URL to the PR.

A maintainer dispatches the workflow from `main`; the workflow then resolves and checks out the pull request's head repository and branch:

```bash
gh workflow run pr-preview.yml \
  --repo openabdev/openab \
  --ref main \
  -f pr_number="$PR_NUMBER" \
  -f variant="$VARIANT"
```

Find the run and wait for it to complete:

```bash
gh run list \
  --repo openabdev/openab \
  --workflow pr-preview.yml \
  --limit 5

RUN_ID=29156240254 # replace with the run ID selected above

gh run watch "$RUN_ID" \
  --repo openabdev/openab \
  --exit-status

gh run view "$RUN_ID" \
  --repo openabdev/openab \
  --json conclusion,url,jobs
```

The `default` preview variant produces `ghcr.io/openabdev/openab:pr<PR>`. Agent variants produce `ghcr.io/openabdev/openab:pr<PR>-<variant>`.

Confirm the pull request head did not move while the image was building:

```bash
CURRENT_HEAD="$(gh pr view "$PR_NUMBER" --repo openabdev/openab --json headRefOid --jq .headRefOid)"
test "$CURRENT_HEAD" = "$PR_HEAD" || {
  printf 'PR head changed: expected %s, found %s\n' "$PR_HEAD" "$CURRENT_HEAD" >&2
  exit 1
}
```

Do not reuse an earlier preview image after a runtime-affecting PR head change. Rebuild it and record the new commit and digest. If the head moved only because of documentation, state that explicitly instead of describing the earlier image as a current-head build.

## Layer 1: Image Inspection

Record the immutable test target before exercising behavior:

```text
PR: <number>
PR head: <full commit SHA>
Image: <tag>
Digest: <sha256 digest>
Platform: <linux/amd64|linux/arm64>
Runtime: <Docker|Kubernetes|ECS|other>
Baseline image: <previous working tag or digest>
```

Inspect the published multi-architecture manifest, pull the platform under test, and record the immutable digest:

```bash
docker buildx imagetools inspect "$IMAGE"
docker pull --platform "$PLATFORM" "$IMAGE"
docker image inspect "$IMAGE" \
  --format '{{json .RepoDigests}}'
```

Inspect the runtime facts relevant to the PR. Examples include:

```bash
AGENT_BINARY=codex-acp

docker run --rm --entrypoint openab "$IMAGE" --version
docker run --rm -e AGENT_BINARY --entrypoint sh "$IMAGE" -c 'command -v "$AGENT_BINARY"'
docker run --rm --entrypoint "$AGENT_BINARY" "$IMAGE" --version
docker inspect "$IMAGE" \
  --format '{{range .Config.Env}}{{println .}}{{end}}'
```

Avoid printing the live container environment because it may include injected credentials. Inspect image configuration or specific non-secret values only.

### Pass Criteria

- The image digest and PR head are recorded.
- Expected OpenAB, adapter, and CLI versions are installed.
- The expected agent binary resolves on `PATH`.
- Relevant non-secret image defaults match the PR.

## Layer 2: ACP Protocol Smoke

For an ACP adapter change, use a minimal bidirectional JSON-RPC client to exercise the same stdio boundary OpenAB uses. The client may be written in any language; JavaScript is not required. A one-way shell pipeline is insufficient for multi-turn and permission tests because the adapter can send requests back to the client while a prompt is running.

### Transport Method

1. Start the preview container with stdin open and the agent binary as its entrypoint. Mount only isolated test credentials and a disposable workspace. The bidirectional client should spawn an equivalent command rather than pipe a fixed list of messages into it. For the Codex preview image:

   ```bash
   AGENT_BINARY=codex-acp
   TEST_AUTH_VOLUME=openab-canary-auth
   TEST_AUTH_PATH=/home/node/.codex
   TEST_WORKSPACE_VOLUME=openab-canary-workspace

   docker volume create "$TEST_AUTH_VOLUME"
   docker volume create "$TEST_WORKSPACE_VOLUME"

   IMAGE_UID="$(docker run --rm --entrypoint id "$IMAGE" -u)"
   IMAGE_GID="$(docker run --rm --entrypoint id "$IMAGE" -g)"

   docker run --rm --user root \
     -v "${TEST_AUTH_VOLUME}:/canary/auth" \
     -v "${TEST_WORKSPACE_VOLUME}:/canary/workspace" \
     --entrypoint chown \
     "$IMAGE" -R "${IMAGE_UID}:${IMAGE_GID}" /canary

   # Skip this login command when the test uses a test-only API key instead.
   docker run --rm -it \
     -e HOME=/home/node \
     -v "${TEST_AUTH_VOLUME}:${TEST_AUTH_PATH}" \
     --entrypoint sh \
     "$IMAGE" -c '$OPENAB_AGENT_AUTH_COMMAND'

   docker run --rm -i \
     --entrypoint "$AGENT_BINARY" \
     -e HOME=/home/node \
     -v "${TEST_AUTH_VOLUME}:${TEST_AUTH_PATH}" \
     -v "${TEST_WORKSPACE_VOLUME}:/workspace" \
     "$IMAGE"
   ```

2. Write one newline-delimited JSON-RPC message at a time to the process's stdin. Read and parse stdout one line at a time. Keep stderr separate so a log line cannot be mistaken for an ACP message.
3. Assign each request a unique numeric `id`. Match responses by `id`, while collecting notifications that have a `method` but no response `id`.
4. When the adapter sends `session/request_permission`, record the advertised options and reply using the request's `id`. Select the actual `optionId` advertised for the intended test outcome; do not assume every adapter uses the literal string `allow_always` as its option ID.
5. Use bounded waits. OpenAB allows up to 120 seconds for `session/new` and 30 seconds for ordinary control requests. Give live model prompts a separate, explicit turn timeout and send `session/cancel` when testing cancellation.
6. Consider a prompt complete only when the response matching its request ID arrives. Record its `stopReason`; streamed notifications alone do not prove the turn completed.

The core wire sequence begins as follows:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{},"clientInfo":{"name":"openab-canary","version":"0.1.0"}}}
{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/workspace","mcpServers":[]}}
```

Read `sessionId` and `configOptions` from the `session/new` response. Use that session ID in subsequent messages:

```json
{"jsonrpc":"2.0","id":3,"method":"session/set_config_option","params":{"sessionId":"<SESSION_ID>","configId":"model","value":"<MODEL_ID>"}}
{"jsonrpc":"2.0","id":4,"method":"session/prompt","params":{"sessionId":"<SESSION_ID>","prompt":[{"type":"text","text":"Reply with ACP_OK."}]}}
{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"<SESSION_ID>"}}
```

If the adapter requests permission with request ID `91`, reply with the exact `optionId` selected from that request's `options` array:

```json
{"jsonrpc":"2.0","id":91,"result":{"outcome":{"outcome":"selected","optionId":"<ADVERTISED_OPTION_ID>"}}}
```

`session/cancel` is a notification and therefore has no request ID. To test resume behavior, initialize a fresh adapter process and send:

```json
{"jsonrpc":"2.0","id":5,"method":"session/load","params":{"sessionId":"<SESSION_ID>","cwd":"/workspace","mcpServers":[]}}
```

Send one intentionally invalid method and require a JSON-RPC error response with the same request ID:

```json
{"jsonrpc":"2.0","id":6,"method":"canary/invalid_method","params":{}}
```

Cover the methods supported by the adapter:

1. `initialize`
2. `session/new`
3. model and configuration selection
4. a text-only `session/prompt`
5. a second prompt in the same session
6. `session/load` or resume
7. `session/cancel`
8. an intentional invalid or unavailable operation to verify actionable error propagation

For a model or runtime migration, select the model that originally exposed the problem and require a completed turn, not only successful initialization.

Record the request sequence, stop reason, relevant adapter events, and a redacted log excerpt. If a temporary client is used, include its source or a minimal reproducible snippet in the PR comment.

### Pass Criteria

- ACP initialization returns valid agent information.
- A new session advertises the expected configuration values.
- Two consecutive turns complete in the same session.
- Session loading and cancellation behave as documented.
- Errors are actionable at the OpenAB boundary.
- The target model completes a live turn when model compatibility is in scope.

## Layer 3: Runtime Isolation Probe

Test configuration values advertised by the adapter rather than assuming mode names are shared by every agent. For each applicable mode, record:

- the selected value reported by the session;
- whether a permission request was emitted;
- the response selected by the client;
- whether reads, workspace writes, shell commands, and network access matched the documented policy;
- whether the workspace changed.

When an image supplies a default, test both an unconfigured session and every supported explicit value. Explicit user configuration should override the image default.

Do not classify a sandbox bootstrap error as a permission-policy result. First determine whether the runtime could create the requested sandbox at all.

For Codex on Linux, these probes can distinguish an ACP permission-flow problem from unavailable user namespaces:

```bash
docker run --rm --entrypoint codex "$IMAGE" sandbox linux /bin/true
docker run --rm --entrypoint sh "$IMAGE" -c 'if command -v bwrap >/dev/null; then exec bwrap --unshare-user --uid 0 --gid 0 /bin/true; else echo "bwrap is not installed"; exit 127; fi'
docker run --rm --entrypoint sh "$IMAGE" -c 'cat /proc/sys/kernel/unprivileged_userns_clone 2>/dev/null || echo "kernel setting unavailable"'
```

Run the `bwrap` command only when the binary is installed. If both commands fail before a turn begins, record the container runtime and kernel error. This is evidence below the ACP adapter boundary, not proof that the adapter rejected a permission response.

### Pass Criteria

- The runtime can establish the sandbox required by each tested mode, or the unsupported boundary is identified and documented.
- Read, write, approval, and network behavior matches the selected policy.
- An explicit supported mode overrides any image default.
- A denied operation leaves protected state unchanged.

## Layer 4: Interactive Validation

When a scripted result is ambiguous, repeat the operation in the agent's interactive CLI. Read each permission prompt and choose the response manually. This removes a pre-programmed ACP permission response as the only explanation for the result.

Reuse the same isolated credentials and workspace. For the Codex preview image:

```bash
docker run --rm -it \
  --entrypoint codex \
  -e HOME=/home/node \
  -v openab-canary-auth:/home/node/.codex \
  -v openab-canary-workspace:/workspace \
  -w /workspace \
  "$IMAGE"
```

Use natural-language requests that exercise the same behavior as the protocol canary:

- list and read a known workspace file;
- create or modify a disposable file;
- run a harmless shell command;
- retry an operation after a permission prompt;
- cancel a running task.

Record the command, selected mode and approval policy, human response, final result, and whether the workspace changed.

### Pass Criteria

- Human-selected permission responses reach the runtime.
- The result matches the selected mode and approval policy.
- A failure reproduced outside ACP is classified below the adapter boundary.
- A failure unique to ACP remains an adapter or client investigation item.

## Layer 5: Discord Gateway E2E

Deploy the preview image to one isolated OpenAB agent. The evidence should include:

- preview image tag and digest;
- deployment or task identifier;
- OpenAB startup and ACP initialization;
- two consecutive user turns through the Discord gateway;
- one relevant tool or file operation;
- cancellation and actionable error behavior;
- restart followed by session resume when persistence is affected;
- absence of the original error in redacted OpenAB logs.

Do not expand beyond one canary agent until its acceptance criteria pass.

### Local Docker Example

The following Codex example uses a dedicated Discord bot, one allowlisted channel and user, isolated named volumes, and a config file outside the repository. Substitute the IDs and model, and never commit either temporary file.

Create `openab-canary.toml`:

```toml
[discord]
bot_token = "${DISCORD_BOT_TOKEN}"
allow_all_channels = false
allow_all_users = false
allowed_channels = ["<DISCORD_CHANNEL_ID>"]
allowed_users = ["<DISCORD_USER_ID>"]

[agent]
working_dir = "/workspace"

[pool]
default_config_options = { model = "gpt-5.6-sol" }
```

Create a mode-`600` env file without placing the token directly in shell history:

```bash
CANARY_NAME=openab-canary
CONFIG_FILE="$(pwd)/openab-canary.toml"
ENV_FILE="$(pwd)/.openab-canary.env"
AGENT_HOME=/home/node
AUTH_PATH=/home/node/.codex

umask 077
read -rsp 'Discord bot token: ' DISCORD_BOT_TOKEN
printf '\nDISCORD_BOT_TOKEN=%s\n' "$DISCORD_BOT_TOKEN" > "$ENV_FILE"
unset DISCORD_BOT_TOKEN
chmod 600 "$ENV_FILE"
```

Create isolated volumes and assign them to the image user before authentication. This prevents a root-owned volume root from blocking an unprivileged agent from creating its state files:

```bash
docker volume create openab-canary-auth
docker volume create openab-canary-state
docker volume create openab-canary-workspace

IMAGE_UID="$(docker run --rm --entrypoint id "$IMAGE" -u)"
IMAGE_GID="$(docker run --rm --entrypoint id "$IMAGE" -g)"

docker run --rm --user root \
  -v openab-canary-auth:/canary/auth \
  -v openab-canary-state:/canary/state \
  -v openab-canary-workspace:/canary/workspace \
  --entrypoint chown \
  "$IMAGE" -R "${IMAGE_UID}:${IMAGE_GID}" /canary
```

Authenticate the isolated agent volume when the image does not already use an API key supplied through a test-only secret:

```bash
docker run --rm -it \
  -e HOME="$AGENT_HOME" \
  -v "openab-canary-auth:${AUTH_PATH}" \
  --entrypoint sh \
  "$IMAGE" -c '$OPENAB_AGENT_AUTH_COMMAND'
```

Start one local canary instance and inspect its startup without printing the env file:

```bash
docker run -d \
  --name "$CANARY_NAME" \
  --env-file "$ENV_FILE" \
  -e HOME="$AGENT_HOME" \
  -v "$CONFIG_FILE:/etc/openab/config.toml:ro" \
  -v "openab-canary-auth:${AUTH_PATH}" \
  -v "openab-canary-state:${AGENT_HOME}/.openab" \
  -v openab-canary-workspace:/workspace \
  "$IMAGE"

docker ps --filter "name=${CANARY_NAME}"
docker logs --since 5m "$CANARY_NAME"
```

Exercise the gateway as a user rather than replaying fixed requests: mention the bot in the allowlisted channel, complete two turns in the created thread, inspect `/models`, run one harmless tool operation, start a long-running prompt and invoke `/cancel` while it is active, then restart the container and send another message in the same thread. Correlate every result with the OpenAB logs.

```bash
docker restart "$CANARY_NAME"
docker logs --since 5m "$CANARY_NAME"
```

Scan for the original regression and new initialization or turn failures before reporting success:

```bash
if docker logs "$CANARY_NAME" 2>&1 | rg -i 'ACP_TURN_FAILED|model.*requires a newer version|panicked|\b(ERROR|WARN)\b'; then
  echo 'Review the matched canary errors before proceeding.' >&2
  exit 1
fi
```

The Discord response after restart proves message delivery only when the logs also show that the same thread and ACP session were restored through `session/load`. Likewise, a `/cancel` acknowledgement proves command routing only when the in-flight turn also ends with the expected cancellation result.

### Pass Criteria

- The test bot receives and replies to messages once, without duplication.
- The expected agent, model, and mode are visible in the session.
- Multi-turn context, tools, cancellation, and errors cross the gateway intact.
- Session state resumes after restart when persistence is in scope.
- No original regression or new initialization failure appears in logs.

## Acceptance Checklist by Agent and Change Type

Select the rows that apply. Mark other rows `Not applicable`.

| Change type | Required evidence |
|------------|-------------------|
| Container or dependency | Image digest, binary path, runtime versions |
| ACP-backed agent | Initialize, new session, two turns, load, cancel, errors |
| Direct CLI agent | Version or help output and an interactive turn |
| Sandboxed agent | Policy matrix, runtime isolation probe, denied-operation state |
| Gateway-backed agent | Non-production gateway E2E and redacted OpenAB logs |
| Model compatibility | Target model selected and one live turn completed |
| Permission or sandbox | Every advertised mode, override behavior, runtime probe |
| Authentication | Isolated credential flow and restart persistence |
| Session persistence | Restart followed by session load and another turn |
| Gateway behavior | Non-production Discord E2E with logs |
| Deployment change | Health, rollout, one canary agent, rollback evidence |

## Report Results

Use one PR comment as the canonical report and link the Discord discussion when maintainers are coordinating there.

```markdown
## Canary report

### Test target

- PR head: `<SHA>`
- Image: `<TAG>`
- Digest: `<DIGEST>`
- Platform and runtime: `<VALUE>`
- Reproducer or client source: `<LINK_OR_CODE_BLOCK>`
- Gateway evidence: `<THREAD_LINK_AND_SCREENSHOT>`

### Results

| Check | Result | Evidence |
|------|--------|----------|
| Image inspection | Pass/Fail/Not verified | ... |
| ACP protocol smoke | Pass/Fail/Not verified | ... |
| Runtime isolation | Pass/Fail/Not verified | ... |
| Interactive validation | Pass/Fail/Not verified | ... |
| Discord gateway E2E | Pass/Fail/Not verified | ... |
| Rollback | Pass/Fail/Not verified | ... |

### Conclusion

State what the evidence proves, what it does not prove, remaining blockers, and whether the PR is ready for the next rollout gate.
```

Use `Pass` only when direct evidence exists. A published image, skipped job, or successful initialization is not evidence for multi-turn, cancellation, permission, persistence, gateway, or rollback behavior.

## Clean Up

After testing:

- stop the canary deployment;
- remove temporary containers, workspaces, and credential volumes;
- revoke temporary credentials when applicable;
- confirm no canary instance still uses the bot identity;
- preserve only redacted evidence required by the PR;
- restore the baseline image if the canary replaced an existing deployment.

For the local Docker example above:

```bash
docker rm -f "$CANARY_NAME" 2>/dev/null || true
docker volume rm \
  openab-canary-auth \
  openab-canary-state \
  openab-canary-workspace
rm -f "$ENV_FILE" "$CONFIG_FILE"

docker ps -a --filter "name=${CANARY_NAME}"
docker volume ls --filter name=openab-canary
```

An empty container and volume listing confirms local cleanup. Rotate or revoke the temporary bot token separately in the provider's control plane when the identity will not be reused.

## Post-Merge Canary

After merge, deploy the released image to one canary agent before broad rollout. Repeat the critical user flow against the merged artifact, monitor OpenAB logs for new initialization or turn errors, and verify session and credential persistence. Roll back to the recorded baseline tag if an acceptance criterion fails.

The preview and post-merge canaries answer different questions. A successful preview proves the PR artifact; a successful post-merge canary proves that the released artifact works in the target deployment path.

For a Kubernetes canary deployment, record the current image before changing it, wait for rollout health, and keep the exact rollback command ready:

```bash
DEPLOYMENT=openab-codex
CONTAINER=openab
RELEASE_TAG=0.9.1
RELEASE_IMAGE="ghcr.io/openabdev/openab:${RELEASE_TAG}-codex"
BASELINE_IMAGE="$(kubectl get deployment "$DEPLOYMENT" -o jsonpath="{.spec.template.spec.containers[?(@.name=='${CONTAINER}')].image}")"

printf 'Baseline: %s\nCandidate: %s\n' "$BASELINE_IMAGE" "$RELEASE_IMAGE"
kubectl set image "deployment/${DEPLOYMENT}" "${CONTAINER}=${RELEASE_IMAGE}"
kubectl rollout status "deployment/${DEPLOYMENT}" --timeout=5m
kubectl logs "deployment/${DEPLOYMENT}" --since=10m
```

If an acceptance criterion fails, restore the recorded image rather than relying on memory:

```bash
kubectl set image "deployment/${DEPLOYMENT}" "${CONTAINER}=${BASELINE_IMAGE}"
kubectl rollout status "deployment/${DEPLOYMENT}" --timeout=5m
```

## Worked Example

The worked [Codex ACP migration canary report][pr-1353-canary] demonstrates five complementary validation layers: image inspection, ACP lifecycle testing, direct runtime probes, human-driven CLI verification, and a dedicated-bot Discord E2E with screenshot evidence. Its temporary ACP client was written in Node.js, but the transport and pass criteria above are language-independent.

[pr-1353-canary]: https://github.com/openabdev/openab/pull/1353#issuecomment-4947408465
