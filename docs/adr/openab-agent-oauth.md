# ADR: openab-agent — Multi-Vendor LLM-Provider OAuth & Credential Storage

- **Status:** Proposed
- **Date:** 2026-06-24
- **Author:** @brettchien
- **Related:** `docs/adr/openab-agent.md` (charter), `docs/adr/openab-agent-mcp.md` §6 (MCP OAuth + §6.1 storage format), PR #1187 (Anthropic OAuth, first provider), PR #1185 (`/auth` device-flow relay), PR #1111 (`--no-browser`)

---

## 1. Context & Motivation

### 1.1 Why now
`openab-agent` reaches LLM providers in two ways: `ANTHROPIC_API_KEY` (pay-per-token) and an existing
Codex subscription-OAuth tenant in `~/.openab/agent/auth.json`. **PR #1187** adds native **Anthropic
(Claude Pro/Max) OAuth** as a second subscription tenant. This is the moment to set the pattern for *every*
future provider rather than let each PR hand-roll its own flow.

### 1.2 What PR #1187 surfaced
Reviewing #1187 exposed a latent, **release-blocker-class storage bug** that is independent of any single
provider: `auth.json` is a shared multi-writer file with an **unlocked read-modify-write**, and openab-agent
runs **one process per Discord thread** (`SessionPool` in `crates/openab-core/src/acp/pool.rs` → `crates/openab-core/src/acp/connection.rs`
spawns one `openab-agent` child per thread). So ordinary concurrent multi-thread usage = concurrent
processes refreshing the same OAuth token → refresh-token-rotation reuse → worst case OAuth 2.1 §10.4
**token-family revocation = fleet-wide logout**. API-key users never hit this (no refresh); **OAuth adoption
is what activates the bug.**

### 1.3 The wider demand
openab packages 14 agent variants (`kiro, claude, codex, copilot, cursor, gemini, grok, hermes, mimocode,
opencode, antigravity, pi, native, agentcore`). Several wrap a model vendor reachable by subscription OAuth.
A coherent extension model lets openab-agent (the `native` variant) host these directly. PR #1185 already
shipped a Discord `/auth` slash command that relays a device-flow login — the agreed near-term auth UX.

---

## 2. Goals & Non-Goals

### In scope
- A single **`OAuthVendor` adapter** (auth axis) reused by all subscription-OAuth providers.
- Keeping the **inference axis** (per-provider request/response transport) **separate** from auth.
- A **concurrency-safe credential store**: all `auth.json` writes funnel through one locked
  read-modify-write helper (covers MCP `CredentialStore` + provider tenants).
- Support for the OAuth styles real vendors use: **PKCE public**, **PKCE + bundled client_secret**,
  **device flow (RFC 8628)**, and **pre-provisioned long-lived token via env** (`CLAUDE_CODE_OAUTH_TOKEN`).
- Compatibility with PR #1185's `/auth` poll-and-exit relay model.

### Out of scope
- **Layer-3 auto-trigger** (agent auto-launches login on a mid-turn 401). DEFERRED (Brett, 2026-06-24);
  the manual `/auth` command is sufficient for now.
- Building every vendor at once. This ADR sets the model; vendors land incrementally.
- Non-OAuth backends: `agentcore` (AWS SigV4/IAM/Bedrock) is explicitly outside the OAuth surface.
- MCP-server OAuth internals — owned by `docs/adr/openab-agent-mcp.md`; this ADR only shares the storage
  layer with it.

---

## 3. Prior Art Survey
(Per `docs/adr/pr-contribution-guidelines.md`, OpenClaw + Hermes are mandatory references.)

