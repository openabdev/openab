#!/usr/bin/env python3
"""Filter timeline tweets: sort by engagement with topic boost, max N per author, min text length."""

import json, re, os

INPUT_FILE = os.environ.get("INPUT_FILE", "/tmp/all_tweets.json")
OUTPUT_FILE = os.environ.get("OUTPUT_FILE", "/tmp/candidates_filtered.json")
MAX_PER_AUTHOR = int(os.environ.get("MAX_PER_AUTHOR", "2"))
MIN_TEXT_LEN = int(os.environ.get("MIN_TEXT_LEN", "20"))

# Topics that get a reply-count boost (multiplier on _replies for sorting)
BOOST_KEYWORDS = re.compile(
    r'openclaw|open.?claw|nanoclaw|karpathy|claude|anthropic|openai|gpt|gemini|'
    r'copilot|codex|grok|deepseek|agent|llm|ai.?model|frontier.?lab',
    re.IGNORECASE
)
BOOST_MULTIPLIER = float(os.environ.get("BOOST_MULTIPLIER", "3"))

def parse_num(s):
    s = str(s).strip().replace(',', '')
    if not s or s == '0': return 0
    m = re.match(r'([\d.]+)\s*([KkMm]?)', s)
    if not m: return 0
    n = float(m.group(1))
    u = m.group(2).upper()
    if u == 'K': n *= 1000
    elif u == 'M': n *= 1000000
    return int(n)

def get_handle(author):
    m = re.search(r'@(\w+)', str(author))
    return m.group(1).lower() if m else str(author).lower()[:20]

tweets = json.load(open(INPUT_FILE))

for t in tweets:
    t['_likes'] = parse_num(t.get('likes', '0'))
    t['_retweets'] = parse_num(t.get('retweets', '0'))
    t['_replies'] = parse_num(t.get('replies', '0'))
    t['_engagement'] = t['_likes'] + t['_retweets'] * 2 + t['_replies'] * 3
    # Boosted engagement score for sorting
    boost = BOOST_MULTIPLIER if BOOST_KEYWORDS.search(t.get('text', '')) else 1.0
    t['_score'] = t['_engagement'] * boost
    t['_is_tech'] = bool(BOOST_KEYWORDS.search(t.get('text', '')))

tweets.sort(key=lambda t: t['_score'], reverse=True)

author_count = {}
filtered = []
for t in tweets:
    handle = get_handle(t.get('author', ''))
    if len(t.get('text', '').strip()) < MIN_TEXT_LEN:
        continue
    if author_count.get(handle, 0) >= MAX_PER_AUTHOR:
        continue
    author_count[handle] = author_count.get(handle, 0) + 1
    filtered.append(t)

tech_count = sum(1 for t in filtered if t.get('_is_tech'))
json.dump(filtered, open(OUTPUT_FILE, "w"), ensure_ascii=False, indent=2)
print(f"Filtered: {len(tweets)} -> {len(filtered)} candidates (max {MAX_PER_AUTHOR}/author, min {MIN_TEXT_LEN} chars)")
print(f"Tech/AI boosted: {tech_count} candidates")
print(f"-> {OUTPUT_FILE}")
