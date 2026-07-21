# Changelog

## Unreleased

### Added
- Added a live-agent CLI facade with named `start`, atomic `prompt`, logical `send-keys`, and server-owned `wait` workflows. Agent startup targets an existing pane without changing topology, validates the requested interactive agent kind and strict agent name, and accepts native arguments after `--`.
- Added transient declarative Agent view queries through `agent.view.set/clear`; filtered and sorted views now define sidebar, mobile, mouse, and agent-keybind navigation order.
- Added one-shot plugin `[[startup]]` hooks for restoring plugin-owned state after server startup and live handoff.
- Added `ui.sidebar_start_collapsed` to launch Herdr with the sidebar collapsed. (#1463)
- Added macOS support for the `HERDR_AGENT=<agent>` foreground-process hint, allowing agents hidden behind host-visible wrappers such as `nono` to use the named agent's screen manifest. (#679)

### Changed
- Agent commands now accept only a unique live agent name or the pane ID currently hosting that agent. Names are cleared when the occupant exits, is released, or is replaced. The old top-level `wait` commands were replaced by `agent wait` and `pane wait-output`, and `agent send` was replaced by `agent send-keys`.

### Fixed
- Linux sound notifications now terminate and reap audio players that do not exit, preventing unavailable audio from leaving CPU-bound `mpg123` processes behind. (#1622)
- Agent prompt waits now report `agent_prompt_stalled` after five seconds without an observed state change instead of waiting indefinitely after an ineffective submission.
- `herdr config check` now reports unknown config keys with their full paths instead of treating ignored typos as valid configuration. (#1573)
- Codex panes with customized static terminal titles now fall back to the live working footer instead of remaining idle, while OSC activity remains preferred. (#1563)
- The Pi, OMP, OpenCode, and Kilo Code integrations can now be installed on Windows and report lifecycle state and native session identity through Herdr's named-pipe API. (#1531)
- Named agent prompts now honor live bracketed-paste mode before sending Enter, preserving OpenCode text such as `A != B` instead of triggering shell mode. (#1525)
- New panes, tabs, layouts, and workspaces using `new_cwd = "follow"` now inherit the foreground process-group leader's working directory instead of an unrelated helper process directory. (#1472)
- Noninteractive update, plugin, integration, sound, custom-command, and Git subprocesses no longer flash console windows on Windows. (#1468)
- Live handoff now preserves installed plugins and no longer lets the next plugin installation overwrite the existing registry. (#893)
- `herdr wait agent-status` now returns `pane_not_found` promptly when its target pane closes instead of waiting for the full timeout. (#1439)

## [0.7.4] - 2026-07-15

### Added
- Added session-modal popup floating terminal panes for `type = "popup"` custom command keybindings and plugin panes, with optional cell or percentage sizing and no changes to the tiled tab layout. (#1125)
- Added `ui.copy_on_select` to disable automatic clipboard copying after mouse selection while keeping the selection visible.
- Added configurable row layouts and per-occurrence foreground, bold, and dim styles for expanded Space and Agent sidebar entries, including built-in display tokens, per-agent overrides, custom metadata tokens, and pane/workspace metadata reporting through the CLI and socket API.
- Added independent `row_gap` settings for expanded Space and Agent sidebar entries.
- Copy mode now supports literal smart-case search with `/` and `?`, repeating with `n` and `N`, match highlighting, and tmux-style cross-line `w`/`b`/`e` word motions. (#1230)
- Added Maki agent support. (#1301, #1302, thanks @tontinton)
- Added a searchable, version-matched configuration reference and a troubleshooting guide covering duplicate terminal key events, modified-arrow shell bindings, updates, remote access, and logs. (#1116, #1370)

### Changed
- Expanded Space and Agent sidebar entries now use a packed layout by default; set the corresponding `row_gap` to `1` to restore the previous spacing.
- Refreshed the bundled Herdr agent skill for current public workspace, tab, and pane ids and the current CLI/API workflow. (#1297)
- Expanded Japanese and Simplified Chinese CLI documentation with shell completion setup and API schema usage. (#1151)

### Fixed
- Collapsed Agent sidebar rows now follow the same ordering and click targets as the expanded panel, and their shortcut numbers are assigned by visible list position instead of repeating across workspaces. (#1168, #1344)
- Shifted indexed bindings such as `prefix+shift+1..9` now match terminals that report the corresponding punctuation characters. (#1184)
- Plugin-driven tab renames now immediately refresh tab-bar geometry and labels. (#1111, #1179, thanks @kovalov)
- New tabs, splits, layouts, and workspaces configured to follow the foreground directory now start from the focused pane's current working directory. (#1245)
- Amp, Codex, and Claude Code detection now recognizes current active-turn UI variants, including reordered Codex title spinners and Claude `/btw` turns. (#1208, #1281, #1366)
- Pi lifecycle state now reanchors after native session replacement, avoiding working panes that remain idle or tied to an abandoned session. (#943, #1189, thanks @dmmulroy)
- OMP lifecycle reports are now retried when startup races drop the first report. (#1310)
- WSL now uses Herdr's drawn cursor by default, matching the native Windows workaround for host cursor flicker. (#930)
- Live handoff now preserves explicit named-session socket paths, waits for slower server shutdowns, and flushes API responses before the old server exits. (#1180, thanks @dvic)
- The Windows installer no longer rewrites an existing config file or creates a duplicate onboarding line during first-run setup. (#1162)
- Config diagnostics now reach CLI-only and attached-client startup paths reliably and clearly identify fallback configuration behavior.
- Detached custom command children are now reaped after exit instead of accumulating zombie processes. (#1360)
- Renamed single tabs now remain visible in the Agents sidebar instead of losing their tab label. (#1369)
- Documentation search results are now scoped to the active locale and stable or preview channel.
- Horizontal wheel and trackpad events now reach pane applications that enable mouse reporting. (#1349)
- Copy mode `$` and End now stop at the final visible character on the row instead of jumping to the pane edge. (#1405)
- Split SGR mouse reports are now reassembled across input reads, and a preceding standalone Escape is preserved instead of being swallowed or leaked as mouse bytes. (#1334, #1382)
- Linux foreground-process discovery now stays within Herdr pane process trees instead of scanning unrelated host processes, reducing CPU use on busy multi-user systems. (#1399)
- Single-codepoint emoji chosen from the Windows emoji picker now reach panes when WezTerm's kitty keyboard support sends them as CSI-u events with associated text. (#1404)
- Outer-terminal focus gained and lost reports now reach the focused pane when its application enables focus reporting, restoring Neovim file autoreload and other focus-aware terminal behavior. (#1337)
- Native Windows servers now detach from the terminal console that launched them, so closing WezTerm, Windows Terminal, or another host terminal no longer stops persistent pane processes. (#1329)
- Windows API clients now remain connected while waiting for initial named-pipe request bytes, so `status server`, `api snapshot`, and other socket commands no longer intermittently fail with BrokenPipe. (#1279)
- `herdr --remote` now installs remote helper binaries without routing the binary stream through a multiline `/bin/sh -c` command, fixing installs for non-POSIX login shells such as xonsh. (#1203, thanks @nhumrich)

## [0.7.3] - 2026-07-08

### Fixed
- The session navigator now keeps the active search query when leaving and re-entering search focus, and its footer now shows shortcuts for the current input mode. (#1115, #1140, thanks @liby)
- Re-focusing an already-focused done agent or pane through the socket API now marks it seen instead of leaving stale done status in API responses.
- Windows foreground-process detection now ignores cyclic process-parent snapshots instead of growing memory until the server aborts. (#1083)
- Terminal redraws now hide the cursor inside synchronized output, reducing focused-pane cursor flicker during active redraws. (#967)
- Headless render streams no longer scan visible plain-text URLs during rendering, reducing redraw work while preserving OSC 8 hyperlink metadata.
- The workspace picker once again honors navigate-mode workspace up/down keys, including custom bindings, after `prefix+w`. (#1149)

## [0.7.2] - 2026-07-07

### Added
- Added MastraCode integration support with lifecycle state reports and native thread restore. (#337, #788, thanks @wardpeet)
- Added `ui.sidebar_collapsed_mode = "hidden"` to make a collapsed sidebar use zero width while keeping the existing compact rail as the default. (#842)
- Added `herdr completion <shell>` / `herdr completions <shell>` to generate shell completion scripts for bash, elvish, fish, PowerShell, and zsh. (#435)
- Added `session.snapshot` to bootstrap client runtime state in one socket API response before subscribing to events.
- Added `herdr api schema` to inspect the bundled socket API schema, with `--json` for the full JSON Schema document and `--output PATH` for file output.
- Added `layout.updated` socket events so protocol clients can keep tab layout snapshots current after pane split, resize, swap, move, zoom, and layout mutations.
- Added pane scroll metrics to pane socket API responses and `pane.scroll_changed` subscriptions for clients that need to show when a pane is scrolled back.
- Added `herdr terminal session observe` for read-only live ANSI terminal streams that bridge processes can consume as newline-delimited JSON.
- Added `herdr terminal session control` for bridge processes that need live ANSI frames plus input, resize, scroll, release, and takeover authority.
- Added `ui.hide_tab_bar_when_single_tab` to hide the tab row when a workspace has one tab. (#448)
- Added Japanese and Simplified Chinese website docs.

### Changed
- The mobile switcher now starts from an agents-first summary and renders worktrees as a tree, making narrow terminals easier to scan.
- macOS prefix input-source switching now runs on the foreground client, so non-Latin input sources are restored reliably after prefix mode. (#774, #1016, thanks @ppggff)
- Nix packaging now uses `xcbuild` instead of custom Apple SDK wrappers for Darwin builds. (#995, thanks @arunoruto)

### Fixed
- Windows clients now send shifted punctuation such as `!`, `?`, and `:` as literal text to Kitty-keyboard-mode pane apps, fixing Kiro CLI TUI prompts while preserving modified key chords. (#1066, #1105)
- Alt-Shift letter chords are now preserved instead of being collapsed into plain uppercase input. (#1088)
- Antigravity background-task waits are now detected even when the UI does not show a `/tasks` hint. (#755)
- `herdr --remote` now prints clean remote attach failures and SSH authentication guidance instead of Rust Debug-formatted I/O errors when SSH authentication is denied. (#1034)
- `herdr server stop` now stops Windows named-pipe servers instead of failing with `named pipes do not support I/O timeouts`. (#1113)
- `herdr server stop` now waits until both server sockets are unreachable before returning, avoiding an immediate first-start failure when restarting right after replacing the binary.
- macOS `herdr --remote` clients now bridge Finder-dropped image files to the remote pane instead of forwarding the local file path as typed text. (#828)
- Grok Build agent detection now tracks the current Grok Build UI: panes report working while responses, tools, and subagents run, and blocked on permission prompts and question dialogs, instead of falling back to idle mid-turn. (#1017, #1055, thanks @TonyxSun)
- GitHub Copilot CLI detection now recognizes the newer Esc interrupt prompt as working. (#1119, #1120, thanks @LaneBirmingham)
- Unix local Herdr clients no longer treat empty bracketed paste as a clipboard-image bridge; `herdr --remote` keeps using it for local-desktop image paste over SSH. (#986)
- Custom command keybindings now run through `cmd.exe /d /c` on Windows instead of `/bin/sh`, so `type = "pane"` and `type = "shell"` bindings can launch native Windows commands. (#1041)
- Plain PageUp/PageDown now reach primary-screen pager apps such as `less -X` and Git diff when they enter application cursor mode, while shell transcripts still use Herdr pane scrollback. (#953)
- Copy mode now supports Ctrl-page navigation, keeps the Herdr prefix key available while copying, and restores the copy context correctly after prefix commands. (#681, #885, #1092, thanks @reobin)
- `prefix+e` scrollback editor panes now open on Windows without trying to run `/bin/sh`; Windows uses `VISUAL`, then `EDITOR`, then `notepad.exe` as the fallback editor. (#914)
- `herdr pane split --current` now resolves to the calling Herdr pane instead of the UI-focused pane when run inside a pane. (#902)
- Native Windows clients running inside Alacritty now preserve mouse reports and `ctrl+j` input instead of leaking mouse escape sequences into panes. `shift+enter` remains dependent on whether the outer terminal reports it as a distinct modified Enter key. (#792)
- Windows clients now preserve bracketed paste, Backspace, modifier-only keys, host cursor drawing, native clipboard copies, recent pane reads, and wait connections across the native input path. (#670, #795, #907, #920, #930, #962, #963, #1067)
- New tabs and workspaces now follow the focused pane's current directory more reliably, including PowerShell panes that report cwd through prompt shell integration on Windows. (#912, #919)
- Pi and OMP integration state now survives internal session reloads, recovers after resumed sessions such as `omp -c`, and reports Ask/tool approval waits as blocked instead of leaving the pane working or stuck on the previous session. (#800, #879, #984, thanks @dmmulroy)
- Pi state socket reports are now retried, reducing stale sidebar state when the report races server startup. (#1049)
- OpenCode now reports subagent permission prompts as blocked and handles object-form `session.status` events. (#838, thanks @soar)
- Remote attach now discovers compatible Homebrew, mise, and Nix profile installs before offering to install a sidecar binary to `~/.local/bin/herdr`. (#840)
- `herdr --remote` sessions now keep the remote server in its own login-independent session and preserve compatible running servers after helper binary updates, so network drops should disconnect only the client instead of killing remote panes.
- `herdr --remote` now reuses one OpenSSH connection across setup probes, installs, server checks, and the final bridge when `[remote].manage_ssh_config` is enabled, so password-based hosts prompt once instead of once per setup command. (#888)
- Foreground agent session reports can now replace stale saved session references, so resumed panes do not stay tied to an older agent session. (#943)
- Kitty graphics panes now repaint streaming image updates reliably and delete replaced host images instead of leaking them. (#947, #948, thanks @DevSrSouza)
- Pane apps that query OSC 12 cursor color now receive a response. (#806)
- ANSI undercurl styles now render in panes. (#895)
- CJK pane border labels, compact keybinding help ranges, and active auto-named tabs now measure by display width, avoiding broken alignment and unreadable labels. (#799, #810, #817, #829)
- Ctrl+/ is now encoded as Ctrl+_, matching terminal expectations for pane apps. (#847)
- PowerShell panes now stay alive after agent Ctrl+C. (#860)
- SGR mouse reports no longer leak into pane input after host-side handling. (#939)
- Wrapped pane links now preserve their target instead of being truncated across soft-wrapped lines. (#1098)
- Linux foreground process-group scans are cached, reducing idle CPU in large sessions. (#936)
- Session autosaves now run off the main loop, reducing UI stalls in busy sessions.
- Worktree removal now focuses the parent workspace after closing the worktree workspace. (#1004)
- Closing a tab from the context menu now exits the menu cleanly. (#945)
- Copy feedback now stays visible above retained pane updates. (#555)
- Windows ARM64 installer fallback now works when the normal checksum path is unavailable. (#897)

## [0.7.1] - 2026-06-24

### Added
- Added `[update].version_check` and `[update].manifest_check` so background Herdr version checks and remote agent-detection manifest checks can be disabled independently. Manual `herdr update` and bundled/local detection manifests still work when the background checks are disabled. (#677)
- Added `HERDR_AGENT=<agent>` as a Linux foreground-process hint for agents hidden behind wrappers such as VMs, Bubblewrap, or `fence`, allowing Herdr to use the named agent's screen manifest when `/proc` cannot expose the real command. (#679)
- Added `ui.pane_borders` and `ui.pane_gaps` to make split pane dividers and spacing configurable. (#271)

### Changed
- Removed the Agents panel workspace/all filter. The panel now always shows all agents, defaults to grouped-by-space ordering, and can switch to priority ordering with `ui.agent_panel_sort = "priority"`. (#318)
- User keybindings now displace conflicting built-in defaults during config load, so overriding a default binding no longer leaves both actions attached to the same key. (#747)
- Worktree creation now checks out an existing local branch when the requested branch already exists instead of failing by trying to create it again. (#729)
- Worktree operations started through the socket API and plugin/UI flows now defer long-running Git work until the app runtime can drive it, keeping clients responsive and preserving plugin lifecycle events for worktree-created panes. (#657, #662, #686)
- OMP, OpenCode, Pi, Devin, and other official hook integrations now scope lifecycle and session reports to the intended root agent process more reliably, reducing stale or cross-process session adoption after restarts, nested commands, and new sessions. (#614, #712, #719, #765)

### Fixed
- Windows Terminal multiline text paste now reaches pane apps as one bracketed paste, so OMP, Pi, and similar prompts no longer submit each pasted line separately. Plain Esc, Shift+Enter, mouse, focus, resize, and Unicode paste handling are preserved on the Windows client path. (#670)
- Local Herdr clients no longer treat raw `Ctrl+V` as a clipboard-image paste trigger, so pane apps such as Vim and Neovim receive block-visual `Ctrl+V` even when the desktop clipboard contains an image. `herdr --remote` keeps `keys.remote_image_paste = "ctrl+v"` by default. (#647)
- Herdr now refreshes cached host terminal colors when terminals report a light/dark color-scheme change, so pane apps that query OSC 10/11 no longer need detach/attach to see updated default colors. Opt-in `[theme].auto_switch` can also switch Herdr's own UI between configured `dark_name` and `light_name` themes. (#675)
- Full-lifecycle hook agents can now recover when an old release/report sequence belongs to a previous agent generation. Herdr keeps process-exit validation active under lifecycle authority and re-anchors hook sequence guards after fresh session references or proven process exits. (#684)
- OMP now reports a native session reference, so an OMP pane reappears in the Agents panel after exiting and rerunning `omp` in the same pane, and Herdr can resume it with `omp --resume=<session>`. Previously the released lifecycle hook stayed suppressed until a server restart. (#614)
- Host terminal color query (OSC 10/11) replies that arrive split at their escape introducer no longer leak as text like `11;rgb:...` into the focused pane, most visible when launching agents that probe terminal colors on startup. (#549)
- Long CJK Git branch names in the sidebar now truncate by display width instead of overflowing or cutting at the wrong cell boundary. (#644)
- Temporary pane commands launched from API flows no longer steal focus from the previously focused pane after they finish. (#658)
- Root agent session restore now ignores child process reports that would otherwise overwrite the saved session for the owning pane. (#712)
- Kitty file-transfer media queries are now answered, allowing pane apps that rely on kitty graphics file support to detect image/file media capability correctly. (#732)
- Idle or slow clients no longer block server writes to other clients while the blocked client is waiting for output. (#726)
- GitHub Copilot CLI `ask_user` accept prompts are now detected as blocked so the Agents panel shows that the pane is waiting for input. (#725)
- Pane reads now skip wide-character spacer cells, avoiding duplicated or malformed output around double-width characters. (#698)
- Split pane border intersections now use the active pane color consistently. (#742, thanks @cullendotdev)
- The Windows installer checksum fallback no longer depends on `Get-FileHash`, improving compatibility with constrained PowerShell environments. (#751)
- Pi launched through npm wrappers on Windows is now detected as Pi instead of a generic wrapped process. (#754)
- Windows builds now force the system ConPTY path through a vendored `portable-pty` patch, avoiding the bundled-path startup failure seen in affected Windows environments. (#761)
- Key release events that fall back to encoded input no longer double-send text into pane apps. (#769)
- Remote clients now allow a longer initial handshake, improving `herdr --remote` startup over high-latency links. (#753)

## [0.7.0] - 2026-06-15

### Added
- Added local plugin v1 support with `plugin.link/list/unlink/enable/disable`, manifest-declared actions, event hooks, managed plugin panes, link handlers, command logs, keybinding integration, and authoring docs under Preview docs.
- Added `herdr plugin install <owner>/<repo>[/subdir...]`, `plugin uninstall`, source metadata in `plugin.list`, offline registry fallback, and a human-readable default `plugin list` with `--json` for scripts.
- Added `herdr plugin config-dir <id>` and automatic plugin config/state directory creation so plugin setup docs can point users at a stable config path.
- Added Devin CLI automatic detection plus `herdr integration install devin` hooks that report session ids for restore with `devin --resume <id>`. Devin state remains screen-detected because Devin hooks do not cover every permission cancellation and user interrupt transition. (#606, #622, thanks @minatoaquaMK2)
- Added supporting plugin host APIs for `pane.current`, `pane.process_info`, `client.window_title.set/clear`, `layout.export/apply`, plugin pane placement, plugin invocation context/env injection, and plugin pane ownership across `pane.move`.
- Added `pane.move` and `herdr pane move` to relocate a running pane into another tab, a new tab, or a new workspace without restarting its terminal process. (#299)
- Tabs containing a zoomed pane are now marked in the tab bar so the zoom state is visible from other tabs.

### Changed
- Bumped the client/server protocol version to 14 for `pane.move` compatibility. (#299)
- Public workspace, tab, and pane ids are now short stable handles such as `w1`, `w1:t1`, and `w1:p1`; closed tab and pane ids no longer retarget later resources. (#569)

### Fixed
- `pane.send_keys` and `pane.send_input.keys` now accept Herdr key-combo strings such as `ctrl+h`, `ctrl+j`, `ctrl+k`, and `ctrl+l`. (#613, thanks @dmmulroy)
- Config startup and reload now warn about unknown top-level table sections, including a `[toast]` hint that points to `[ui.toast]`, instead of silently ignoring them.
- Claude Code session restore now accepts real `/clear`, `/resume`, and compacted session identity changes while still ignoring nested `claude -p` startup sessions that inherit the pane environment. (#620)
- Auto-named tab labels now stay compact after closing, moving, or creating tabs while public tab ids remain stable.
- F1-F4 key presses sent as `ESC[11~` through `ESC[14~` now reach pane apps instead of being dropped. (#574)
- Numeric keypad keys sent through the kitty keyboard protocol now enter their digits and operators instead of being dropped. (#570)
- Pane resize keybindings now shrink panes again instead of only being able to grow them. (#562)
- Windows pane cursor rendering is now stable instead of showing a misplaced or flickering cursor. (#556)
- Tab identity is now preserved across restored sessions.
- Idle panes now poll their PTY less frequently, reducing CPU use while sessions are inactive.
- Captured pane URL clicks, including plugin link handlers, now use Ctrl-click on macOS too because captured terminal mouse reports do not expose Cmd-click separately from plain click. (#307)

## [0.6.10] - 2026-06-11

This is a hotfix release for v0.6.9. See the v0.6.9 notes for the full feature release.

### Fixed
- Lifecycle-authority agent integrations such as Pi and OpenCode no longer trigger a repeated detection reset loop that could flood logs, drive high CPU, and make the UI lag or stop responding. (#560, #565, thanks @dzevs)

## [0.6.9] - 2026-06-10

### Fixed
- Copy mode page scrolling now stops at the same top and bottom boundaries as normal pane scrolling instead of overshooting or getting stuck near the edges. (#459, #460, thanks @reobin)
- Clipboard-copy feedback no longer stays visible after the related selection state has gone stale. (#443)
- The session navigator now uses live workspace labels, so renamed workspaces and cwd-derived labels stay current while navigating. (#377)
- Hermes Agent integration installs now preserve flat plugin-list settings instead of rewriting them into nested lists. (#479)
- Host-terminal focus redraws now stay pending until the client can send them, so panes refresh after focus returns even when redraw delivery was briefly busy.
- Numeric keypad keys that send VT100 application-keypad escape sequences now enter their digits and operators instead of being dropped. (#493)
- Codex panes now stay marked working when the live status header uses reasoning-summary text such as `Investigating code output` instead of the literal `Working` label. (#501)
- Codex blocker detection now ignores stale prompt text outside the live prompt region, reducing false blocked states from old scrollback.
- Native pane URL clicks now use Cmd-click on macOS and Ctrl-click on other platforms. (#307)
- Worktree open, create, and remove actions now work from bare repositories instead of assuming a normal checkout. (#497)
- Pane mouse handling no longer sends empty PTY writes for mouse events that produce no terminal input. (#496)
- Pane output now renders flag emoji and other multi-codepoint grapheme clusters as complete symbols instead of blank cells. (#243)
- Starting Herdr with no restored workspaces, or closing the last workspace, now opens a default workspace instead of leaving the client on an empty screen where direct keybindings such as `cmd+n` were shown but ignored. (#366)
- Resizing restored panes no longer aborts the server when libghostty-vt reflows a terminal whose pre-resize cursor row is past the new height. (#465)
- Full-screen TUIs such as Neovim now receive resize-generated terminal responses after Herdr internal pane resizes, so grown panes redraw without waiting for extra input. (#471)
- Nested agent session reports from child terminals no longer overwrite the owning pane's restored agent session id. (#511)
- Headless servers now avoid repeated scrollback rendering work for inactive panes, reducing CPU in large sessions. (#512)
- Mouse-click handling now respects `ui.prompt_new_tab_name`, so mouse-created tabs follow the same naming prompt setting as keyboard-created tabs. (#521, thanks @imrajyavardhan12)
- Pasting now works in modal text inputs, including rename prompts, command prompts, and worktree dialogs. (#302)
- Linux clipboard image reads now validate image payloads before accepting them, preventing malformed clipboard data from reaching pane image paste flows. (#534)

### Added
- Added remote auto-updates for agent detection manifests, with per-agent validation, local override precedence, `herdr server agent-manifests` diagnostics, and explain output showing remote manifest status.
- Added `herdr server update-agent-manifests` to fetch remote agent detection manifests immediately, reload the running server, and print the updated manifest status.
- Added `herdr agent explain` to show the manifest source, matched rule, evaluated matcher and region evidence, visible evidence flags, skipped-update reason, and idle fallback reason for live panes or saved screen fixtures.
- Added `herdr integration install kimi` for Kimi Code CLI hooks that report lifecycle state and session ids through Herdr's socket API. When native agent session restore is enabled, Herdr can resume Kimi panes with `kimi --session <id>`. (#431, #463, thanks @wbxl2000)
- Added `herdr integration install droid` for Factory Droid hooks that report session ids through Herdr's socket API. When native agent session restore is enabled, Herdr can resume Droid panes with `droid --resume <id>`.
- Added `herdr integration install kilo` for Kilo Code CLI plugins that report lifecycle state and session ids through Herdr's socket API. When native agent session restore is enabled, Herdr can resume Kilo panes with `kilo --session <id>`.
- Added `herdr integration install cursor` for Cursor Agent CLI hooks that report session ids through Herdr's socket API. When native agent session restore is enabled, Herdr can resume Cursor panes with `cursor-agent --resume <id>`. (#506, thanks @udirom)
- Added directional pane swap with `prefix+shift+h/j/k/l`, a pane context-menu swap action, pane layout/neighbor/edge/focus/resize socket APIs, matching CLI commands, and optional `pane split --ratio` support. (#330, #421)
- Added `herdr pane zoom` and the `pane.zoom` socket API to toggle, set, or clear tab-local pane zoom from scripts and integrations.
- Added toast ergonomics controls for delayed agent notifications, in-app toast placement, copied-to-clipboard feedback, and the `notification.show` socket API with `herdr notification show` and optional `none`, `done`, or `request` sounds. (#486)

### Changed
- OpenCode installed with the current Herdr plugin now reports lifecycle state directly instead of relying on screen manifest detection. Kimi Code CLI `0.14.0` or newer now reports full lifecycle state through hooks, including interrupts. Droid and Qoder CLI now report native session identity while leaving lifecycle state to screen manifest detection.

## [0.6.8] - 2026-06-04

This is a hotfix release for v0.6.7, prioritizing a server-crash fix for panes that print complex Unicode or emoji output.

### Fixed
- Fixed a Herdr server crash triggered by pane output containing complex Unicode, emoji, or decomposed accent graphemes. Affected sessions could lose running pane processes or crash again after restore if the same saved pane output was replayed. (#453)
- Direct installs managed by mise now update through the mise install path instead of failing to replace the active binary.
- Claude Code panes that are actively thinking or streaming no longer flicker to blocked because of custom status text. (#409)
- Claude Code panes now detect running shell-command status more reliably.
- OpenCode installed through pnpm is now detected as `opencode` instead of being missed because the packaged executable is named `opencode.exe`. (#447)

### Added
- Added opt-in macOS input-source switching during prefix mode with `experimental.switch_ascii_input_source_in_prefix`, so users typing with a non-Latin IME can run prefix commands through an ASCII-capable input source and return to the previous input source when prefix mode ends. (#400, #434, thanks @sf-jin-ku)

## [0.6.7] - 2026-06-03

### Added
- Added a compact collapse control to the expanded sidebar so mouse users can collapse and expand the sidebar from visible controls. (#278, #291, thanks @turgaybulut)
- Added an opt-in preview update channel with `herdr channel set preview`, `[update].channel`, automated preview manifests, and GitHub prerelease publishing for users who want fixes before stable releases as Herdr transitions toward less frequent, more stable releases.
- Added a remote SSH bridge keepalive fallback. `herdr --remote` now generates a temporary SSH config that includes the user's SSH config first, then adds `ServerAliveInterval` and `ServerAliveCountMax` only when the user has not already configured keepalives. Set `[remote].manage_ssh_config = false` to disable this. (#354, #355, thanks @SunskyXH)
- Added `ui.right_click_passthrough_modifier` so a configured modifier such as `ctrl` can forward right-click hold and drag gestures to mouse-reporting pane apps while normal right-click still opens Herdr's pane menu. (#148)
- Added Kilo Code CLI automatic detection for idle, working, and blocked terminal states. (#270)
- Added `herdr integration install copilot` for GitHub Copilot CLI hooks that report native session ids through Herdr's socket API. Copilot state still comes from Herdr's screen detection because Copilot hooks do not provide complete lifecycle coverage. When native agent session restore is enabled, Herdr can resume Copilot panes with `copilot --resume=<id>`. (#232, #386, thanks @LaneBirmingham)

### Changed
- Native agent session restore is now enabled by default for supported panes with current official integrations. Set `[session] resume_agents_on_restore = false` to disable it.
- Claude Code, Codex, GitHub Copilot CLI, Droid, Kimi Code CLI, and Qoder CLI integrations now report session identity only. Native state for those agents comes from Herdr's screen detection, while Pi, OMP, OpenCode, Kilo Code CLI, Hermes Agent, and custom socket integrations can still report state.

### Fixed
- Large long-running sessions no longer hit the frame-streaming crash fixed by the vendored libghostty-vt update. (#276)
- Copy mode now preserves linewise selection after `shift+v` while moving the cursor. (#360, #389, thanks @reobin)
- Leaving copy mode now restores the previous scroll position, or returns to the bottom when copy mode started at the bottom. (#398, #410, thanks @reobin)
- Git branch labels now resolve correctly in repositories that use Git's reftable ref format instead of showing `.invalid`. (#384, #423, thanks @LaneBirmingham)
- The official Nix flake now builds on macOS by providing Darwin SDK discovery helpers and Darwin cctools to the vendored libghostty-vt build. (#405, #407, thanks @DeevsDeevs)
- Commands launched after `--`, such as `herdr agent start ... -- opencode --session <id>`, now preserve child argv flags instead of parsing them as Herdr flags. (#383)
- Pane apps that request any-motion mouse tracking now receive hover/move events, making Textual-style TUI mouse interaction more reliable inside Herdr. (#419)
- Claude Code background-agent wait text in scrollback no longer keeps an idle pane marked working after the background agent has completed.
- Claude Code and Codex transcript or expanded-detail viewers no longer publish a false idle state while the pane is still showing active agent status.
- Claude Code question prompts that use the arrow-glyph selector are now detected as blocked.
- Kiro sub-agent tool approval prompts are now detected as blocked instead of working. (#388)
- Shift-letter prefix bindings such as `prefix+shift+n` now work in legacy SSH terminal sessions that send uppercase letters without separate Shift metadata. (#312)
- Idle panes now avoid repeated full foreground-process scans, reducing idle CPU on sessions with many panes. (#439)
- Restored native agent sessions now resume across background workspaces and tabs after the first client provides terminal context instead of waiting until each pane is focused.
- Pane input no longer waits behind the PTY actor's idle read poll, restoring responsive typing at quiet shell prompts. (#379)
- Pane apps that query OSC 4 ANSI palette colors now receive the active terminal palette response, so OpenCode and similar TUIs can enable system-theme behavior inside Herdr. (#387)
- Pane apps that query terminal capabilities with XTGETTCAP now receive supported capability responses, improving feature detection in Neovim and similar terminal apps. (#393)
- Pane text selection now derives its highlight colors from the host terminal or active Herdr palette instead of forcing the theme's blue accent. (#298)
- `herdr channel set preview` and `herdr channel set stable` now update direct installs from the selected channel immediately, reject preview on Homebrew and Nix installs before changing config, and show package-manager guidance for managed installs.
- Plain `herdr update` and remote binary replacement now ask before stopping running sessions, avoid protocol-heavy prompt text, and leave the current install untouched when the user chooses not to stop active pane processes. Explicit `--handoff` update flows try live handoff without a second handoff prompt.
- Remote bootstrap now uses the remote shell only for PATH discovery and runs internal probes through `/bin/sh`, so `herdr --remote` can detect existing installs when the remote login shell is fish. (#396)

## [0.6.6] - 2026-05-31

### Added
- Custom command keybindings now accept an optional `description` field to provide user-defined descriptions shown in the keybind help panel instead of the default `'custom command'` label. (#362)

### Fixed
- The OpenCode integration no longer treats `session.created` or `session.updated` plugin events as idle signals, so active sessions stay marked working until OpenCode reports `session.status` or `session.idle`. (#351)
- New interactive panes now use login-shell startup on macOS by default so Homebrew and other login PATH setup is available, with `terminal.shell_mode = "non_login"` as an opt-out. (#350)
- Claude Code panes no longer stay blocked after stale permission-prompt reports when the visible screen has returned to idle or working state. (#349)
- Codex panes no longer stay working because stale `esc to interrupt` text remains above a visible idle prompt, and visible approval-review work is now preserved as working. (#352)
- Sidebar Git status refresh now deduplicates workspaces from the same checkout and reuses cached ahead/behind results when refs have not changed, reducing idle CPU from repeated `git` polling. (#353)
- Update prompts, toasts, and docs now distinguish installing a new binary from stopping or reattaching a running Herdr session to use it.
- Large restored sessions no longer leave restored or newly split panes without shells after startup, and live handoff keeps PTY ownership bounded to one master fd per pane. (#357)
- Pane shutdown no longer warns that a pane is still alive after the direct child has already exited and been reaped. (#338)
- Closing the last pane or tab in a parent worktree workspace now shows the existing confirmation before closing the whole worktree group. (#369)

## [0.6.5] - 2026-05-29

### Added
- Added pane copy mode at `prefix+[` with keyboard navigation, visual selection, and clipboard yank support. (#231)
- Added `foreground_cwd` to pane and agent API/CLI responses so integrations can inspect the active foreground process directory without changing the existing pane/workspace `cwd` semantics. (#345)
- Added read-only `agent_session` metadata to pane and agent API/CLI responses when official integrations report native session references.

### Fixed
- Live handoff now preserves terminal state when transferring supported running panes to a replacement server.
- WSL clipboard writes now prefer OSC 52 before WSLg clipboard tools, so mouse selection and double-click copy populate Windows clipboard history in Windows Terminal. (#333)
- Incomplete host terminal OSC default-color replies no longer get misread as Alt-key input and forwarded into panes, preventing interactive prompts such as `gh auth login --web` from aborting on split `ESC ]` input. (#279, #306, #344)
- Workspace rename prompts and background notifications now use live cwd-derived workspace labels instead of stale session labels. (#332)
- `herdr session stop` no longer fails on zero-duration socket timeouts when the stop deadline is nearly exhausted.
- Update preview instructions now wrap long package-manager commands instead of truncating the shell command suffix.
- Restored native agent resume panes now fall back to a shell when the resumed agent exits instead of closing the whole pane.

## [0.6.4] - 2026-05-27

### Fixed
- Fixed macOS server startup with large restored sessions by raising the server file descriptor soft limit, preventing new panes from failing with `dup of fd N failed` or `Too many open files` around 40 live panes. (#327)

This is a hotfix for v0.6.3. See the v0.6.3 notes for the full feature release.

## [0.6.3] - 2026-05-27

### Added
- Added native agent session restore behind `[session] resume_agents_on_restore`, allowing supported Pi, Claude Code, Codex, OpenCode, and Hermes panes with current official integrations to restart into their previous agent conversation after a Herdr server restart. (#233)
- Added opt-in pane screen history across full server restarts with `[experimental] pane_history = true` and Settings > Experiments > pane screen history. (#217, #248, thanks @icedac)
- Added a session navigator at `prefix+g` with a searchable workspace/tab/pane tree, agent state filters, mouse switching, and keyboard navigation. (#157)
- Added configurable navigate-mode movement bindings for workspace and pane navigation keys. (#193)
- Added a configurable `last_pane` keybinding action for tmux-style back-and-forth navigation to the last focused pane across workspaces and tabs. It is unset by default. (#287)
- Added scrollback support to direct agent terminal attaches. Mouse wheel and plain PageUp/PageDown now scroll the attached terminal viewport, while terminal apps that request mouse or alternate-scroll input still receive those events. The client/server protocol is now version 11.
- Added `ui.redraw_on_focus_gained` to keep the existing full redraw on outer-terminal focus gain by default while allowing users to opt out of the visible refresh. (#282)
- Added `ui.mobile_width_threshold` to configure the terminal width at which Herdr switches to the mobile single-column layout. (#317)
- Added `--handoff` for `herdr update` and `herdr --remote` to opt into live server handoff for supported running servers. Plain update and remote attach use the normal restart/stop flow by default.
- Added `pane.report_metadata` and `herdr pane report-metadata` so user hooks can customize pane titles, displayed agent names, compact status labels, and visible state labels without taking over integration-owned lifecycle or session state. (#36)
- Added tmux-style double-click token copy in panes, with temporary copy feedback and mouse passthrough preserved for terminal apps that request mouse input. (#142, #296, thanks @babymastodon)
- Added Ctrl-click URL opening inside panes for OSC 8 hyperlinks and visible `http://` or `https://` URLs when the host terminal sends the modified click to Herdr. (#307)
- Added Qoder CLI detection, terminal state heuristics, and `herdr integration install qodercli` hook support. (#308, #309, thanks @wayneleelwc)

### Fixed
- Remote bootstrap now downloads exact-version release assets for Homebrew and Nix clients instead of copying package-manager-managed local binaries into `~/.local/bin/herdr`.
- `website/latest.json` now stores asset URLs for archived releases under `releases[version].assets`, so remote bootstrap can fetch the current client version even when Homebrew and the top-level latest release are temporarily out of sync.
- App and server event queues no longer stall under load, improving delivery of pane and agent state updates. (#265)
- Agent status subscriptions now deliver already-matching states and event-hub notifications reliably for waits and automation. (#288, #295)
- Codex background terminal waits are detected more reliably, and idle agent checking uses less CPU. (#300)
- Split OSC 10/11 host color replies are buffered correctly, so terminal apps still receive host foreground/background color responses when replies arrive in chunks. (#306, #310)
- `herdr session stop` is more reliable when the server closes the socket early or stops without sending a full response.
- The OpenCode integration now releases pane ownership on plugin dispose, preventing stale integration state after OpenCode exits. (#314)
- Linux sound alerts no longer fall back to `aplay` for mp3 files, preventing static noise on systems without `paplay`. Herdr now tries mp3-capable players such as `pw-play`, `ffplay`, `mpg123`, and `mpv` instead. (#290)

## [0.6.2] - 2026-05-23

### Added
- Added optional Nix flake support for building, running, installing, and developing Herdr with Nix. (#208, #221, #264)
- Added `terminal.new_cwd` to choose whether new panes, tabs, and workspaces follow the source pane/workspace, start in `$HOME`, use Herdr's process directory, or use a fixed path.
- Added `herdr integration install omp` for OMP's `.omp` extension directory. The extension reports OMP pane state through Herdr's socket API without relying on native `omp` process detection.
- Added CLI and socket API support for Git worktrees with `herdr worktree list/create/open/remove`, optional worktree provenance on workspace responses, and client/server protocol version 10.

### Fixed
- GitHub Copilot CLI sessions now use tested terminal heuristics for approval prompts, freeform input, plan review, and thinking states in the Agents panel. (#232, #256, thanks @LaneBirmingham)
- Kiro approval prompts are now detected as blocked in the Agents panel. (#255)
- Workspace labels now follow the live pane working directory after directory changes.
- Remote clients using local keybindings no longer show stale server keybinding warnings from the remote host.

## [0.6.1] - 2026-05-22

### Added
- Added `ui.mouse_scroll_lines` to configure how many pane scrollback lines each mouse wheel notch scrolls. The default remains 3. (#236)
- Added `--remote-keybindings local|server` for `herdr --remote`. Remote attach now uses the launching client's local keybindings by default without copying config files to the remote host; use `--remote-keybindings server` to keep the remote server's keybindings. The client/server protocol is now version 9.
- Added `experimental.reveal_hidden_cursor_for_cjk_ime = false` (opt-in), `experimental.cjk_ime_agents = []` (optional allow-list), and `experimental.cjk_ime_cursor_shape = "steady_block"` to expose the focused pane's cursor anchor to the outer terminal even when the pane requested `?25l`, restoring macOS IME candidate-window tracking for TUIs that paint their own cursor (Claude Code, pi, codex). When `cjk_ime_agents` is non-empty, the reveal applies only to focused panes whose detected agent matches one of the listed names. When the pane reports no cursor position, the anchor falls back to the pane's top-left so a stable IME hint is always available. Trade-off when enabled: an extra hardware cursor may appear in the outer terminal for apps that hide the cursor without painting a replacement. (#149, thanks @ChihGodlee)
- Added explicit sidebar Git worktree groups plus native worktree creation, existing checkout open, and safe checkout cleanup flows, configured by `[worktrees].directory`, `keys.new_worktree`, optional `keys.open_worktree`, and optional `keys.remove_worktree`. (#137)
- Added named-session reattach and stop command hints so detach and update guidance point back to the active session. (#199, thanks @Golden-Pigeon)

### Fixed
- Pane apps that query OSC 10/11 default foreground/background colors now receive the host terminal colors, so OpenCode and similar TUIs can detect light terminal themes inside Herdr. (#253)
- Codex Plan mode question prompts now override stale integration `working` reports when the visible terminal UI is clearly waiting for an answer, stale hook authority is cleared when foreground process detection sees Codex exit back to the shell, and Claude Code cancellations now recover from stale hook `working` reports when the idle prompt returns. (#249)
- Keybinding parsing now accepts non-ASCII printable keys such as `ö`, `é`, and `ğ`, including UTF-8 Alt chords. (#247)
- Kimi Code CLI sessions now use structural terminal detection for approval prompts and live thinking/tool status, improving working and blocked state reporting in the Agents panel. (#215)
- Antigravity CLI (`agy`) sessions are now detected, and their terminal UI now reports working and blocked states in the Agents panel. (#207)
- Cursor Agent sessions launched as `cursor-agent` or symlink aliases such as `agent` are now detected, and their terminal UI now reports working and blocked states in the Agents panel. (#225)
- Agent detection now ignores runtime argument strings when identifying foreground processes, reducing false positives from helper commands and wrapped processes. (#238)
- In-app notifications now stay below interactive floating overlays, so dialogs and menus remain readable and clickable while a toast is visible. (#228)
- `herdr --remote` now offers to restart the remote server after installing or replacing a remote binary, or when the running server version differs, even if the client/server protocol is still compatible.

## [0.6.0] - 2026-05-20

### Added
- Added keybinding v2 with explicit `prefix+...` syntax, array bindings per action, configurable prefix-mode pane focus, tab switching, and direct modified chords for users who opt in. (#154, #201, #202, #219)
- Added `herdr config reset-keys` to back up `config.toml` and remove custom keybindings so built-in v2 defaults apply on restart or config reload. (#154)
- Added an integrations tab in settings and first-run onboarding so users can install recommended agent integrations from inside Herdr.
- Added update badges on the sidebar menu, settings menu item, and integrations settings tab when installed integrations are outdated.
- Added `terminal.default_shell` to choose the executable used for new interactive panes. When unset, Herdr still falls back to `$SHELL`, then `/bin/sh`. (#196)
- Added native Kiro CLI detection with idle and working state heuristics. (#185)

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
- Added CLI wrappers on top of the socket API with `herdr workspace ...`, `herdr pane ...`, and `herdr wait ...`, using compact public ids for scripting and agent orchestration.
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
