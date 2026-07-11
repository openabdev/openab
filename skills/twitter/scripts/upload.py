#!/usr/bin/env python3
"""Fix timestamp in index.txt to current Taipei time, then upload to Cloudflare R2."""

import subprocess, re
from datetime import datetime, timezone, timedelta

TPE = timezone(timedelta(hours=8))
now = datetime.now(TPE).strftime("%Y-%m-%d %H:%M")

txt = open("/tmp/index.txt").read()
txt = re.sub(r"最後更新：台北時間 .+", f"最後更新：台北時間 {now}", txt)
open("/tmp/index.txt", "w").write(txt)

print(f"Timestamp: {now}")
r = subprocess.run(
    'aws --profile cloudflare s3 cp /tmp/index.txt s3://x-deepsrt-cc/index.txt --content-type "text/plain; charset=utf-8"',
    shell=True, capture_output=True, text=True
)
print(r.stdout.strip())
