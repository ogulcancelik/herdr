---
name: herdr-pre-release-audit
description: Audit herdr release readiness by comparing commits since the base release against next-release changelog and docs. Use when asked to run or apply the repo's pre-release audit, validate docs/next before release, inspect issue refs that release CI will close, or finalize release docs for herdr.
---

# Herdr Pre-release Audit

Use this skill only inside the herdr repository.

Read `references/pre-release-audit.md` and follow its workflow. Treat it as the source of truth for:

- choosing the release base ref
- inspecting first-parent history and merged PRs
- auditing `docs/next/CHANGELOG.md`
- auditing `docs/next/README.md` and staged website docs
- checking issue reference lines
- deciding when to run `just release-docs-check`
- producing the final release-readiness report

Do not edit files during the audit unless the user explicitly asks to apply fixes. When applying fixes, keep changes scoped to the files named in the reference workflow.
