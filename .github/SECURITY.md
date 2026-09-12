# Security policy

## Reporting a vulnerability

Please report it privately, not as a public issue:

- **Preferred:** [open a private advisory](https://github.com/Ville-Mattila/Kuvatin/security/advisories/new)
  on this repository ("Report a vulnerability" under the Security tab).
- **Or:** email ville.mattila@ensemble.fi with "Kuvatin security" in the
  subject.

Kuvatin is maintained by one person in their own time, so please allow a few
days for a first reply. A report that turns out to be a plain bug is welcome
either way — it will simply be moved to a public issue.

Useful things to include: the Kuvatin version (Settings, or the file version of
`kuvatin.exe`), the Windows version, what an attacker would need to be able to
do, and an input file or command line that shows the problem. The diagnostic log
at `%LOCALAPPDATA%\Kuvatin\kuvatin.log` often helps.

## Supported versions

The latest release is the supported one. Fixes ship in a new release rather than
as patches to old ones.

## What is in scope

Kuvatin is a desktop application with no server, no account and no telemetry. The
interesting boundaries are:

- **Untrusted input files.** Images, video and image sequences are parsed by the
  bundled decoders. A file that causes memory corruption, an unbounded
  allocation or code execution is in scope.
- **The Explorer context-menu handler**, which runs inside `explorer.exe`, and
  the command lines it builds from file names and preset names.
- **The installer**, which runs elevated: anything that lets a non-administrator
  influence what it installs or executes.
- **The update check**, which is off by default and, when on, sends one HTTPS
  HEAD request a day to GitHub's "latest release" address.

Out of scope: findings that require an attacker to already be an administrator
on the machine, anything about SmartScreen warnings on an unsigned installer
(known, documented in the README), and vulnerabilities in Windows itself.

## Dependencies

The dependency tree is checked on every CI run and weekly on a schedule
(`cargo deny check`, RUSTSEC advisories and licences). A bill of materials in
CycloneDX format ships with each release. If you are reporting an advisory in a
dependency, please say whether Kuvatin reaches the affected code — for many of
them it does not.
