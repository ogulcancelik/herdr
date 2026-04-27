# herdr

Terminal workspace manager for AI coding agents. Rust + ratatui.

## Principles

- **State is separated from runtime.** `AppState` is pure data, testable without PTYs or async. `PaneState` is separate from `PaneRuntime`. Workspace logic doesn't need real terminals.
- **Render is pure.** `compute_view()` handles geometry and mutations. `render()` takes `&AppState` and only draws. Never mutate state during render.
- **No god objects.** If a module is doing too many things, split it. `app/` is already split into state, actions, and input. Keep it that way.
- **Platform code is isolated.** OS-specific behavior lives in `src/platform/`. Core modules don't have `#[cfg(target_os)]`.
- **Detection is decoupled.** The detector reads a screen snapshot, never touches the parser or viewport state.
- **UI patterns should be reused.** Herdr is a mouse-first TUI. New dialogs, onboarding, settings, and post-update flows should follow the existing UI/UX language and interaction patterns instead of inventing one-off screens. Prefer reusing existing modal/screen structure, affordances, and close actions so the app feels consistent.

## Multi-agent isolation

Read-only investigation can happen in the shared checkout.

Small changes or small tasks are fine in the default main worktree. If you find unrelated implementation changes already in progress in the main worktree, use a dedicated worktree instead. Use a dedicated worktree for bigger features too.

Use this layout:

- shared integration checkout: `../herdr`
- task worktrees: `../herdr-worktrees/<task-slug>`
- task branches: `issue/<id>-<slug>` when an issue exists

Do all code edits, tests, and validation inside the task worktree.

Commit on the task branch in that worktree.

When the change is ready, fast-forward the shared checkout at `../herdr` to the task branch commit, then push `origin/master` from `../herdr`. Do not treat the task branch as the final landing branch.

If the current session is already inside an isolated task worktree, keep using it. Do not create nested worktrees.

Before committing, propose the commit message and get alignment.

After the change is integrated, remove the task worktree and delete the task branch locally and remotely.

## Testing

```bash
just test               # cargo nextest + maintenance script tests
just check              # formatting check + cargo nextest + maintenance script tests
```

Default flow: run `just check` before committing.

Unit tests live next to the code (`#[cfg(test)] mod tests`). If you add behavior to `AppState` or `Workspace`, it should be testable with `AppState::test_new()` and `Workspace::test_new()` — no PTYs.

## Conventions

- Conventional commits, lowercase, no emojis.
- Do not edit `CHANGELOG.md` during normal feature or fix work. Changelog entries are drafted and applied immediately before release.
- When a commit fully fixes a GitHub issue, include `fixes #<issue-number>` in the commit body so GitHub closes it on merge.
- Rust: no `unwrap()` in production code. `tracing` for logging. `#[allow]` only with a comment explaining why.
- Don't bypass checks. If tests fail, fix them before committing.
- Don't add dependencies without a reason. Check if the existing deps cover it first.

## Releases

Before cutting a release, draft the upcoming notes under `## Unreleased` in `CHANGELOG.md`. The release script promotes that section into the versioned entry.

Default release flow:

```bash
just check
just release 0.x.y
# wait for the GitHub release and all four binary assets to exist
just update-latest-json 0.x.y
just verify-release-state 0.x.y
```

`just release 0.x.y` prepares the changelog entry, bumps `Cargo.toml`, runs tests, commits, tags, and pushes. GitHub Actions builds the binaries after the tag is pushed.

`just update-latest-json 0.x.y` is the post-release website step. It uses `gh release view` against `ogulcancelik/herdr`, verifies the published release exists, rejects draft or prerelease releases, requires a non-empty release body, requires these four assets to exist, and only then rewrites `website/latest.json`:

- `herdr-linux-x86_64`
- `herdr-linux-aarch64`
- `herdr-macos-x86_64`
- `herdr-macos-aarch64`

It also refuses to run if `website/latest.json` is already at the same or newer version. It leaves the file unstaged and prints the next commands to review, commit, and push.

`just verify-release-state 0.x.y` is the final read-only verification step. It checks that the published GitHub release, the local `website/latest.json`, the live `https://herdr.dev/latest.json`, and the release asset URLs all match the same version and manifest contents.

`website/latest.json` is the shipped updater source of truth. Keep its schema aligned with `src/update.rs`:

```json
{
  "version": "0.x.y",
  "notes": "### ...",
  "assets": {
    "linux-x86_64": "...",
    "linux-aarch64": "...",
    "macos-x86_64": "...",
    "macos-aarch64": "..."
  }
}
```

The app update check and the in-app **What's New** flow both depend on that exact manifest shape.
