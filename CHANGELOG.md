# Changelog

## Unreleased

## [0.4.1] - 2026-04-05

### Fixed
- Fixed Linux release binaries crashing on startup.

## [0.4.0] - 2026-04-05

### Major Changes
- Herdr now uses a Ghostty-backed terminal engine as its pane runtime.
- The legacy vt100 pane backend has been removed, making Ghostty the single terminal backend going forward.

### UX and Interaction
- Workspaces can now be reordered by dragging them in the sidebar.
- Notification sounds now support custom mp3 file overrides, with either one shared file or separate files for finished vs needs-attention alerts.

### API and Integration
- Workspace API ids are now stable, making socket and CLI automation more predictable across workspace changes and restores.

### Packaging and Runtime
- macOS builds now statically link the vendored `libghostty-vt`, preserving the single-binary install and update flow.

## [0.3.2] - 2026-04-03

### Changed
- The global launcher now surfaces update-related actions more clearly: when release notes are available you can open **What's New**, and when an update has been downloaded you can **quit to apply update** directly from the menu.
- Release notes are now retained as the latest available notes after you dismiss the startup modal, so you can reopen them later from the UI instead of only seeing them once.

### Fixed
- Fixed held-key repeat in terminal panes on macOS terminals that send explicit repeat events through the enhanced keyboard protocol, restoring continuous backspace, character, and arrow-key repeat without letting modal close/confirm key repeats leak into the shell.

## [0.3.1] - 2026-04-03

### Added
- New tabs now open directly into the rename flow, with the default tab name prefilled and replaced on first type so you can name tabs as you create them.

### Changed
- Polished modal layout and spacing across onboarding, settings, keybind help, and release notes so overlays feel more consistent and their content/actions line up more cleanly.
- Debug builds now use separate runtime/config paths from normal releases, which avoids local development sessions colliding with your main herdr install.

### Fixed
- Starting a second herdr instance against an active socket now fails fast with a clear error instead of clobbering the running session.
- Fixed pane and agent state updates being dropped under internal event queue pressure, which could leave a pane showing stale status after work finished.
- Fixed onboarding modal sizing and click targets, and corrected release-notes scroll calculations when a scrollbar is present.

## [0.3.0] - 2026-04-03

### Major Changes
- Added tabs within workspaces, so a single workspace can now hold multiple terminal tab contexts with their own pane layouts.
- Added first-class tab support to the local socket API and CLI wrappers, including `herdr tab ...` commands and tab ids like `1:2` alongside workspace-scoped pane ids.
- Added built-in direct integrations for pi, claude code, codex, and opencode, plus authoritative hook-driven state reporting so supported agents can report semantic state directly instead of relying only on screen heuristics.
- Added a post-update release-notes screen so herdr can explain what changed after an update is installed.

### UX and Controls
- Added optional direct pane-focus keybindings for terminal mode, so you can switch panes with modifier shortcuts like `alt+h` or `alt+right` without entering navigate mode first.
- Reworked keybind discoverability so the in-app keybind help now shows all supported actions, including optional bindings that are currently unset.
- Keybind help now uses a centered scrollable modal with mouse and keyboard scrolling, matching the release-notes interaction model more closely.
- Popups and action-button interactions now use more consistent modal geometry and button semantics across the UI.
- Polished the sidebar agent section so it focuses on detected agents only and uses clearer two-line agent cards with more breathing room.

### Behavior Fixes
- Hook-driven agent state updates now stay correct in tabbed workspaces.
- Modifier-only keypresses no longer leak into panes as stray input.
- Multi-tab agent labels now include tab names when that extra context matters.
- Workspace identity now follows the first tab's root pane again instead of stale creation-time cwd.
- Background notification suppression is now tab-aware rather than workspace-wide, so background tabs in the current workspace can still alert correctly.

### Documentation
- Updated the README, configuration guide, integrations guide, skill, and socket API docs to reflect tabs, direct integrations, unset optional keybindings, direct terminal-mode navigation examples, workspace-scoped pane ids, and the current workspace identity/sidebar model.

## [0.2.4] - 2026-04-01

### Fixed
- Fixed a macOS-only startup misdetection where pi could briefly appear as codex in the sidebar because process environment entries were being parsed as command-line arguments.

## [0.2.3] - 2026-03-31

### Changed
- Mouse wheel handling now follows the tmux/Ghostty model more closely: fullscreen apps receive wheel input when they own scrolling, while herdr keeps host scrollback for panes that are behaving like a normal terminal transcript.
- Pane scrollbars now only appear when herdr has real host scrollback for that pane, instead of implying a host-managed scroll position for app-owned scrolling.

### Fixed
- Fixed Codex and pi panes becoming unscrollable in herdr by preserving recoverable host history for top-anchored normal-screen output, without relying on alternate-screen scrollback retention.
- Fixed pane wheel routing so apps using mouse reporting or alternate-scroll behavior can receive scroll input directly instead of having herdr always intercept it.

## [0.2.2] - 2026-03-31

