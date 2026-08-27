# Rollback and maintenance pause

1. Obtain the previous known-good digest or unique build tag from the successful release's
   Actions summary and release-digest artifact. Keep deployment history and persistent-data
   backups separately. Do not assume an older image can read newer data/schema formats.
2. For a container, set the deployment's image to `ghcr.io/OWNER/IMAGE@sha256:DIGEST`
   (or its immutable `sha-SHA-rRUN-ATTEMPT` tag), then explicitly pull and recreate the service.
   Never use `docker compose down -v` during rollback and never delete good registry tags.
3. For ZebraTamer, stop the service, back up its config/data, download a known-good executable
   for the correct CPU from an immutable release, verify its SHA256, replace the executable,
   and restart/check the service. The installer uses stable releases, not main prereleases.
4. To stop promotions, disable Container Release / Binary Release in Actions. Pin consumers
   to a known-good immutable reference. Do not replace an immutable release with new bytes.
5. To pause update merges, restore the final bootstrap guard with automerge false and disable
   auto-merge on any already queued PRs. Keep alerts and read-only scans running.
6. Fix the source/lock in a PR and run all gates. A rerun receives a new run-attempt image tag;
   version releases require a new version rather than overwriting an existing release.
7. A security exception needs explicit owner approval, exact CVE/package, reason and expiry,
   a tracking issue and a narrowly tested gate change. No exceptions ship by default.

Scheduled rebuilds are best effort. Check Actions history and notification delivery.
A successful publish does not establish that any production host has pulled or restarted.
