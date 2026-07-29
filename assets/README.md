# Assets

`icon.png` is the source. Everything in `generated/` is derived from it by:

```sh
cargo run -p xtask -- icons
```

Generated files **are committed**. `crates/hidegit/build.rs` reads `generated/hidegit.ico` at build
time, so a fresh checkout has to contain it — a contributor should not need to run a task before the
project compiles.

## What gets generated, and why each format exists

| File | Used by |
|---|---|
| `window-icon-256.png` | `include_bytes!` in `crates/hidegit/src/main.rs`, decoded at startup and handed to the window |
| `hidegit.ico` | linked into `hidegit.exe` as a resource, via `packaging/windows/hidegit.rc.in` |
| `hidegit.icns` | `hideGit.app/Contents/Resources/`, assembled by `cargo run -p xtask -- bundle-macos` |
| `hicolor/<N>x<N>/apps/com.youhide.hidegit.png` | installed under `share/icons/hicolor/` by `packaging/linux/install.sh` |
| `icon-512.png` | the icon at the top of the root `README.md` |

Four formats rather than one because no single mechanism covers the platforms:

- **macOS ignores window icons entirely.** winit's backend discards them, so the Dock icon can only
  come from the `.app` bundle's `.icns`.
- **Windows draws the executable's icon** in Explorer, the taskbar and Alt-Tab from a linked
  resource, independently of anything the running window sets.
- **Wayland also ignores window icons**, and matches the window's `app_id` against an installed
  `.desktop` file instead. X11 is the one place the runtime window icon is what you see.

## Framing

The source tile is 1066×1039 — very slightly wider than tall — so it is padded to a square canvas
rather than stretched. Distorting a rounded square is the kind of thing a viewer notices without
being able to say why.

Two framings come out of that square:

- **Full-bleed** for the `.ico`, the hicolor set, the window icon and the README, matching the
  convention on those platforms.
- **Inset** for the `.icns`, scaled into Apple's safe area — an 824px tile on a 1024px canvas — so
  the Dock icon reads at the same optical size as everything beside it.

## Regenerating

The generator is pure Rust (`image`, `ico`, `icns`), so regenerating needs no ImageMagick and no C
toolchain, on any platform. The `.icns` round-trips through Apple's own `iconutil` if you want to
check it:

```sh
iconutil -c iconset -o /tmp/check.iconset assets/generated/hidegit.icns
```
