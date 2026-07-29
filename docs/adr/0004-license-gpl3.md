# 0004 — License under GPL-3.0

- **Status:** Accepted
- **Date:** 2026-07-29

## Context

hideGit is an end-user desktop application, not a library. The license choice determines whether
improvements to it can be taken private, and it constrains which dependencies can be used.

The Rust ecosystem's convention is dual MIT OR Apache-2.0, chosen so libraries can be embedded
anywhere without friction. That reasoning is about libraries. hideGit is a binary nobody links
against, so the convention's main justification does not apply here.

Two facts had to be checked rather than assumed:

1. **Dependency compatibility.** A GPL-3.0 work can depend on permissively licensed crates, but
   not the reverse. The main dependencies are `iced` (MIT), `gix` (MIT OR Apache-2.0) and
   `octocrab` (MIT). All three are compatible as dependencies of a GPL-3.0 application.
2. **Distribution.** GPL-3.0 requires source availability for distributed binaries and permits
   redistribution — nothing in it prevents publishing signed installers through the usual
   channels, provided the source is available.

## Decision

License hideGit under **GPL-3.0**.

The `LICENSE` file is the canonical FSF text, copied verbatim from
`https://www.gnu.org/licenses/gpl-3.0.txt`. Not paraphrased, not reformatted.

Contributions are accepted under GPL-3.0. No CLA and no copyright assignment — contributors keep
their copyright, and no single party can therefore relicense the project unilaterally.

## Alternatives considered

**MIT OR Apache-2.0.** The Rust ecosystem default: maximum adoption, zero friction, patent
protection from the Apache side. Rejected because it permits a fork to be taken proprietary, and
for an application — as opposed to a library — that is precisely the outcome copyleft exists to
prevent. The friction argument is weak here: nobody links against a desktop Git client.

**MIT alone.** Simplest and shortest. Rejected for the same reason, and additionally because it
has no patent grant.

**AGPL-3.0.** Stronger: extends copyleft to network use. Rejected because hideGit is a desktop
application with no server component, so the network clause protects against nothing that could
actually happen. It would add real friction — many organisations prohibit AGPL software outright —
in exchange for no benefit. If a hosted component ever appears, this deserves reconsideration for
that component specifically.

**MPL-2.0.** File-level copyleft; a middle ground allowing proprietary combination while keeping
modified files open. Rejected because the file-level boundary is a poor fit for an application,
where meaningful improvements are usually new files rather than modifications to existing ones.

**Dual GPL-3.0 plus a commercial license.** Would keep a paid option open. Rejected because it
requires a CLA or copyright assignment to be workable, which discourages contribution and
concentrates control in one party. Keeping copyright distributed among contributors is the point.

## Consequences

**What this buys**

- Forks stay open. Anyone distributing a modified hideGit distributes its source.
- Patent protection, via GPL-3.0's patent grant.
- Contributors' work cannot be relicensed out from under them — there is no CLA, so no single
  party can change the terms.
- Compatible with the entire intended dependency set.

**What this costs**

- **Some organisations prohibit GPL software** on developer machines. Those users cannot adopt
  hideGit. This is an accepted, deliberate cost.
- **Dependency choice is constrained.** No proprietary or GPL-incompatible crates, ever. Any
  future dependency needs a license check, not just a vibe check — a permissively licensed crate
  is fine, an Elastic-License or SSPL one is not.
- **Distributing binaries carries obligations.** Source must be available alongside every release.
  Practically satisfied by the public repository plus source tarballs attached to releases, but it
  is a release-process requirement, not automatic.
- **Contributions must be GPL-clean.** Code copied from a proprietary product or from an
  incompatibly licensed project cannot be accepted — including code produced by an AI assistant
  where there is reason to believe it reproduces such a source. Stated in
  [CONTRIBUTING.md](../../CONTRIBUTING.md#licensing-of-contributions).
- **No commercial dual-licensing later** without contacting every contributor. That door is
  closed deliberately.
