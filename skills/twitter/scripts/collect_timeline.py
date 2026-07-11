#!/usr/bin/env python3
"""Collect timeline tweets from X.com/home using persistent JS collector with virtual scroll handling."""

import json, subprocess, time, os, re, tempfile

OUTPUT_FILE = os.environ.get("OUTPUT_FILE", "/tmp/all_tweets.json")
SCROLL_ROUNDS = int(os.environ.get("SCROLL_ROUNDS", "150"))

env = {
    **os.environ,
    "AGENT_BROWSER_PROVIDER": "agentcore",
    "AGENTCORE_REGION": os.environ.get("AGENTCORE_REGION", "us-east-1"),
    "AGENTCORE_PROFILE_ID": os.environ.get("AGENTCORE_PROFILE_ID", "pahudnet_gmail-attEctm8Qe"),
    "PATH": "/home/pahud/.local/bin:" + os.environ.get("PATH", ""),
}

ANSI_RE = re.compile(r'\x1b\[[0-9;]*m')

def run_ab(cmd):
    try:
        r = subprocess.run(cmd, shell=True, capture_output=True, text=True, env=env, timeout=30)
        return ANSI_RE.sub('', r.stdout.strip())
    except:
        return ""

def eval_js(js):
    """Write JS to temp file to avoid shell quoting issues, then eval."""
    with tempfile.NamedTemporaryFile(mode='w', suffix='.js', delete=False) as f:
        f.write(js)
        f.flush()
        result = run_ab(f'agent-browser eval "$(cat {f.name})"')
        os.unlink(f.name)
        return result

COLLECTOR_JS = """
window.__collectedTweets = {};
window.__collectTweets = function() {
  var articles = document.querySelectorAll('article[data-testid="tweet"]');
  var n = 0;
  articles.forEach(function(el) {
    var linkEl = el.querySelector('a[href*="/status/"]');
    if (!linkEl) return;
    var href = linkEl.getAttribute('href');
    var match = href.match(/\\/status\\/(\\d+)/);
    if (!match) return;
    var id = match[1];
    if (window.__collectedTweets[id]) return;
    var textEl = el.querySelector('[data-testid="tweetText"]');
    var nameEl = el.querySelector('[data-testid="User-Name"]');
    var likeEl = el.querySelector('[data-testid="like"] span span');
    var rtEl = el.querySelector('[data-testid="retweet"] span span');
    var replyEl = el.querySelector('[data-testid="reply"] span span');
    window.__collectedTweets[id] = {
      id: id,
      text: (textEl ? textEl.textContent : '').substring(0, 300),
      author: nameEl ? nameEl.textContent : '',
      url: 'https://x.com' + href,
      likes: likeEl ? likeEl.textContent : '0',
      retweets: rtEl ? rtEl.textContent : '0',
      replies: replyEl ? replyEl.textContent : '0'
    };
    n++;
  });
  return n;
};
'ok'
"""

SCROLL_JS = 'window.__collectTweets(); window.scrollBy(0, 1500); Object.keys(window.__collectedTweets).length'
EXPORT_JS = 'JSON.stringify(Object.values(window.__collectedTweets))'

# Navigate to x.com/home and wait for load
print("Opening x.com/home...", flush=True)
run_ab('agent-browser open "https://x.com/home"')
time.sleep(8)

# Inject collector
print("Injecting tweet collector...", flush=True)
eval_js(COLLECTOR_JS)

# Scroll and collect
prev_count = 0
stale_rounds = 0
for i in range(1, SCROLL_ROUNDS + 1):
    raw = eval_js(SCROLL_JS)
    try:
        count = int(raw)
    except:
        count = prev_count
    if count == prev_count:
        stale_rounds += 1
        if stale_rounds >= 10:
            print(f"  No new tweets for 10 rounds, stopping at {count}", flush=True)
            break
    else:
        stale_rounds = 0
    prev_count = count
    if i % 10 == 0:
        print(f"  Round {i}: {count} tweets", flush=True)
    time.sleep(1.5)

# Export
raw = eval_js(EXPORT_JS)
try:
    tweets = json.loads(raw)
    if isinstance(tweets, str):
        tweets = json.loads(tweets)
except:
    tweets = []

json.dump(tweets, open(OUTPUT_FILE, "w"), ensure_ascii=False, indent=2)
print(f"\nDone! {len(tweets)} tweets -> {OUTPUT_FILE}")
