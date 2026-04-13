#!/usr/bin/env node
// Copilot RPC dispatcher for OpenAB.
//
// Usage: node copilot-rpc.js <subcommand> [args...]
//
// Outputs JSON on stdout. Exit code 0 on success, 1 on failure.
// Expected output format: {"ok": true, "kind": "<subcommand>", "data": {...}}
//                      or {"ok": false, "error": "..."}

const SDK_PATH = 'C:/Users/Administrator/AppData/Local/copilot/pkg/win32-x64/1.0.21/copilot-sdk/index.js';

async function main() {
  const [, , sub, ...args] = process.argv;
  if (!sub) {
    return fail('usage: copilot-rpc.js <subcommand> [args...]');
  }

  let sdk;
  try {
    sdk = await import('file:///' + SDK_PATH);
  } catch (e) {
    return fail('SDK import failed: ' + e.message);
  }

  // Start a CopilotClient — spawns the CLI server in the background
  const client = new sdk.CopilotClient();
  try {
    await client.start();
  } catch (e) {
    return fail('client.start() failed: ' + e.message);
  }

  try {
    const result = await dispatch(client, sdk, sub, args);
    console.log(JSON.stringify(result));
  } catch (e) {
    console.log(JSON.stringify({
      ok: false,
      error: `${e.name || 'Error'}: ${e.message || String(e)}`,
    }));
  } finally {
    try { await client.stop(); } catch (_) {}
  }
}

async function dispatch(client, sdk, sub, args) {
  switch (sub) {
    // ---------- Client-level (no session needed) ----------
    case 'status': {
      const [status, auth, models] = await Promise.all([
        client.getStatus(),
        client.getAuthStatus().catch(() => null),
        client.listModels().catch(() => null),
      ]);
      return ok('status', { cli: status, auth, model_count: models?.length ?? null });
    }

    case 'usage': {
      const quota = await client.rpc.account.getQuota();
      return ok('usage', quota);
    }

    case 'models': {
      // SDK's client.listModels() returns the full catalog (11 items including
      // models the user can't actually access like Claude Sonnet 4.5 / Opus 4.5).
      // The TUI-visible filtered list (8 items for our test account) lives in
      // `configOptions` which is only exposed via the ACP protocol layer, not
      // via SDK's session.create response.
      //
      // Workaround: spawn `copilot --acp` as a subprocess, send initialize +
      // session/new, parse the session.create response's models.availableModels.
      // This is slower (~5s subprocess spawn) but returns the correct list.
      const acpModels = await fetchAcpFilteredModels();
      if (acpModels && acpModels.length) {
        return ok('models', { models: acpModels.map(m => ({ ...m, source: 'acp' })) });
      }
      // Fallback: unfiltered full catalog
      const models = await client.listModels();
      return ok('models', { models: (models || []).map(m => ({ ...m, source: 'catalog' })) });
    }

    case 'auth': {
      const auth = await client.getAuthStatus();
      return ok('auth', auth);
    }

    case 'ping': {
      const res = await client.ping('openab-ping');
      return ok('ping', res);
    }

    case 'sessions': {
      const list = await client.listSessions();
      return ok('sessions', { count: list.length, items: list.slice(0, 20) });
    }

    // ---------- Session-level (needs ephemeral session) ----------
    // These commands create a throwaway session to talk to session-scoped RPCs.
    case 'model-current':
    case 'mode-current':
    case 'agents':
    case 'agent-current':
    case 'skills':
    case 'mcp-list':
    case 'plugins':
    case 'extensions':
    case 'plan-read':
    case 'files':
    case 'capabilities':
    // Action subcommands (take an argument)
    case 'agent-select':
    case 'agent-deselect':
    case 'mode-set':
    case 'skill-enable':
    case 'skill-disable':
    case 'mcp-enable':
    case 'mcp-disable':
    case 'extension-enable':
    case 'extension-disable':
    // Reload subcommands
    case 'agent-reload':
    case 'skill-reload':
    case 'mcp-reload':
    case 'extension-reload': {
      const session = await client.createSession({
        onPermissionRequest: sdk.approveAll || (async () => ({ decision: 'approved' })),
      });
      try {
        const data = await callSession(session, sub, args);
        return ok(sub, data);
      } finally {
        try { await session.disconnect(); } catch (_) {}
      }
    }

    default:
      return { ok: false, error: `unknown subcommand: ${sub}` };
  }
}

