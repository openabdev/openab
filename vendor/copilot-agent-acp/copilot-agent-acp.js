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

const SDK_PATH =
  'C:/Users/Administrator/AppData/Local/copilot/pkg/win32-x64/1.0.21/copilot-sdk/index.js';

const sdkPromise = import('file:///' + SDK_PATH);

// Default model applied to every new session the bridge creates. Override
// via COPILOT_DEFAULT_MODEL env var. Intent: Discord use-case is quick Q&A
// → cheap model; terminal copilot-cli keeps its own config-level default.
const DEFAULT_MODEL = process.env.COPILOT_DEFAULT_MODEL || 'gpt-5-mini';

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
  client = new sdk.CopilotClient();
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
  const session = await c.createSession({
    cwd,
    onPermissionRequest: sdk.approveAll || (async () => ({ kind: 'approved' })),
  });

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
    models = {
      currentModelId: appliedDefault ? DEFAULT_MODEL : (cur?.modelId || 'default'),
      availableModels: sessionModels,
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
    const session = await c.resumeSession(sessionId, {
      onPermissionRequest:
        sdk.approveAll || (async () => ({ kind: 'approved' })),
    });

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

/// Try to handle a slash command typed as prompt text. Returns a string to
/// display to the user, or null if the command isn't recognized (so the
/// caller falls back to normal LLM processing).
async function handleSlashCommand(sessionId, state, raw) {
  // Parse: "/cmd arg1 arg2..." → ["cmd", "arg1", "arg2", ...]
  const parts = raw.slice(1).trim().split(/\s+/);
  const cmd = parts[0]?.toLowerCase();
  const args = parts.slice(1);
  if (!cmd) return null;

  const session = state.session;
  const rpc = session.rpc;

  try {
    switch (cmd) {
      case 'usage':
      case 'quota': {
        const q = await client.rpc.account.getQuota();
        const p = q.quotaSnapshots?.premium_interactions;
        if (!p) return '⚠️ No quota data available';
        return `📊 **Copilot Usage**\n` +
          `Premium remaining: **${p.remainingPercentage}%** (used ${p.usedRequests}/${p.entitlementRequests})\n` +
          `Resets: ${p.resetDate}`;
      }
      case 'tokens': {
        const u = state.lastUsage;
        if (!u) return '_(no usage captured yet — send a regular message first)_';
        const pct = u.tokenLimit ? Math.round((u.currentTokens / u.tokenLimit) * 100) : 0;
        return `🧮 **Session Tokens** (${pct}% full)\n` +
          `Used: ${(u.currentTokens / 1000).toFixed(1)}k / ${(u.tokenLimit / 1000).toFixed(0)}k\n` +
          `System: ${(u.systemTokens / 1000).toFixed(1)}k · ` +
          `Tools: ${(u.toolDefinitionsTokens / 1000).toFixed(1)}k · ` +
          `Conversation: ${(u.conversationTokens / 1000).toFixed(1)}k`;
      }
      case 'model': {
        if (args.length === 0) {
          const cur = await rpc.model.getCurrent();
          return `🤖 Current model: \`${cur?.modelId || 'default'}\``;
        }
        await rpc.model.switchTo({ modelId: args[0] });
        return `✅ Switched to \`${args[0]}\``;
      }
      case 'mode': {
        if (args.length === 0) {
          const cur = await rpc.mode.get();
          return `🔀 Current mode: \`${cur?.modeId || 'default'}\``;
        }
        await rpc.mode.set({ modeId: args[0] });
        return `✅ Mode set to \`${args[0]}\``;
      }
      case 'agents': {
        const list = await rpc.agent.list();
        const names = (list.agents || []).map(a => `• ${a.name}`).join('\n');
        return `🤖 **Agents** (${list.agents?.length || 0})\n${names || '_(none)_'}`;
      }
      case 'skills': {
        const list = await rpc.skills.list();
        const names = (list.skills || []).slice(0, 25).map(s => `• ${s.name}`).join('\n');
        return `⚡ **Skills** (${list.skills?.length || 0}, showing 25)\n${names || '_(none)_'}`;
      }
      case 'mcp': {
        const list = await rpc.mcp.list();
        const names = (list.servers || []).map(s => `• ${s.name}`).join('\n');
        return `🔌 **MCP Servers** (${list.servers?.length || 0})\n${names || '_(none)_'}`;
      }
      case 'plan': {
        const p = await rpc.plan.read();
        return `📋 **Plan**\n\`\`\`\n${(p?.content || '(empty)').slice(0, 1500)}\n\`\`\``;
      }
      case 'compact': {
        const r = await rpc.compaction.compact();
        return `✅ Compacted (${r?.tokensRemoved || 0} tokens removed)`;
      }
      case 'help': {
        return `🛠 **Bridge slash commands**\n` +
          `/usage /tokens /model [id] /mode [id] /agents /skills /mcp /plan /compact /help\n` +
          `(Type without args to show current value; unknown commands fall through to LLM.)`;
      }
      default:
        return null; // Unknown — let LLM handle it
    }
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
      };
      state.costTotals.turns += 1;
      state.costTotals.inputTokens += u.inputTokens || 0;
      state.costTotals.outputTokens += u.outputTokens || 0;
      state.costTotals.cacheReadTokens += u.cacheReadTokens || 0;
      state.costTotals.cacheWriteTokens += u.cacheWriteTokens || 0;
      state.costTotals.cost += Number(u.cost) || 0;
      state.costTotals.lastModel = u.model || state.costTotals.lastModel;
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
