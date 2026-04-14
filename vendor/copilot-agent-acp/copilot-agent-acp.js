#!/usr/bin/env node
// copilot-agent-acp
// ------------------
// An ACP (Agent Client Protocol) bridge for GitHub Copilot CLI.
//
// Why this exists: `copilot --acp` (the built-in ACP server mode) does NOT
// forward SDK-level telemetry like `session.usage_info` — so clients like
// OpenAB cannot display per-session token usage. This bridge uses the
// Copilot SDK directly, captures all session events, and re-exposes them
// via ACP notifications (and custom `_meta/*` methods).
//
// It implements the ACP Agent interface:
//   initialize, session/new, session/prompt, session/set_model, session/set_mode
// Plus custom extensions:
//   _meta/getUsage, _meta/getStats
//
// Design: single Node process, stdin/stdout JSON-RPC, one CopilotClient
// shared across all sessions spawned by this process.

'use strict';

const fs = require('fs');
const path = require('path');

// Dynamically resolve Copilot SDK path — searches the standard install location
// for the newest version available on this machine.
function findCopilotSdk() {
  const localApp = process.env.LOCALAPPDATA || path.join(process.env.USERPROFILE || process.env.HOME || '', 'AppData', 'Local');
  const pkgDir = path.join(localApp, 'copilot', 'pkg');
  // Try platform dirs (win32-x64, win32-arm64, darwin-*, linux-*)
  try {
    const platforms = fs.readdirSync(pkgDir).filter(d => fs.statSync(path.join(pkgDir, d)).isDirectory());
    for (const platform of platforms) {
      const versions = fs.readdirSync(path.join(pkgDir, platform))
        .filter(d => fs.statSync(path.join(pkgDir, platform, d)).isDirectory())
        .sort()
        .reverse(); // newest first
      for (const ver of versions) {
        const sdkPath = path.join(pkgDir, platform, ver, 'copilot-sdk', 'index.js');
        if (fs.existsSync(sdkPath)) return sdkPath;
      }
    }
  } catch {}
  // Fallback: env override
  if (process.env.COPILOT_SDK_PATH) return process.env.COPILOT_SDK_PATH;
  throw new Error(`Copilot SDK not found in ${pkgDir}. Set COPILOT_SDK_PATH env var.`);
}

const SDK_PATH = findCopilotSdk();
const sdkPromise = import('file:///' + SDK_PATH);

// Default model applied to every new session the bridge creates. Override
// via COPILOT_DEFAULT_MODEL env var. Intent: Discord use-case is quick Q&A
// → cheap model; terminal copilot-cli keeps its own config-level default.
const DEFAULT_MODEL = process.env.COPILOT_DEFAULT_MODEL || 'gpt-5-mini';

// MCP servers injected into every session via SDK's SessionConfig.mcpServers.
// Reads from ~/.copilot/mcp-config.json (same file as `copilot mcp` CLI).
const MCP_CONFIG_PATH = path.join(
  process.env.USERPROFILE || process.env.HOME || 'C:/Users/Administrator',
  '.copilot', 'mcp-config.json'
);
let SESSION_MCP_SERVERS = {};
try {
  const raw = JSON.parse(fs.readFileSync(MCP_CONFIG_PATH, 'utf8'));
  if (raw.mcpServers && typeof raw.mcpServers === 'object') {
    // Convert our config format to SDK format (add tools: ["*"] default)
    for (const [name, cfg] of Object.entries(raw.mcpServers)) {
      SESSION_MCP_SERVERS[name] = { tools: ['*'], ...cfg };
    }
  }
} catch {}
const mcpCount = Object.keys(SESSION_MCP_SERVERS).length;
if (mcpCount > 0) {
  process.stderr.write(`[bridge] loaded ${mcpCount} MCP server(s) from ${MCP_CONFIG_PATH}\n`);
}

// System message appended to every new session. Loads persona from
// gitx-system-prompt.txt (七海建人 style), falls back to basic language rules.
// Override via COPILOT_APPEND_SYSTEM env var.
const PROMPT_FILE = path.resolve(__dirname, '../../gitx-system-prompt.txt');
let loadedPrompt = '';
try { loadedPrompt = fs.readFileSync(PROMPT_FILE, 'utf8').trim(); } catch {}

const DEFAULT_APPEND_SYSTEM =
  process.env.COPILOT_APPEND_SYSTEM ||
  (loadedPrompt || [
    'Always respond in **Traditional Chinese (zh-TW / 繁體中文)**. Never use Simplified Chinese.',
    'Use 台灣 conventions (e.g. 軟體 not 软件, 檔案 not 文件, 程式 not 程序).',
    'Technical terms may remain in English.',
    'You are running inside a Discord bot, so format replies for Discord Markdown (no tables — use bullet points).',
    'Keep responses concise unless the user asks for depth.',
  ].join('\n'));

// ---- stdio JSON-RPC plumbing ---------------------------------------

let stdinBuf = '';
process.stdin.setEncoding('utf8');
process.stdin.on('data', chunk => {
  stdinBuf += chunk;
  let idx;
  while ((idx = stdinBuf.indexOf('\n')) !== -1) {
    const line = stdinBuf.substring(0, idx).trim();
    stdinBuf = stdinBuf.substring(idx + 1);
    if (!line) continue;
    try {
      const msg = JSON.parse(line);
      handleMessage(msg).catch(err => {
        logError('handleMessage failed: ' + (err.stack || err.message));
      });
    } catch (e) {
      logError('stdin parse error: ' + e.message);
    }
  }
});

