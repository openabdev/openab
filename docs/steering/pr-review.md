# PR Review Guide for openabdev/openab

## Review Framework

When reviewing a PR, always address these four questions:

1. **What problem does it solve** — What pain point or requirement does this PR address? Explain the background in plain language.
2. **How does it solve it** — The specific technical approach, architecture decisions, and key implementation details.
3. **Were alternatives considered** — Does the PR description explain why this approach was chosen over alternatives? If not, suggest the author clarify.
4. **Is this the best approach** — Evaluate whether the current approach is optimal. Point out areas for improvement and potential risks.

## Severity Levels

Use emoji + color to classify each review comment:

| Level | Emoji | Meaning | Action Required |
|-------|-------|---------|-----------------|
| 🔴 Critical | `## 🔴 Critical:` | Correctness bug, security issue, data loss risk | Must fix before merge |
| 🟡 Minor | `## 🟡 Minor:` | Style inconsistency, missing defense-in-depth, non-blocking improvement | Should fix, not really a blocker but nice to have |
| 🟢 Info | `## 🟢 Info:` | Knowledge sharing, future consideration, design tradeoff note | No action needed |

## Comment Format

Each review comment should include:

1. **What's wrong** — describe the issue clearly
2. **Where** — file path and line number
3. **Why it matters** — what breaks or what risk it introduces
4. **Fix** — provide a concrete code suggestion

### Example (from PR #210)

```
## 🔴 Critical: resize() distorts aspect ratio

**File:** `src/discord.rs`, line ~320

`image::Image::resize(w, h, filter)` resizes the image to the exact
given dimensions — it does not preserve aspect ratio automatically.
A 4000×2000 landscape image gets squashed into 1200×1200.

### Fix

```rust
let ratio = f64::from(IMAGE_MAX_DIMENSION_PX) / f64::max(w, h);
let new_w = (f64::from(w) * ratio) as u32;
let new_h = (f64::from(h) * ratio) as u32;
img.resize(new_w, new_h, image::imageops::FilterType::Lanczos3)
```
```

## Self-Review Checklist (before opening PR)

Authors should check these before requesting review:

- [ ] **API behavior verified** — read the docs for any new library/function used
- [ ] **Safety checks preserved** — if refactoring, confirm all original validation/security checks are still present
- [ ] **Code style consistent** — logging, error handling, naming match the existing codebase
- [ ] **Tests test the right thing** — assertions reflect actual expected behavior, not just "passes"

## Review Etiquette

- **Be specific** — "this is wrong" is not helpful; show what's wrong and how to fix it
- **Classify severity** — not everything is a blocker; use 🔴🟡🟢 so the author knows what to prioritize
- **Acknowledge good work** — if something is well done, say so
- **One round if possible** — batch all feedback in one review pass to avoid back-and-forth

## Output Format — Traffic Light

After the review, structure the output using this format:

🟢 **INFO** — Things that look good, patterns followed correctly, positive observations.

🟡 **NIT** — Minor suggestions, documentation improvements, nice-to-haves. Not blockers.

🔴 **SUGGESTED CHANGES** — Significant concerns, DX footguns, missing pieces that should be addressed. Frame as suggestions for maintainers to consider (we are triagers, not maintainers).
