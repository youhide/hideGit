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
| Linux, glibc 2.35+ | `hideGit-<version>-x86_64.AppImage` | one file: binary, `.desktop` entry and icons, run in place |
| Linux, glibc 2.35+ | `hidegit-<version>-linux-x86_64.tar.gz` | binary, `install.sh`, `.desktop` entry, hicolor icons |

Three decisions in there are not obvious:

- **Universal on macOS**, built by compiling both targets and fusing them with `lipo`, rather than
  publishing two files someone has to choose between while not knowing which Mac they have.
- **`+crt-static` on Windows.** Without it the executable needs a Visual C++ redistributable that a
  clean Windows install does not have, and the first thing a new user meets is a missing-DLL dialog.
- **Linux builds on `ubuntu-22.04`, not `ubuntu-latest`.** A binary linked against a newer glibc will
  not start on an older distribution; the reverse is fine. The runner picks the floor, for the
  AppImage as much as for the tarball.
- **The AppImage bundles nothing, and that is not an oversight.** `linuxdeploy` copies what `ldd`
  reports, and `ldd` on hideGit reports only glibc — every windowing and graphics library it uses is
  reached through `dlopen` at runtime. Forcing them in would be worse than leaving them out:
  libwayland, libGL and libvulkan have to match the host, and bundling them is the classic way to
  produce an AppImage that starts on the machine that built it and nowhere else. What the format buys
  here is one file with the desktop entry and icons inside it, not freedom from a distribution's
  packages.
- **No certificate is involved.** An AppImage is unsigned, exactly like the tarball beside it. This
  repository used to claim in `packaging/linux/install.sh` that AppImage and Flatpak were waiting on
  a signing certificate; that was wrong, and only the macOS and Windows installers ever were.
- **The release job runs the AppImage before publishing it.** `--version` returns before any window
  opens, so it exercises the AppRun and the extraction path on a machine with no display — which
  `linuxdeploy` exiting 0 does not.

## Why there is no Flatpak

Not cost, and not effort: **a Flatpak sandbox takes away the thing ADR-0002 chose the architecture
for.**

Every write hideGit performs is `Command::new("git")` resolved from `PATH`
(`crates/hidegit-core/src/process.rs`). Inside a Flatpak that resolves to the *runtime's* git, and
[ADR-0002](./adr/0002-git-backend-hybrid.md) lists what the user's git brings that the runtime's does
not:

| What the user configured | Where it lives | Inside the sandbox |
|---|---|---|
| Credential helpers — `libsecret`, `manager`, `!gh auth git-credential` | host binaries named in `~/.gitconfig` | **absent**, and `GIT_TERMINAL_PROMPT=0` turns that into a failed push rather than a prompt |
| Hooks — `pre-commit` running `npx`, `python3`, `husky` | host interpreters | **absent** |
| `.gitattributes` filters, Git LFS | host binaries | **absent**; an LFS checkout produces pointer files |
| SSH agent | `SSH_AUTH_SOCK`, already inherited | works, given `--socket=ssh-auth` |

Three of those four are the exact list ADR-0002 gives as the reason writes go through the CLI at all:
they "work exactly as the user has already configured them — inherited, not reimplemented". A Flatpak
that quietly cannot push over HTTPS, cannot run a repository's hooks and turns LFS files into
pointers is not a package worth shipping.

There are three ways out, and choosing between them is a maintainer's decision rather than a
packaging detail:

1. **`--talk-name=org.freedesktop.Flatpak`, and run every `git` through `flatpak-spawn --host`.**
   Restores all of it. The cost is that the sandbox then permits arbitrary host command execution —
   a sandbox in name only — and `hidegit-core` has to know it is inside a Flatpak and spawn a
   different program, which is an architectural change ADR-0002 would need to be superseded to make.
2. **Ship against the runtime's git and accept the losses.** Cheapest to build, and it contradicts
   the ADR without saying so to the person whose push just failed.
3. **Do not ship a Flatpak.** The AppImage already covers "one file, nothing to install", and it has
   none of these problems because it is not sandboxed.

Until that is decided, option 3 is what happens by default, and this section exists so the next
person to reach for `flatpak-builder` finds the reason rather than the gap.

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
