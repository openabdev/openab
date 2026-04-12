#!/usr/bin/env node
// Get Codex CLI remaining quota via node-pty /status command.
// Reads "XX% left" from /status output.

const TIMEOUT_MS = 40000;
const CODEX_PATH = 'C:/Users/Administrator/AppData/Roaming/npm/codex.cmd';

const pty = require('C:/Users/Administrator/AppData/Roaming/npm/node_modules/node-pty');

const p = pty.spawn(CODEX_PATH, [], {
  name: 'xterm-color',
  cols: 200,
  rows: 50,
  cwd: 'C:/Users/Administrator',
  env: process.env
});

let buf = '';
let done = false;
let statusSent = false;

function tryMatch() {
  // Pattern: "100% left" or "XX% left"
  const m = buf.match(/(\d+)%\s*left/);
  if (m && !done) {
    done = true;
    // Also try to grab model name: "gpt-5.4 xhigh" before the %
    const modelMatch = buf.match(/([\w.-]+)\s+\w+\s*·\s*\d+%\s*left/);
    console.log(JSON.stringify({
      ok: true,
      remaining_pct: parseInt(m[1]),
      model: modelMatch ? modelMatch[1] : 'unknown',
      ts: new Date().toISOString()
    }));
    p.kill();
    process.exit(0);
  }
}

p.onData(d => {
  buf += d;
  // Send /status once we see the prompt
  if (!statusSent && buf.includes('esc to interrupt')) {
    statusSent = true;
    setTimeout(() => p.write('/status\r'), 2000);
  }
  tryMatch();
});

const poller = setInterval(tryMatch, 2000);

// Also force send /status after 8s as fallback
setTimeout(() => { if (!statusSent) { statusSent = true; p.write('/status\r'); } }, 8000);

setTimeout(() => {
  clearInterval(poller);
  if (!done) {
    tryMatch();
    if (!done) {
      // Check if we see "context left" as alternative
      const ctx = buf.match(/(\d+)%\s*context\s*left/);
      if (ctx) {
        console.log(JSON.stringify({
          ok: true,
          remaining_pct: parseInt(ctx[1]),
          note: 'context remaining',
          ts: new Date().toISOString()
        }));
      } else {
        console.log(JSON.stringify({
          ok: true,
          remaining_pct: 100,
          note: 'quota not found, assuming healthy',
          ts: new Date().toISOString()
        }));
      }
      p.kill();
      process.exit(0);
    }
  }
}, TIMEOUT_MS);
