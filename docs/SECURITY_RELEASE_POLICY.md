# Security and release policy

Owner: @Hartmannlight. Reviewed: 2026-08-28. Scope: Linux executable, current main / 0.x train.

## Update ownership and rollout

Renovate owns version, base-image digest, Action SHA and vulnerability-fix PRs.
Dependabot Alerts stays enabled; overlapping Dependabot version/security PRs stay disabled.
Renovate App installation and vulnerability-alert access must be verified by the owner.
If alert access is unavailable, disable Renovate vulnerabilityAlerts and enable Dependabot
security-update PRs instead. Never run both security PR systems for the same ecosystem.

**Automerge is intentionally blocked by the final BOOTSTRAP SAFETY GUARD in renovate.json.**
No protected branches were present during discovery. Remove the guard only after the owner
completes MANUAL_GITHUB_SETUP.md, including the deliberately failing PR test.
Security-alert automerge and lock maintenance also remain off until explicitly enabled then.

| Update | Policy after verified activation |
| --- | --- |
| Stable runtime patch/minor | PR auto-merge after required checks and 14 days |
| Fixed vulnerability | Immediate PR; no release-age delay; checks still mandatory |
| Base digest / Action SHA | PR auto-merge after required checks |
| Major, 0.x, prerelease | Human review, never broad auto-merge |
| Lockfile maintenance | Separate PR; checks and dependency review required |
| New direct dependencies | Human decision |

During bootstrap all rows are PR-only. Platform PR auto-merge never bypasses branch rules.
SDK-revision changes in Studio are reviewable, pinned source changes, not floating build inputs.

## Gates

Required contexts after their first successful runs: `quality-and-binary-gate` and `dependency-review`.
The source CI validates committed locks and runs the existing test suite. Container repos
additionally smoke-test and scan the exact candidate on native AMD64 and ARM64 runners.
Node projects run type checking through their builds; Rust uses Clippy and compiler checks.
Python projects currently have syntax checks and tests but no established static-type or
coverage threshold. No coverage percentage or whole-program type guarantee is claimed.
PrintHub's external Labelary tests remain optional; offline/API/registry tests are required.
CodeQL runs for Python and TypeScript; Rust uses Clippy plus the lockfile vulnerability scan.
CodeQL is advisory until its initial run and alert triage are verified.

## Scans and exceptions

Trivy scans image OS/language packages, source locks, secrets and Docker configuration.
Image scans apply to container repositories; library and binary repositories scan sources.
`scripts/security_gate.py` blocks **all fixable High/Critical findings**, stricter than only
blocking newly introduced findings. High/Critical secret and configuration findings also block.
Unpatched High/Critical findings remain visible in full reports and the gate summary and
require owner triage; they are not hidden by --ignore-unfixed. A failing scanner fails CI.
Debian Bookworm images currently report unpatched OS findings, including Critical findings.
A passing gate is not a claim that the image is vulnerability-free. Reports retain all
severities; secret matches and source snippets are removed from uploaded reports.
There are no approved vulnerability exceptions. Any proposed exception needs a CVE, reason,
owner, expiry and reviewed implementation; do not add a broad ignore or disable the gate.

## Build, tags and evidence

Push to main creates an immutable prerelease build-SHA-rRUN-ATTEMPT. Exact vMAJOR.MINOR.PATCH tags create immutable stable releases. No agent container is published. ARM64/AMD64 run native PTY integration tests on Debian Bookworm; ARMv7 is cross-compiled and is not runtime-tested.

Docker base images use tag+digest. Runtime dependencies come from the committed lock.
Tool versions and third-party Actions are pinned; base digest changes remain tracked PRs.
Weekly container rebuilds may refresh explicitly listed OS packages, never apt-get upgrade.
Image SHA tags include run attempt so repeated builds of identical source remain distinct.
GitHub attestations link provenance and SPDX SBOMs to immutable published artifacts.
Failed tests/scans/attestations never move floating tags; a failed multi-stage publish can
leave immutable partial artifacts which must not be treated as a promoted release.

GitHub Actions notifications and the Renovate dashboard are the default failure channels.
No webhook or production credentials are required. Publication does not deploy containers,
update a Pi service or patch a host kernel. latest means last validated, not guaranteed secure.
Maintain host updates, backups and controlled rollouts separately; see ROLLBACK.md.