function send(obj) {
  process.stdout.write(JSON.stringify(obj) + '\n');
}

function sendResponse(id, result) {
  send({ jsonrpc: '2.0', id, result });
}

function sendError(id, code, message, data) {
  send({ jsonrpc: '2.0', id, error: { code, message, data } });
}

function sendNotification(method, params) {
  send({ jsonrpc: '2.0', method, params });
}

function logError(msg) {
  process.stderr.write('[copilot-agent-acp] ERROR ' + msg + '\n');
}

function logInfo(msg) {
  process.stderr.write('[copilot-agent-acp] INFO ' + msg + '\n');
}

// ---- state ----------------------------------------------------------

let client = null;
let sdk = null;
const sessions = new Map(); // sessionId -> { session, lastUsage, turnInProgress }

// ---- lifecycle ------------------------------------------------------

/// Map from sessionId → raw `session.create` RPC response body. Populated
/// by the monkey-patched `connection.sendRequest` below. Used to extract
/// the session-scoped configOptions (including the filtered model list)
/// which the SDK's CopilotClient.createSession() discards.
const rawSessionCreateResponses = new Map();

async function ensureClient() {
  if (client) return client;
  sdk = await sdkPromise;
  client = new sdk.CopilotClient({
    cliArgs: ['--enable-all-github-mcp-tools'],
  });
  await client.start();
  logInfo('CopilotClient started');

  // Monkey-patch connection.sendRequest to capture session.create responses.
  // The SDK's high-level createSession() reads only workspacePath + capabilities
  // from the response and drops configOptions — which is exactly where the
  // user-accessible model list (filtered by plan/entitlements) lives.
  try {
    const conn = client.connection;
    if (conn && typeof conn.sendRequest === 'function') {
      const origSend = conn.sendRequest.bind(conn);
      conn.sendRequest = async function (method, ...args) {
        const resp = await origSend(method, ...args);
        if (method === 'session.create' && resp && resp.sessionId) {
          rawSessionCreateResponses.set(resp.sessionId, resp);
        }
        return resp;
      };
      logInfo('connection.sendRequest monkey-patched for configOptions capture');
    } else {
      logError('client.connection.sendRequest not accessible — cannot capture configOptions');
    }
  } catch (e) {
    logError('failed to patch sendRequest: ' + e.message);
  }

  return client;
}

/// Extract the session-scoped model list from a captured session.create response.
/// Falls back to client.listModels() (unfiltered) if configOptions isn't available.
function extractSessionModels(sessionId) {
  const raw = rawSessionCreateResponses.get(sessionId);
  if (!raw || !Array.isArray(raw.configOptions)) return null;
  const modelOpt = raw.configOptions.find(o => o.id === 'model');
  if (!modelOpt || !Array.isArray(modelOpt.options)) return null;
  return modelOpt.options.map(o => ({
    modelId: o.value,
    name: o.name || o.value,
    description: o.description || o.name || '',
  }));
}

// ---- request dispatch ----------------------------------------------

async function handleMessage(msg) {
  const { id, method, params } = msg;

  // Only handle requests (ignore responses from the client side)
  if (method === undefined) return;

  try {
    switch (method) {
      case 'initialize':
        return sendResponse(id, await handleInitialize(params));

      case 'session/new':
        return sendResponse(id, await handleSessionNew(params));

      case 'session/load':
        return sendResponse(id, await handleSessionLoad(params));

      case 'session/prompt':
        return sendResponse(id, await handleSessionPrompt(params));

      case 'session/set_model':
        return sendResponse(id, await handleSetModel(params));

      case 'session/set_mode':
        return sendResponse(id, await handleSetMode(params));

      case 'authenticate':
        return sendResponse(id, {});

      case 'fs/read_text_file':
      case 'fs/write_text_file':
        // OpenAB doesn't use these; stub them out.
        return sendResponse(id, {});

      // --- custom extensions ---
      case '_meta/getUsage':
        return sendResponse(id, await handleGetUsage(params));

      case '_meta/getStats':
        return sendResponse(id, handleGetStats(params));

      case '_meta/respondPermission':
        return sendResponse(id, await handleRespondPermission(params));

      case '_meta/compactSession':
        return sendResponse(id, await handleCompactSession(params));

      case '_meta/getRecentPermissions':
        return sendResponse(id, handleGetRecentPermissions(params));

      case '_meta/ping':
        return sendResponse(id, { pong: true, ts: Date.now(), sessions: sessions.size });

      case '_meta/getSessionModels':
        return sendResponse(id, handleGetSessionModels(params));

      default:
        return sendError(id, -32601, `Method not found: ${method}`);
    }
  } catch (err) {
    logError(`${method} failed: ${err.stack || err.message}`);
    return sendError(id, -32000, err.message || String(err));
  }
}

// ---- handler implementations ---------------------------------------

