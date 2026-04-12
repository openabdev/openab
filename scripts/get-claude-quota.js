#!/usr/bin/env node
// Drive Claude Code interactive mode via node-pty to run /usage and capture output.
// Prints JSON with parsed usage data.

const pty = require('C:/Users/Administrator/AppData/Roaming/npm/node_modules/node-pty');

const TIMEOUT_MS = 45000;
const CLAUDE_PATH = 'C:/Users/Administrator/AppData/Roaming/npm/claude.cmd';

const p = pty.spawn(CLAUDE_PATH, ['--add-dir', 'C:/Users/Administrator'], {
  name: 'xterm-color',
  cols: 200,
  rows: 60,
  cwd: 'C:/Users/Administrator',
  env: process.env
});

let buf = '';
let done = false;

p.onData(d => {
  buf += d;

  if (done) return;

  // Once we see Esc to cancel (end of usage panel) after /usage, parse and exit
  if (buf.includes('/usage') && buf.match(/Esc to cancel[\s\S]*Esc to cancel/) === null && buf.match(/Usage Stats/)) {
    // Wait a bit more for full rendering
    if (!done && buf.match(/Current week.*%.*used/s)) {
      done = true;
      setTimeout(() => {
        parseAndExit();
      }, 1500);
    }
  }
});

function parseAndExit() {
  const clean = buf.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, '').replace(/\x1b\][^\x07]*\x07/g, '');

  // Parse percentages
  const session = clean.match(/Current session\s*[█▌_\s]*(\d+)%\s*used/);
  const weekAll = clean.match(/Current week\s*\(all models\)\s*[█▌_\s]*(\d+)%\s*used/);
  const weekSonnet = clean.match(/Current week\s*\(Sonnet only\)\s*(\d+)%\s*used/);

  // Reset lines: use lazy match + stop at "Current" OR "Extra" OR EOL to avoid
  // the session_reset regex greedily swallowing into the next section.
  const sessionReset = clean.match(/Current session[\s\S]*?Resets?\s+([^\n]+?)(?=\s*Current|\s*Extra|$)/);
  const weekReset = clean.match(/Current week \(all models\)[\s\S]*?Resets?\s+([^\n]+?)(?=\s*Current|\s*Extra|$)/);

  const result = {
    ok: true,
    session_pct: session ? parseInt(session[1]) : null,
    week_all_pct: weekAll ? parseInt(weekAll[1]) : null,
    week_sonnet_pct: weekSonnet ? parseInt(weekSonnet[1]) : null,
    session_reset: sessionReset ? sessionReset[1].trim() : null,
    week_reset: weekReset ? weekReset[1].trim() : null,
    ts: new Date().toISOString()
  };

  console.log(JSON.stringify(result));
  p.kill();
  process.exit(0);
}

// Press Enter to accept trust dialog (option 1 is default), then send /usage
setTimeout(() => p.write('\r'), 4000);
setTimeout(() => p.write('/usage\r'), 8000);

setTimeout(() => {
  if (!done) {
    const clean = buf.replace(/\x1b\[[0-9;?]*[a-zA-Z]/g, '').replace(/\x1b\][^\x07]*\x07/g, '');
    // Try parsing whatever we got
    const session = clean.match(/Current session\s*[█▌_\s]*(\d+)%\s*used/);
    if (session) {
      parseAndExit();
    } else {
      console.log(JSON.stringify({ok: false, error: 'timeout', ts: new Date().toISOString()}));
      p.kill();
      process.exit(1);
    }
  }
}, TIMEOUT_MS);
