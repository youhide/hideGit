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

Forge access tokens live in the operating system keychain (macOS Keychain, Windows Credential
Manager, Secret Service on Linux) via the `keyring` crate. They are **never** written to the
configuration file, never logged, and never included in diagnostic output.

Authentication uses the OAuth Device Authorization Flow. hideGit embeds **no client secret** — in
an open source desktop application, an embedded secret is public by definition. A report that
hideGit has begun shipping one is a valid security report.

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
