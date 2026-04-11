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

async function ensureClient() {
  if (client) return client;
  sdk = await sdkPromise;
  client = new sdk.CopilotClient();
  await client.start();
  logInfo('CopilotClient started');
  return client;
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

  // Extract models + mode info from session for ACP initial response.
  let models = null;
  try {
    const cur = await session.rpc.model.getCurrent();
    const listed = await c.listModels();
    models = {
      currentModelId: cur?.modelId || 'default',
      availableModels: (listed || []).map(m => ({
        modelId: m.id || m.modelId,
        name: m.name || m.id,
        description: m.description || m.name || '',
      })),
    };
  } catch (_) {}

  logInfo(`session/new id=${session.sessionId}`);
  return {
    sessionId: session.sessionId,
    models,
  };
}

async function handleSessionLoad(params) {
  // OpenAB calls session/load when resuming; for now just treat as session/new.
  return await handleSessionNew(params);
}

async function handleSessionPrompt(params) {
  const { sessionId, prompt } = params;
  const state = sessions.get(sessionId);
  if (!state) throw new Error(`unknown sessionId: ${sessionId}`);

  // Extract text from prompt content blocks.
  const text = extractPromptText(prompt);
  if (!text) {
    return { stopReason: 'end_turn' };
  }

  // Create a promise that resolves when the assistant turn ends.
  let resolveTurn;
  const turnPromise = new Promise(res => (resolveTurn = res));
  state.turnInProgress = { resolveTurn, sessionId };

  // Kick off the send — SDK will emit events that our handler forwards.
  const sendOpts = { prompt: text };
  try {
    await state.session.send(sendOpts);
  } catch (err) {
    state.turnInProgress = null;
    throw err;
  }

  // Wait for assistant.turn_end
  await turnPromise;
  state.turnInProgress = null;
  return { stopReason: 'end_turn' };
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
      account_quota: await client.rpc.account.getQuota(),
    };
  }
  // No session specified — return account quota only
  return {
    account_quota: await client.rpc.account.getQuota(),
    session_usage: null,
  };
}

function handleGetStats(_params) {
  return {
    sessions: sessions.size,
    bridge_version: '0.1.0',
  };
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
      // Auto-approve for now (matches --allow-all-tools behavior).
      // Later we can forward to the ACP client as session/request_permission.
      try {
        const requestId = ev.data?.requestId;
        if (requestId && state.session.clientSessionApis?.respondToPermissionRequest) {
          await state.session.clientSessionApis.respondToPermissionRequest({
            requestId,
            result: { kind: 'approved' },
          });
        }
      } catch (e) {
        logError('permission auto-approve: ' + e.message);
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
