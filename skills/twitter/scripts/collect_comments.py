#!/usr/bin/env python3
"""Visit each candidate tweet, collect comments. Tech topics get lower threshold + reserved slots."""

import json, subprocess, time, os, re, tempfile

CANDIDATES_FILE = os.environ.get("CANDIDATES_FILE", "/tmp/candidates_filtered.json")
OUTPUT_FILE = os.environ.get("OUTPUT_FILE", "/tmp/qualified.json")
TARGET = int(os.environ.get("TARGET", "100"))
MIN_COMMENTS = int(os.environ.get("MIN_COMMENTS", "10"))
MIN_COMMENTS_TECH = int(os.environ.get("MIN_COMMENTS_TECH", "5"))
MAX_COMMENTS_PER = int(os.environ.get("MAX_COMMENTS_PER", "20"))
RESERVED_TECH = int(os.environ.get("RESERVED_TECH", "15"))

env = {
    **os.environ,
    "AGENT_BROWSER_PROVIDER": "agentcore",
    "AGENTCORE_REGION": os.environ.get("AGENTCORE_REGION", "us-east-1"),
    "AGENTCORE_PROFILE_ID": os.environ.get("AGENTCORE_PROFILE_ID", "pahudnet_gmail-attEctm8Qe"),
}

ANSI_RE = re.compile(r'\x1b\[[0-9;]*m')

def run_ab(cmd):
    try:
        r = subprocess.run(cmd, shell=True, capture_output=True, text=True, env=env, timeout=30)
        return ANSI_RE.sub('', r.stdout.strip())
    except:
        return ""

def eval_js(js):
    with tempfile.NamedTemporaryFile(mode='w', suffix='.js', delete=False) as f:
        f.write(js)
        f.flush()
        result = run_ab(f'agent-browser eval "$(cat {f.name})"')
        os.unlink(f.name)
        return result

def collect_comments(url):
    run_ab(f'agent-browser navigate "{url}"')
    time.sleep(5)
    for _ in range(3):
        run_ab('agent-browser eval "window.scrollBy(0, 2000)"')
        time.sleep(2)
    js = 'JSON.stringify([...document.querySelectorAll("article[data-testid=\\"tweet\\"]")].slice(1).map(el => ({text: (el.querySelector("[data-testid=\\"tweetText\\"]")?.textContent || "").substring(0, 200),likes: el.querySelector("[data-testid=\\"like\\"] span span")?.textContent || "0"})))'
    raw = run_ab(f"agent-browser eval '{js}'")
    try:
        data = json.loads(raw)
        if isinstance(data, str):
            data = json.loads(data)
        return data if isinstance(data, list) else []
    except:
        return []

candidates = json.load(open(CANDIDATES_FILE))
qualified = []
tech_qualified = 0
general_qualified = 0
general_slots = TARGET - RESERVED_TECH

for i, t in enumerate(candidates):
    if len(qualified) >= TARGET:
        break
    is_tech = t.get('_is_tech', False)
    # If general slots full, only accept tech
    if not is_tech and general_qualified >= general_slots:
        continue
    # If tech slots full, only accept general
    if is_tech and tech_qualified >= RESERVED_TECH and general_qualified >= general_slots:
        continue

    print(f"[{i+1}/{len(candidates)}] {'🤖' if is_tech else '📰'} {t['_replies']}💬 {t['text'][:55]}", flush=True)
    comments = collect_comments(t["url"])
    threshold = MIN_COMMENTS_TECH if is_tech else MIN_COMMENTS
    print(f"  -> {len(comments)} comments (need {threshold})", flush=True)

    if len(comments) >= threshold:
        t["comments"] = comments[:MAX_COMMENTS_PER]
        qualified.append(t)
        if is_tech:
            tech_qualified += 1
        else:
            general_qualified += 1
        print(f"  ✓ ({len(qualified)}/{TARGET}) [tech:{tech_qualified}/{RESERVED_TECH} general:{general_qualified}/{general_slots}]", flush=True)
    else:
        print(f"  ✗ skip ({len(comments)})", flush=True)

json.dump(qualified, open(OUTPUT_FILE, "w"), ensure_ascii=False, indent=2)
print(f"\nDone! {len(qualified)} tweets (tech:{tech_qualified} general:{general_qualified}) -> {OUTPUT_FILE}")
