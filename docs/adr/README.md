# Architecture Decision Records

An ADR records a decision that was genuinely contested — what was chosen, what was rejected, and
what it costs — at the moment it was made. It is not documentation of how the system works
(that is [ARCHITECTURE.md](../ARCHITECTURE.md)); it is the reasoning that would otherwise be lost
and re-litigated every six months.

## Index

| # | Decision | Status |
|---|---|---|
| [0001](./0001-gui-toolkit-iced.md) | Use iced as the GUI toolkit | Accepted |
| [0002](./0002-git-backend-hybrid.md) | Hybrid Git backend: gitoxide for reads, `git` CLI for writes | Accepted |
| [0003](./0003-forge-github-first.md) | GitHub first, behind a `Forge` trait; device flow, no embedded secret | Accepted |
| [0004](./0004-license-gpl3.md) | License under GPL-3.0 | Accepted |
| [0005](./0005-progress-and-cancellation.md) | Progress and cancellation by parsing stderr and killing the child | Accepted |

## Conventions

- Filename: `NNNN-short-kebab-title.md`, numbered sequentially from 0001.
- **Never edit an accepted ADR to change its decision.** Write a new one that supersedes it, and
  mark the old one `Superseded by NNNN`. The record of what was believed at the time is the point.
- Correcting a typo or adding a link is fine. Rewriting the reasoning is not.

**Status** is one of: `Proposed` · `Accepted` · `Superseded by NNNN` · `Deprecated`.

## Structure

```markdown
# NNNN — Title

- **Status:** Accepted
- **Date:** YYYY-MM-DD

## Context
What forced a decision. Constraints, facts, and what was actually verified.

## Decision
What was chosen, stated plainly.

## Alternatives considered
Each option, and the specific reason it was not chosen.

## Consequences
What this buys, what it costs, and what has to be revisited later.
```

## When to write one

Write an ADR when a choice is expensive to reverse, when a reasonable engineer would ask "why not
the obvious thing instead", or when a constraint drove the decision and that constraint will not
be visible from the code.

Do not write one for choices that are conventional, cheap to reverse, or self-evident from the
code itself.

If you want to change a decision an ADR recorded, **open an issue proposing a superseding ADR
before writing code.** The decision may well be wrong — [ADR-0002](./0002-git-backend-hybrid.md)
in particular is expected to be superseded — but it should be reversed deliberately, not in
passing.
