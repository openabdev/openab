# Contributing to OpenAB

Thanks for your interest in contributing! This guide covers what we expect in issues and pull requests.

For the full rationale behind the PR guidelines, see the [PR Contribution Guidelines ADR](/docs/adr/pr-contribution-guidelines.md).

## Issue Guidelines

The fastest way to file an issue is to use the [issue templates](https://github.com/openabdev/openab/issues/new/choose) — they auto-apply the correct labels and pass automated validation.

### Issue Types

| Type | Template | Required Sections |
|------|----------|-------------------|
| Bug | [bug.yml](/.github/ISSUE_TEMPLATE/bug.yml) | Description, Steps to Reproduce, Expected Behavior |
| Feature | [feature.yml](/.github/ISSUE_TEMPLATE/feature.yml) | Description, Use Case |
| Documentation | [documentation.yml](/.github/ISSUE_TEMPLATE/documentation.yml) | Description |
| Guidance | [guidance.yml](/.github/ISSUE_TEMPLATE/guidance.yml) | Question |
| RFC | [rfc.yml](/.github/ISSUE_TEMPLATE/rfc.yml) | Proposal (free-form, no heading validation) |

### Filing Without a Template

If you file an issue via CLI or API (bypassing the template UI), keep these rules in mind:

1. **Title prefix helps auto-detection.** The following prefixes are recognized:
   - `fix(...)` or `bug(...)` → Bug
   - `feat(...)` or `feature(...)` → Feature
   - `docs(...)` or `documentation(...)` → Documentation
   - `RFC:` → RFC

2. **Use the required headings** (as `##` or `###`). Common synonyms are accepted:

   | Required Field | Accepted Synonyms |
   |----------------|-------------------|
   | Description | Problem, Summary, Overview, Background, What happened, Bug description |
   | Steps to Reproduce | Reproduction, How to reproduce, Repro steps, Steps to replicate, Repro |
   | Expected Behavior | Expected result, What should happen, Expected behaviour, Expected outcome |
   | Use Case | Motivation, Why, Rationale, Use cases, Why it matters, Benefits, Proposal |
   | Question | (no synonyms) |

3. **Headings must have content.** An empty heading or `_No response_` does not count.

### Automated Validation

A GitHub Action ([issue-check.yml](/.github/workflows/issue-check.yml)) runs on every issue open, edit, and label event:

- **Missing type label + unrecognizable format** → `incomplete` label is added, a bot comment asks you to wait for a maintainer to apply a type label.
- **Type is known but required sections are missing** → `incomplete` label is added, a bot comment lists exactly what's missing.
- **All required sections present** → `incomplete` label is automatically removed.

To fix an `incomplete` issue, simply edit the issue body to add the missing sections — no need to close and re-open.

## Pull Request Guidelines

Every PR must address the following in its description. The [PR template](/.github/pull_request_template.md) will prompt you for each section.

### 0. Discord Discussion URL

All PRs **must** include a Discord Discussion URL in the PR body (e.g. `https://discord.com/channels/...`). Discussing your idea in Discord before opening a PR helps align on direction and avoids wasted effort. PRs without a Discord Discussion URL will be **automatically closed in 24 hours**.

### 1. What problem does this solve?

Describe the pain point or requirement in plain language. Link the related issue.

### 2. At a Glance

Provide an ASCII diagram showing the high-level flow or where your change fits in the system. For docs-only or trivial changes, write "N/A".

### 3. Prior Art & Industry Research

**Required for architectural, runtime, agent, scheduling, delivery, or persistence changes.** For docs-only, chore, CI, release, or trivial bug fixes, write "Not applicable" with a brief reason.

When prior art research is required, investigate at minimum:

- **[OpenClaw](https://github.com/openclaw/openclaw)** — the largest open-source AI agent gateway
- **[Hermes Agent](https://github.com/NousResearch/hermes-agent)** — Nous Research's self-hosted agent with multi-platform messaging

Include links to relevant source code, documentation, or discussions. If neither project addresses the problem, state that explicitly with evidence.

### 4. Proposed Solution

Describe your technical approach, architecture decisions, and key implementation details.

### 5. Why This Approach

Explain why you chose this approach over the alternatives found in your research. Be explicit about:

- Tradeoffs you accepted
- Known limitations
- How this could evolve in the future

### 6. Alternatives Considered

List approaches you evaluated but did not choose, and explain why they were rejected.

### 7. Validation

Pick the checks relevant to your PR type:

- **Rust changes:** `cargo check`, `cargo test`, `cargo clippy`
- **Helm chart changes:** `helm lint`, `helm template`
- **CI/workflow changes:** workflow syntax validation, dry-run where possible
- **Docs-only changes:** links are valid, renders correctly in GitHub preview

Describe any manual testing performed and add unit tests for new functionality.

## Why We Require Prior Art Research

OpenAB is a young project. We want every design decision to be informed by what's already working in production elsewhere. This:

- Prevents reinventing the wheel
- Surfaces better patterns we might not have considered
- Documents the design space for future contributors
- Makes reviews faster — reviewers don't have to do the research themselves

## Development Setup

```bash
cargo build
cargo test
cargo check
```

## Development Tips

### Agent subprocess environment

OAB spawns agent adapters (agy-acp, codex-acp, etc.) as child processes with a **minimal environment** — only env vars explicitly listed in `[agent].env` config are passed. No `.profile`, `.bashrc`, or login shell is sourced.

If your agent adapter spawns further subprocesses (e.g. `agy`, `codex`), those tools may depend on PATH entries set up by shell init files (fnm, nvm, cargo, etc.). **Do not rely on login shells (`bash -lc`)** — shell metacharacters in user prompts will break argument passing.

Instead, augment PATH directly in your adapter code:

```rust
fn augmented_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/home/agent".to_string());
    let base = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".to_string());
    format!("{home}/bin:{home}/.local/bin:{home}/.local/share/fnm/aliases/default/bin:{base}")
}

// Then when spawning:
Command::new("/usr/local/bin/agy")
    .args(&args)
    .env("PATH", augmented_path())
    .spawn();
```

### E2E testing PRs

Use the PR Preview Build workflow for fast iteration. For the complete
workflow, safety requirements, acceptance criteria, evidence template, and
cleanup steps, see [Canary Testing Pull Requests](/docs/canary-tests.md).

```bash
# 1. Push code to PR branch
# 2. A maintainer builds the image from the upstream workflow on main.
#    The workflow resolves and checks out the PR head automatically.
gh workflow run pr-preview.yml --repo openabdev/openab \
  --ref main -f pr_number=<N> -f variant=<antigravity|codex|claude|default>

# 3. Wait for build
gh run view <run_id> --repo openabdev/openab --json conclusion -q .conclusion

# 4. Deploy and test (depends on your environment)
#    - Kubernetes: kubectl rollout restart deployment/<name>
#    - ECS Fargate (OAB fleet): [ecsctl](https://github.com/oablab/ecsctl) restart <bot> --wait
#    - Local: docker run with the PR image tag
```

**Never run two instances with the same bot token** — both receive messages and send duplicate/conflicting responses.

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and address warnings
- Keep PRs focused — one feature or fix per PR

## Platform Schema

When modifying a platform adapter (`crates/openab-gateway/src/adapters/*.rs`), check whether the change affects the platform's documented capabilities or feature status. If it does, update the corresponding `docs/platforms/schema/<platform>.toml`.

See [`docs/platforms/README.md`](docs/platforms/README.md) for:
- The three-schema structure (capability, feature-support, quirks)
- How to add a new feature to the closed set
- How to add a new platform
- Architecture: TOML (machine facts) vs `docs/<platform>.md` (human setup guide)

CI runs conformance tests on schema changes — missing features, invalid enums, or broken code-ref `source` fields will fail the build.

## PR Lifecycle

Every PR follows a label-driven lifecycle that keeps the review loop moving.

```
┌──────────────┐
│  PR Created  │
└──────┬───────┘
       │
       ▼
┌──────────────────────┐
│  Automated Checks    │
│  (CI, rebase, etc.)  │
└──────┬───────────────┘
       │
       ├── all pass ──────────────────────►┌──────────────────────┐
       │                                   │ pending-maintainer   │
       │                                   └──────────┬───────────┘
       │                                              │
       │                                              ├── LGTM → approve & merge (or request
       │                                              │          another maintainer review)
       │                                              │          stays pending-maintainer
       │                                              │
       │                                              └── pending actions for contributor
       │                                                         │
       │                                                         ▼
       └── any fail ──────────────────────►┌──────────────────────┐
                                           │ pending-contributor  │◄─────────┐
                                           └──────────┬───────────┘          │
                                                      │                      │
                                                      │ stale 2 days         │
                                                      │ (no author activity) │
                                                      ▼                      │
                                           ┌───────────────────┐             │
                                           │   closing-soon    │             │
                                           │ (or immediate if  │             │
                                           │  blocker detected)│             │
                                           └────────┬──────────┘             │
                                                    │                        │
                                       ┌────────────┴──────────┐             │
                                       │                       │             │
                                       ▼                       ▼             │
                             author comments            3 more days          │
                             within 3 days             no activity           │
                                       │                       │             │
                                       ▼                       ▼             │
                             ┌────────────────────┐  ┌────────────┐          │
                             │ pending-maintainer  │  │  PR Closed │          │
                             │ (labels removed)    │  └────────────┘          │
                             └────────┬───────────┘                          │
                                      │                                      │
                                      └── re-check fails ────────────────────┘
```

### Label Transitions

| Current State | Trigger | Action |
|---------------|---------|--------|
| `pending-contributor` | No author activity for 2 days | Add `closing-soon` |
| `closing-soon` | No author activity for 3 more days | Auto-close PR |
| `pending-contributor` | Author adds a comment | Remove `pending-contributor`, add `pending-maintainer` |
| `closing-soon` | Author adds a comment | Remove `closing-soon` and `pending-contributor`, add `pending-maintainer` |

### Key Rules

- **`pending-contributor`** — the ball is on the contributor; maintainers are waiting for updates.
- **`closing-soon`** — warning that the PR will be auto-closed if no response within 3 days. For PRs missing a Discord Discussion URL, auto-close happens in 24 hours.
- **Author comment always resets** — any comment by the PR author removes `pending-contributor` and `closing-soon`, flipping the PR back to `pending-maintainer`.
- **Re-check may re-apply `closing-soon`** — after the flip, automated checks still run. If blockers remain (e.g., missing Discord URL, CI failure, `needs-rebase`), `closing-soon` will be re-applied immediately, keeping the ball on the contributor.
- **Immediate `closing-soon`** — in some cases (e.g., missing Discord Discussion URL), `closing-soon` is applied immediately without waiting for the stale period. Auto-close follows in 24 hours.