### Fixed
- Fixed pane scrollbars so they reserve their own lane instead of drawing over terminal content, which makes scrolling and scrollbar dragging behave more cleanly in narrow panes.
- Fixed alternate-screen scrollback handling so full-screen terminal apps can preserve recoverable history inside herdr panes instead of losing rows that scroll off.
- Fixed Codex in herdr panes losing transcript/history while running in alternate screen, so past output remains scrollable instead of disappearing as the session grows.
- Hid the rendered terminal cursor while a pane is scrolled back, avoiding stray cursor blocks appearing in the wrong place during history navigation.

## [0.2.1] - 2026-03-31

### Added
- Herdr now checks for updates at startup and periodically while it stays open, so long-running sessions can still discover new releases without a restart cycle.
- Added a lightweight bottom-right toast when an update has been downloaded and is ready, with a simple restart-to-use-it flow.

### Changed
- Rendering is now driven more directly by app events instead of relying as much on polling, which makes the UI feel snappier and cuts unnecessary redraw work.

### Fixed
- Restored smooth fast spinner animation for working agents.
- Closing a pane or workspace now reliably terminates the processes running inside that pane session instead of leaving shells or child processes behind.
- Fixed bracketed paste handling so incomplete paste sequences are preserved across read timeouts instead of being dropped or misread.

## [0.2.0] - 2026-03-30

### Added
- Added a local Unix socket API for controlling running herdr sessions, including workspace and pane management, pane reads, text/key input, pane splitting, and output waits.
- Added event subscriptions over the socket API for workspace and pane lifecycle events, pane output matches, and agent state changes.
- Added CLI wrappers on top of the socket API with `herdr workspace ...`, `herdr pane ...`, and `herdr wait ...`, using compact public ids like `1` and `1-2` for scripting and agent orchestration.
- Added a settings popup with mouse support for changing themes, sound alerts, and toast notifications from inside herdr.
- Added 9 built-in themes: catppuccin, tokyo night, dracula, nord, gruvbox, one dark, solarized, kanagawa, and rosé pine.
- Added interactive pane scrollbars, manual sidebar resizing, and upstream git ahead/behind indicators in the workspace sidebar.

### Changed
- Redesigned the sidebar into a two-section layout that separates workspace-level triage from per-agent detail, making it easier to supervise multiple agents in parallel.
- Agent state names exposed in the UI and integration surfaces now use `working` and `blocked`.
- Herdr now blocks nested launches by default when started inside a herdr-managed pane; set `advanced.allow_nested = true` to opt back in.

### Fixed
- Improved terminal keyboard protocol parsing and input forwarding across terminal variants, including better handling for shifted printable keys.
- Fixed Ghostty on macOS misparsing some arrow-key and modifier/enhanced key sequences.
- Refined sidebar rollups and pane ordering so workspace status and agent lists stay more stable and predictable.

### Documentation
- Refreshed the README, socket API reference, and reusable agent skill docs to better explain herdr's agent multiplexer model and integration surface.

## [0.1.2] - 2026-03-28

### Added
- Added first-run onboarding flow that lets you choose notification preferences (sound and toast) on startup.
- Added optional visual toast notifications in the top-right corner for background workspace events (completion and attention-needed alerts).
- Added configurable keybindings for all navigate mode actions: new workspace, rename workspace, close workspace, resize mode, and toggle sidebar. See `CONFIGURATION.md` for the full key reference.
- Added configuration validation with startup diagnostics. Invalid key combinations or duplicate bindings now fall back to safe defaults with a visible warning.

### Changed
- **Breaking:** Default prefix key changed from `ctrl+s` to `ctrl+b` to avoid common terminal flow control conflicts.
- Workspaces now derive their identity from the repository or folder of their root pane, updating automatically as you navigate. Custom names act as overrides rather than static labels.
- Sidebar now shows workspace numbers again in expanded view.
- Refined sidebar presentation with consistent marker/name/state ordering and comma-separated agent summaries.
- Keybinding parser now accepts special keys (`enter`, `esc`, `tab`, `backspace`, `space`) and function keys (`f1`–`f12`).

### Documentation
- Split configuration reference into a dedicated `CONFIGURATION.md` file with full keybinding documentation and config diagnostics explanation.

## [0.1.1] - 2026-03-28

### Added
- Added optional sound notifications for agent state changes, including a completion chime when background work finishes and an alert when an agent needs input.
- Added per-agent sound overrides under `[ui.sound.agents]`, so you can mute or enable notifications by agent instead of using one global setting. Droid notifications are muted by default.

### Changed
- Request alerts now play even when the agent is in the active workspace, while completion sounds remain limited to background workspaces.

### Fixed
- Improved foreground job detection on Linux and macOS so herdr can recognize agents that run through wrapper processes or generic runtimes, including cases like Codex running under `node`.
- Made Claude Code state detection more stable by handling more spinner variants and smoothing short busy/idle flicker during screen updates.

## [0.1.0] - 2026-03-27

### Added
- Initial release.
