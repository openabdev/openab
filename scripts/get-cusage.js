#!/usr/bin/env node
// Custom usage report: daily breakdown per model from stats-cache.json
const fs = require('fs');
const path = require('path');

const STATS_PATH = path.join(process.env.USERPROFILE || 'C:/Users/Administrator', '.claude', 'stats-cache.json');

function fmt(n) {
  if (n >= 1e6) return (n / 1e6).toFixed(1) + 'M';
  if (n >= 1e3) return (n / 1e3).toFixed(1) + 'K';
  return String(n);
}

try {
  const stats = JSON.parse(fs.readFileSync(STATS_PATH, 'utf8'));
  const now = new Date();
  const weekAgo = new Date(now - 7 * 86400000).toISOString().slice(0, 10);
  const monthStart = now.toISOString().slice(0, 7); // "2026-04"

  const days = (stats.dailyModelTokens || []).filter(d => d.date >= weekAgo);

  // Daily lines (most recent first)
  const daily_lines = [];
  const weekTotals = {}; // per-model week totals

  for (const d of days.reverse()) {
    const models = Object.entries(d.tokensByModel || {});
    const parts = models
      .sort((a, b) => b[1] - a[1])
      .map(([m, t]) => {
        const short = m.replace('claude-', '').replace('-20251001', '').replace('-4-6', '');
        weekTotals[m] = (weekTotals[m] || 0) + t;
        return `${short} ${fmt(t)}`;
      });
    daily_lines.push(`**${d.date.slice(5)}**: ${parts.join(' · ')}`);
  }

  // Week summary: separate Claude vs non-Claude
  let claudeTotal = 0, otherTotal = 0;
  const claudeModels = {}, otherModels = {};
  for (const [m, t] of Object.entries(weekTotals)) {
    if (m.includes('claude')) {
      claudeTotal += t;
      claudeModels[m] = t;
    } else {
      otherTotal += t;
      otherModels[m] = t;
    }
  }

  // Claude breakdown
  const claude_parts = Object.entries(claudeModels)
    .sort((a, b) => b[1] - a[1])
    .map(([m, t]) => {
      const short = m.replace('claude-', '').replace('-20251001', '').replace('-4-6', '');
      return `${short} ${fmt(t)}`;
    });

  // Other breakdown
  const other_parts = Object.entries(otherModels)
    .sort((a, b) => b[1] - a[1])
    .map(([m, t]) => `${m} ${fmt(t)}`);

  // Month totals (all days in current month)
  const monthDays = (stats.dailyModelTokens || []).filter(d => d.date.startsWith(monthStart));
  let monthClaude = 0, monthOther = 0;
  for (const d of monthDays) {
    for (const [m, t] of Object.entries(d.tokensByModel || {})) {
      if (m.includes('claude')) monthClaude += t;
      else monthOther += t;
    }
  }

  // Activity stats
  const todayStr = now.toISOString().slice(0, 10);
  const todayActivity = (stats.dailyActivity || []).find(d => d.date === todayStr);

  console.log(JSON.stringify({
    ok: true,
    daily: daily_lines.join('\n'),
    week_claude: fmt(claudeTotal),
    week_other: fmt(otherTotal),
    week_claude_detail: claude_parts.join(' · ') || 'N/A',
    week_other_detail: other_parts.join(' · ') || 'none',
    month_claude: fmt(monthClaude),
    month_other: fmt(monthOther),
    month_total: fmt(monthClaude + monthOther),
    today_msgs: todayActivity?.messageCount || 0,
    today_sessions: todayActivity?.sessionCount || 0,
    ts: now.toISOString(),
  }));
} catch (e) {
  console.log(JSON.stringify({ ok: false, error: e.message }));
  process.exit(1);
}
