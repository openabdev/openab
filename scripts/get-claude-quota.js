#!/usr/bin/env node
// Get Claude Code usage: rate limits from Anthropic API + local stats-cache.json.
const fs = require('fs');
const https = require('https');
const path = require('path');
const STATS_PATH = path.join(process.env.USERPROFILE || 'C:/Users/Administrator', '.claude', 'stats-cache.json');
const CREDS_PATH = path.join(process.env.USERPROFILE || 'C:/Users/Administrator', '.claude', '.credentials.json');
function fmt(n) { return n >= 1e6 ? (n/1e6).toFixed(1)+'M' : n >= 1e3 ? (n/1e3).toFixed(1)+'K' : String(n); }
function fmtCD(epoch) { const d=epoch*1000-Date.now(); if(d<=0)return'resetting...'; const h=Math.floor(d/3600000),m=Math.floor((d%3600000)/60000); return h>0?`${h}h${m}m`:`${m}m`; }
try {
  const stats = JSON.parse(fs.readFileSync(STATS_PATH, 'utf8'));
  const today = new Date().toISOString().slice(0,10), weekAgo = new Date(Date.now()-7*86400000).toISOString().slice(0,10);
  let wO=0,wS=0,wH=0,wT=0;
  for (const d of (stats.dailyModelTokens||[])) { if(d.date>=weekAgo) for(const[m,t]of Object.entries(d.tokensByModel||{})){wT+=t;if(m.includes('opus'))wO+=t;else if(m.includes('sonnet'))wS+=t;else if(m.includes('haiku'))wH+=t;} }
  const ta=(stats.dailyActivity||[]).find(d=>d.date===today);
  const creds = JSON.parse(fs.readFileSync(CREDS_PATH, 'utf8'));
  const token = creds.claudeAiOauth?.accessToken;
  if (!token) throw new Error('no token');
  const body = JSON.stringify({model:'claude-haiku-4-5-20251001',max_tokens:1,messages:[{role:'user',content:'1'}]});
  const req = https.request({hostname:'api.anthropic.com',path:'/v1/messages',method:'POST',headers:{'x-api-key':token,'anthropic-version':'2023-06-01','content-type':'application/json','content-length':Buffer.byteLength(body)}}, res => {
    let data=''; res.on('data',d=>data+=d); res.on('end',()=>{
      const h5u=parseFloat(res.headers['anthropic-ratelimit-unified-5h-utilization']||'0');
      const h5r=parseInt(res.headers['anthropic-ratelimit-unified-5h-reset']||'0');
      const d7u=parseFloat(res.headers['anthropic-ratelimit-unified-7d-utilization']||'0');
      const d7r=parseInt(res.headers['anthropic-ratelimit-unified-7d-reset']||'0');
      console.log(JSON.stringify({ok:true,session_5h_remaining:Math.round((1-h5u)*100),session_5h_used:Math.round(h5u*100),session_5h_reset:fmtCD(h5r),week_7d_remaining:Math.round((1-d7u)*100),week_7d_used:Math.round(d7u*100),week_7d_reset:fmtCD(d7r),today_messages:ta?.messageCount||0,today_sessions:ta?.sessionCount||0,week_total:fmt(wT),week_opus:fmt(wO),week_sonnet:fmt(wS),week_haiku:fmt(wH),total_sessions:(stats.totalSessions||0).toLocaleString(),total_messages:(stats.totalMessages||0).toLocaleString(),tier:creds.claudeAiOauth?.subscriptionType==='max'?'Claude Max':'Claude Pro',ts:new Date().toISOString()}));
    });
  });
  req.on('error',e=>{console.log(JSON.stringify({ok:false,error:e.message}));process.exit(1);});
  req.setTimeout(10000,()=>{req.destroy();process.exit(1);});
  req.write(body); req.end();
} catch(e) { console.log(JSON.stringify({ok:false,error:e.message})); process.exit(1); }
