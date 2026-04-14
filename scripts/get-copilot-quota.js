#!/usr/bin/env node
// Get Copilot quota via copilot-agent-acp bridge's _meta/getUsage RPC.
// Also reads live session cost_totals from disk for per-model breakdown.

const { spawn } = require('child_process');
const fs = require('fs');
const path = require('path');
const TIMEOUT = 45000;
const BRIDGE = 'C:/Users/Administrator/openab/vendor/copilot-agent-acp/copilot-agent-acp.js';

const child = spawn('node', [BRIDGE], {
  stdio: ['pipe', 'pipe', 'ignore'],
  env: { ...process.env, COPILOT_DEFAULT_MODEL: 'gpt-5-mini' },
});

let buf = '', id = 0, sessionId = null;

function send(m, p) {
  child.stdin.write(JSON.stringify({ jsonrpc: '2.0', id: ++id, method: m, params: p }) + '\n');
}

child.stdout.on('data', c => {
  buf += c.toString();
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
      // session/new response
      if (msg.id === 2 && msg.result) {
        sessionId = msg.result.sessionId;
        send('_meta/getUsage', { sessionId });
      }
      // _meta/getUsage response
      if (msg.id === 3) {
        const r = msg.result || {};
        const quota = r.account_quota?.quotaSnapshots?.premium_interactions;
        if (quota) {
          const resetDate = quota.resetDate ? new Date(quota.resetDate).toLocaleDateString('zh-TW') : '';
          const used = quota.usedRequests || 0;
          const overage = quota.overage || 0;

          // Read live session cost_totals from disk
          let session_lines = '';
          try {
            const dir = path.join(process.env.APPDATA || 'C:/Users/Administrator/AppData/Roaming', 'openab');
            const files = fs.readdirSync(dir).filter(f => f.startsWith('cost-totals-'));
            // Pick the most recently modified file
            let best = null, bestMtime = 0;
            for (const f of files) {
              const fp = path.join(dir, f);
              const st = fs.statSync(fp);
              if (st.mtimeMs > bestMtime) { bestMtime = st.mtimeMs; best = fp; }
            }
            if (best && Date.now() - bestMtime < 86400000) {
              const ct = JSON.parse(fs.readFileSync(best, 'utf8'));
              const parts = [];
              for (const [m, d] of Object.entries(ct.perModel || {})) {
                const input = d.inputTokens ? `${(d.inputTokens/1000).toFixed(1)}k in` : '';
                const output = d.outputTokens ? `${(d.outputTokens/1000).toFixed(1)}k out` : '';
                const cached = d.cacheReadTokens ? `${(d.cacheReadTokens/1000).toFixed(1)}k cached` : '';
                const tokens = [input, output, cached].filter(Boolean).join(' · ');
                const turnLabel = d.turns === 1 ? 'turn' : 'turns';
                parts.push(`${d.turns} ${turnLabel} **${m}** (${tokens})`);
              }
              if (parts.length) session_lines = parts.join('\n');
            }
          } catch {}

          console.log(JSON.stringify({
            ok: true,
            remaining_pct: quota.remainingPercentage,
            used: used,
            overage: overage,
            reset_date: resetDate,
            session_breakdown: session_lines,
            ts: new Date().toISOString(),
          }));
        } else {
          console.log(JSON.stringify({ ok: true, remaining_pct: 100, note: 'no quota data', ts: new Date().toISOString() }));
        }
        child.kill();
        process.exit(0);
      }
    } catch {}
  }
});

send('initialize', { protocolVersion: 1, clientCapabilities: {}, clientInfo: { name: 'probe', version: '0.1' } });
setTimeout(() => send('session/new', { cwd: 'C:/Users/Administrator', mcpServers: [] }), 3000);
setTimeout(() => {
  console.log(JSON.stringify({ ok: false, error: 'timeout' }));
  child.kill();
  process.exit(1);
}, TIMEOUT);
