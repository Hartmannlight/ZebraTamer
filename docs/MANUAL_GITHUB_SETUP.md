# Owner setup required before autonomous merges

1. Install the Renovate GitHub App for Hartmannlight/ZebraTamer. Confirm onboarding, permission
   to open PRs and access to vulnerability alerts. Keep Dependency graph and Dependabot Alerts
   enabled under Settings → Advanced Security / Code security. Keep overlapping Dependabot
   version/security PR automation disabled, or use the documented fallback in the policy.
2. Settings → Rules → Rulesets → New branch ruleset: target `main`, enforcement Active.
   Require PRs, require up-to-date branches, block force pushes and branch deletion.
   After the initial successful runs, require checks `quality-and-binary-gate` and `dependency-review`.
   Check exact context names in the PR UI. Do not add untested CodeQL as a required check yet.
   Avoid blanket administrator bypass. Do not require human reviews for every bot PR if
   tested automatic patch merges are wanted.
3. Create a temporary PR with a deliberately failing test. Verify GitHub refuses its merge.
   Close that PR without merging. This verification has NOT been performed automatically.
4. Settings → General → Pull Requests → Allow auto-merge. Then remove only the final
   BOOTSTRAP SAFETY GUARD rule in renovate.json in a checked PR. Enable vulnerabilityAlerts
   automerge and lockFileMaintenance automerge only after their checks are verified.
   Major, 0.x and prerelease rules remain disabled. Confirm one passing Renovate PR merges
   and a failing PR remains open; review the first update PR before broad unattended use.
5. Settings → Actions → General: default GITHUB_TOKEN read-only, restrict actions as desired,
   and keep fork PR secrets disabled. The release job alone requests write/OIDC permissions.
   Do not expose a privileged home-lab runner to untrusted pull requests.
6. Enable private vulnerability reporting, dependency graph, secret scanning and push
   protection where available. Review CodeQL's first run and triage its alerts.
7. For container repositories, open the GHCR package settings: choose visibility intentionally,
   grant the repository Actions access if needed, and test pulling as the intended consumer.
   Retain exact release tags, recent immutable builds, current aliases and rollback digests.
   No registry password or PAT is needed; publishing uses GITHUB_TOKEN.
8. Enable GitHub Actions failure notifications. No additional secrets are required. If a
   failure webhook is later introduced, store its URL as FAILURE_WEBHOOK_URL, never in source.
9. On deployment hosts, maintain OS unattended security updates, reboot monitoring, Docker
   runtime updates and persistent-data backups. Publishing here does not perform deployment.

The repository files implement gates and releases, but they cannot prove the Renovate App,
package visibility or required-branch checks were enabled. Record verification dates here.
