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

If your idea changes or contradicts that direction, do not start with a PR. Start with an issue.

If you have a suggestion, disagreement, or contradiction with the current product direction, that belongs in an issue first.

## First-time contributors

We use an approval gate for new contributors.

Before opening your first PR, open an issue describing what you want to change and why. Keep it short. Write in your own voice. If you intend to implement it yourself, add the `intends-to-pr` label. A maintainer will comment `lgtm` if the direction is approved. Once approved, you can open PRs.

This exists because AI makes it trivial to generate plausible-looking contributions that do not fit the app.

## What to put in a first issue

Your first issue should answer these questions clearly:

- what is the current behavior
- what do you want to change
- why this change belongs in herdr
- whether this changes UI, interaction, or workflow expectations
- whether you intend to implement it yourself

If your proposal changes the visual language, interaction model, or overall product direction, say that directly. That is exactly what issues are for.

If your issue does not make the direction clear, it will likely be closed.

If you plan to implement the change yourself, say that directly in the issue and use the `intends-to-pr` label. That signals intent. It is not approval.

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

## PR scope

Small bug fixes that clearly match the existing design are good candidates for PRs after approval.

Bigger changes to UI, behavior, interaction patterns, persistence, or architecture need issue discussion first.

If a PR introduces a feature without prior alignment, or changes herdr's feel without discussion, it will likely be closed.

## Questions?

Open an issue first.

---

clank'd from [pi](https://github.com/badlogic/pi-mono/)
