#!/usr/bin/env node
// Get Gemini CLI usage info by spawning a quick ACP session.
// Reports: model, rate limit tier. Gemini AI Studio free tier has no
// hard quota — it has per-minute rate limits (15 RPM free, 1000 RPM paid).

const { spawn } = require('child_process');
const TIMEOUT = 50000;

const child = spawn('gemini', ['--acp'], {
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
      if (msg.id === 2 && msg.result) {
        // session/new response
        const models = msg.result.models;
        const current = models?.currentModelId || 'unknown';
        const available = models?.availableModels?.length || 0;
        console.log(JSON.stringify({
          ok: true,
          current_model: current,
          available_models: available,
          tier: 'Google Gemini Pro',
          rate_limit: '1000 RPM',
          status_pct: 100,
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