async function handleInitialize(params) {
  await ensureClient();
  return {
    protocolVersion: 1,
    agentCapabilities: {
      loadSession: true,
      promptCapabilities: {
        image: true,
        audio: false,
        embeddedContext: true,
      },
      sessionCapabilities: { list: {} },
      _meta: {
        bridge: 'copilot-agent-acp/0.1.0',
        features: ['usage-tracking', 'real-compact', 'tool-stream'],
      },
    },
    agentInfo: {
      name: 'copilot-agent-acp',
      title: 'GitHub Copilot (bridge)',
      version: '0.1.0',
    },
    authMethods: [],
  };
}

async function handleSessionNew(params) {
  const c = await ensureClient();
  const cwd = params?.cwd || process.cwd();
  const sessionOpts = {
    cwd,
    onPermissionRequest: sdk.approveAll || (async () => ({ kind: 'approved' })),
    systemMessage: {
      mode: 'append',
      content: DEFAULT_APPEND_SYSTEM,
    },
  };
  // Inject MCP servers + configDir so SDK picks up mcp-config.json
  if (Object.keys(SESSION_MCP_SERVERS).length > 0) {
    sessionOpts.mcpServers = SESSION_MCP_SERVERS;
    logInfo(`session/new injecting ${Object.keys(SESSION_MCP_SERVERS).length} MCP server(s) via mcpServers`);
  }
  // Also pass configDir so SDK can find mcp-config.json, skills, etc.
  sessionOpts.configDir = path.join(
    process.env.USERPROFILE || process.env.HOME || 'C:/Users/Administrator',
    '.copilot'
  );
  logInfo(`session/new configDir=${sessionOpts.configDir}`);
  const session = await c.createSession(sessionOpts);

  const state = { session, lastUsage: null, turnInProgress: null };
  sessions.set(session.sessionId, state);

  // Subscribe to session events and keep cached state.
  session.on(ev => {
    handleSessionEvent(session.sessionId, ev).catch(e =>
      logError('event handler: ' + e.message)
    );
  });

  // Apply the bridge default model. Best-effort: if the model isn't
  // available or switchTo fails, keep whatever the CLI default was.
  let appliedDefault = false;
  try {
    await session.rpc.model.switchTo({ modelId: DEFAULT_MODEL });
    appliedDefault = true;
    logInfo(`session/new default model → ${DEFAULT_MODEL}`);
  } catch (e) {
    logError(`default model switch failed (${DEFAULT_MODEL}): ${e.message}`);
  }

  // Extract models from the captured session.create response (filtered by
  // user entitlements) — falls back to the unfiltered client.listModels()
  // if the capture failed.
  let models = null;
  try {
    const cur = await session.rpc.model.getCurrent().catch(() => ({}));
    let sessionModels = extractSessionModels(session.sessionId);
    if (!sessionModels) {
      const listed = await c.listModels();
      sessionModels = (listed || []).map(m => ({
        modelId: m.id || m.modelId,
        name: m.name || m.id,
        description: m.description || m.name || '',
      }));
    }
    // Filter out Claude models — Copilot Pro doesn't include them
    const filteredModels = sessionModels.filter(m => !m.modelId?.startsWith('claude-'));
    models = {
      currentModelId: appliedDefault ? DEFAULT_MODEL : (cur?.modelId || 'default'),
      availableModels: filteredModels,
    };
  } catch (_) {}

  logInfo(`session/new id=${session.sessionId} default=${DEFAULT_MODEL}`);
  return {
    sessionId: session.sessionId,
    models,
  };
}

async function handleSessionLoad(params) {
  const c = await ensureClient();
  const sessionId = params?.sessionId;
  if (!sessionId) {
    // No sessionId to resume — fall through to new session.
    return await handleSessionNew(params);
  }

  // Attempt real SDK session resume. Falls back to new session on error
  // (e.g. expired / non-existent session).
  try {
    const resumeOpts = {
      onPermissionRequest:
        sdk.approveAll || (async () => ({ kind: 'approved' })),
      systemMessage: {
        mode: 'append',
        content: DEFAULT_APPEND_SYSTEM,
      },
    };
    if (Object.keys(SESSION_MCP_SERVERS).length > 0) {
      resumeOpts.mcpServers = SESSION_MCP_SERVERS;
    }
    resumeOpts.configDir = path.join(
      process.env.USERPROFILE || process.env.HOME || 'C:/Users/Administrator',
      '.copilot'
    );
    const session = await c.resumeSession(sessionId, resumeOpts);

    const state = { session, lastUsage: null, turnInProgress: null };
    sessions.set(session.sessionId, state);

    session.on(ev => {
      handleSessionEvent(session.sessionId, ev).catch(e =>
        logError('event handler: ' + e.message)
      );
    });

    let models = null;
    try {
      const listed = await c.listModels();
      const cur = await session.rpc.model.getCurrent().catch(() => ({}));
      models = {
        currentModelId: cur?.modelId || 'default',
        availableModels: (listed || []).map(m => ({
          modelId: m.id || m.modelId,
          name: m.name || m.id,
          description: m.description || m.name || '',
        })),
      };
    } catch (_) {}

    logInfo(`session/load resumed id=${session.sessionId}`);
    return { sessionId: session.sessionId, models };
  } catch (err) {
    logError(`session/load: resume failed (${err.message}), creating new session`);
    return await handleSessionNew(params);
  }
}

