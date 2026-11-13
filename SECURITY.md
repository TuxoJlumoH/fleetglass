# Security Policy

Fleetglass holds credentials to your Kubernetes clusters, so we take security reports seriously.

## Reporting a vulnerability
**Do not open a public issue.** Report privately via
[GitHub Security Advisories](https://github.com/TuxoJlumoH/fleetglass/security/advisories/new)
("Report a vulnerability" on the repository's Security tab).

Please include the affected version, platform, steps to reproduce and impact. Never send real
kubeconfigs, tokens or certificates; redact them.

## What to expect
- Acknowledgement within 3 working days.
- An initial assessment within 10 working days.
- A fix and a coordinated disclosure date agreed with you; we aim for 90 days or less.
- Credit in the advisory and changelog, if you want it.

## Supported versions
Fleetglass is a preview (0.x). Only the latest release receives security fixes.

## Scope
In scope: the desktop app (`crates/app`, `ui/`), the core library (`crates/core`), release artifacts
and packaging, and the update mechanism. Examples: credential leaks (kubeconfig, tokens, exec plugin
output), IPC commands reachable from untrusted content, unsafe handling of cluster data in the
webview (XSS from object names/annotations/logs), bypass of read-only or protected-context checks,
update signature bypass.

Out of scope: vulnerabilities in Kubernetes itself or in your clusters' configuration.

## Our design commitments
No telemetry and no network calls except to your API servers (an opt-in update check is planned,
not built yet). Releases ship with SHA256 checksums and an SBOM; installer code signing is planned. See [GOVERNANCE.md](GOVERNANCE.md).
