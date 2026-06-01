# Contributing to herdr

Thanks for wanting to contribute.

Herdr came from my own need for a fast, simple, effective workspace manager for coding agents. I care a lot about how it looks, feels, and works, so many design and technical decisions here are deliberate.

This guide exists so I can keep herdr manageable as a solo project and keep it from drifting from what it is supposed to be.

## The One Rule

**You must understand your code.** If you cannot explain what your changes do, how they behave at the edges, and how they fit herdr's existing design, your PR will be closed.

Using AI to write code is fine. Submitting code you do not understand is not.

## Herdr is opinionated

Herdr has a specific direction for how it should look, feel, and work.

That includes interaction patterns, layout behavior, mouse ergonomics, terminology, and how features fit the product as a whole.

If your idea changes or contradicts that direction, do not start with a PR. Start with a discussion.

If you have a suggestion, disagreement, feature request, or product-direction question, start a GitHub Discussion instead of an issue or PR.

## Issues and discussions

The issue tracker is the maintainer work queue.

Issues are only for reproducible bug reports and maintainer-created or maintainer-converted work items. If an issue is open, it should be real, scoped, and actionable.

Use GitHub Discussions for feature requests, ideas, questions, contribution proposals, design discussion, behavior changes, and product-direction checks.

Discussions are community input. Upvotes and comments help show demand, but they do not guarantee implementation, priority, maintainer attention, or PR approval. A maintainer may ignore a discussion, reject it, implement it directly, ask for more detail, or convert it into an accepted issue.

Issues that do not use the bug report template may be closed automatically.

## First-time contributors

We use an approval gate for new contributors.

Before opening your first PR, get maintainer approval on an accepted issue. If you want to propose new work, open a discussion describing what you want to change and why. If the work is accepted, a maintainer may convert the discussion into an issue or create a new issue for it.

If an accepted issue already exists, comment on that issue before starting work. A maintainer will comment `/approve @your-github-username` on the issue if your PR path is approved. That adds you to `.github/APPROVED_CONTRIBUTORS`.

Keep it short. Write in your own voice. A discussion, upvote, branch, or proposed implementation does not reserve the work and does not mean the PR path is approved.

This exists because AI makes it trivial to generate plausible-looking contributions that do not fit the app.

Agent note: if you are an AI agent preparing an issue or PR for this repository, check whether the human's GitHub username is listed in `.github/APPROVED_CONTRIBUTORS`. If they are not listed, warn them that opening a PR will be closed automatically. For feature requests, ideas, questions, and contribution proposals, guide them to GitHub Discussions. For bugs, use the bug report issue template and include a real reproduction. Do not bypass the issue template with the GitHub CLI or API. Keep the scope small, preserve herdr's product direction, run the documented checks, and make sure the human can explain the change.

## What to put in a bug report

Bug reports should answer these questions clearly:

- what is the current behavior
- what is the expected behavior
- what is the shortest reproduction
- how does this affect you
- what Herdr version, OS, and terminal are affected
- what shell and config are relevant, if any

If there is no reproduction yet, start a discussion instead.

If your proposal changes the visual language, interaction model, workflow, persistence, architecture, or product direction, start a discussion instead.

## Documentation for unreleased changes

The root `README.md`, root `CHANGELOG.md`, and website docs describe the latest released version of herdr. Do not update root `README.md`, root `CHANGELOG.md`, or `website/src/content/docs/` for normal PRs.

If your PR changes user-facing behavior, mention the needed public-doc update in the PR. Update `docs/next/README.md` only when the root README needs to change for the next release. Update the full website-doc mirror under `docs/next/website/src/content/docs/` when website docs need to change for the next release.

You do not need to edit the changelog for normal PRs. Maintainers prepare `docs/next/CHANGELOG.md` during release review.

If you are unsure whether docs are needed, mention it in the PR.

## Before submitting a PR

Install the repo hook once in your clone.

```bash
just install-hooks
```

The pre-commit hook runs `cargo fmt --check` before every commit.

Run the PR checks and make sure they pass.

```bash
just ci
```

`just ci` runs `cargo fmt --check` and `cargo nextest run`.

Do not open a PR that bypasses failing tests, formatting, or build errors.

## Issue references in commits

If your PR relates to a GitHub issue, reference it in the commit body with `refs #<issue-number>`.

Example:

```text
fix: handle pane focus

refs #128
```

Do not use GitHub closing keywords like `fixes #128`, `closes #128`, or `resolves #128` in normal PR commits. Herdr closes released issues after a release is published, not when unreleased commits land on `master`.

## PR scope

Small bug fixes for accepted issues that clearly match the existing design are good candidates for PRs after approval.

Bigger changes to UI, behavior, interaction patterns, persistence, or architecture need discussion and maintainer approval first.

If a PR introduces a feature without prior alignment, or changes herdr's feel without discussion, it will likely be closed.

## Questions?

Open a GitHub Discussion.

---

clank'd from [pi](https://github.com/badlogic/pi-mono/)