- **Pi (`earendil-works/pi`)** — primary source ported for #1187's Anthropic flow (PKCE endpoints, Claude
  Code identity headers, system block, tool-name casing). Also ships `CLAUDE_CODE_OAUTH_TOKEN` support
  (pi #3591) and provider extensions for Kiro / Cursor / xAI — evidence the per-vendor adapter shape works.
- **OpenClaw** — API keys + subscription OAuth. Anthropic via **setup-token** or **reuse of local Claude
  CLI** (no native PKCE login). Codex via full PKCE. Stores per-profile `{access,refresh,expires,accountId}`
  and treats the profile file as a **token sink refreshed under a file lock** — direct corroboration for the
  locked-RMW decision (§5.4).
- **Hermes Agent (NousResearch)** — `PROVIDER_REGISTRY` dataclasses declare each provider's auth type +
  URLs + env vars; one `resolve_runtime_provider()` entry point. Anthropic is **API-key only** (or reuse
  `~/.claude/.credentials.json`). `auth.json` guarded with `fcntl`/`msvcrt` file locks — again corroborates
  §5.4. The registry pattern is the spiritual model for `OAuthVendor`.
- **Vendor CLIs (evidence for the matrix, §6):** Gemini CLI (`code_assist/oauth2.ts`), Antigravity
  (`opencode-antigravity-auth` + `ANTIGRAVITY_API_SPEC.md`), GitHub Copilot CLI, Kiro CLI, xAI/Grok,
  Xiaomi MiMo Code — surveyed 2026-06-24 (§6).

**How this ADR differs:** like OpenClaw/Hermes it keeps one namespaced multi-tenant credential file with
atomic writes + per-refresh rotation handling, and (unlike both) adds native PKCE logins. It adds two things
neither documents cleanly: a **two-axis** auth/inference split, and an explicit **cross-process** locked-RMW
invariant (both flag file locks but for the simpler single-process case).

---

## 4. Design Decision

1. **Adopt a two-axis model.** Auth (how a credential is obtained/refreshed/stored) and inference (how a
   request is sent) are **orthogonal** and must not be coupled. A vendor that serves Claude over Google's
   Code Assist envelope (agy — the Antigravity CLI variant; see §6) reuses neither Anthropic's Messages-V1
   transport nor its auth.
2. **Auth axis = one `OAuthVendor` descriptor + a shared driver built on the official `oauth2` crate** (§5.1;
   the crate is already in-tree via the MCP side). New vendor = new descriptor; PKCE/CSRF/auth-code
   exchange/refresh come from the crate, **not hand-rolled**. The few vendor quirks (e.g. Anthropic's JSON
   token body) are applied through the crate's custom http-client hook, not by forking the flow.
3. **Inference axis = one provider per wire format** (§5.2). Four formats today; no reuse across them.
4. **Credential storage = locked-RMW funnel + per-tenant refresh lock** (§5.4). *Every* write to `auth.json`
   goes through `with_auth_locked` (global lock — file integrity); *every* token refresh serializes on a
   **per-tenant** lock so concurrent processes perform exactly one network refresh per tenant and never
   present a rotated `RT_old` twice (which would trigger OAuth 2.1 §10.4 token-family revocation). A
   Consequence of the multi-writer/cross-process reality, not an optional perf tweak.
5. **Credential-source precedence:** explicit API key → pre-provisioned long-lived OAuth token env
   (`CLAUDE_CODE_OAUTH_TOKEN` and equivalents) → stored interactive OAuth tenant. Rationale + why the env
   path is the preferred fleet mode: §5.3.

---

## 5. Detailed Design

### 5.1 `OAuthVendor` (auth axis)
```rust
trait OAuthVendor {
    fn namespace(&self) -> &str;                 // "codex" / "anthropic-oauth" / "antigravity" ...
    fn client_id(&self) -> String;               // env override + default
    fn client_secret(&self) -> Option<String> { None }    // Gemini = Some(bundled); agy TBD (§9 Q2); Anthropic/Codex = None
    fn authorize_url(&self) -> &str;
    fn token_url(&self) -> &str;
    fn redirect(&self) -> Option<(u16, &'static str)> { None } // Some((port,path)) for loopback PKCE; None for device flow (no redirect endpoint)
    fn scope(&self) -> &str;
    fn extra_authorize_params(&self) -> &[(&str,&str)] { &[] }       // Anthropic: ("code","true")
    fn token_body(&self) -> TokenBodyFormat { TokenBodyFormat::Form } // Anthropic = Json-no-scope
    fn grant(&self) -> AuthGrant { AuthGrant::Pkce }                  // DeviceCode for copilot/kiro
}
enum TokenBodyFormat { Form, Json }
enum AuthGrant { Pkce, DeviceCode }
```
The shared driver is built on the **official `oauth2` crate** (already a dependency via the MCP side): it
supplies PKCE, CSRF `state`, the authorization-code exchange, and refresh; the descriptor only feeds it
per-vendor config. Hand-rolled code is limited to what the crate does not cover — the loopback/paste/
device-code callback plumbing (fold the existing Codex flow into the shared `accept_callback_code` helper —
its comment already says "fold it in"; unify the `127.0.0.1` vs `localhost` bind) and the single
body-encoding override (Anthropic's JSON-no-scope token request, applied via the crate's custom http-client
hook rather than a separate flow).

### 5.2 Inference providers (inference axis — no reuse)
| Provider | Endpoint | Wire format | Vendors |
|---|---|---|---|
| `AnthropicProvider` (exists) | `api.anthropic.com/v1/messages` | Anthropic Messages V1 | claude; mimocode `/anthropic` mirror |
| `OpenAiProvider` (exists) | OpenAI-style `/v1/chat/completions` | OpenAI Chat/Responses | codex, grok, copilot, mimocode |
| `AntigravityProvider` (new) | `cloudcode-pa.googleapis.com` | Google Code Assist (`{project,model,request}`→`{candidates[]}`) | gemini, agy |
| `AwsQProvider` (new, heaviest) | AWS CodeWhisperer/Q | AWS proprietary event-stream | kiro |

OAuth-mode request decoration (Bearer + identity headers/system-block/tool-name casing) stays in the
inference provider; if shared, a small `decorate_request()` hook — never folded into `OAuthVendor`.

### 5.3 Credential-source precedence & the env route
Anthropic offers a route that bypasses interactive login entirely: `claude setup-token` mints a long-lived
subscription OAuth token (~1-year per Anthropic's Claude Code docs) exposed as **`CLAUDE_CODE_OAUTH_TOKEN`**. For pods, ops mints it once and injects
it as a k8s secret — no interactive flow, no `auth.json` write, near-zero race exposure. openab-agent should
read it as a Bearer subscription source, precedence: `ANTHROPIC_API_KEY` → `CLAUDE_CODE_OAUTH_TOKEN` →
stored `anthropic-oauth` tenant. This is the recommended fleet mode; interactive OAuth is for self-service.

### 5.4 Concurrency & storage invariant (folds in the flock decision)
`auth.json` is multi-tenant (`codex`, `anthropic-oauth`, `mcp:<server>`×N) and written by two code paths
(`save_tokens_for`, `McpCredentialStore`) across **multiple processes** (one per Discord thread). Two
distinct hazards demand two locks.

**(a) File integrity — one global lock.** Every write funnels through a single locked RMW so concurrent
writers never lost-update the shared file:
```rust
// ALL writers funnel through this. auth.rs storage layer.
fn with_auth_locked<R>(f: impl FnOnce(&mut HashMap<String, AuthEntry>) -> R) -> Result<R> {
    let _g = flock_exclusive("auth.json.lock")?;  // sidecar file (NOT auth.json — rename swaps its inode)
    let mut map = read_auth_file(&path)?;          // re-read inside lock (anti lost-update)
    let r = f(&mut map);
    write_auth_file(&path, &map)?;                 // existing atomic tmp+rename
    Ok(r)
}
```

**(b) Refresh-token rotation — one lock per tenant.** An earlier draft ran the refresh *outside* the lock
and committed the result inside, claiming "N processes do 1 real refresh." **That is wrong** (Mira review,
2026-06-24): re-read-on-commit only prevents a lost *write* — every process has already *sent* a network
refresh carrying the same `RT_old` before it reaches the commit. Under OAuth 2.1 §10.4 refresh-token
rotation, the second `RT_old` presentation reads as reuse and the AS **revokes the whole token family** =
exactly the fleet-wide logout this ADR exists to prevent. Holding the *global* exclusive lock across the
network refresh would serialize it, but then a slow refresh for one tenant head-of-line-blocks every other
tenant (MCP servers, Codex). So: **one exclusive lock file per tenant**, network I/O held under the tenant
lock only — never under the global lock:
```rust
fn get_or_refresh(tenant: &str) -> Result<String> {
    // 1. fast path — fresh token under a shared (read) global lock
    if let Some(t) = read_fresh(tenant)? { return Ok(t); }
    // 2. serialize refreshes for THIS tenant only (other tenants unaffected)
    let _tg = flock_exclusive(&format!("auth.json.{tenant}.lock"))?;
    // 3. double-check — another process may have refreshed while we waited on the tenant lock
    if let Some(t) = read_fresh(tenant)? { return Ok(t); }
    // 4. exactly one network refresh per tenant per expiry — tenant lock held, global lock NOT
    let fresh = perform_network_refresh(tenant)?;
    // 5. commit under the global lock (fast inode swap, no network I/O inside)
    with_auth_locked(|m| m.insert(tenant.into(), fresh.clone()))?;
    Ok(fresh.access_token)
}
```
- **`flock(2)`, not a sentinel lockfile**: kernel auto-releases on fd close / process death → a hung or
  killed refresher frees its tenant lock instantly. No stale lock, no manual timeout/orphan cleanup.
- **try-lock + timeout** on the global lock so a wedged writer degrades to a graceful error, never a wedged
  worker.
- **Crate:** `rustix::fs::flock` (already in the tree, safe API), gated `#[cfg(unix)]` with a non-unix
  no-op — mirroring the existing atomic-write cfg split. (openab-agent is de-facto unix-only: its
  `ci-openab-agent.yml` is linux, deploy is always container; Windows binaries are the broker only.)
- Each MCP `mcp:<server>` tenant takes its own tenant lock by the same rule, so the MCP `CredentialStore`
  refreshes are serialized per server too — the invariant serves it directly (see `openab-agent-mcp.md`
  §6.1).
- **Until this lands**, prefer the `CLAUDE_CODE_OAUTH_TOKEN` env route (§5.3 — no refresh write, no race);
  treat interactive Anthropic OAuth as not-yet-hardened for high concurrency.

---

## 6. Vendor feasibility matrix (surveyed 2026-06-24)
```
Variant      OAuth style                Inference bucket               Native feasibility
──────────────────────────────────────────────────────────────────────────────────────────
claude       PKCE public  (+env token)  Anthropic Messages V1         ✅ done (#1187) + add env route
codex        PKCE public / device       OpenAI                        ✅ done (has device flow)
grok (xAI)   xai-oauth (sub) / api-key  OpenAI-compatible             🟢 easy (reuse OpenAiProvider)
mimocode     MiMo Platform OAuth/key    OpenAI-compat (+/anthropic)   🟢 easy (dual-bucket; OAuth low-ROI)
copilot      GitHub device flow         OpenAI-compat (githubcopilot) 🟡 token exchange + CC headers
gemini       PKCE + bundled secret      Google Code Assist            🟡 new provider
antigravity  PKCE + bundled secret      Google Code Assist            🟡 same provider; ToS-risk*
kiro         AWS Builder ID device flow AWS Q/CodeWhisperer (propr.)  🔴 hard (event-stream)
cursor       Cursor browser OAuth       Cursor proprietary proxy      🔴 reverse-eng, ToS-risk*
hermes       API-key                    multi                         ⚪ agent shell, not a vendor
opencode     BYO (per-auth plugins)     multi                         ⚪ agent shell
pi           BYO (provider extensions)  multi                         ⚪ agent shell
native       —                          —                             = openab-agent itself
agentcore    AWS SigV4/IAM (not OAuth)  AWS Bedrock                   ❌ out of OAuth scope
```
Concrete values (verified): codex `app_EMoamEEZ73f0CkXaXp7hrann` (no secret, form); claude
`9d1c250a-e61b-44d9-88ed-5944d1962f5e` (no secret, JSON no-scope); gemini
`681255809395-oo8ft2oprdrnp9e3aqf6av3hmdib135j` (+ bundled `GOCSPX-…` — non-confidential by spec but **not** safe as raw repo text; storage decided in §9 Q2); agy
`1071006060591-tmhssin2h21lcre235vtolojh4g403ep` (likely bundled — UNCONFIRMED; redirect `localhost:51121`,
scopes add `cclog`/`experimentsandconfigs`; inference `cloudcode-pa`, needs GCP `project` field; one OAuth
unlocks Claude+Gemini+GPT-OSS; agy ≠ Messages V1).

\* **ToS-risk** = relies on the vendor's official-client OAuth credentials + subscription quota from a
third-party application (openab-agent) rather than the vendor's own client — which may violate that vendor's
Terms of Service.

---

## 7. Auth-trigger UX (PR #1185)
`/auth` is broker-side (`crates/openab-core/src/discord.rs`) — openab-agent advertises no slash commands;
it exposes CLI subcommands the relay shells out to via `$OPENAB_AGENT_AUTH_COMMAND`. The relay is
**poll-and-exit**: print URL+code to stdout, poll the AS, exit 0.
- **Anthropic has NO device flow** (claude.ai = authorization_code only; RFC 8628 unshipped,
  anthropics/claude-code #22992) and #1187's `--no-browser` reads the code from **stdin**, which the relay
  cannot feed → undrivable by `/auth`.
- **Resolution for interactive Claude self-service = two-step, code-as-CLI-arg:** `/auth claude` persists
  the PKCE verifier+state as a pending entry **keyed by the initiating Discord user id**
  (`pending:claude:<discord_user_id>`, reuse the existing `mcp-pending`/`AuthEntry::Pending` machinery) +
  prints the `code=true` URL (claude.ai shows a copyable code); `/auth claude <code>` forwards that same user
  id so `anthropic-oauth --code <code>` loads the matching verifier and completes. No stdin. **Per-user
  keying is required** (Mira review, 2026-06-24): a single global pending entry lets a second concurrent
  user's verifier overwrite the first's → PKCE mismatch on exchange, and worse, lets user B complete a flow
  user A initiated (session hijack). (Fallback: broker pipes a follow-up DM/modal to child stdin — #1185 v2.)
  For pods, the §5.3 env route avoids all of this.
- **Pending-entry GC** (Mira review, 2026-06-24): stamp each `AuthEntry::Pending` with `created_at`;
  `with_auth_locked` opportunistically drops pending entries older than 15 min on every write, so abandoned
  `/auth` attempts (user never pastes a code) don't accumulate stale verifiers in `auth.json`.

---

## 8. Rejected alternatives
- **Per-vendor bespoke flows (status quo):** rejected — N copies of PKCE/loopback/refresh; #1187 already
  duplicated the Codex flow. Doesn't scale to 5+ vendors.
- **Force everything through rmcp `CredentialStore`:** rejected — lossy. `TokenStore` (provider) and rmcp
  `StoredCredentials` are different on-disk shapes (untagged `AuthEntry`); the translation drops fields
  (see `openab-agent-mcp.md` §6.1). The shared layer must sit *below* both (file RMW), not in one's trait.
- **Fully hand-rolled OAuth flow:** rejected — it reimplements PKCE/CSRF/exchange/refresh that the official
  `oauth2` crate (already in-tree) provides. The crate is the chosen basis (§4 decision 2, §5.1); its one
  friction — it defaults to RFC form-encoded token bodies while Anthropic needs JSON-no-scope — is handled
  via the crate's custom http-client hook, not by abandoning it. (`oauth2` is stateless and does **not**
  solve the auth.json race — that's the storage-layer's job, §5.4.)
- **In-process `Mutex` / tokio single-flight, or a sentinel lockfile (create→delete), for the race:**
  rejected — see §5.4 (in-process locks are useless across the per-thread processes; a sentinel lockfile
  deadlocks if a holder dies, whereas `flock(2)` auto-releases on death).
- **Device flow for Anthropic:** not available (Anthropic ships no device endpoint). Hence the env route +
  two-step interactive (§7).
- **Layer-3 auto-trigger now:** deferred — `/auth` manual is sufficient (Brett, 2026-06-24).

---

## 9. Decisions & open questions
1. **Default-model staleness — DECIDED (Brett 2026-06-24): no hardcoded default; require via config/env,
   fail-loud.** Hardcoding `claude-opus-4-8` is a recurring 404 timebomb: this PR exists because the prior
   dated default 404'd on the subscription endpoint, and 4.6+ dateless IDs are **fixed canonical IDs, not
   evergreen aliases** — there is no floating "-latest" to lean on, and Messages V1 mandates a `model`.
   Resolve model as ACP/CLI `model_override` → `OPENAB_AGENT_MODEL` → **error** (no hardcoded fallback);
   drop the three duplicated default sites (`llm.rs:153`, `acp.rs:385/446`). Consequence: removes the
   zero-config default (deployments set model via values.yaml/env already; needs a clear error message +
   CHANGELOG note). Also eliminates the silent Opus cost bump for API-key users.
2. **Bundled `client_secret` storage — REFINED (Mira review 2026-06-24).** Google Code-Assist vendors
   (gemini, agy) ship a `GOCSPX-…` desktop-app secret. By RFC 8252 and Google's own docs this value is
   **non-confidential** (installed-app secret, "obviously not treated as a secret") — there is no
   confidentiality to protect, so obscuring it adds zero cryptographic security. But it is **not safe as raw
   text in a public repo for operational reasons**: GitHub push-protection covers Google secrets **by default**
   (changelog 2026-03), so a raw `GOCSPX-` literal blocks contributor `git push`, and GitHub↔Google partner
   token-scanning may **auto-revoke** the credential once it lands in a public commit. Decision: do **not**
   commit the raw literal; pick per vendor —
   (a) **encode-at-rest** (split/base64) in source — purely scanner-evasion for an already-public value, *not*
   a security control; label it as such inline so reviewers aren't misled into treating it as a real secret; or
   (b) **inject at runtime via env** (no secret in the repo at all) — cleaner provenance, consistent with the
   §5.3 env-route preference, at the cost of the bundled zero-config UX.
   Still confirm whether agy actually *requires* a secret, from the plugin `src/constants.ts` / agy binary.
3. Which heavy/ToS-risk vendors to actually build: kiro (AWS proprietary), cursor (reverse-eng + ToS-risk),
   agy (ToS-risk). Needs an explicit go/no-go.
4. Does the locked-RMW fix also subsume `openab-agent-mcp.md` open items #1 (reqwest 0.12/0.13 split) and
   #8 (doctor/runtime two-store split)? Likely partial.

---

## 10. References

### Internal
- `docs/adr/openab-agent.md` — agent charter (4 tools, no SDK, thin HTTP)
- `docs/adr/openab-agent-mcp.md` — MCP client + §6 OAuth + §6.1 storage format
- `docs/adr/pr-contribution-guidelines.md` — prior-art requirements
- PR #1187 (Anthropic OAuth), PR #1185 (`/auth`), PR #1111 (`--no-browser`)

### External — projects
- Pi `earendil-works/pi` (ported flow; `CLAUDE_CODE_OAUTH_TOKEN` #3591) · OpenClaw · Hermes Agent
- Gemini CLI `code_assist/oauth2.ts` · `NoeFabris/opencode-antigravity-auth` (+ `ANTIGRAVITY_API_SPEC.md`)
- GitHub `copilot-cli` · Kiro CLI / `pi-provider-kiro` / `kiro-gateway` · xAI API / `pi-xai-oauth`
- Xiaomi `MiMo-Code`

### External — specs
- RFC 8628 (Device Authorization Grant) · OAuth 2.1 §10.4 (refresh-token rotation/reuse)
- anthropics/claude-code #22992 (device-flow request), #20215 (MCP device flow)
- [Documenting Architecture Decisions — Nygard (2011)](https://cognitect.com/blog/2011/11/15/documenting-architecture-decisions.html)
