# 0006 — Poll pull request state over GraphQL, not conditional REST

- **Status:** Accepted
- **Date:** 2026-07-30

## Context

M4 sends a desktop notification when a pull request needs attention. The events it has to detect
are listed in [UI_SPEC.md](../UI_SPEC.md#pr-panel), and two of them are about CI:
`ChecksFailed` when a build starts failing, `ChecksPassed` when it starts passing.

[ARCHITECTURE.md](../ARCHITECTURE.md#polling) originally specified how to poll for all of them:
GitHub's REST API, every request carrying `If-None-Match` with the previous `ETag`. A `304 Not
Modified` costs nothing against the rate limit, which is what makes a one-minute interval
affordable at all. It is the textbook design, and hideGit was going to build it.

It does not work, for a reason that only becomes visible once the event list and the transport are
looked at together.

**A check run completing does not modify the pull request.** Checks attach to a *commit*; a pull
request's `updated_at` moves when someone edits it, comments, labels it, or changes its state.
So the sequence that matters most — push, CI runs for six minutes, CI fails — leaves the pull
request record untouched throughout. Its `ETag` stays valid, the conditional request returns 304,
and hideGit concludes nothing has changed. `ChecksFailed` never fires. The one notification a
developer actually waits for is the one the design cannot deliver.

The second problem is cost. A pull request row in the sidebar shows review state, CI state and
conflict state. Over REST those are three different resources:

| What | Request |
|---|---|
| The pull request, and `mergeable` | `GET /repos/{o}/{r}/pulls/{n}` |
| Review decision | `GET /repos/{o}/{r}/pulls/{n}/reviews` |
| CI rollup | `GET /repos/{o}/{r}/commits/{sha}/check-runs` |

That is three requests per pull request per poll, plus one to list them. Twenty open pull requests
is sixty-one requests, and only the ones that 304 are free — the check-runs requests are exactly
the ones that will not.

## Decision

**Poll over GitHub's GraphQL API. One query per repository per poll returns every field the
sidebar and every notification need.**

```graphql
repository(owner: $owner, name: $name) {
  pullRequests(states: OPEN, first: 50) {
    nodes {
      number title url isDraft updatedAt
      author { login }
      headRefName baseRefName
      mergeable
      reviewDecision
      reviewRequests(first: 5)  { nodes { requestedReviewer { ... } } }
      assignees(first: 5)       { nodes { login } }
      latestReviews(first: 5)   { nodes { author { login } state } }
      comments { totalCount }
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }
    }
  }
}
rateLimit { limit cost remaining resetAt }
```

`statusCheckRollup` is read on every poll regardless of whether the pull request changed, which is
precisely what the conditional design could not do.

**`PollCursor` stays on the trait, and becomes opaque.** GraphQL has no conditional requests, so
the GitHub implementation always returns an empty cursor. A REST-based forge — GitLab, post-1.0 —
would put an `ETag` in it. Keeping the shape provider-defined rather than deleting it is what lets
both live behind one trait, which is what [ADR-0003](./0003-forge-github-first.md) promised the
trait would be.

**Nested page sizes are kept small deliberately, because they are what a query costs.** GitHub
computes a GraphQL point cost by summing the `first`/`last` arguments across a query's connections
and dividing by 100. Requesting 20 reviewers, 20 assignees and 20 reviews for each of 50 pull
requests is roughly 2,600 nodes — about 26 points a poll, or 1,560 an hour at the 60-second
interval, against a 5,000-point hourly budget. Five each brings the same query to roughly 9 points,
or 540 an hour, which leaves room for several repositories open at once. Five reviewers is also
simply enough to render a row.

## Alternatives considered

**Conditional REST, as originally documented.** Rejected because of the `updated_at` gap above: it
cannot detect a CI transition, and CI transitions are two of the seven events M4 exists to deliver.
Everything else about it is better — free 304s, a stable and versioned API, `ETag`s that work — and
if the event list did not include CI this is what hideGit would build.

**Conditional REST for the list, plus a check-runs request per pull request.** Fixes correctness
and keeps the cheap list. Rejected on cost: the requests it adds are exactly the ones that never
304, so twenty open pull requests still cost twenty-one requests a minute, or 1,260 an hour against
a 5,000-request budget — for one repository. It also means two transports, two rate-limit buckets
and two parsing surfaces to maintain, for a result GraphQL returns in one call.

**GraphQL for the list, REST for detail on what changed.** The hybrid, and briefly attractive.
Rejected because once the GraphQL query has run there is nothing left for the REST call to fetch —
the fields the detail pane adds (body, review list, diff stats) are in the same schema. It would be
a second transport earning nothing.

**Webhooks.** Correct by construction, instant, and free of polling entirely. Rejected because a
desktop application has no public endpoint to receive them, and giving it one means running a
server — which is the hosted service [ROADMAP.md](../ROADMAP.md#what-is-deliberately-not-planned)
says hideGit will not have.

**GitHub's notifications API** (`GET /notifications`). Genuinely conditional, genuinely cheap, and
it is what the website's own bell uses. Rejected because it reports what GitHub decided to notify
you about under *your* account's notification settings, not what is true about a pull request.
hideGit's event list and per-event toggles would become a filter over somebody else's policy, and
`PrConflicting` — which GitHub does not notify on at all — would be unimplementable.

## Consequences

**What this buys**

- CI transitions are actually detected, which is the milestone's headline event.
- One request per repository per poll, whatever the number of open pull requests.
- Review state, check state and conflict state arrive together and therefore cannot disagree with
  each other, which a three-request assembly can.
- `rateLimit` comes back inside the response, so the budget that drives the poll interval is known
  without a separate call.

**What this costs**

- **Every poll spends points. There is no free 304 any more.** The budget stops being effectively
  unlimited and becomes something the scheduler has to respect — which it already did, since the
  20% and 5% thresholds were in the design regardless.
- **The cost of a query is a function of its page sizes**, so adding a field with a `first:` to the
  query is a rate-limit change and not only a schema change. Anyone editing the query has to know
  that; it is written down here because it is invisible at the call site.
- **GraphQL has no API version.** REST is versioned by date header; GraphQL fields are deprecated
  and removed on a schedule. Translation fails soft — an unrecognised enum variant costs one field,
  never the poll — but the schema remains a parsing surface, exactly like subprocess output is on
  the Git side.
- **A second provider will not necessarily have a GraphQL API**, and GitLab's is shaped quite
  differently. The `Forge` trait absorbs this: `PollCursor` stays for the forge that needs it.

**Revisit when:** GitHub ships conditional requests for GraphQL, or a second provider lands and the
trait is revised anyway ([ADR-0003](./0003-forge-github-first.md) expects that).
