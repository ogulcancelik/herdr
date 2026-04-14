<p align="center">
  <img src="assets/logo.png" alt="herdr" width="120" />
</p>

<h1 align="center">herdr</h1>

<p align="center"><strong>supervise multiple coding agents in one terminal.</strong></p>

<p align="center">herd your agents.</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#supported-agents">supported agents</a> · <a href="./INTEGRATIONS.md">integrations</a> · <a href="./CONFIGURATION.md">configuration</a> · <a href="./SKILL.md">agent skill</a> · <a href="./SOCKET_API.md">socket api</a>
</p>

---

herdr is a terminal-native workspace manager for coding agents.

run Claude Code, Codex, pi, opencode, droid, amp and plain shells side by side in your existing terminal. herdr gives you workspaces, tabs, panes, automatic agent detection, and notification alerts so you can see which agent is blocked, done, or still working without leaving the command line.

it runs inside ghostty, alacritty, kitty, wezterm, and even inside tmux. it is a single Rust binary, not a separate GUI window, electron wrapper, or web dashboard.

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

## why herdr

running one coding agent is easy. running several in parallel gets messy fast.

most tools in this space either replace your environment or give you panes without any awareness of what those panes are doing. herdr stays inside your terminal and adds the missing layer: supervision.

with herdr you can:

- run multiple coding agents in parallel in one terminal-native workspace
- scan workspaces quickly and see which one needs attention
- spot whether an agent is blocked, finished, working, or idle
- jump between repos, tabs, and panes without losing context
- let agents create panes, spawn helpers, read output, and wait on each other through the local api

## install

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

