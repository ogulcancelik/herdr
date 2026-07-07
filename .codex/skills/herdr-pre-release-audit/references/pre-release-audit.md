---
description: Audit next-release docs and changelog before release
---
Audit release readiness for this repo.

Optional starting ref override: `$1`
Extra user intent/context: `${@:2}`

Process:

1. Determine the base ref.
   - If `$1` is non-empty and looks like a ref/tag, use it.
   - Otherwise use the latest release tag, preferring the repo's semver tag style:
     ```bash
     git describe --tags --abbrev=0
     ```

2. Inspect the range from base ref to `HEAD`.
   - Use first-parent history for release context:
     ```bash
     git log --first-parent --reverse --format='%H%x09%s' <base>..HEAD
     ```
   - Also inspect full commits and commit bodies when needed:
     ```bash
     git log --reverse --format='%H%x09%s%n%b' <base>..HEAD
     ```

3. Detect merged PRs if any.
   - Look for first-parent subjects that indicate PR merges, including squash merges like `title (#123)`.
   - If GitHub CLI is available and the PR number is known, use it to fetch PR title/body for context.
   - Treat a merged PR as the primary release unit.
   - Do **not** also list individual commits that belong to that PR.

4. Handle direct commits separately.
   - Any commit in the range not represented by a merged PR should be considered on its own.

5. Infer what matters.
   - For each PR or direct commit, inspect changed files and diff stats.
   - Read the most relevant files in full when needed to understand user-facing impact.
   - Ignore pure housekeeping unless it has release value:
     - version bumps
     - release/tag commits
     - changelog-only commits
     - formatting-only changes
     - comment-only/doc-only changes unless they materially affect users

6. Audit `docs/next/CHANGELOG.md` and issue references.
   - Treat root `CHANGELOG.md` as the latest released changelog.
   - Treat `docs/next/CHANGELOG.md` as the next-release changelog.
   - Compare meaningful user-facing changes in the commit range against `docs/next/CHANGELOG.md`.
   - Flag missing entries for new features, bug fixes, removals, breaking changes, defaults, compatibility changes, user-visible command/config/API behavior, and security-relevant changes.
   - Do not require changelog entries solely for internal client/server protocol version bumps. Mention protocol only when the release intentionally changes user-facing compatibility guidance beyond the normal restart requirement.
   - Inspect commit bodies for issue reference lines in the form `refs #<issue-number>`.
   - Flag normal commits that use GitHub closing keywords like `fixes #<issue-number>`, `closes #<issue-number>`, or `resolves #<issue-number>`, because they close issues before release when they land on `master`.
   - For each shipped issue reference, check whether the changelog has a matching user-facing entry that mentions `#<issue-number>` when appropriate.
   - For each merged external human PR, check whether the changelog entry mentions the PR number and thanks the contributor in the existing style, e.g. `(#129, thanks @username)`. If the PR primarily ships an issue fix, include both the issue and PR numbers when useful, e.g. `(#128, #129, thanks @username)`. Do not add thanks text for maintainer-owned bots or automation accounts such as `kangal-bot` or `dependabot`.
   - Do not require or add GitHub closing keywords like `fixes #<issue-number>`, `closes #<issue-number>`, or `resolves #<issue-number>` to changelog entries or release notes.
   - List shipped issue references under `Issue references to close after release:` so the release operator can verify what release CI will close after the GitHub Release is published.
   - Flag stale entries that do not appear to correspond to shipped changes in the range.
   - Flag entries that are too implementation-focused or unclear for end users.
   - Preserve the existing changelog style and sections: `Added`, `Changed`, `Fixed`, `Removed`, and `Breaking Changes` when applicable.

7. Audit next-release public docs.
   - Treat root `README.md` and `website/src/content/docs/` as the latest released public docs.
   - Treat `docs/next/README.md` as the next-release root README, and `docs/next/website/src/content/docs/` as the full next-release mirror of website docs.
   - Compare meaningful user-facing changes in the range against next-release docs first.
   - Flag missing release docs for new or changed features, commands, config keys, protocol behavior, integrations, defaults, and compatibility notes.
   - Compare English next-release website docs against `docs/next/website/src/content/docs/ja/` and `docs/next/website/src/content/docs/zh-cn/`. Flag missing localized files, stale localized files, and heading-outline drift where translated docs do not have the same section structure as English.
   - Compare `docs/next/README.md` against root `README.md`, and compare the staged website-doc mirror against `website/src/content/docs/`. Flag each difference as intended to ship in this release, stale, or needing user decision.
   - Also audit example config snippets for release readiness.

8. Verify finalization state.
   - Before `just release`, approved `docs/next/README.md` must be copied to root `README.md`, approved staged website docs must be copied from `docs/next/website/src/content/docs/` to `website/src/content/docs/`, and the deleted root doc files must stay deleted.
   - If the release will change `Cargo.lock` or the package version, check `nix/package.nix` after the release version bump and refresh `cargoHash` before tagging. A stale hash fails both the `Nix` workflow and the release workflow's `flake-check` job with a fixed-output derivation mismatch. Use the `got:` hash printed by `nix flake check --print-build-logs` or CI, then rerun the Nix check when available.
   - Run or recommend:
     ```bash
     just release-docs-check
     ```
   - This check must include root `README.md`, root `CHANGELOG.md`, the removed root doc files, exact 1:1 sync between `docs/next/website/src/content/docs/` and `website/src/content/docs/`, and localized heading-outline parity between English, Japanese, and Simplified Chinese docs.
   - Do not run `just release` unless the working tree is clean and the docs check passes.

9. Apply changes only when asked.
   - Do not edit files during the audit unless the user explicitly asks you to apply fixes.
   - When asked to apply audit fixes, update `docs/next/CHANGELOG.md`, `docs/next/README.md`, and any required staged website docs under `docs/next/website/src/content/docs/`.
   - When asked to finalize release docs, copy approved next-release README and changelog into root, copy approved staged website docs into `website/src/content/docs/`, then run `just release-docs-check`.

Output format:

```md
Release readiness: READY | NOT READY

Base: <base ref>
Range: <base ref>..HEAD
Meaningful shipped changes: yes | no

Changelog: OK | MISSING ENTRIES | NEEDS ATTENTION
Missing:
- <only user-facing shipped changes missing from docs/next/CHANGELOG.md>

Docs: OK | MISSING | INACCURATE | NEEDS DECISION
Missing:
- <only required next-release public docs gaps>

Wrong or questionable:
- <docs that disagree with implementation, if any>

Issue refs: OK | NEEDS ATTENTION
Will close after release:
- #<issue>

Accepted/no action:
- <items the user explicitly accepted, such as known closing-keyword commits>

Root docs finalized: YES | NO
<result of just release-docs-check or why it was not run>

Nix cargoHash: OK | NEEDS UPDATE | NOT CHECKED
<result of nix flake check or the hash-refresh status>

Required before release:
1. <short action>
```

Keep the main output glanceable. Put commit inventories, excluded housekeeping, and commands run in an appendix only when they materially help the operator.

If the range has no meaningful user-facing changes, say that plainly instead of forcing entries.
