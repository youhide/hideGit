# 0003 — GitHub first, behind a `Forge` trait; device flow, no embedded secret

- **Status:** Accepted
- **Date:** 2026-07-29

## Context

hideGit surfaces pull request state and sends desktop notifications when a PR needs attention.
That requires talking to a hosting provider's API, and providers differ in ways that reach further
than the transport: GitLab merge requests have different approval semantics, Bitbucket models
reviewers differently, and self-hosted instances live on arbitrary domains.

Two decisions were needed: how many providers to support before 1.0, and how to authenticate.

Authentication has a constraint that is not negotiable. hideGit is open source, so **anything
compiled into the binary is public**. The standard OAuth Authorization Code flow for a desktop
application assumes a client secret the application can keep; hideGit cannot keep one. Publishing
a secret in a public repository and calling it confidential would be theatre, and it would be
grounds for the provider to revoke the application.

## Decision

**Support GitHub only before 1.0, behind a `Forge` trait from the start.**

The trait is deliberately narrow. It covers listing pull requests, fetching one in detail,
creating one, and reporting poll state — not the whole of any provider's API. Anything beyond that
opens the browser.

The trait's data model is provider-neutral: a GitLab merge request will become a `PullRequest`
here, translated at the boundary, so the UI never branches on provider.

**Authenticate with the OAuth 2.0 Device Authorization Flow** ([RFC 8628][rfc]), with a personal
access token as a first-class fallback.

The device flow is designed for exactly this case — a public client that cannot hold a secret. The
user gets a short code, approves it in a browser, and hideGit polls for the token. **No client
secret is embedded.**

Tokens are stored in the OS keychain via `keyring`. Never in the config file, never in logs.

Trait, polling and token handling in
[ARCHITECTURE.md](../ARCHITECTURE.md#forge-integration).

## Alternatives considered

**GitHub, GitLab and Bitbucket before 1.0.** Broadest coverage. Rejected because it triples the
authentication surface and the data-model translation work in the milestone where none of it is
proven yet, and would delay a usable release substantially. Worse, designing a trait against three
providers at once tends to produce an abstraction shaped by guesses about two of them.

**GitHub and GitLab before 1.0.** The middle option, and the closest call. Rejected on the same
reasoning at smaller scale: GitLab's approval and pipeline models differ enough from GitHub's that
supporting it properly is its own milestone, not a variation on M4.

**No trait — implement GitHub directly and generalise later.** Faster in the short term and avoids
designing an abstraction with one implementation. Rejected because forge details would leak into
`hidegit-ui` — provider-specific state names, provider-specific errors — and extracting them later
is a far larger change than defining the boundary up front. The trait costs little now.

**A generic Git-hosting abstraction covering issues, releases, CI and code review.** Rejected as
scope creep. hideGit is a Git client that tells you about pull requests, not a forge client.
A wide surface would need maintaining against four providers' API churn.

**Authorization Code flow with PKCE and an embedded client ID.** Considered seriously, since PKCE
exists precisely to let public clients avoid a secret. It requires either a loopback redirect URI
or a custom URI scheme, both of which add platform-specific handling — registering a scheme on
each OS, or binding a local port and dealing with firewalls. The device flow achieves the same
security property with no redirect handling at all, and works in environments where the browser
is not on the same machine. PKCE remains a reasonable future option; it is not better enough to
justify the extra platform work.

**PAT only.** Simplest to implement and gives the user precise control. Rejected as the *primary*
path because creating a scoped token by hand is a poor first-run experience. Kept as a fallback,
because it is genuinely better for GitHub Enterprise, restricted environments, and users who
prefer a credential they scope and revoke themselves.

## Consequences

**What this buys**

- One provider to get right, so PR alerts can actually ship in M4 rather than being three
  half-finished integrations.
- A trait boundary that keeps provider details out of `hidegit-ui` from day one.
- No embedded secret, so nothing to leak and nothing for a provider to revoke.
- Tokens in the OS keychain, which is the credential store the user already trusts.
- The device flow works when the browser is on a different machine — useful over SSH and in VMs.

**What this costs**

- **Non-GitHub users get nothing before 1.0.** Stated plainly in the README and roadmap rather
  than implied by a "coming soon" that never arrives.
- **The trait will need revision when the second provider lands.** A trait designed against one
  implementation almost never survives contact with the second unchanged. This is anticipated, not
  a failure — the point of having it now is that revising a boundary is cheaper than creating one.
- **The device flow requires the user to type a code.** More friction than a redirect, and it is
  the most likely place a first-run drops off.
- **Polling, not webhooks.** A desktop application has no public endpoint to receive webhooks, so
  alerts are as fresh as the poll interval. Conditional requests keep this affordable; see
  [ARCHITECTURE.md](../ARCHITECTURE.md#polling).
- **If the keychain is unavailable** — a headless Linux session with no Secret Service — forge
  features are disabled rather than falling back to a file. That is a deliberate refusal to
  silently downgrade credential storage, and it will occasionally frustrate someone.

**Revisit when:** a second provider is implemented (post-1.0). Expect a superseding ADR describing
what the trait had to become.

[rfc]: https://datatracker.ietf.org/doc/html/rfc8628