async function handleSessionPrompt(params) {
  const { sessionId, prompt } = params;
  const state = sessions.get(sessionId);
  if (!state) throw new Error(`unknown sessionId: ${sessionId}`);

  // Extract text + attachments from prompt content blocks.
  const text = extractPromptText(prompt);
  const attachments = extractPromptAttachments(prompt);

  // If neither text nor attachments — skip
  if (!text && attachments.length === 0) {
    return { stopReason: 'end_turn' };
  }

  // Slash-command interception (text only — skip if attachments present so
  // the LLM sees the image alongside the slash-looking text).
  if (text && attachments.length === 0) {
    const trimmed = text.trim();
    if (trimmed.startsWith('/')) {
      const slashResult = await handleSlashCommand(sessionId, state, trimmed);
      if (slashResult) {
        sendNotification('session/update', {
          sessionId,
          update: {
            sessionUpdate: 'agent_message_chunk',
            content: { type: 'text', text: slashResult },
          },
        });
        return { stopReason: 'end_turn' };
      }
    }
  }

  // Create a promise that resolves when the assistant turn ends.
  let resolveTurn;
  const turnPromise = new Promise(res => (resolveTurn = res));
  state.turnInProgress = { resolveTurn, sessionId };

  // Send with attachments if any were present.
  const sendOpts = { prompt: text || '(see attached image)' };
  if (attachments.length > 0) {
    sendOpts.attachments = attachments;
    logInfo(`session/prompt with ${attachments.length} attachment(s)`);
  }

  try {
    await state.session.send(sendOpts);
  } catch (err) {
    state.turnInProgress = null;
    throw err;
  }

  await turnPromise;
  state.turnInProgress = null;
  return { stopReason: 'end_turn' };
}

// ---- Slash command registry ----------------------------------------
//
// A single table of all bridge-handled slash commands. Each entry maps a
// command name → RPC call (or literal function). Adding a new slash
// command is now one-line.
//
// arg spec:
//   'none'            — no args
//   'single'          — exactly one word
//   'optional_single' — zero or one word
//   'rest'            — all remaining text as a single string
//
// Example:
//   { name:'foo', desc:'do foo', args:'single',
//     call: (s, a) => s.session.rpc.foo.bar({x: a[0]}),
//     format: r => `✅ foo=${r.foo}` }
//
// The dispatcher catches errors and returns `⚠️ /foo failed: <msg>`.
// Return null (or undefined) from `call` to fall through to LLM.

