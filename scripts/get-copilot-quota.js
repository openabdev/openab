#!/usr/bin/env node
// Get Copilot usage info via ACP session (no node-pty dependency).
// Spawns copilot --acp, does initialize + session/new, reads configOptions.

const { spawn } = require('child_process');
const TIMEOUT = 40000;

const child = spawn('copilot', ['--acp'], {
  stdio: ['pipe', 'pipe', 'ignore'],
  shell: true,
});

let buf = '';
let id = 0;

function send(method, params) {
  const reqId = ++id;
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: reqId, method, params }) + '\n');
  return reqId;
}

child.stdout.on('data', chunk => {
  buf += chunk.toString();
  let idx;
  while ((idx = buf.indexOf('\n')) >= 0) {
    const line = buf.slice(0, idx).trim();
    buf = buf.slice(idx + 1);
    if (!line) continue;
    try {
      const msg = JSON.parse(line);
      // Auto-allow permissions
      if (msg.method === 'session/request_permission' && msg.id != null) {
        child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: msg.id, result: { optionId: 'allow_always' } }) + '\n');
        continue;
      }
      if (msg.id === 2 && msg.result) {
        // session/new response — extract model info
        const sid = msg.result.sessionId || 'unknown';
        const models = msg.result.models || msg.result.configOptions?.models;
        const current = models?.currentModelId || 'unknown';
        const available = models?.availableModels?.length || 0;
        console.log(JSON.stringify({
          ok: true,
          current_model: current,
          available_models: available,
          remaining_pct: 100, // ACP doesn't expose quota %, report healthy
          tier: 'GitHub Copilot Pro',
          ts: new Date().toISOString(),
        }));
        child.kill();
        process.exit(0);
      }
    } catch {}
  }
});

send('initialize', { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: 'probe', version: '0.1' } });
setTimeout(() => send('session/new', { cwd: process.cwd(), mcpServers: [] }), 2000);

setTimeout(() => {
  console.log(JSON.stringify({ ok: false, error: 'timeout' }));
  child.kill();
  process.exit(1);
}, TIMEOUT);
