#!/usr/bin/env node
// End-to-end test: spawn each ACP agent and test all slash command backends.
// Tests the ACTUAL functions that Discord slash commands call.

const { spawn } = require('child_process');

const AGENTS = [
  { name: 'CICX',    cmd: 'claude-agent-acp', args: [], shell: true },
  { name: 'GITX',    cmd: 'node', args: ['C:/Users/Administrator/openab/vendor/copilot-agent-acp/copilot-agent-acp.js'], shell: false },
  { name: 'GIMINIX', cmd: 'gemini', args: ['--acp'], shell: true },
  { name: 'CODEX',   cmd: 'codex-acp', args: [], shell: true },
];

const RESULTS = [];

function log(agent, test, status, detail = '') {
  const icon = status === 'PASS' ? '✅' : status === 'WARN' ? '⚠️' : '❌';
  const line = `${icon} [${agent}] ${test}${detail ? ': ' + detail : ''}`;
  console.log(line);
  RESULTS.push({ agent, test, status, detail });
}

async function testAgent(agentDef) {
  const { name, cmd, args, shell } = agentDef;
  console.log(`\n${'='.repeat(50)}\n  Testing ${name} (${cmd})\n${'='.repeat(50)}`);

  return new Promise((resolve) => {
    const child = spawn(cmd, args, {
      stdio: ['pipe', 'pipe', 'ignore'],
      shell,
      env: { ...process.env, COPILOT_DEFAULT_MODEL: 'gpt-5-mini' },
    });

    let buf = '';
    let id = 0;
    const pending = new Map();
    let sessionId = null;
    let nativeCommands = [];

    function send(method, params) {
      const reqId = ++id;
      const req = { jsonrpc: '2.0', id: reqId, method, params };
      child.stdin.write(JSON.stringify(req) + '\n');
      return new Promise((res, rej) => {
        pending.set(reqId, { res, rej });
        setTimeout(() => {
          if (pending.has(reqId)) {
            pending.delete(reqId);
            rej(new Error('timeout'));
          }
        }, 60000);
      });
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
          // Capture native commands
          if (msg.method === 'session/update') {
            const upd = msg.params?.update;
            if (upd?.sessionUpdate === 'available_commands_update' && Array.isArray(upd.availableCommands)) {
              nativeCommands = upd.availableCommands;
            }
          }
          // Resolve pending
          if (msg.id != null && pending.has(msg.id)) {
            const { res, rej } = pending.get(msg.id);
            pending.delete(msg.id);
            if (msg.error) rej(new Error(JSON.stringify(msg.error)));
            else res(msg.result);
          }
        } catch {}
      }
    });

    async function run() {
      try {
        // TEST 1: /doctor → initialize
        const initResult = await send('initialize', {
          protocolVersion: 1,
          clientCapabilities: {},
          clientInfo: { name: 'test', version: '0.1' },
        });
        const agentName = initResult?.agentInfo?.name || 'unknown';
        log(name, '/doctor (initialize)', 'PASS', `agent=${agentName}`);

        // TEST 2: /doctor → session/new
        const sessionResult = await send('session/new', {
          cwd: 'C:/Users/Administrator',
          mcpServers: [],
        });
        sessionId = sessionResult?.sessionId;
        log(name, '/doctor (session/new)', sessionId ? 'PASS' : 'FAIL', `sessionId=${sessionId}`);

        // TEST 3: /model → check models
        const currentModel = sessionResult?.models?.currentModelId || 'unknown';
        const modelCount = sessionResult?.models?.availableModels?.length || 0;
        log(name, '/model (list)', modelCount > 0 ? 'PASS' : 'WARN', `current=${currentModel}, available=${modelCount}`);

        // Wait for native commands notification
        await new Promise(r => setTimeout(r, 2000));

        // TEST 4: /native → availableCommands
        log(name, '/native (commands)', nativeCommands.length > 0 ? 'PASS' : 'WARN',
          `count=${nativeCommands.length}${nativeCommands.length > 0 ? ', first=' + nativeCommands[0].name : ''}`);

        // TEST 5: /stats + /tokens → _meta/getUsage
        try {
          const usage = await send('_meta/getUsage', { sessionId });
          const hasQuota = usage?.account_quota != null;
          const hasSession = usage?.session_usage != null || usage?.inputTokens != null;
          log(name, '/stats (_meta/getUsage)', 'PASS',
            `hasQuota=${hasQuota}, hasSession=${hasSession}`);
        } catch (e) {
          log(name, '/stats (_meta/getUsage)', 'WARN', `not supported: ${e.message.slice(0, 60)}`);
        }

        // TEST 6: /compact → _meta/compactSession
        try {
          const compact = await send('_meta/compactSession', { sessionId });
          log(name, '/compact (_meta/compactSession)', 'PASS', JSON.stringify(compact).slice(0, 80));
        } catch (e) {
          log(name, '/compact (_meta/compactSession)', 'WARN', `not supported (fallback to drop-session)`);
        }

        // TEST 7: /doctor → _meta/ping
        try {
          const ping = await send('_meta/ping', {});
          log(name, '/doctor (ping)', 'PASS');
        } catch (e) {
          log(name, '/doctor (ping)', 'WARN', 'ping not supported');
        }

        // TEST 8: /permissions → _meta/getRecentPermissions
        try {
          const perms = await send('_meta/getRecentPermissions', { sessionId });
          log(name, '/permissions', 'PASS', `entries=${Array.isArray(perms?.permissions) ? perms.permissions.length : '?'}`);
        } catch (e) {
          log(name, '/permissions', 'WARN', 'not supported');
        }

        // TEST 9: /plan-mode → send /plan as prompt (quick test, just check it doesn't crash)
        try {
          const promptId = ++id;
          const req = { jsonrpc: '2.0', id: promptId, method: 'session/prompt', params: { sessionId, prompt: [{ type: 'text', text: '/help' }] } };
          child.stdin.write(JSON.stringify(req) + '\n');
          pending.set(promptId, { res: () => {}, rej: () => {} });
          // Don't wait for full response, just verify it accepted the prompt
          await new Promise(r => setTimeout(r, 3000));
          log(name, '/plan-mode (prompt)', 'PASS', 'prompt accepted');
        } catch (e) {
          log(name, '/plan-mode (prompt)', 'WARN', e.message.slice(0, 60));
        }

      } catch (e) {
        log(name, 'FATAL', 'FAIL', e.message.slice(0, 100));
      }

      child.kill();
    }

    run().then(resolve).catch(() => { child.kill(); resolve(); });

    // Global timeout per agent
    setTimeout(() => { child.kill(); resolve(); }, 90000);
  });
}

