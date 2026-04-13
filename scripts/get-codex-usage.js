#!/usr/bin/env node
// Get Codex CLI usage from ChatGPT backend API + local files.
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const CODEX_DIR = path.join(process.env.USERPROFILE || 'C:/Users/Administrator', '.codex');
function fmt(n) { return n >= 1e6 ? (n/1e6).toFixed(1)+'M' : n >= 1e3 ? (n/1e3).toFixed(1)+'K' : String(n); }
function fmtCD(s) { if(s<=0)return'resetting...'; const h=Math.floor(s/3600),m=Math.floor((s%3600)/60); return h>0?`${h}h${m}m`:`${m}m`; }
try {
  const auth = JSON.parse(fs.readFileSync(path.join(CODEX_DIR, 'auth.json'), 'utf8'));
  const token = auth.tokens?.access_token;
  if (!token) throw new Error('no token');
  let currentModel = 'unknown', effort = 'medium';
  try { const cfg = fs.readFileSync(path.join(CODEX_DIR, 'config.toml'), 'utf8'); const mm = cfg.match(/^model\s*=\s*"([^"]+)"/m); const em = cfg.match(/^model_reasoning_effort\s*=\s*"([^"]+)"/m); if(mm) currentModel=mm[1]; if(em) effort=em[1]; } catch {}
  let totalTokens = 0, threadCount = 0;
  try { const { DatabaseSync } = require('node:sqlite'); const db = new DatabaseSync(path.join(CODEX_DIR, 'state_5.sqlite'), { open: true, readOnly: true }); const t = db.prepare('SELECT SUM(tokens_used) as total, COUNT(*) as cnt FROM threads WHERE tokens_used > 0').get(); totalTokens = t.total||0; threadCount = t.cnt||0; db.close(); } catch {}
  let usage;
  try { const r = execSync(`curl -s "https://chatgpt.com/backend-api/codex/usage?client_version=0.120.0" -H "Authorization: Bearer ${token}" -H "User-Agent: codex-cli/0.120.0"`, { timeout: 15000, encoding: 'utf8' }); usage = JSON.parse(r); } catch { console.log(JSON.stringify({ ok: true, current_model: currentModel, effort, total_tokens: fmt(totalTokens), thread_count: threadCount, note: 'API unavailable', ts: new Date().toISOString() })); process.exit(0); }
  const rl = usage.rate_limit || {}, pw = rl.primary_window || {}, sw = rl.secondary_window || {};
  console.log(JSON.stringify({ ok: true, h5_remaining: 100-(pw.used_percent||0), h5_used: pw.used_percent||0, h5_reset: fmtCD(pw.reset_after_seconds||0), wk_remaining: 100-(sw.used_percent||0), wk_used: sw.used_percent||0, wk_reset: fmtCD(sw.reset_after_seconds||0), rate_allowed: rl.allowed, plan: usage.plan_type==='plus'?'ChatGPT Plus':(usage.plan_type||'unknown'), current_model: currentModel, effort, total_tokens: fmt(totalTokens), thread_count: threadCount, ts: new Date().toISOString() }));
} catch(e) { console.log(JSON.stringify({ ok: false, error: e.message })); process.exit(1); }
