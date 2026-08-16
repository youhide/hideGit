# Releasing

hideGit publishes one downloadable archive per platform. There is no installer and no paid signing
certificate behind any of them — this page says what that costs the person downloading, and what it
would take to change.

## Cutting a release

1. Bump `version` under `[workspace.package]` in `Cargo.toml`, and commit.
2. Tag it and push the tag:

```sh
git tag v0.0.1 && git push origin v0.0.1
```

[`.github/workflows/release.yml`](../.github/workflows/release.yml) does the rest: it builds on
three runners, attaches the archives and a `SHA256SUMS.txt` to a GitHub release, and marks it a
pre-release.

The tag and the manifest version have to agree. A mismatch fails the first job, before anything
compiles, because the macOS bundle takes its `CFBundleShortVersionString` from the manifest and
would otherwise ship a version string that contradicts its own download URL.

To test a change to the workflow without publishing, run it from the Actions tab
(`workflow_dispatch`). The same archives get built and uploaded as workflow artifacts, and the
publishing job is skipped.

Releases are published as **pre-releases**, which has a consequence worth knowing: GitHub excludes
pre-releases from "latest". `/releases/latest` redirects to the releases list rather than to a
release, and the API endpoint behind it answers 404. That is why the README points at
`/releases` — a link that would only start working at 1.0 is worse than one that always works.

## What ships

| Platform | Archive | Contents |
|---|---|---|
| macOS 11+ | `hidegit-<version>-macos-universal.zip` | `hideGit.app`, universal — one download for Intel and Apple Silicon |
| Windows 10+ | `hidegit-<version>-windows-x86_64.zip` | `hidegit.exe`, statically linked against the MSVC runtime |
| Linux, glibc 2.35+ | `hidegit-<version>-linux-x86_64.tar.gz` | binary, `install.sh`, `.desktop` entry, hicolor icons |

Three decisions in there are not obvious:

- **Universal on macOS**, built by compiling both targets and fusing them with `lipo`, rather than
  publishing two files someone has to choose between while not knowing which Mac they have.
- **`+crt-static` on Windows.** Without it the executable needs a Visual C++ redistributable that a
  clean Windows install does not have, and the first thing a new user meets is a missing-DLL dialog.
- **Linux builds on `ubuntu-22.04`, not `ubuntu-latest`.** A binary linked against a newer glibc will
  not start on an older distribution; the reverse is fine. The runner picks the floor.

## Why the archives are not signed, and what that looks like

Signing costs money and an organisational identity, neither of which this project has yet. Rather
than block downloads on that, the archives ship unsigned and say so. What each system does:

**macOS** asks the user to allow the app once — System Settings → Privacy & Security → **Open
Anyway**. Control-clicking → Open stopped bypassing Gatekeeper in macOS 15, so that older piece of
advice is no longer worth giving.

The bundle *is* ad-hoc signed, by `cargo run -p xtask -- bundle-macos`. That is not the same thing
as being signed by a developer and it does not quiet Gatekeeper. It is there because **arm64 macOS
refuses to execute a Mach-O carrying no signature at all**, and `lipo` strips the signature the
linker applied. An unsigned universal build would not launch on any Apple Silicon Mac.

**Windows** shows SmartScreen's "Windows protected your PC" — More info → Run anyway. SmartScreen
reputation is per-certificate and accrues over downloads, so this does not improve on its own while
the builds are unsigned.

**Linux** does nothing; there is nothing to bypass.

## What signing would take

Recorded so the estimate does not have to be rebuilt later:

- **macOS**: Apple Developer Program membership (annual fee), a Developer ID Application
  certificate, then `codesign` with that identity followed by `xcrun notarytool submit` and
  `xcrun stapler staple`. Notarisation is what removes the Gatekeeper prompt — signing alone does
  not. The certificate and an app-specific password live in repository secrets.
- **Windows**: an OV or EV code-signing certificate, or Azure Trusted Signing, plus `signtool`.
  EV certificates carry SmartScreen reputation immediately; OV ones build it over time.
- **Linux**: nothing to sign. What is missing there is packaging — AppImage and Flatpak — not
  signatures.

`.dmg` and `.msi` are worth building at the same time as signing, not before: an unsigned installer
adds a step for the user without removing the warning it would have been justified by.
