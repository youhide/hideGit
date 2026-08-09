# Security Policy

## Reporting a vulnerability

**Please do not open a public issue for a security vulnerability.**

Report it privately through GitHub's [private vulnerability reporting][pvr] on this repository
(Security → Report a vulnerability), or by email to **youri@youhide.com.br**.

Please include:

- What the vulnerability allows an attacker to do
- Steps to reproduce, or a proof of concept
- The affected version or commit
- Your platform (hideGit shells out to `git`, and the surface differs between Windows and Unix)

You can expect an acknowledgement within 72 hours and an assessment within 7 days. We will keep
you updated while a fix is prepared and will credit you in the release notes unless you prefer
otherwise. Please give us a reasonable window to ship a fix before disclosing publicly.

## Supported versions

hideGit is pre-1.0. Only the latest release, and `main`, receive security fixes.

## Security-relevant design

These are the areas where a bug is most likely to be exploitable, and where review attention is
most welcome.

### Credentials are never written to disk by hideGit

Forge access tokens — **and the refresh tokens that come with them** — live in the operating system
keychain (macOS Keychain, Windows Credential Manager, Secret Service on Linux) via the `keyring`
crate. They are **never** written to the configuration file, never logged, and never included in
diagnostic output.

That is enforced by the type rather than by discipline: a token is a `SecretString`, whose `Debug`
and `Display` both redact, so a struct that derives `Debug` around one cannot print it either.
Reading the real value requires calling `expose`, which is named so that grepping for it finds
every place a token can escape.

**If no keychain is available** — a headless Linux session with no Secret Service — forge features
are disabled. There is no file fallback, deliberately: silently downgrading to plaintext storage
would give a user who chose an encrypted credential store a different one without telling them.

Authentication uses the OAuth Device Authorization Flow against hideGit's registered GitHub App,
with a personal access token as a first-class fallback. hideGit embeds **no client secret** — in an
open source desktop application, an embedded secret is public by definition. A report that hideGit
has begun shipping one is a valid security report. The App's *client identifier* is compiled in and
is not a secret: it names the application and authorises nothing on its own, which is exactly the
property the device flow is designed around.

Git remote credentials are not handled by hideGit at all. They are delegated to your configured
Git credential helper, which is one of the reasons operations that touch a remote are delegated
to the system `git` binary. See [ADR-0002](./docs/adr/0002-git-backend-hybrid.md).

### Shelling out to `git`

hideGit invokes the system `git` binary for push, merge and rebase. This is the most
security-sensitive boundary in the application, so it is constrained:

- **Arguments are passed as a vector, never as a shell string.** No shell is spawned, so shell
  metacharacters in a branch name, path or remote URL are not interpreted.
- **`--` separates options from operands** wherever Git supports it, so a ref or path beginning
  with `-` cannot be absorbed as a flag.
- **`--end-of-options` is used instead where the command takes revisions and no paths.** To
  `git reset`, `git rev-parse` and `git rev-list`, `--` means *paths follow*, so
  `git reset --hard -- HEAD~1` asks to reset a path named `HEAD~1` and fails. `--end-of-options`
  ends flag parsing without making that claim, which is the guarantee actually wanted. Using `--`
  reflexively on those commands trades a real bug for an imagined one.
- **`GIT_TERMINAL_PROMPT=0`** — a subprocess must never silently block waiting on hidden input.
- The environment passed to the subprocess is controlled, not inherited wholesale.

A repository is untrusted input. Branch names, tag names, remote URLs, commit messages, author
names and file paths all come from a repository that may have been cloned from anywhere. Any code
path where one of those reaches a command line, a file path, or a rendered UI string without
validation is a bug worth reporting.

### Rendering untrusted repository content

Commit messages and file contents are attacker-controllable. hideGit renders them as text; it does
not interpret them as markup and does not follow links found in them without explicit user action.

## Out of scope

- Vulnerabilities in `git`, gitoxide, iced or other dependencies — report those upstream. If a
  dependency vulnerability is exploitable *specifically because of how hideGit uses it*, that is
  in scope and we want to hear about it.
- Anything requiring an attacker to already have local code execution as the user.

[pvr]: https://docs.github.com/en/code-security/security-advisories/guidance-on-reporting-and-writing-information-about-vulnerabilities/privately-reporting-a-security-vulnerability
