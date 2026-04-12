#!/usr/bin/env node
// Get Claude Code usage from local stats-cache.json — no PTY needed.
// Reads ~/.claude/stats-cache.json for token usage and session stats.

const fs = require('fs');
const path = require('path');

const STATS_PATH = path.join(process.env.USERPROFILE || 'C:/Users/Administrator', '.claude', 'stats-cache.json');

try {
  const raw = fs.readFileSync(STATS_PATH, 'utf8');
  const stats = JSON.parse(raw);

  // Today and this week's token usage
  const today = new Date().toISOString().slice(0, 10); // YYYY-MM-DD
  const weekAgo = new Date(Date.now() - 7 * 86400000).toISOString().slice(0, 10);

  // Weekly tokens by model
  let weekOpus = 0, weekSonnet = 0, weekHaiku = 0, weekTotal = 0;
  for (const day of (stats.dailyModelTokens || [])) {
    if (day.date >= weekAgo) {
      const t = day.tokensByModel || {};
      for (const [model, tokens] of Object.entries(t)) {
        weekTotal += tokens;
        if (model.includes('opus')) weekOpus += tokens;
        else if (model.includes('sonnet')) weekSonnet += tokens;
        else if (model.includes('haiku')) weekHaiku += tokens;
      }
    }
  }

  // Today's activity
  const todayActivity = (stats.dailyActivity || []).find(d => d.date === today);
  const todayMessages = todayActivity?.messageCount || 0;
  const todaySessions = todayActivity?.sessionCount || 0;
  const todayTools = todayActivity?.toolCallCount || 0;

  // Lifetime stats
  const totalSessions = stats.totalSessions || 0;
  const totalMessages = stats.totalMessages || 0;

  // Model usage totals
  const opusUsage = stats.modelUsage?.['claude-opus-4-6'] || {};
  const sonnetUsage = stats.modelUsage?.['claude-sonnet-4-6'] || {};
  const haikuUsage = stats.modelUsage?.['claude-haiku-4-5-20251001'] || {};

  // Claude Max doesn't have hard quota %, but we can show activity level
  // Calculate a "session intensity" as a proxy
  const weekActivity = (stats.dailyActivity || [])
    .filter(d => d.date >= weekAgo)
    .reduce((sum, d) => sum + d.messageCount, 0);

  console.log(JSON.stringify({
    ok: true,
    // Today
    today_messages: todayMessages,
    today_sessions: todaySessions,
    today_tools: todayTools,
    // This week tokens
    week_opus: weekOpus,
    week_sonnet: weekSonnet,
    week_haiku: weekHaiku,
    week_total: weekTotal,
    // Lifetime
    total_sessions: totalSessions,
    total_messages: totalMessages,
    // Plan info
    tier: 'Claude Max (Opus 4.6)',
    ts: new Date().toISOString(),
  }));
} catch (e) {
  console.log(JSON.stringify({ ok: false, error: e.message }));
  process.exit(1);
}