const SLASH_REGISTRY = [
  // ---- Client-level (no session) ----
  { name: 'usage', desc: 'Account quota (premium requests)', args: 'none',
    call: async () => (await client.rpc.account.getQuota()).quotaSnapshots,
    format: (q) => {
      const p = q.premium_interactions;
      return p
        ? `📊 **Usage** — Premium **${p.remainingPercentage}%** remaining (${p.usedRequests}/${p.entitlementRequests}). Resets ${p.resetDate}`
        : '⚠️ No quota data';
    } },

  { name: 'tokens', desc: 'Session context-window token usage', args: 'none',
    call: async (s) => s.lastUsage,
    format: (u) => {
      if (!u) return '_(no usage captured yet — send a regular message first)_';
      const pct = u.tokenLimit ? Math.round((u.currentTokens / u.tokenLimit) * 100) : 0;
      return `🧮 **Tokens** (${pct}% full)\n` +
        `Used ${(u.currentTokens / 1000).toFixed(1)}k / ${(u.tokenLimit / 1000).toFixed(0)}k · ` +
        `sys ${(u.systemTokens / 1000).toFixed(1)}k · ` +
        `tools ${(u.toolDefinitionsTokens / 1000).toFixed(1)}k · ` +
        `conv ${(u.conversationTokens / 1000).toFixed(1)}k`;
    } },

  { name: 'models', desc: 'List catalog-available models', args: 'none',
    call: async () => client.listModels(),
    format: (arr) => `🤖 **Models** (${(arr || []).length})\n` +
      (arr || []).slice(0, 20).map(m => `• \`${m.id}\` — ${m.name}`).join('\n') },

  { name: 'auth', desc: 'GitHub authentication status', args: 'none',
    call: async () => client.getAuthStatus(),
    format: (a) => a?.isAuthenticated
      ? `✅ Authenticated as **${a.login}** via ${a.authType}`
      : `❌ Not authenticated` },

  { name: 'ping', desc: 'Ping the Copilot CLI server', args: 'none',
    call: async () => client.ping('bridge-ping') },

  // ---- Session: model ----
  { name: 'model', desc: 'Get or switch session model', args: 'optional_single',
    call: async (s, a) => a[0]
      ? (await s.session.rpc.model.switchTo({ modelId: a[0] }), { switched: a[0] })
      : s.session.rpc.model.getCurrent(),
    format: (r) => r.switched ? `✅ Switched to \`${r.switched}\`` : `🤖 Current: \`${r?.modelId || 'default'}\`` },

  // ---- Session: mode ----
  { name: 'mode', desc: 'Get or set session mode (agent/plan/autopilot)', args: 'optional_single',
    call: async (s, a) => a[0]
      ? (await s.session.rpc.mode.set({ modeId: a[0] }), { set: a[0] })
      : s.session.rpc.mode.get(),
    format: (r) => r.set ? `✅ Mode → \`${r.set}\`` : `🔀 Current mode: \`${r?.modeId || 'default'}\`` },

  // ---- Session: plan ----
  { name: 'plan', desc: 'Read session plan.md', args: 'none',
    call: async (s) => s.session.rpc.plan.read(),
    format: (p) => `📋 **Plan**\n\`\`\`\n${(p?.content || '(empty)').slice(0, 1500)}\n\`\`\`` },

  { name: 'plan-update', desc: 'Replace session plan content', args: 'rest',
    call: async (s, a) => (await s.session.rpc.plan.update({ content: a[0] || '' }), true),
    format: () => '✅ Plan updated' },

  { name: 'plan-delete', desc: 'Delete session plan', args: 'none', dangerous: true,
    call: async (s) => (await s.session.rpc.plan.delete(), true),
    format: () => '✅ Plan deleted' },

  // ---- Session: workspace ----
  { name: 'files', desc: 'List workspace files', args: 'none',
    call: async (s) => s.session.rpc.workspace.listFiles(),
    format: (r) => {
      const arr = r?.files || [];
      return `📁 **Files** (${arr.length})\n` + arr.slice(0, 30).map(f => `• \`${f.path || f}\``).join('\n');
    } },

  { name: 'read', desc: 'Read a file from workspace', args: 'single',
    call: async (s, a) => s.session.rpc.workspace.readFile({ path: a[0] }),
    format: (r) => `\`\`\`\n${(r?.content || '').slice(0, 1800)}\n\`\`\`` },

  // ---- Session: agents ----
  { name: 'agents', desc: 'List session agents', args: 'none',
    call: async (s) => s.session.rpc.agent.list(),
    format: (r) => `🤖 **Agents** (${r?.agents?.length || 0})\n` +
      (r?.agents || []).slice(0, 25).map(a => `• ${a.name}`).join('\n') },

  { name: 'agent', desc: 'Get current or select agent', args: 'optional_single',
    call: async (s, a) => a[0]
      ? (await s.session.rpc.agent.select({ name: a[0] }), { selected: a[0] })
      : s.session.rpc.agent.getCurrent(),
    format: (r) => r.selected ? `✅ Agent → \`${r.selected}\`` : `🤖 Current agent: \`${r?.name || '(none)'}\`` },

  { name: 'agent-deselect', desc: 'Clear agent selection', args: 'none', dangerous: true,
    call: async (s) => (await s.session.rpc.agent.deselect(), true),
    format: () => '✅ Agent deselected' },

  { name: 'agent-reload', desc: 'Reload agents registry', args: 'none',
    call: async (s) => (await s.session.rpc.agent.reload(), true),
    format: () => '✅ Agents reloaded' },

  // ---- Session: skills ----
  { name: 'skills', desc: 'List session skills', args: 'none',
    call: async (s) => s.session.rpc.skills.list(),
    format: (r) => `⚡ **Skills** (${r?.skills?.length || 0}, showing 25)\n` +
      (r?.skills || []).slice(0, 25).map(x => `• ${x.name}`).join('\n') },

  { name: 'skill-on', desc: 'Enable a skill', args: 'single',
    call: async (s, a) => (await s.session.rpc.skills.enable({ name: a[0] }), { enabled: a[0] }),
    format: (r) => `✅ Skill \`${r.enabled}\` enabled` },

  { name: 'skill-off', desc: 'Disable a skill', args: 'single', dangerous: true,
    call: async (s, a) => (await s.session.rpc.skills.disable({ name: a[0] }), { disabled: a[0] }),
    format: (r) => `✅ Skill \`${r.disabled}\` disabled` },

  // ---- Session: mcp ----
  { name: 'mcp', desc: 'List MCP servers', args: 'none',
    call: async (s) => s.session.rpc.mcp.list(),
    format: (r) => `🔌 **MCP Servers** (${r?.servers?.length || 0})\n` +
      (r?.servers || []).map(x => `• ${x.name}`).join('\n') },

  { name: 'mcp-on', desc: 'Enable an MCP server', args: 'single',
    call: async (s, a) => (await s.session.rpc.mcp.enable({ name: a[0] }), { enabled: a[0] }),
    format: (r) => `✅ MCP \`${r.enabled}\` enabled` },

  { name: 'mcp-off', desc: 'Disable an MCP server', args: 'single', dangerous: true,
    call: async (s, a) => (await s.session.rpc.mcp.disable({ name: a[0] }), { disabled: a[0] }),
    format: (r) => `✅ MCP \`${r.disabled}\` disabled` },

  // ---- Session: plugins / extensions ----
  { name: 'plugins', desc: 'List installed plugins', args: 'none',
    call: async (s) => s.session.rpc.plugins.list(),
    format: (r) => `🧩 **Plugins** (${r?.plugins?.length || 0})\n` +
      (r?.plugins || []).map(x => `• ${x.name}`).join('\n') },

  { name: 'extensions', desc: 'List extensions', args: 'none',
    call: async (s) => s.session.rpc.extensions.list(),
    format: (r) => `🧬 **Extensions** (${r?.extensions?.length || 0})\n` +
      (r?.extensions || []).map(x => `• ${x.name}`).join('\n') },

  // ---- Session: lifecycle ----
  { name: 'compact', desc: 'LLM-compact session history', args: 'none', dangerous: true,
    call: async (s) => s.session.rpc.compaction.compact(),
    format: (r) => `✅ Compacted — ${r?.tokensRemoved || 0} tokens removed` },

  { name: 'fleet', desc: 'Start parallel subagents', args: 'rest', dangerous: true,
    call: async (s, a) => s.session.rpc.fleet.start({ prompt: a[0] || '' }),
    format: (r) => `🚀 Fleet started\n\`\`\`json\n${JSON.stringify(r, null, 2).slice(0, 1500)}\n\`\`\`` },

  // ---- Session: shell (owner-only, allowlist enforced at OpenAB layer) ----
  { name: 'shell', desc: 'Execute a shell command directly', args: 'rest', dangerous: true,
    call: async (s, a) => s.session.rpc.shell.exec({ command: a[0] || '' }),
    format: (r) => {
      const out = r?.output || r?.stdout || JSON.stringify(r, null, 2);
      const trunc = String(out).slice(0, 1800);
      return `\`\`\`\n${trunc}\n\`\`\`${String(out).length > 1800 ? '\n_(output truncated)_' : ''}`;
    } },

  // ---- Meta ----
  { name: 'help', desc: 'List all bridge slash commands', args: 'none',
    call: async () => SLASH_REGISTRY,
    format: (reg) => {
      const lines = reg
        .filter(e => e.name !== 'help')
        .map(e => {
          const marker = e.dangerous ? ' ⚠️' : '';
          return `• \`/${e.name}\`${marker} — ${e.desc}`;
        });
      return `🛠 **Bridge slash commands** (${lines.length})\n${lines.join('\n')}\n\n` +
        `_Unknown slash commands fall through to the LLM._\n` +
        `_⚠️ = destructive, requires \`--confirm\` suffix._`;
    } },
];

