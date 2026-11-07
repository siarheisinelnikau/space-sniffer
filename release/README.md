# Release from any computer

Open [Actions → Release from GitHub](https://github.com/siarheisinelnikau/space-sniffer/actions/workflows/release-from-github.yml), choose **Run workflow** and select **main**. No local build toolchain or local GitHub CLI login is needed.

Leave **Publish a NEW version** unchecked to build, test and stage a candidate only. The `release-candidate` Actions artifact expires after 14 days. Check the option only for a prepared new version: update the source version and add reviewed English notes at `release/notes/VERSION.md` in the same commit. Publication rejects existing releases and drafts, conflicting version tags and versions older than any published stable release. Tags and historical downloads are never overwritten. An interrupted upload leaves a draft for inspection; remove only that confirmed unpublished draft before retrying, or prepare a new version.

The build workflow reuses the project's checked build commands on GitHub-hosted runners. Its build and staging jobs have read-only repository access; only the final publication job has `contents: write`. Only artifacts from the same workflow run are used. Package metadata, executable architecture, supplied build checksums, required companion files and license notices are checked before staging. GitHub's uploaded sizes and SHA-256 digests are checked again before making the complete draft public. Action dependencies are pinned to inspected full commit SHAs and checkout does not retain credentials.

This setup does not introduce a project signing key or Windows Authenticode signing. `SHA256SUMS.txt` verifies bytes but is not a cryptographic publisher signature. Existing signing identities in other projects remain separate. No personal access token or signing key belongs in source, release files or Actions artifacts. See [GitHub's secure-use guidance](https://docs.github.com/en/actions/reference/security/secure-use).

The original CI and publication workflows remain available. Changes to the release tooling or notes on `main` automatically run this path in build-only mode; they never publish a release. Actual publication remains an explicit manual action. A successful candidate run verifies CI packaging, not interactive installation or hardware compatibility.

At setup, `main` declares 0.6.1 while the latest stable release is 0.6.4. Reconcile the intended newer release source into `main` before preparing another version. Publication from the stale version is deliberately refused.
