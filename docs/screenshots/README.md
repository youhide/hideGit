# Screenshots

The images the [README](../../README.md) shows. Nothing here is a mockup: each one is the release
build, opened on **this repository**, so the commits in the graph are the ones in the log.

## Reproducing them

Two things matter, and the second is the one worth copying.

**Build the bundle, not just the binary.** The window is the same either way, but a bundle is what
macOS gives an icon and a name to.

```sh
cargo build --release
cargo run -p xtask -- bundle-macos
```

**Run against a throwaway profile.** hideGit keeps its config, state and recent-repository list
under `$HOME`, so pointing `HOME` at an empty directory gives a session that has never opened
anything else. That is not tidiness — a screenshot of a real sidebar publishes the name and path of
every repository the author has open, and those are usually somebody else's to disclose.
`HIDEGIT_NO_KEYCHAIN=1` does the same job for the forge: signed out, so no pull request, review or
account detail can appear.

```sh
profile=$(mktemp -d)
open -a target/release/bundle/hideGit.app \
  --env HOME="$profile" --env HIDEGIT_NO_KEYCHAIN=1 \
  --args "$PWD"
```

The trade is that the pull request pane cannot be shown this way, since it needs an account. A
screenshot of it would have to come from a repository whose PRs are already public, with the same
care taken over everything else on screen.

**Capture the window, not a region of the screen.** A rectangle captures whatever is on top of it;
a window id captures the window.

```sh
screencapture -x -o -l<window-id> docs/screenshots/graph.png
```

## Keeping them honest

A screenshot is documentation that cannot be tested, so it is the first thing to go stale. When a
change alters what one of these shows — the sidebar's sections, the diff's colours, the composer —
retake it in the same commit, the way any other document in `docs/` is updated with the change it
describes.