/// Parse raw args array according to an arg-spec string.
function parseSlashArgs(spec, raw) {
  if (!spec || spec === 'none') return [];
  if (spec === 'single') return [raw[0] || ''];
  if (spec === 'optional_single') return raw[0] ? [raw[0]] : [];
  if (spec === 'rest') return [raw.join(' ')];
  // Array spec — positional with last as rest
  if (Array.isArray(spec)) {
    const out = [];
    for (let i = 0; i < spec.length; i++) {
      if (spec[i] === 'rest') {
        out.push(raw.slice(i).join(' '));
        break;
      }
      out.push(raw[i] || '');
    }
    return out;
  }
  return raw;
}

/// Try to handle a slash command typed as prompt text. Returns a string to
/// display to the user, or null if the command isn't recognized (so the
/// caller falls back to normal LLM processing).
///
/// Dangerous commands (flagged `dangerous: true` in the registry) require
/// a `--confirm` flag anywhere in the args to execute. Without it, the
/// dispatcher returns a warning explaining how to confirm.
async function handleSlashCommand(sessionId, state, raw) {
  const parts = raw.slice(1).trim().split(/\s+/);
  const cmd = parts[0]?.toLowerCase();
  const rawArgs = parts.slice(1);
  if (!cmd) return null;

  const entry = SLASH_REGISTRY.find(e => e.name === cmd);
  if (!entry) return null; // unknown — fall through to LLM

  // Separate --confirm from real args
  const hasConfirm = rawArgs.includes('--confirm');
  const argsNoConfirm = rawArgs.filter(a => a !== '--confirm');

  if (entry.dangerous && !hasConfirm) {
    const preview = argsNoConfirm.length > 0
      ? `\n**Command:** \`/${cmd} ${argsNoConfirm.join(' ')}\``
      : `\n**Command:** \`/${cmd}\``;
    return `⚠️ **\`/${cmd}\` is a destructive command.**${preview}\n\n` +
      `To execute, append **\`--confirm\`**:\n` +
      `\`\`\`\n/${cmd}${argsNoConfirm.length ? ' ' + argsNoConfirm.join(' ') : ''} --confirm\n\`\`\``;
  }

  try {
    const parsedArgs = parseSlashArgs(entry.args, argsNoConfirm);
    const result = await entry.call(state, parsedArgs);
    if (entry.format) return entry.format(result);
    return '```json\n' + JSON.stringify(result, null, 2).slice(0, 1800) + '\n```';
  } catch (e) {
    return `⚠️ /${cmd} failed: ${e.message}`;
  }
}

async function handleSetModel(params) {
  const { sessionId, modelId } = params;
  const state = sessions.get(sessionId);
  if (!state) throw new Error(`unknown sessionId: ${sessionId}`);
  await state.session.rpc.model.switchTo({ modelId });
  return {};
}

async function handleSetMode(params) {
  const { sessionId, modeId } = params;
  const state = sessions.get(sessionId);
  if (!state) throw new Error(`unknown sessionId: ${sessionId}`);
  await state.session.rpc.mode.set({ modeId });
  return {};
}

