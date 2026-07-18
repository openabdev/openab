# Review Contract

A Review Contract defines the agreed boundary for a pull request: what problem it
must solve, what it intentionally does not solve, which risks are accepted, and
what evidence is required for LGTM. Its purpose is to make review rigorous
without allowing the acceptance bar to expand indefinitely between rounds.

## Required PR Body

Every pull request must contain this exact structure and provide meaningful
content under every subsection:

```markdown
## Review Contract

### Goal
What user problem this PR must solve.

### Non-goals
What this PR intentionally does not attempt to solve.

### Accepted Residual Risks
Known failure modes or trade-offs accepted for this version, including
mitigations and recovery procedures.

### Acceptance Criteria
Concrete, testable conditions required for LGTM.

### Follow-ups
Useful hardening or broader designs explicitly deferred from this PR.
```

For a small or documentation-only change, a section may say `None` or `Not
applicable` only when it also gives a brief reason. Do not leave template
comments, `TBD`, or an empty checkbox as the section's only content.

## Responsibilities

- **Author:** proposes the initial contract and supplies validation evidence.
- **Reviewers:** challenge omissions, unsafe assumptions, and untestable
  acceptance criteria during the first review round.
- **Maintainer/owner:** freezes the contract and explicitly decides whether
  residual correctness, security, operational, or data-loss risks are
  acceptable. An author cannot accept those risks unilaterally.

The PR description is the canonical current copy of the contract. Discussion
comments may explain decisions, but they do not silently change the contract.

## Review Lifecycle

### Round 1: define and freeze

Round 1 is the full review. It may challenge the implementation and the proposed
Goal, Non-goals, Accepted Residual Risks, Acceptance Criteria, and Follow-ups.
Before requesting fixes, the maintainer should resolve material contract
questions and state that the contract is frozen.

The freeze must also be durable: record a contract revision identifier, the
exact reviewed head commit, and either the full contract text or a SHA-256 of
that exact text in a submitted review or equivalent immutable review record.
That freeze record identifies the approved version if the PR description later
changes. Any mismatch is a proposed contract revision and must follow the
revision process below.

A frozen contract is not permission to merge broken code. It is the agreed test
for what counts as broken within this PR.

### Later rounds: incremental verification

After the freeze, review only:

1. unresolved findings from earlier rounds;
2. changes since the last reviewed commit;
3. regressions caused by those changes; and
4. compliance with the frozen Acceptance Criteria.

Do not restart unrestricted architecture discovery on every push. Preferences
for broader hardening or a different future architecture belong in Follow-ups
unless they pass the Late Blocker Gate below.

## Finding Lineage

Every blocking finding raised after Round 1 should identify its lineage:

| Lineage | Meaning |
|---------|---------|
| `ORIGINAL` | An unresolved finding already raised within the frozen scope. |
| `REGRESSION` | A new defect introduced by changes made during this PR. |
| `NEW EVIDENCE` | Newly discovered, direct evidence that the PR violates the frozen contract. |
| `SCOPE EXPANSION` | A request outside the frozen Goal or Acceptance Criteria. |

`SCOPE EXPANSION` is non-blocking by default and should be recorded under
Follow-ups or in a separate issue.

## Late Blocker Gate

A new blocker raised after Round 1 must provide concrete, reproducible evidence
of at least one of the following:

- an Acceptance Criterion is not met;
- the implementation does not achieve the frozen Goal;
- the PR introduces a correctness, security, or data-loss defect within the
  frozen scope; or
- the latest changes regress behavior covered by the frozen contract.

The finding must state the affected contract clause, evidence, impact, and a
testable requested change. Hypothetical hardening, general architecture
preferences, unrelated pre-existing defects, and requests to eliminate an
explicitly accepted residual risk do not pass this gate.

## Contract Revisions

A contract may change after it is frozen only with explicit maintainer/owner
approval. Update the PR description and record:

- what changed;
- why the prior contract was insufficient; and
- which earlier findings or decisions must be reconsidered.

The revised portion receives a new full review. Unchanged portions remain
frozen.

## Stopping Rule

The default review sequence is capped at three stages:

1. full review and contract freeze;
2. fix verification; and
3. final regression check.

If the PR still cannot meet the contract, the maintainer/owner chooses whether
to authorize another focused round, revise or split the contract, or close the
PR. The cap does not force LGTM and never suppresses a blocker that passes the
Late Blocker Gate.

## LGTM and Follow-ups

LGTM requires all frozen Acceptance Criteria to be satisfied, all blocking
findings to be resolved, and required CI to pass. Follow-ups are explicitly
non-blocking for this PR unless the contract is formally revised.

## Automated Validation and Exemptions

The `Review Contract` workflow checks only structure: all required headings
must appear once, in order, with non-placeholder content. It does not decide
whether the contract is safe or sufficient.

A maintainer may apply the `review-contract-exempt` label to generated,
emergency, or otherwise exceptional PRs. The reason should be documented in the
PR. Exemption is an auditable maintainer decision, not an author opt-out.