or download the binary directly from [releases](https://github.com/ogulcancelik/herdr/releases).

requirements: linux or macos.

### update

herdr checks for updates automatically in the background. when a new version is ready, you'll see a notification in the ui. restart to apply it.

for a manual update:

```bash
herdr update
```

## quick start

launch herdr:

```bash
herdr
```

by default, herdr now launches or attaches to a background session server. closing a client detaches from that shared session. use `herdr server stop` when you want to stop the server itself. use `herdr --no-session` for the old single-process mode.

then do this:

1. press `n` to create a workspace
2. run an agent in the root pane
3. press `ctrl+b` to enter navigate mode
4. use `v` or `-` to split panes, or `c` to create a new tab
5. run more agents side by side
6. watch the sidebar to see which workspace or agent needs attention

on first run, herdr opens a short onboarding flow so you can choose your notification style. after that, if a session is restored you'll land in terminal mode; otherwise you'll start in **navigate mode**.

## what you get

### workspaces, tabs, and panes

herdr is organized around workspaces. each workspace can contain multiple tabs, and each tab can contain multiple panes.

workspaces open immediately as real terminal contexts. the first pane in a workspace is the **root pane**. it anchors the workspace and gives it its default identity:

- if the root pane is inside a git repo, the workspace label defaults to the repo name
- otherwise it falls back to the current folder name
- manual rename is supported, but the default model is repo or folder first

this keeps workspaces lightweight. they are project contexts first, labels second.

### awareness and notifications

the sidebar is split into two layers:

- **top:** workspaces, each with one aggregate state dot
- **bottom:** detected agents inside the selected workspace, or across all workspaces when you switch the agent panel scope

herdr automatically detects running agents by looking at the foreground process and reading terminal output. workspace rollups surface the most urgent thing happening in each workspace so you can scan the full list quickly.

in expanded view, the workspace list and agent list have their own resizable sections. in collapsed view, you still get compact per-pane agent indicators.

states map to:

- 🔴 **blocked** — agent needs input or approval
- 🔵 **done** — work finished and you have not looked at it yet
- 🟡 **working** — agent is actively running
- 🟢 **idle** — done, seen, and calm

rollups prefer the most urgent state in a workspace: blocked first, then unseen finished work, then working, then idle.

plain shells still matter to the workspace itself, but the sidebar stays focused on actual agents.

if ambient sidebar awareness is not enough, herdr can also play sounds or show top-right toast notifications for background events. notification suppression is tab-aware: the active tab stays quiet, but background tabs in the same workspace can still alert.

### agents can use herdr too

herdr is not only for humans supervising agents. it is also becoming a shared control surface for the agents themselves.

agents running inside herdr can use the local socket api and cli wrappers to:

- create new workspaces for parallel tasks
- create, focus, rename, and close tabs
- split panes for servers, logs, tests, or scratch work
- spawn other agents in sibling panes
- read pane output or wait for output matches
- send text and keys into other panes
- wait for another agent to finish before continuing

for that workflow, start with [`SKILL.md`](./SKILL.md) if you want a reusable agent skill, or [`SOCKET_API.md`](./SOCKET_API.md) if you want the low-level socket protocol and cli wrapper reference.

## supported agents

herdr detects supported agents automatically with zero setup. it identifies the foreground process in each pane and reads the live bottom of the terminal buffer to infer agent state. for agents that expose hooks or plugins, direct integrations are the more robust path because they forward semantic state to herdr over the local socket api.

the following agents have been tested:

| agent | idle / done | working | blocked |
|-------|-------------|---------|---------|
| [pi](https://pi.dev) | ✓ | ✓ | partial |
| [claude code](https://docs.anthropic.com/en/docs/claude-code) | ✓ | ✓ | ✓ |
| [codex](https://github.com/openai/codex) | ✓ | ✓ | ✓ |
| [droid](https://factory.ai) | ✓ | ✓ | ✓ |
| [amp](https://ampcode.com) | ✓ | ✓ | ✓ |
| [opencode](https://github.com/anomalyco/opencode) | ✓ | ✓ | ✓ |

heuristics also exist for these agents but have not been fully tested yet:

- [gemini cli](https://github.com/google-gemini/gemini-cli)
- [cursor agent](https://cursor.com/cli)
- [cline](https://github.com/cline/cline)
- [kimi](https://kimi.ai)
- [github copilot cli](https://cli.github.com)

for any other cli agent, herdr still works as a terminal-native multiplexer. you still get workspaces, panes, tiling, and notifications even when richer state detection is not available yet.

for agents outside the built-in list, herdr also supports custom hook or plugin integrations over the local socket api. a custom integration can report any non-empty agent label, and that label will appear in pane info, events, and the sidebar without needing first-party built-in detection. see [`SOCKET_API.md`](./SOCKET_API.md) for the raw api surface.

## optional direct integrations

automatic detection works out of the box. if an agent exposes hooks or plugins, the better path is to let it report state to herdr over the local socket api.

herdr ships one-command integrations for:

- [pi](./INTEGRATIONS.md#pi)
- [claude code](./INTEGRATIONS.md#claude-code)
- [codex](./INTEGRATIONS.md#codex)
- [opencode](./INTEGRATIONS.md#opencode)

install them with:

```bash
herdr integration install pi
herdr integration install claude
herdr integration install codex
herdr integration install opencode
```

remove them with:

```bash
herdr integration uninstall pi
herdr integration uninstall claude
herdr integration uninstall codex
herdr integration uninstall opencode
```

these integrations forward semantic state to herdr over the local socket api. that gives you more robust status reporting, but it does not replace herdr's core process detection model. for setup details, installed file locations, caveats, and the exact bundled hook or plugin source files, see [`INTEGRATIONS.md`](./INTEGRATIONS.md).

known codex caveat: codex currently renders hook lifecycle lines in its own tui when hooks are enabled. that noise is upstream codex behavior, not herdr-specific.

## usage

### navigate mode

navigate mode is the workspace control layer. press `ctrl+b` to enter it.

movement actions stay in navigate mode. mutating actions like split, close, new workspace, new tab, and sidebar toggle return you to terminal mode.

common defaults:

- `n` new workspace
- `shift+n` rename workspace
- `shift+d` close workspace
- `c` new tab
- `v` / `-` split pane
- `x` close pane
- `f` fullscreen
- `r` resize mode
- `b` toggle sidebar

optional direct bindings are available but **unset by default**. you can bind workspace, tab, and pane switching directly in terminal mode without going through the prefix first.

example:

```toml
[keys]
previous_workspace = "ctrl+alt+["
next_workspace = "ctrl+alt+]"
previous_tab = "alt+["
next_tab = "alt+]"
focus_pane_left = "alt+h"
focus_pane_down = "alt+j"
focus_pane_up = "alt+k"
focus_pane_right = "alt+l"
```

full keybinding and config reference: [`CONFIGURATION.md`](./CONFIGURATION.md)

### resize mode

| key | action |
|-----|--------|
| `h` `l` | resize width |
| `j` `k` | resize height |
| `esc` | exit resize mode |

### mouse

mouse support is built in. herdr is not keyboard-only.

- click a workspace in the sidebar to switch
- click tabs to switch within the active workspace
- click the tab-bar `<` / `>` buttons to scroll overflowing tabs
- drag tabs to reorder them
- click a pane to focus it
- drag split borders to resize
- drag in a pane to select text; release to copy it to your system clipboard
- right-click a workspace for a context menu
- scroll in the sidebar to navigate workspaces
- click `«` / `»` at the sidebar bottom to collapse or expand it

text copy uses OSC 52, so it depends on your terminal's clipboard support.

### terminal mode

terminal mode is a real terminal. your shell, vim, htop, ssh, and other full-screen tools work normally. press the prefix key (`ctrl+b`) to go back to navigate mode.

## docs map

use the dedicated docs for detailed setup and automation work:

- [`CONFIGURATION.md`](./CONFIGURATION.md) — config file, keybindings, themes, notifications, onboarding, ui options, and environment variables
- [`INTEGRATIONS.md`](./INTEGRATIONS.md) — install and behavior notes for pi, Claude Code, Codex, and opencode integrations
- [`SKILL.md`](./SKILL.md) — reusable agent skill for agents already running inside herdr
- [`SOCKET_API.md`](./SOCKET_API.md) — canonical socket protocol and the `herdr workspace`, `herdr tab`, `herdr pane`, and `herdr wait` cli wrappers

## configuration

config file: `~/.config/herdr/config.toml`

print the full default config with:

```bash
herdr --default-config
```

### themes

herdr ships with 9 built-in themes: catppuccin (default), tokyo night, dracula, nord, gruvbox, one dark, solarized, kanagawa, and rosé pine.

```toml
[theme]
name = "tokyo-night"
```

you can also override individual color tokens on top of any base theme. see [`CONFIGURATION.md`](./CONFIGURATION.md) for the full token reference.

for day-to-day changes, you do not have to edit the config by hand. herdr also has an in-app settings screen for theme, sound, and toast preferences.

## session persistence

herdr now runs in server/client mode by default. the first `herdr` launch starts a background server, restores your last session, and attaches a thin client to it. later `herdr` launches attach to that same running session.

that means:

- pane processes survive when you detach a client
- `herdr` from another terminal reattaches to the same session
- in-app quit actions like navigate-mode `q` detach the current client by default
- `herdr server stop` shuts down the background server explicitly

session state is still saved at `~/.config/herdr/session.json`. that snapshot stores workspaces, tabs, pane layouts, pane working directories, and focused tab or pane so the server can restore them after a full restart.

current model: attached clients share one session view. this is a shared persistent session, not fully independent tmux-style per-client navigation yet.

through the socket api, tabs are first-class too. raw integrations and cli wrappers can list, create, focus, rename, and close tabs directly while pane ids stay workspace-scoped.

use `--no-session` to start fresh in the old monolithic mode.

## how agent detection works

herdr does not require hooks or agent-side configuration for built-in detection. automatic detection works by:

1. identifying the foreground process of each pane's pty via `/proc` on linux or `proc_pidinfo` on macos
2. matching the process name against known agents
3. reading the live bottom of the terminal buffer and applying per-agent heuristics to determine state

this means detection works with any supported agent, installed any way, with zero setup. if it runs in a terminal, herdr can see it.

when an agent exposes hooks or plugins, the more robust option is to forward state to herdr over the local socket api. that is what the built-in pi, claude code, codex, and opencode integrations do.

the heuristics are matched against each agent's real terminal output: prompt boxes, spinners, waiting-for-input messages, and tool execution indicators. detection runs on a separate async task per pane, polled every 300 to 500 ms, decoupled from terminal rendering.

## cli wrappers

herdr's workspace, tab, pane, and wait commands are documented in [`SOCKET_API.md`](./SOCKET_API.md) together with the socket methods they wrap.

both `workspace create` and `tab create` support optional `--label` flags, so scripts and agents can name contexts immediately instead of renaming them after creation. both create commands also return the created root pane in their json response, so clients can act on the new pane without an extra lookup.

raw api responses use stable ids for workspaces, tabs, and panes. cli wrappers and raw requests still accept positional shorthand like `1`, `1:2`, and `1-2` for convenience in the current live session.

## built with agents

i had never written Rust before starting this project. herdr was built almost entirely through ai coding agents, the same ones it is designed to multiplex. i supervised the architecture and specs. agents wrote the code.

that is part of the point here. herdr is a useful tool, but it is also a proof of what current coding agents can build when they are directed carefully inside a real workflow.

there will be rough edges. if you hit one, [open an issue](https://github.com/ogulcancelik/herdr/issues).

## building from source

```bash
git clone https://github.com/ogulcancelik/herdr
cd herdr
cargo build --release
./target/release/herdr
```

## testing

```bash
just test               # unit tests
just test-all           # full local test suite
```

## license

AGPL-3.0: free to use, modify, and distribute. if you distribute a modified version, you must open-source your changes under the same license.
