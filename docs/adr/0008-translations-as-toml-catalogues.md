# 0008 — Translate through TOML catalogues, with no i18n runtime

- **Status:** Accepted
- **Date:** 2026-08-17

## Context

[ROADMAP](../ROADMAP.md#post-10) says translation scaffolding lands before 1.0 so that translating
is not a retrofit, with **PT-BR** as the first translation after it. Nothing existed: no `fluent`,
no `gettext`, no `rust-i18n`, no `.ftl` or `.po` anywhere, and every user-facing string was a
literal at its call site.

Measured rather than estimated: **about 690 prose literals** in `hidegit-ui` outside its tests —
142 in `app.rs`, 80 in the sidebar, 64 in the command palette, 59 in the pull-request pane. That is
the size of the eventual migration, and it is why the mechanism has to be decided before anyone
starts moving strings rather than after.

Three facts about hideGit narrow the problem considerably:

- **`hidegit-core` returns typed errors, never strings.** The domain layer has nothing to
  translate; the entire surface is one crate.
- **Git's own stderr reaches the user verbatim, deliberately** ([ADR-0002](./0002-git-backend-hybrid.md)).
  That text is not ours and must not be translated — a paraphrase of a Git error is worse than the
  error, and worse still in a second language.
- **`hidegit-ui` already parses TOML**, for the custom themes added in M6. A translation file that
  looks exactly like a theme file is one fewer format for a contributor to learn.

## Decision

**Translations are TOML catalogues of dotted keys, loaded at startup. No i18n runtime.**

- The English catalogue is `crates/hidegit-ui/locales/en.toml`, compiled in with `include_str!`.
  It is the only place a user-facing string is written; the code carries keys.
- A translation is the same file with the values replaced, dropped in a `locales` directory beside
  `config.toml` as `pt-BR.toml`. The same shape as `themes/`, in the same place, found the same way.
- **A translation the project maintains is compiled in as well**, for the reason English is: one that
  existed only as a file the user had to find and copy would not be a translation hideGit *has*. A
  file of the same name in the user's own `locales` directory still wins, because being overruled by
  the binary would make writing one pointless — so the order is user file, then bundled, then
  English. A bundled translation must cover every key; a user's own may be half-finished.
- A key missing from a translation falls back to English and is **reported**, exactly as a theme
  file that will not parse is. A half-finished translation shows English where it is unfinished
  rather than a key or an empty label.
- Plurals are an explicit two-form helper — `plural(n, "one", "other")` — with the forms in the
  catalogue.

## Alternatives considered

**Fluent (`fluent-bundle` + `unic-langid`).** The most capable option: CLDR plural rules for every
language, gendered selectors, message references. Rejected for now on cost against need. It adds
roughly ten crates for a first translation into a language whose plural rule is `n == 1` — the same
rule as English — and its selector syntax is a second language for translators to learn on top of
the strings. **This is the decision to revisit first** if hideGit gains a translation into a
language with three or more plural forms, and that revisit is a superseding ADR, not a quiet swap.

**`rust-i18n`.** Fewer dependencies than Fluent and a familiar macro. Rejected because what it
mostly buys is the file loading and key lookup, which is the small half of this problem — and it
would still be a dependency deciding a file format hideGit already has a parser for.

**GNU gettext / `.po`.** The format translators are most likely to already know, with mature
tooling. Rejected because extracting from Rust needs `xgettext` support that is patchy, the
compiled `.mo` step adds a build requirement, and the plural syntax in a `.po` header is exactly
the complexity being avoided.

**English strings as the keys.** No catalogue needed for English and no key can go stale. Rejected
because every copy edit then silently breaks every translation, and the code loses the one place
where all user-facing text can be read and reviewed together.

## Consequences

- **One dependency-free mechanism**, in a format the project already parses and a contributor has
  already seen in `themes/`.
- **The English catalogue is a review surface.** All user-facing copy in one file is worth having
  even with no translation at all — it is where a tone or a terminology slip is visible.
- **Plural rules beyond two forms are not supported.** Stated plainly rather than discovered: a
  language needing them needs Fluent, and needs this ADR superseded.
- **The migration is still ~690 strings**, and this ADR does not do it. The mechanism ships proven
  on one screen; moving the rest is separate, and until it is done the application is a mixture of
  catalogue keys and literals.
- **Git's output stays untranslated**, along with branch names, paths, remote URLs and anything else
  that came out of a repository.