async function callSession(session, sub, args = []) {
  const rpc = session.rpc;
  switch (sub) {
    // --- read-only listing ---
    case 'model-current':
      return await rpc.model.getCurrent();
    case 'mode-current':
      return await rpc.mode.get();
    case 'agents':
      return await rpc.agent.list();
    case 'agent-current':
      return await rpc.agent.getCurrent();
    case 'skills':
      return await rpc.skills.list();
    case 'mcp-list':
      return await rpc.mcp.list();
    case 'plugins':
      return await rpc.plugins.list();
    case 'extensions':
      return await rpc.extensions.list();
    case 'plan-read':
      return await rpc.plan.read();
    case 'files':
      return await rpc.workspace.listFiles();
    case 'capabilities':
      return session.capabilities || {};

    // --- action subcommands (take args[0] as name/id) ---
    case 'agent-select': {
      const name = args[0];
      if (!name) throw new Error('agent-select requires <name>');
      return await rpc.agent.select({ name });
    }
    case 'agent-deselect':
      return await rpc.agent.deselect();
    case 'mode-set': {
      const modeId = args[0];
      if (!modeId) throw new Error('mode-set requires <modeId>');
      return await rpc.mode.set({ modeId });
    }
    case 'skill-enable': {
      const name = args[0];
      if (!name) throw new Error('skill-enable requires <name>');
      return await rpc.skills.enable({ name });
    }
    case 'skill-disable': {
      const name = args[0];
      if (!name) throw new Error('skill-disable requires <name>');
      return await rpc.skills.disable({ name });
    }
    case 'mcp-enable': {
      const name = args[0];
      if (!name) throw new Error('mcp-enable requires <name>');
      return await rpc.mcp.enable({ name });
    }
    case 'mcp-disable': {
      const name = args[0];
      if (!name) throw new Error('mcp-disable requires <name>');
      return await rpc.mcp.disable({ name });
    }
    case 'extension-enable': {
      const name = args[0];
      if (!name) throw new Error('extension-enable requires <name>');
      return await rpc.extensions.enable({ name });
    }
    case 'extension-disable': {
      const name = args[0];
      if (!name) throw new Error('extension-disable requires <name>');
      return await rpc.extensions.disable({ name });
    }
    case 'agent-reload':
      return await rpc.agent.reload();
    case 'skill-reload':
      return await rpc.skills.reload();
    case 'mcp-reload':
      return await rpc.mcp.reload();
    case 'extension-reload':
      return await rpc.extensions.reload();

    default:
      throw new Error('unhandled session subcommand: ' + sub);
  }
}

function ok(kind, data) {
  return { ok: true, kind, data, ts: new Date().toISOString() };
}

/// Spawn `copilot --acp` as a subprocess, walk the ACP protocol to get a
/// session/new response (which includes configOptions with the user-filtered
/// model list), then kill the subprocess. Returns an array of `{id, name}`
/// or null on failure. Times out after 15 seconds.
async function fetchAcpFilteredModels() {
  return new Promise(resolve => {
    const { spawn } = require('child_process');
    const proc = spawn(
      'C:/Users/Administrator/AppData/Local/Microsoft/WinGet/Links/copilot.exe',
      ['--acp'],
      { stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true }
    );

    let buf = '';
    let done = false;

    const finish = result => {
      if (done) return;
      done = true;
      try { proc.kill(); } catch (_) {}
      resolve(result);
    };

    proc.stdout.on('data', chunk => {
      buf += chunk.toString();
      let idx;
      while ((idx = buf.indexOf('\n')) !== -1) {
        const line = buf.substring(0, idx).trim();
        buf = buf.substring(idx + 1);
        if (!line) continue;
        try {
          const msg = JSON.parse(line);
          if (msg.id === 2 && msg.result) {
            // session/new response
            const avail = msg.result.models?.availableModels;
            if (Array.isArray(avail)) {
              finish(
                avail.map(m => ({
                  id: m.modelId || m.id,
                  name: m.name || m.modelId || m.id,
                  description: m.description || '',
                }))
              );
              return;
            }
            finish(null);
            return;
          }
        } catch (_) {}
      }
    });

    proc.on('error', () => finish(null));
    proc.on('exit', () => { if (!done) finish(null); });

    // initialize
    proc.stdin.write(
      JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'initialize',
        params: {
          protocolVersion: 1,
          clientCapabilities: {
            fs: { readTextFile: false, writeTextFile: false },
            terminal: false,
          },
          clientInfo: { name: 'copilot-rpc', version: '0.1' },
        },
      }) + '\n'
    );
    setTimeout(() => {
      if (done) return;
      proc.stdin.write(
        JSON.stringify({
          jsonrpc: '2.0',
          id: 2,
          method: 'session/new',
          params: { cwd: 'C:/Users/Administrator', mcpServers: [] },
        }) + '\n'
      );
    }, 1500);

    setTimeout(() => finish(null), 15000);
  });
}

function fail(msg) {
  console.log(JSON.stringify({ ok: false, error: msg }));
  process.exit(1);
}

main().catch(err => {
  console.log(JSON.stringify({
    ok: false,
    error: `uncaught: ${err.message || err}`,
  }));
  process.exit(1);
});