async function handleGetUsage(params) {
  const { sessionId } = params || {};
  if (sessionId) {
    const state = sessions.get(sessionId);
    if (!state) throw new Error(`unknown sessionId: ${sessionId}`);
    return {
      session_usage: state.lastUsage || null,
      cost_totals: state.costTotals || null,
      account_quota: await client.rpc.account.getQuota(),
    };
  }
  return {
    account_quota: await client.rpc.account.getQuota(),
    session_usage: null,
    cost_totals: null,
  };
}

function handleGetStats(_params) {
  return {
    sessions: sessions.size,
    bridge_version: '0.1.0',
  };
}

/// Handle _meta/respondPermission — relay the client's decision to the SDK.
async function handleRespondPermission(params) {
  const { sessionId, requestId, decision } = params || {};
  const state = sessions.get(sessionId);
  if (!state) throw new Error(`unknown sessionId: ${sessionId}`);
  const pending = state.pendingPermissions?.get(requestId);
  if (!pending) throw new Error(`no pending permission for requestId: ${requestId}`);
  state.pendingPermissions.delete(requestId);

  const sdkDecision =
    decision === 'approve_once' || decision === 'approved'
      ? { kind: 'approved' }
      : decision === 'approve_always' || decision === 'allow_always'
      ? { kind: 'approved' }
      : { kind: 'rejected', reason: 'user denied' };

  if (state.session.clientSessionApis?.respondToPermissionRequest) {
    await state.session.clientSessionApis.respondToPermissionRequest({
      requestId,
      result: sdkDecision,
    });
  }
  return { ok: true };
}

/// Handle _meta/getRecentPermissions — return the audit trail of recent
/// tool permission requests for the given session.
function handleGetRecentPermissions(params) {
  const { sessionId } = params || {};
  const state = sessions.get(sessionId);
  if (!state) throw new Error(`unknown sessionId: ${sessionId}`);
  return {
    permissions: state.recentPermissions || [],
    count: (state.recentPermissions || []).length,
  };
}

/// Handle _meta/getSessionModels — return the session-scoped model list
/// (filtered by user plan/entitlements), extracted from the captured
/// session.create RPC response. Falls back to client.listModels() if
/// the capture isn't available for this sessionId.
function handleGetSessionModels(params) {
  const { sessionId } = params || {};
  if (!sessionId) throw new Error('sessionId required');
  const sessionModels = extractSessionModels(sessionId);
  return {
    models: sessionModels || [],
    fallback: !sessionModels,
  };
}

/// Handle _meta/compactSession — LLM-summarize the conversation history.
async function handleCompactSession(params) {
  const { sessionId } = params || {};
  const state = sessions.get(sessionId);
  if (!state) throw new Error(`unknown sessionId: ${sessionId}`);
  if (!state.session.rpc?.compaction?.compact) {
    throw new Error('SDK compaction API not available');
  }
  const result = await state.session.rpc.compaction.compact();
  return { ok: true, tokens_removed: result?.tokensRemoved ?? null, result };
}

// ---- session event → ACP notification translation -----------------