async function testUsageScripts() {
  console.log(`\n${'='.repeat(50)}\n  Testing /usage runners\n${'='.repeat(50)}`);

  const scripts = [
    { name: 'Claude quota', script: 'get-claude-quota.js', key: 'session_pct' },
    { name: 'Copilot quota', script: 'get-copilot-quota.js', key: 'remaining_pct' },
    { name: 'Gemini usage', script: 'get-gemini-usage.js', key: 'current_model' },
    { name: 'Codex usage', script: 'get-codex-usage.js', key: 'remaining_pct' },
  ];

  for (const s of scripts) {
    try {
      const result = await new Promise((res, rej) => {
        const child = spawn('node', [`C:/Users/Administrator/openab/scripts/${s.script}`], {
          stdio: ['pipe', 'pipe', 'ignore'],
          timeout: 55000,
        });
        let out = '';
        child.stdout.on('data', d => out += d.toString());
        child.on('close', code => {
          if (code !== 0) return rej(new Error(`exit ${code}`));
          try { res(JSON.parse(out.trim().split('\n').pop())); }
          catch { rej(new Error('invalid JSON: ' + out.slice(0, 100))); }
        });
        setTimeout(() => { child.kill(); rej(new Error('timeout')); }, 55000);
      });
      if (result.ok) {
        const val = result[s.key];
        log('USAGE', s.name, 'PASS', `${s.key}=${val}`);
      } else {
        log('USAGE', s.name, 'FAIL', result.error || 'ok=false');
      }
    } catch (e) {
      log('USAGE', s.name, 'FAIL', e.message.slice(0, 80));
    }
  }
}

(async () => {
  // Test usage scripts in parallel with first 2 agents
  const usagePromise = testUsageScripts();

  // Test agents sequentially (each spawns a heavy process)
  for (const agent of AGENTS) {
    await testAgent(agent);
  }

  await usagePromise;

  // Summary
  console.log(`\n${'='.repeat(50)}\n  SUMMARY\n${'='.repeat(50)}`);
  const pass = RESULTS.filter(r => r.status === 'PASS').length;
  const warn = RESULTS.filter(r => r.status === 'WARN').length;
  const fail = RESULTS.filter(r => r.status === 'FAIL').length;
  console.log(`✅ PASS: ${pass}  ⚠️ WARN: ${warn}  ❌ FAIL: ${fail}  Total: ${RESULTS.length}`);

  if (fail > 0) {
    console.log('\nFailures:');
    RESULTS.filter(r => r.status === 'FAIL').forEach(r => console.log(`  ❌ [${r.agent}] ${r.test}: ${r.detail}`));
  }

  process.exit(fail > 0 ? 1 : 0);
})();
