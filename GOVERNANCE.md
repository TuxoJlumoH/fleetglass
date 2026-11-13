# Fleetglass Governance

## Our promise to users
These commitments are why Fleetglass exists. They are not features that can be traded away.

1. **Apache-2.0, forever.** The project will not be relicensed. There is no "open core", no paid
   tier that removes limits, and no source-available edition.
2. **No CLA.** Contributors sign off commits under the [DCO](https://developercertificate.org/)
   and keep their copyright. Because there is no copyright assignment, no single party can relicense
   the code.
3. **No login, no account.** The app works fully without registering anywhere.
4. **No telemetry.** No analytics, crash reporting or usage tracking. The only network connections
   are to the user's own Kubernetes API servers and an opt-in update check.
5. **No AI in the core.** Any AI integration (e.g. a read-only MCP server) is optional, off by
   default, and outside the core.
6. **No cluster-size or node limits.**
7. **Auditable releases.** Releases are built in public CI, signed, and ship with SHA256 checksums
   and an SBOM.

Changing any of these requires a public RFC, a 30-day comment period and unanimous approval of the
maintainers. Points 1 and 2 cannot be changed at all: without a CLA there is no legal path to
relicense other people's contributions.

## Roles
- **Users and contributors.** Anyone who files issues, reviews, writes docs or code. Contributions
  are welcome from everyone, subject to the [Code of Conduct](CODE_OF_CONDUCT.md).
- **Reviewers.** Regular contributors trusted to review and triage in an area (`core`, `app`, `ui`,
  `bench`, packaging). Nominated by a maintainer, approved by lazy consensus of the maintainers.
- **Maintainers.** Have merge rights and release rights. Listed in `MAINTAINERS.md` (created once
  there is more than one maintainer).

## Maintainers model
Solo projects in this space have died (Seabird, OpenLens). To reduce the bus factor:
- The goal is **at least two maintainers with merge rights** by the 12-month mark, and co-maintainers
  are recruited from Phase 1 (see [docs/PLAN.md](docs/PLAN.md) §6 and §7).
- New maintainers are nominated by an existing maintainer after sustained, quality contributions
  and approved by a majority of current maintainers.
- No single company may hold a majority of maintainer seats once there are three or more.
- Inactive maintainers (no activity for 6 months) move to emeritus status and can return on request.
- Changes need at least one approving review from a maintainer other than the author once two or
  more maintainers exist.

## Decisions
- Day-to-day decisions use **lazy consensus** on the issue or PR.
- Architecture decisions are recorded as ADRs in `docs/adr/`.
- UI redesigns, new major features outside [PLAN §1](docs/PLAN.md) and changes to this document go
  through an **RFC** (an issue labelled `rfc`, open for at least 14 days).
- If consensus fails, maintainers vote; a simple majority decides (except for the user promise above).

## Working in public
- Public roadmap (GitHub Projects) and task board in [docs/TASKS.md](docs/TASKS.md).
- Weekly changelog; community chat on Discord or Matrix.
- Benchmarks are published with a reproducible script (kwok), never as marketing numbers.
- We do not astroturf. Let real users talk about the project.
- We aim to respond to new issues and PRs within a few days.

## Code of conduct and security
See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) and [SECURITY.md](SECURITY.md).
