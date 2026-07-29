# 0001 — Use iced as the GUI toolkit

- **Status:** Accepted
- **Date:** 2026-07-29

## Context

hideGit is a desktop Git client for macOS, Windows and Linux. The GUI toolkit is the single
hardest decision to reverse — it shapes the architecture, the concurrency model, the rendering of
the commit graph, and the contributor pool.

Requirements that drove the choice:

1. **A custom, high-performance commit graph.** The centrepiece is a virtualised, GPU-drawn graph
   targeting 60fps on a 100,000-commit repository. Whatever the toolkit, it must expose a fast
   immediate-mode or retained drawing surface.
2. **Native feel, native performance.** A key argument for the project is that a Git client does
   not need to ship a browser engine. Memory footprint and cold start are features.
3. **One codebase, three platforms.**
4. **Rust.** Non-negotiable — it is the project's premise.
5. **A licence compatible with GPL-3.0.**

## Decision

Use **iced 0.14** as the GUI toolkit.

iced implements the Elm architecture — `State`, `Message`, `update`, `view` — and renders through
`wgpu` on Vulkan, Metal and DirectX. Its `canvas` widget gives direct access to a retained drawing
surface, which is what the commit graph needs.

Version 0.14 (released 2025-12-07) is, per the project's own FAQ, the **last experimental release
before 1.0**. It added reactive rendering, headless testing, hot reloading and time-travel
debugging. Headless testing in particular affects how `hidegit-ui` is tested — state transitions
can be asserted without a window.

iced is MIT licensed, which is compatible with a GPL-3.0 work.

## Alternatives considered

**egui** — immediate mode, very easy to get started with, excellent for tooling. Rejected because
immediate mode makes complex, state-heavy layouts (a virtualised graph with stable scroll anchoring
and rich text selection in diffs) harder rather than easier, and its default look is recognisably
"a debug tool". For an application meant to be used for hours at a time, fighting the toolkit's
aesthetic for years is a poor trade.

**Tauri** — mature, excellent tooling, huge UI ecosystem, and the fastest path to a polished
interface. Rejected because the frontend is a web view: it reintroduces a browser engine,
HTML/CSS/JS in the stack, and a serialisation boundary between the UI and the Git layer that the
commit graph would have to cross on every scroll. Native rendering with direct access to the Git
layer is a design goal here, and a web view gives up both.

**GTK4 via gtk4-rs** — genuinely native on Linux, mature, and accessible. Rejected for
cross-platform reasons: GTK on macOS and Windows is a second-class experience for both users and
for anyone trying to build the project. It also reintroduces a C toolchain dependency, which
[ADR-0002](./0002-git-backend-hybrid.md) works to avoid, and GTK's LGPL licensing adds
distribution constraints for signed platform installers.

**Slint** — capable, good tooling, strong embedded story. Rejected primarily on licensing: its
GPL/commercial dual model is workable for a GPL-3.0 project but constrains any future
relicensing, and the community around it is smaller than iced's for desktop application work.

**Dioxus** — nice developer experience, but its desktop target is a web view again, so the same
objection as Tauri applies.

## Consequences

**What this buys**

- No browser engine, no HTML/CSS/JS, no IPC boundary between UI and Git layer.
- The `canvas` widget makes the commit graph straightforward rather than a fight.
- The Elm architecture forces every side effect through `Task` or `Subscription`, which is exactly
  the discipline needed to keep blocking Git work off the UI thread.
- A single Rust codebase; contributors need one language.
- Headless testing (new in 0.14) makes UI logic genuinely testable.

**What this costs**

- **iced is pre-1.0.** 0.14 is the last experimental release, so a breaking upgrade to 1.0 is
  expected. Mitigation: iced types stay inside `hidegit-ui`, so the blast radius is one crate.
- **Smaller widget ecosystem.** Things a web stack gets free — rich text editing, complex tables,
  a date picker — may need building. Budgeted for; the graph and diff views were always going to
  be custom.
- **Fewer contributors know iced** than know React. Offset by the fact that Rust developers are
  the likely contributor pool for a Rust Git client.
- **A GPU is required.** Vulkan, Metal or OpenGL 3.3. Rules out some remote and virtualised
  environments.
- **Accessibility is weaker** than a mature native toolkit. iced's support is improving; this is
  a real limitation and is tracked as work in M6, not waved away.

**Revisit if:** iced 1.0 turns out to break more than a contained upgrade can absorb, or if
accessibility remains inadequate at the point of a 1.0 release.