async function handleSessionEvent(sessionId, ev) {
  const state = sessions.get(sessionId);
  if (!state) return;

  switch (ev.type) {
    case 'session.usage_info':
      // Capture usage info for /usage queries.
      state.lastUsage = ev.data;
      break;

    case 'assistant.usage': {
      // Per-turn usage with cost — accumulate across the session.
      const u = ev.data || {};
      state.costTotals = state.costTotals || {
        turns: 0,
        inputTokens: 0,
        outputTokens: 0,
        cacheReadTokens: 0,
        cacheWriteTokens: 0,
        cost: 0,
        lastModel: null,
        perModel: {},
      };
      const model = u.model || 'unknown';
      state.costTotals.turns += 1;
      state.costTotals.inputTokens += u.inputTokens || 0;
      state.costTotals.outputTokens += u.outputTokens || 0;
      state.costTotals.cacheReadTokens += u.cacheReadTokens || 0;
      state.costTotals.cacheWriteTokens += u.cacheWriteTokens || 0;
      state.costTotals.cost += Number(u.cost) || 0;
      state.costTotals.lastModel = model;
      // Per-model breakdown
      const pm = state.costTotals.perModel[model] = state.costTotals.perModel[model] || {
        turns: 0, inputTokens: 0, outputTokens: 0, cacheReadTokens: 0,
      };
      pm.turns += 1;
      pm.inputTokens += u.inputTokens || 0;
      pm.outputTokens += u.outputTokens || 0;
      pm.cacheReadTokens += u.cacheReadTokens || 0;
      // Persist cost_totals to disk for /usage probe
      try {
        const fs = require('fs');
        const path = require('path');
        const dir = path.join(process.env.APPDATA || 'C:/Users/Administrator/AppData/Roaming', 'openab');
        fs.mkdirSync(dir, { recursive: true });
        fs.writeFileSync(path.join(dir, `cost-totals-${sessionId}.json`), JSON.stringify(state.costTotals));
      } catch {}
      break;
    }

    case 'assistant.message': {
      // Forward any text content as an agent_message_chunk.
      const content = ev.data?.content || '';
      if (content) {
        sendNotification('session/update', {
          sessionId,
          update: {
            sessionUpdate: 'agent_message_chunk',
            content: { type: 'text', text: content },
          },
        });
      }
      break;
    }

    case 'assistant.reasoning': {
      // Optional: forward reasoning as a thought notification.
      const content = ev.data?.content || ev.data?.reasoning || '';
      if (content) {
        sendNotification('session/update', {
          sessionId,
          update: {
            sessionUpdate: 'agent_thought_chunk',
            content: { type: 'text', text: content },
          },
        });
      }
      break;
    }

    case 'tool.execution_start': {
      const name = ev.data?.toolName || 'tool';
      const args = ev.data?.arguments || {};
      sendNotification('session/update', {
        sessionId,
        update: {
          sessionUpdate: 'tool_call',
          toolCallId: ev.data?.toolCallId,
          title: `${name}`,
          status: 'in_progress',
          kind: classifyToolKind(name),
          rawInput: args,
        },
      });
      break;
    }

    case 'tool.execution_complete': {
      sendNotification('session/update', {
        sessionId,
        update: {
          sessionUpdate: 'tool_call_update',
          toolCallId: ev.data?.toolCallId,
          status: ev.data?.success ? 'completed' : 'failed',
          rawOutput: ev.data?.result,
        },
      });
      break;
    }

    case 'permission.requested': {
      // Auto-approve (for non-interactive Discord) but publish an audit
      // notification so OpenAB can surface a recent-permissions log.
      try {
        const req = ev.data?.permissionRequest || ev.data || {};
        const requestId = ev.data?.requestId || req.requestId;

        // Forward as ACP notification for client-side audit / display.
        sendNotification('session/permission_audit', {
          sessionId,
          requestId,
          kind: req.kind || 'shell',
          command: req.fullCommandText || req.command || '',
          intention: req.intention || req.description || '',
          toolCall: req.toolCall || null,
          ts: new Date().toISOString(),
        });

        // Record in session-level ring buffer (last 50 entries).
        state.recentPermissions = state.recentPermissions || [];
        state.recentPermissions.push({
          requestId,
          kind: req.kind || 'shell',
          command: req.fullCommandText || req.command || '',
          intention: req.intention || req.description || '',
          ts: new Date().toISOString(),
        });
        if (state.recentPermissions.length > 50) {
          state.recentPermissions = state.recentPermissions.slice(-50);
        }

        // Approve (we're running --allow-all-tools semantically)
        if (requestId && state.session.clientSessionApis?.respondToPermissionRequest) {
          await state.session.clientSessionApis.respondToPermissionRequest({
            requestId,
            result: { kind: 'approved' },
          });
        }
      } catch (e) {
        logError('permission handler: ' + e.message);
      }
      break;
    }

    case 'assistant.turn_end': {
      if (state.turnInProgress) {
        state.turnInProgress.resolveTurn();
      }
      break;
    }

    case 'session.end':
    case 'session.closed': {
      if (state.turnInProgress) state.turnInProgress.resolveTurn();
      sessions.delete(sessionId);
      break;
    }

    default:
      // Other events: ignore for now.
      break;
  }
}

// ---- helpers -------------------------------------------------------

function extractPromptText(prompt) {
  if (!prompt) return '';
  if (typeof prompt === 'string') return prompt;
  if (Array.isArray(prompt)) {
    return prompt
      .filter(b => b && b.type === 'text' && typeof b.text === 'string')
      .map(b => b.text)
      .join('\n');
  }
  return '';
}

/// Extract image/file attachments from an ACP content-block array and
/// convert them to Copilot SDK MessageOptions.attachments shape
/// (type: "blob" with base64 data + mimeType).
/// Returns [] if no attachments present.
function extractPromptAttachments(prompt) {
  if (!Array.isArray(prompt)) return [];
  const out = [];
  let imgCount = 0;
  for (const b of prompt) {
    if (!b || typeof b !== 'object') continue;
    if (b.type === 'image' && typeof b.data === 'string') {
      // ACP ImageContent: { type: 'image', data: base64, mimeType: 'image/...' }
      imgCount += 1;
      out.push({
        type: 'blob',
        data: b.data,
        mimeType: b.mimeType || b.media_type || 'image/png',
        displayName: `discord-image-${imgCount}.${(b.mimeType || 'image/png').split('/')[1] || 'png'}`,
      });
    } else if (b.type === 'resource' && b.resource) {
      // ACP EmbeddedContext resource — best-effort forward as blob
      const r = b.resource;
      if (r.blob) {
        out.push({
          type: 'blob',
          data: r.blob,
          mimeType: r.mimeType || 'application/octet-stream',
          displayName: r.uri || 'resource',
        });
      }
    }
  }
  return out;
}

function classifyToolKind(name) {
  const lower = (name || '').toLowerCase();
  if (lower.includes('read') || lower.includes('fetch')) return 'read';
  if (lower.includes('write') || lower.includes('edit')) return 'edit';
  if (lower.includes('shell') || lower.includes('powershell') || lower.includes('bash')) return 'execute';
  if (lower.includes('search') || lower.includes('grep')) return 'search';
  return 'other';
}

// ---- cleanup -------------------------------------------------------

process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);

async function shutdown() {
  logInfo('shutting down');
  for (const [id, state] of sessions) {
    try { await state.session.disconnect(); } catch (_) {}
  }
  sessions.clear();
  if (client) {
    try { await client.stop(); } catch (_) {}
  }
  process.exit(0);
}

logInfo('copilot-agent-acp started, waiting for ACP input on stdin');
