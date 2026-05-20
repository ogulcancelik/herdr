# Changelog

## Unreleased

### Fixed
- `herdr --remote` now offers to restart the remote server after installing or replacing a remote binary, or when the running server version differs, even if the client/server protocol is still compatible.

## [0.6.0] - 2026-05-20

### Added
- Added keybinding v2 with explicit `prefix+...` syntax, array bindings per action, configurable prefix-mode pane focus, tab switching, and direct modified chords for users who opt in. (#154, #201, #202, #219)
- Added `herdr config reset-keys` to back up `config.toml` and remove custom keybindings so built-in v2 defaults apply on restart or config reload. (#154)
- Added an integrations tab in settings and first-run onboarding so users can install recommended agent integrations from inside Herdr.
- Added update badges on the sidebar menu, settings menu item, and integrations settings tab when installed integrations are outdated.
- Added `terminal.default_shell` to choose the executable used for new interactive panes. When unset, Herdr still falls back to `$SHELL`, then `/bin/sh`. (#196)
- Added native Kiro CLI detection with idle and working state heuristics. (#185)
- Added `experimental.reveal_hidden_cursor_for_cjk_ime = false` (opt-in), `experimental.cjk_ime_agents = []` (optional allow-list), and `experimental.cjk_ime_cursor_shape = "steady_block"` to expose the focused pane's cursor anchor to the outer terminal even when the pane requested `?25l`, restoring macOS IME candidate-window tracking for TUIs that paint their own cursor (Claude Code, pi, codex). When `cjk_ime_agents` is non-empty, the reveal applies only to focused panes whose detected agent matches one of the listed names. When the pane reports no cursor position, the anchor falls back to the pane's top-left so a stable IME hint is always available. Trade-off when enabled: an extra hardware cursor may appear in the outer terminal for apps that hide the cursor without painting a replacement. (#149)

### Fixed
- Keybinding conflict warnings now stay visible and show one readable yellow row per conflicting binding.
- Update prompts that need to stop a running server now default Enter to yes and show `[Y/n]`.
- Pending release notes no longer open automatically on startup; the latest notes remain available from the menu.
- Running `herdr server` directly now prints socket and log paths and explains that normal TUI users should run `herdr`.
- Kitty graphics virtual Unicode placeholders now render image placements instead of leaving placeholder cells behind. (#136)
- Clipboard image reads are now capped to Herdr's image payload limit, preventing oversized local clipboard images from being read into memory.
- The install script now reads Herdr's public latest-release manifest, so fresh installs use the same binary URLs as `herdr update`.
- The Claude Code integration no longer lets subagent completion hooks report durable `working`, preventing delayed recap or subagent completion events from reviving an idle pane. (#198)
- Remote clients now bridge local clipboard images into the remote pane by staging them as temporary image files and pasting the remote path, so Claude Code image paste works over `herdr --remote`. (#205)

### Breaking Changes
- Removed the separate `keys.quit` binding. Use `keys.detach`, which detaches in server mode and exits in `--no-session` mode. The default detach binding is now `prefix+q`.
- Keybindings now use explicit trigger syntax: `prefix+c` means prefix mode, while `ctrl+alt+c` is direct. Bare printable direct bindings such as `new_tab = "c"` are rejected with diagnostics because they intercept normal typing. The default keymap now gives tmux-style tab actions to `prefix+c`, `prefix+n`/`prefix+p`, and `prefix+1..9`, uses `prefix+w` for workspace navigation, and moves pane focus to `prefix+h/j/k/l`. (#154)
- The client/server protocol is now version 8. Stop and restart any running v0.5.12 server before attaching with this release.

## [0.5.12] - 2026-05-19

### Fixed
- The Claude Code integration no longer reports successful or failed post-tool hooks as `working`, and installing the updated integration removes Herdr's deprecated post-tool hook entries from existing Claude settings. (#198)
- The Codex integration now reports native `PermissionRequest` hooks as `blocked`, so permission prompts no longer stay pinned as `working` after a tool-use hook. (#198)
- Workspace and tab rename prompts now handle Backspace, Ctrl+Backspace, Alt+Backspace, Cmd+Backspace, Ctrl+H, Ctrl+W, and Ctrl+U as editing shortcuts instead of inserting stray characters or clearing unexpectedly. (#204)

## [0.5.11] - 2026-05-19

### Added
- Added the `terminal` built-in theme, which uses the host terminal's ANSI palette for Herdr UI colors. (#140, #146, thanks @babymastodon)
- Added Hermes Agent foreground-process detection with basic idle, working, and blocked heuristics. (#144)
- Added a Hermes Agent plugin integration for direct state reporting. (#144)
- Added `ui.sidebar_min_width` and `ui.sidebar_max_width` to configure the sidebar's expanded resize bounds. Defaults remain 18 and 36 columns; existing configs are unchanged. (#132, #135, thanks @ChihGodlee)

### Fixed
- Running the internal `herdr client` command from inside Herdr now respects the nested-launch guard, and the command is no longer advertised in root help. (#187)
- The Herdr agent skill now refuses to claim pane ownership unless it is running inside Herdr. (#152)
- Terminal-style docs code blocks now keep their copy button in the top-right corner. (#190)
- The sidebar `new` workspace button now aligns with the sidebar's left padding. (#189)
- Herdr now preserves `session.json` symlinks when saving persistent session state. (#139, #147, thanks @cloudmanic)
- Alt+Backspace is now preserved when forwarded into panes. (#155, #165)
- Directional pane focus now works while a tab is zoomed. (#151, #167)
- Agent detection now prefers the foreground process group leader, reducing false matches from child helper processes. (#161, #172)
- Remote attach now uses a matching `herdr` already available on the remote `PATH` before installing a new copy. (#170)
- Modified Enter input such as Shift+Enter is now preserved in supported terminals. (#168)
- Sidebar agent entries now show user-assigned agent names when available. (#145)

### Breaking Changes
- The client/server protocol is now version 7. Stop and restart any running v0.5.10 server before attaching with this release.

## [0.5.10] - 2026-05-17

### Added
- Added indexed keybind families under `[keys.indexed]` for jumping directly to workspace, tab, or visible agent positions 1-9.
- Added hook-owned custom agent status labels, so integrations can show short visual states like `indexing` without changing semantic agent status.
- Added terminal-backed agent commands and socket API methods for listing, reading, sending to, renaming, focusing, waiting on, attaching to, and starting agent terminals.
- Added direct terminal attach with `herdr agent attach <target>` and `herdr terminal attach <terminal_id>`.
- Added `ui.prompt_new_tab_name = false` for creating new tabs immediately with generated names instead of opening the rename dialog. (#123)
- Added optional `keys.edit_scrollback` to open the focused pane's retained scrollback in `$EDITOR` inside a temporary zoomed pane. (#122)

### Changed
- Renamed the focused pane fullscreen keybinding to `keys.zoom`; `keys.fullscreen` remains supported as a legacy alias.

### Fixed
- Grok Build is now detected as `grok`, with basic working, blocked, and idle state detection. Conflicting known-agent hook labels are ignored once native foreground-process detection identifies a different known agent. (#133)
- Terminal cursor shapes now forward through attached clients. (#116)
- Herdr now redraws immediately when the outer terminal regains focus.
- GitHub Copilot is now correctly detected when its process name is `copilot`. (#118)
- Integration installs now respect `PI_CODING_AGENT_DIR`, `CLAUDE_CONFIG_DIR`, and `CODEX_HOME` when choosing Pi, Claude Code, and Codex config paths. (#121)
- Split pane resize hit areas no longer overlap the first content column or row, making text selection work from the start of right and bottom panes. (#120)
- Dragging text selections near pane edges now autoscrolls into scrollback, and selection state now clears correctly when switching workspaces, tabs, or panes. (#128, #129, thanks @leeeanh)
- Zoomed panes now keep their border visible in tabs that contain multiple panes. (#115)

## [0.5.9] - 2026-05-15

### Added
- Added experimental Kitty graphics rendering for local panes and attached clients behind `experimental.kitty_graphics`, including support for larger graphics frames.
- Added `ui.toast.delivery = "system"` for OS-level background notifications, using `notify-send` on Linux and `terminal-notifier` or `osascript` on macOS.
- Added light variants for Catppuccin, Tokyo Night, Gruvbox, One, Solarized, Kanagawa, and Rosé Pine themes.
- Added `ui.mouse_capture = false` for tmux-style mouse behavior, letting the terminal handle normal clicks while still forwarding mouse input to pane apps that request it.

### Changed
- Moved experimental settings into `[experimental]`.

### Fixed
- PageUp and PageDown now scroll Herdr pane scrollback for normal panes while still forwarding keys to full-screen or mouse-reporting apps.
- Enhanced tilde key sequences now parse correctly, improving compatibility with terminals that emit them.
- `herdr integration install codex` now enables the current Codex `[features] hooks = true` flag and migrates the deprecated top-level `codex_hooks` flag.

### Breaking Changes
- `advanced.allow_nested` has moved to `experimental.allow_nested`; update configs that allow nested Herdr launches.
- The client/server protocol is now version 5. Stop and restart any running v0.5.8 server before attaching with this release.

## [0.5.8] - 2026-05-12

### Added
- Added manual pane labels through `herdr pane rename`, the `pane.rename` socket API, an optional `keys.rename_pane` binding, and the right-click pane menu.
- Added `ui.show_agent_labels_on_pane_borders`, which can show detected or reported agent names in split pane borders when no manual pane label is set.
- Added `herdr integration status [--outdated-only]` so installed agent integrations can be checked for legacy or outdated versions.
- Added an optional `keys.open_notification_target` binding for jumping to the pane behind the current notification.
- Added optional `keys.previous_agent` and `keys.next_agent` bindings for cycling through sidebar agent entries.

### Changed
- Scrolling over the tab bar now switches tabs directly, including overflowing tab bars.

### Fixed
- Indexed terminal palette colors now render correctly for 256-color terminal apps.
- Hook-based agent integrations now reject stale out-of-order reports and base notifications on effective agent state, reducing duplicate or stuck state changes.
- Background tabs now resize when the outer terminal size changes, preventing stale pane dimensions when switching back to them.
- Client shutdown now drains queued control messages more reliably.
- Pane cursors are now hidden while scrolled back, and omitted while the mobile switcher is open.
- Mobile agent switcher entries now include tab context, making agents easier to identify on narrow terminals.
- macOS foreground job detection now uses process groups, improving agent state tracking for foreground commands.
- Remote SSH no longer fails before connecting when macOS temporary bridge socket paths exceed Unix socket length limits. (#103, thanks @moonsphere)
- Nix-wrapped agent commands are now detected by their underlying agent entrypoint.
- Pane renames made through the socket API now rerender immediately.

## [0.5.7] - 2026-05-10

### Added
- Added ANSI-formatted pane reads to the CLI and socket API with `herdr pane read --format ansi` / `--ansi`, preserving colors and styles for visible and recent pane output.

### Changed
- The agents panel now highlights the currently focused agent entry, matching the active workspace styling. (#84, thanks @soomtong)

### Fixed
- Git branch and ahead/behind refreshes now run off the main loop, preventing slow Git status checks from freezing the UI.
- Update and startup flows now detect incompatible running servers earlier and give clear stop/restart guidance instead of trying to attach with a mismatched client/server protocol.
- `herdr update` now downloads and prepares the new binary before stopping a running server, reducing the chance of interrupting an active session when download or install preparation fails.

## [0.5.6] - 2026-05-09

### Added
- Added the `vesper` built-in theme. (#71, thanks @nexxeln)
- Added `herdr --remote <ssh-target>`, so you can use Herdr as a thin client for remote servers without SSHing in first. Herdr connects over SSH, bootstraps a matching remote `herdr` binary when needed, starts the remote server automatically, and streams an efficient terminal view back to your local terminal.

### Changed
- Updated the bundled `libghostty-vt` engine and removed the custom Linux C++ runtime link workaround from static builds.
- CLI workspace, tab, and pane creation now preserve the current focus by default; pass `--focus` to switch to the newly created item.

### Fixed
- OSC 8 hyperlinks emitted inside panes now remain clickable after Herdr renders them, including titled markdown-style links.
- Agent panel scope now defaults to `all` and is saved to config when changed, so choosing `current` or `all` survives session resets and upgrades.
- Native agent hook state now clears when the detected native agent exits, preventing stale hook-reported status from sticking to a pane.
- Clicking an in-app agent toast now jumps to the relevant pane and clears the toast after focus.

## [0.5.5] - 2026-05-06

### Added
- Added a mobile layout for narrow terminals, making it practical to SSH into your machine and run herdr from your phone.

### Fixed
- Non-ASCII terminal input is no longer dropped when UTF-8 characters arrive split across multiple reads.
- Native agent detection now clears agents after their foreground process exits and control returns to the shell, preventing stale agent status in the sidebar.
- Pane contents no longer shift horizontally when scrollback appears, keeping the scrollbar gutter stable.

## [0.5.4] - 2026-05-03

### Fixed
- Visible active-tab panes that finish while the outer terminal is unfocused are now marked as seen when you return to herdr, preventing stale done/attention indicators.
- IME candidate windows and mobile SSH cursor tracking now stay anchored to the focused pane during client redraws, including apps that hide the cursor, instead of drifting to sidebar or repaint positions.

## [0.5.3] - 2026-04-30

### Added
- Added named persistent sessions, so you can keep separate herdr environments for different projects or contexts while sharing the same global config. See the docs for the full session CLI. (#57, thanks @fbettag)
- Added `herdr status`, `herdr status server`, and `herdr status client` to inspect the local client, running server, protocol compatibility, socket path, and whether a restart is needed.

### Changed
- Focused panes can now still alert you through terminal notifications when the herdr terminal window is unfocused, so active work does not go quiet just because you switched to another app.

### Fixed
- Dragging pane split borders now works when the app inside the pane has mouse reporting enabled, including Claude Code no-flicker mode. (#61, thanks @EYH0602)
- Pressing the prefix key twice now forwards a literal prefix key into the focused pane in client mode again.
- `herdr integration install` and `herdr integration uninstall` now work without requiring a running herdr server.
- Pane PTYs now keep their last attached size while detached, preventing detached output from being resized or rewrapped to fallback dimensions.

## [0.5.2] - 2026-04-27

### Added
- Config can now be reloaded in the running app/server from the global menu or with `herdr server reload-config`, applying safe live settings without restarting the persistent server.

### Fixed
- Persistent server startup now surfaces config diagnostics in attached clients instead of silently hiding parse or validation errors.
- Pane backgrounds now stay transparent when the host terminal background color is unknown, while explicit terminal cell backgrounds still render correctly.
- Persistent-session toast and sound notifications now target the foreground attached client instead of firing across every connected client.
- Claude Code subagent hook events no longer make the parent Claude pane look idle or released when a subagent finishes, and permissioned tool-call completion keeps the pane in the correct working state.

## [0.5.1] - 2026-04-25

### Added
- Toast notifications can now be delivered through the outer terminal as desktop notifications. Configure this with `ui.toast.delivery = "terminal"`; see the [configuration docs](https://herdr.dev/docs/configuration/) for details.
- Herdr now writes separate capped support logs for app, client, and server modes, making persistent-session issue reports easier to diagnose without unbounded log growth.
- The bundled opencode plugin now reports question prompts as blocked while waiting for user input, then returns to working or idle when answered or dismissed. Question prompts are also detected by the default terminal-screen heuristics. (#51, thanks @mspiegel31)

### Changed
- Routine API request traces now log at debug level by default, making normal support logs smaller and easier to read while preserving detailed traces when debug logging is enabled.

### Fixed
- Pasted text and other reverse-video terminal content now stays readable when pane backgrounds are transparent. (#45, thanks @EYH0602)
- Panes now advertise a stable `TERM=xterm-256color` and `COLORTERM=truecolor` by default, improving redraw and cursor behavior in shells and remote sessions.
- Pane scrollbars once again reserve their own rightmost column instead of overlaying terminal content in persistent session mode.
- Terminal-delivered toast notifications now use the server-approved delivery decision in persistent session mode, so attaching clients do not incorrectly suppress them.
- In-app toast delivery now stays inside herdr instead of also forwarding a terminal/desktop notification.

## [0.5.0] - 2026-04-21

### Breaking Changes Please Read
- herdr now defaults to a persistent server/client session model. running `herdr` starts or reattaches to a background session server instead of launching the old single-process UI.
- quitting the UI in default mode now detaches the current client and leaves the shared session running. use `herdr server stop` to stop the background server explicitly.
- the old monolithic behavior is still available as an escape hatch with `herdr --no-session`.

### Added
- Persistent sessions are now the default product behavior. You can detach and reattach without stopping pane processes.
- Added the thin client and headless server as first-class product components, including auto-detect launch, explicit `herdr client`, and `herdr server stop`.
- Sessions now restore cleanly after full restart, preserving workspaces, tabs, panes, and running process state.
- Multi-client attach is now supported. Multiple clients can connect to the same shared session.

### Changed
- In persistence mode, in-app quit actions now detach the current client by default instead of shutting down the whole background server.
- The current persistence model is a shared session view across attached clients. It is not yet full tmux-style per-client independent navigation.
- Restored sessions now land in terminal mode, while fresh sessions still start in navigate mode.

## [0.4.11] - 2026-04-16

### Breaking Changes Please Read
- The update flow changes in `0.4.11`. Herdr no longer installs updates silently in the background. Starting with this release, herdr only checks for updates and shows them in the UI. To install a new release, quit herdr and then run `herdr update` manually in your shell.
- This prepares the upcoming `0.5.0` persistence release. Herdr is moving from the old single-binary update model toward a persistent server/client session model, so your workspace can keep running while clients attach, detach, and reconnect.
- The reason for this change is upgrade safety. Herdr needs to stop the old running process cleanly before the new client/server model takes over, so manual update avoids mixed-version states during the transition.

### Added
- Hook-reported agent state can now use custom agent labels, so integrations are no longer limited to herdr’s built-in agent names. Custom labels now flow through pane/workspace UI and the socket API anywhere agent names are shown.

## [0.4.10] - 2026-04-14

### Added
- Prefix mode now supports custom command keybindings via `[[keys.command]]`, so you can launch detached shell helpers or open temporary overlay panes from inside herdr using the active workspace, tab, pane, and cwd context.
- Pressing the prefix key twice now forwards a literal prefix keystroke into the focused pane, which makes nested tools and terminal apps that use the same prefix easier to control.

### Fixed
- App-level key handling now normalizes enhanced keyboard reporting consistently, so shifted bindings and text like `?` and uppercase characters work correctly in navigate mode and text-entry UI.
- Ctrl+letter input is now encoded correctly when pane apps enable kitty keyboard mode, improving compatibility with terminal programs that expect CSI-u style key reporting.
- The collapsed sidebar now keeps the active workspace visibly highlighted even while you stay in terminal mode.
- Droid Mission Control screens are now treated as idle instead of active work, reducing false busy-state detection.

## [0.4.9] - 2026-04-13

### Fixed
- Droid's primary-screen redraws no longer erase pane scrollback inside herdr, while normal scrollback-clear behavior is preserved elsewhere.
- `q` is now dedicated to quitting in navigate mode instead of also acting as a generic cancel key in modals and overlays, reducing accidental quits.
- Tab bar scrolling is tighter: the scroll-right button and new-tab button now sit directly adjacent to the last visible tab without a gap, and manual scroll no longer overscrolls past the last tab.

## [0.4.8] - 2026-04-12

### Added
- Themes can now set `panel_bg = "reset"` to let herdr’s panel chrome inherit the host terminal background instead of painting an opaque panel fill. This also accepts the aliases `default`, `none`, and `transparent`.
- Ghostty-backed panes now preserve the host terminal’s default background when it matches the outer terminal theme, so terminal window transparency can show through pane content instead of being repainted as an opaque color.

### Fixed
- Clipboard writes now prefer native platform clipboard tools (`pbcopy`, `wl-copy`, `xclip`, or `xsel`) before falling back to OSC 52, which makes copy operations from panes more reliable across terminal setups.

## [0.4.7] - 2026-04-10

### Added
- The tab bar now handles large tab sets better: you can scroll overflowing tabs with the mouse controls or wheel, and reorder tabs by dragging them.
- `workspace create` and `tab create` now return the created root pane in their JSON response, so automation can act on the new pane immediately without an extra lookup.

### Fixed
- Background panes that start idle no longer show up as `done` or trigger finished-state attention until they have actually transitioned from working or blocked to idle.
- Left-click now focuses panes and right-click now opens the pane context menu even when the inner TUI has mouse reporting enabled, fixing apps like Claude Code. (#25, thanks @othavioquiliao)
- OSC 52 clipboard writes from apps running inside panes now reach the host clipboard correctly, including copy requests emitted by child processes inside the pane.
- `pane close` now removes only the targeted tab when other tabs still exist in the workspace, instead of closing the whole workspace.
- Amp approval prompts are now detected more reliably as blocked, including tool-call, command, and file edit/create approval screens.

### Breaking Changes
- Socket API clients that match `result.type` exactly need to handle `workspace_created` and `tab_created` for `workspace.create` and `tab.create`; these calls no longer return `workspace_info` and `tab_info`.

## [0.4.6] - 2026-04-09

### Fixed
- Agent state detection is now more reliable when panes are scrolled back, when Codex is running in narrow panes, and when Claude opens slash-command or settings menus, reducing false blocked or idle states.
- Mouse-driven terminal text selection now autoscrolls into pane scrollback and clears cleanly after copy, so selecting beyond the visible viewport works as expected.
- Pane terminal colors now return to the outer terminal theme after fullscreen TUIs exit, fixing cases like Droid leaving stale background colors behind. This restore path now also works correctly on macOS.

## [0.4.5] - 2026-04-09

### Added
- `herdr workspace create` and `herdr tab create` now support `--label`, so scripts and agents can name new workspaces and tabs immediately instead of creating them first and renaming them afterward.
- The global menu now includes a manual **reload keybinds** action, so you can apply `config.toml` keybinding changes without restarting herdr.
- The socket API and CLI now expose a `done` agent status, including `herdr wait agent-status --status done`, so automation can distinguish finished agent runs from panes that are merely idle.

### Changed
- Session state is now saved automatically with a debounce while you work, so recent workspace, tab, pane, and sidebar changes are preserved more reliably even if herdr exits unexpectedly.

### Fixed
- Only the focused pane now owns the terminal cursor, which removes stray cursor blocks from unfocused panes.
- In-app **What's New** / release notes now render inline code spans and fenced code blocks correctly.
- Default numbered tabs now stay auto-named when you keep or rename them back to their numeric label, so generated tab numbering stays compact and predictable.

## [0.4.4] - 2026-04-08

### Changed
- The expanded sidebar can now be split into resizable workspace and agent sections with a draggable divider, and that section sizing is preserved across restarts.

### Fixed
- IME input now works properly for Chinese and other UTF-8 input methods in pane terminals, so candidate selection no longer falls back to typing raw digit keys. (#9, thanks @Edmund-a7)
- `herdr pane run ...` now uses the bracketed-paste-aware input path, improving compatibility with shells and terminal apps that expect pasted command text to arrive atomically.
- The local socket API is more robust and secure: its Unix socket is now restricted to the current user, and long-running output waits and subscriptions stop cleanly on disconnect or shutdown instead of hanging indefinitely.

## [0.4.3] - 2026-04-07

### Fixed
- Update checks and in-app **What's New** release notes no longer depend on GitHub’s release API, which avoids the transient 403 failures from the previous update path.
- `herdr pane run ...` now submits the full command atomically in one request, fixing cases where scripted commands did not reliably execute because the final Enter was sent separately.
- Bare line-feed input is now preserved in raw terminal input instead of being normalized to Enter, fixing Linux terminal cases where inputs like Shift+Enter or Ctrl+J could be interpreted incorrectly.

## [0.4.2] - 2026-04-07

### Added
- The expanded sidebar agent panel can now switch between the current workspace and all workspaces, so you can scan and jump to agents across the whole session.
- The collapsed sidebar now shows compact per-pane agent indicators, so you can keep an eye on agent activity without reopening the full sidebar.

### Changed
- The sidebar now handles larger workspace sets more cleanly: the workspace section has headers, its own scrolling, better-aligned drag/drop slots, and manual width changes persist across restarts. Double-clicking the divider resets it to the configured default width.
- Pane scrollback is now configured with `advanced.scrollback_limit_bytes`, matching Ghostty's byte-based scrollback limit. Set it to `0` to disable pane scrollback entirely. The old `advanced.scrollback_lines` key is still accepted as an alias, but it now uses the same byte-based value.
- Linux release binaries now ship with libghostty SIMD enabled again without reintroducing the musl startup issue, restoring the optimized Linux build path.

### Fixed
- Typing in pane terminals on macOS is responsive again after the Ghostty migration, by keeping a persistent per-pane Ghostty key encoder instead of rebuilding it on every keypress.
- The collapsed sidebar expand toggle works again.
- Creating a new tab now waits until you confirm the dialog, so cancelling the new-tab flow no longer leaves behind an unwanted tab.
- Copying selected pane text now uses Ghostty's native selection extraction, which preserves wrapped text and wide characters more accurately.
- Session restore is more tolerant of older and current snapshot formats, including pre-tab session files.

## [0.4.1] - 2026-04-06

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
- Added configurable keybindings for all navigate mode actions: new workspace, rename workspace, close workspace, resize mode, and toggle sidebar. See the [configuration docs](https://herdr.dev/docs/configuration/) for the full key reference.
- Added configuration validation with startup diagnostics. Invalid key combinations or duplicate bindings now fall back to safe defaults with a visible warning.

### Changed
- **Breaking:** Default prefix key changed from `ctrl+s` to `ctrl+b` to avoid common terminal flow control conflicts.
- Workspaces now derive their identity from the repository or folder of their root pane, updating automatically as you navigate. Custom names act as overrides rather than static labels.
- Sidebar now shows workspace numbers again in expanded view.
- Refined sidebar presentation with consistent marker/name/state ordering and comma-separated agent summaries.
- Keybinding parser now accepts special keys (`enter`, `esc`, `tab`, `backspace`, `space`) and function keys (`f1`–`f12`).

### Documentation
- Split configuration reference into dedicated configuration docs with full keybinding documentation and config diagnostics explanation.

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
