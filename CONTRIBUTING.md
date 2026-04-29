# Contributing to OpenAB

Thanks for your interest in contributing! This guide covers what we expect in pull requests.

## Pull Request Guidelines

Every PR must address the following in its description. The [PR template](/.github/pull_request_template.md) will prompt you for each section.

### 0. Discord Discussion URL

Every PR must include a Discord Discussion URL in the body (e.g. `https://discord.com/channels/...`). PRs without one will be labeled `closing-soon` and auto-closed after 3 days.

### 1. What problem does this solve?

Describe the pain point or requirement in plain language. Link the related issue.

### 2. Prior Art & Industry Research

Before proposing a solution, research how the industry handles the same problem. At minimum, investigate:

- **[OpenClaw](https://github.com/openclaw/openclaw)** — the largest open-source AI agent gateway
- **[Hermes Agent](https://github.com/NousResearch/hermes-agent)** — Nous Research's self-hosted agent with multi-platform messaging

Include links to relevant source code, documentation, or discussions. If neither project addresses the problem, state that explicitly with evidence.

### 3. Proposed Solution & Why This Approach

Describe your technical approach, then explain why you chose it over the alternatives found in your research. Be explicit about:

- Tradeoffs you accepted
- Known limitations
- How this could evolve in the future

### 4. Alternatives Considered

List approaches you evaluated but did not choose, and explain why they were rejected.

### 5. Test Plan

- `cargo check` and `cargo test` must pass
- Describe any manual testing performed
- Add unit tests for new functionality

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

## Code Style

- Run `cargo fmt` before committing
- Run `cargo clippy` and address warnings
- Keep PRs focused — one feature or fix per PR
