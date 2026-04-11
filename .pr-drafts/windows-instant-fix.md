# fix: Windows `Instant` underflow panic in `SessionPool::cleanup_idle`

## Problem

On Windows, `tokio::time::Instant::now() - Duration::from_secs(86400)`
panics with `overflow when subtracting duration from instant` when the
system has been running less than 24 hours.

`Instant` on Windows is backed by `QueryPerformanceCounter`, which
starts from system boot. The subtraction operator (`-`) on `Instant`
uses `checked_sub` internally and panics on underflow.

Repro:
```
$ reboot
$ cargo run --release    # openab starts
# within the first minute, cleanup_idle tries to compute:
#   Instant::now() - Duration::from_secs(86400)
# which underflows because Instant::now() < 86400 seconds
```

The panic happens in the cleanup task spawned by `main.rs:80` every
60 seconds, killing the entire bot process.

## Fix

Use `saturating_duration_since` which safely returns zero on underflow:

```diff
 pub async fn cleanup_idle(&self, ttl_secs: u64) {
-    let cutoff = Instant::now() - std::time::Duration::from_secs(ttl_secs);
+    let now = Instant::now();
+    let ttl = std::time::Duration::from_secs(ttl_secs);
     let mut conns = self.connections.write().await;
     let stale: Vec<String> = conns
         .iter()
-        .filter(|(_, c)| c.last_active < cutoff || !c.alive())
+        .filter(|(_, c)| now.saturating_duration_since(c.last_active) >= ttl || !c.alive())
         .map(|(k, _)| k.clone())
         .collect();
```

This computes "how long since last_active" instead of "is last_active
before a computed cutoff", avoiding the underflow entirely. Semantically
equivalent on Linux where the original code worked by luck.

## Testing

- Verified openab runs for 24h+ without panic on Windows 11 (23H2)
  since this patch landed
- No behavior change on Linux: `saturating_duration_since` returns the
  same value as `Instant::now() - Duration` for any `last_active` that
  is actually in the past

## Platform

- Rust 1.94.1 (stable)
- Windows 11 Pro 23H2
- tokio 1.x

Signed-off-by: <Your-Name>
