<p align="center">
  <img src="assets/logo.png" alt="herdr" width="120" />
</p>

<h1 align="center">herdr</h1>

<p align="center">herd your agents.</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="#usage">usage</a>
</p>

---

herdr is a terminal workspace manager for AI coding agents. it runs inside your existing terminal — ghostty, alacritty, kitty, wezterm, even inside tmux. a single rust binary that gives you workspaces, tiled panes, and intelligent agent state detection.

you keep your terminal. herdr keeps track of your agents.

<p align="center">
  <img src="assets/screenshot.png" alt="herdr screenshot" width="900" />
</p>

## philosophy

every tool in this space is building more. desktop apps, electron wrappers, web dashboards — all trying to replace your terminal with their own environment.

herdr takes the opposite approach. it's a tool that lives where you already work. it sits alongside tmux, inside your terminal emulator, part of your existing workflow. it does one thing well: lets you run multiple coding agents in parallel and know when they need you.

the terminal is already a great environment for coding agents. what's been missing is awareness — seeing at a glance which agent is idle, which is working, and which needs your input. herdr adds that layer.

## how it works

herdr embeds real terminal emulators using PTY and vt100 parsing. each pane is a full terminal — your shell, your agent, your tools, exactly as they'd run anywhere else.

the sidebar shows your workspaces. each workspace can have multiple tiled panes. the agent detection system reads terminal output in real time and determines what state each agent is in:

- **●** red — agent is waiting for your input
- **●** yellow — agent is working
- **●** blue — agent finished (you haven't looked yet)
- **○** green — agent is idle, you've seen it

when an agent finishes work in a background workspace, its dot turns blue. you see it at a glance and switch over. it just reads the terminal.

## supported agents

herdr detects agent state by identifying the foreground process and reading terminal output patterns. the following agents have been tested:

| agent | idle | busy | waiting |
|-------|------|------|---------|
| [pi](https://pi.dev) | ✓ | ✓ | ✓ |
| [claude code](https://docs.anthropic.com/en/docs/claude-code) | ✓ | ✓ | ✓ |
| [codex](https://github.com/openai/codex) | ✓ | ✓ | ✓ |
| [droid](https://factory.ai) | ✓ | ✓ | ✓ |
| [amp](https://ampcode.com) | ✓ | ✓ | — |
| [opencode](https://github.com/anomalyco/opencode) | ✓ | ✓ | ✓ |

detection heuristics also exist for these agents but haven't been fully tested yet. if you use them and run into issues, please [open an issue](https://github.com/ogulcancelik/herdr/issues):

- [gemini cli](https://github.com/google-gemini/gemini-cli)
- [cursor agent](https://cursor.com/cli)
- [cline](https://github.com/cline/cline)
- [kimi](https://kimi.ai)
- [github copilot cli](https://cli.github.com)

for any other CLI agent, herdr still works as a workspace manager — you get workspaces, panes, and tiling. a hook system for custom agent state reporting is coming soon.

## install

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

or download the binary directly from [releases](https://github.com/ogulcancelik/herdr/releases).

requirements: linux or macos.

### update

herdr checks for updates automatically in the background. when a new version is ready, you'll see a notification in the UI — just restart to apply. you can also update manually:

```bash
herdr update
```

## usage

launch herdr:

```bash
herdr
```

herdr starts in **navigate mode**. press `n` to create your first workspace, type a name, press enter. you're in **terminal mode** with a shell ready.

press `ctrl+s` (the prefix key) to switch back to navigate mode. from there you can manage workspaces and panes.

### navigate mode (prefix: ctrl+s)

| key | action | configurable |
|-----|--------|:---:|
| `n` | new workspace | |
| `N` | rename workspace | |
| `d` | close workspace | |
| `1`-`9` | switch to workspace by number | |
| `↑` `↓` | select workspace | |
| `enter` | open selected workspace | |
| `v` | split pane vertically | ✓ |
| `-` | split pane horizontally | ✓ |
| `h` `j` `k` `l` | navigate between panes | |
| `tab` | cycle panes | |
| `f` | toggle fullscreen | ✓ |
| `x` | close pane | ✓ |
| `r` | enter resize mode | |
| `b` | toggle sidebar collapse | |
| `q` | quit | |

keys marked ✓ can be changed in `~/.config/herdr/config.toml` under `[keys]`. the prefix key is also configurable.

### resize mode

| key | action |
|-----|--------|
| `h` `l` | resize width |
| `j` `k` | resize height |
| `esc` | exit resize mode |

### mouse

- click a workspace in the sidebar to switch
- click a pane to focus it
- drag split borders to resize
- right-click a workspace for context menu
- scroll in sidebar to navigate workspaces
- click `«` / `»` at the sidebar bottom to collapse/expand

### terminal mode

you're in a real terminal. everything works — your shell, vim, htop, ssh, anything. press the prefix key (`ctrl+s`) to go back to navigate mode.

## configuration

config file: `~/.config/herdr/config.toml`

generate the default config with all options:

```bash
herdr --default-config
```

```toml
[keys]
# prefix key to enter navigate mode
prefix = "ctrl+s"

# pane controls (in navigate mode)
split_vertical = "v"
split_horizontal = "-"
close_pane = "x"
fullscreen = "f"

[ui]
# accent color: hex (#89b4fa), named (cyan, blue), or rgb(r,g,b)
accent = "cyan"

# ask for confirmation before closing a workspace
confirm_close = true

# play sounds when agents change state in background workspaces
# a chime when an agent finishes, an alert when one needs input
sound = true
```

### environment variables

| variable | description |
|----------|-------------|
| `HERDR_LOG` | log level filter (default: `herdr=info`) |

logs: `~/.config/herdr/herdr.log`

## session persistence

herdr saves your workspace layout, pane working directories, and focused pane on exit. when you restart, everything is restored. sessions are stored at `~/.config/herdr/session.json`.

use `--no-session` to start fresh.

## how agent detection works

herdr doesn't require hooks or agent-side configuration. detection works by:

1. identifying the foreground process of each pane's PTY (via `/proc` on linux, `proc_pidinfo` on macos)
2. matching the process name against known agents
3. reading terminal screen content and applying per-agent heuristics to determine state

this means detection works with any supported agent, installed any way, with zero setup. if it runs in a terminal, herdr can see it.

the heuristics are pattern-matched against each agent's actual terminal output — prompt boxes, spinners, "waiting for input" messages, tool execution indicators. detection runs on a separate async task per pane, polled every 300-500ms, decoupled from terminal rendering.

## what's coming

- **notification hooks** — a socket API so any agent or script can report its state to herdr. for agents without built-in detection, wire up a simple hook.
- **native notifications** — OS-level notifications when an agent needs attention and herdr isn't in focus.
- **agent API** — `herdr create`, `herdr split`, `herdr send` — so agents and scripts can manage herdr workspaces programmatically.

## built with agents

i had never written rust before starting this project. herdr was built almost entirely through AI coding agents — the same ones it's designed to manage. i supervised the architecture and specs, agents wrote the code.

this is a proof of concept in more ways than one. it's a functional tool, but it's also a statement about what's possible right now. if you can build a terminal multiplexer in a language you don't know, by directing the same agents the tool is built for — that says something about where we are.

there will be rough edges. if you hit one, [open an issue](https://github.com/ogulcancelik/herdr/issues). that's why it's open source.

## cli

```
herdr                   launch herdr
herdr update            download and install the latest version
herdr --version         print version
herdr --default-config  print default configuration
herdr --no-session      start without restoring or saving sessions
herdr --help            show help
```

## building from source

```bash
git clone https://github.com/ogulcancelik/herdr
cd herdr
cargo build --release
./target/release/herdr
```

## testing

```bash
just test               # unit tests (157 tests)
just test-integration   # LLM-based integration tests
just test-all           # both
```

## license

AGPL-3.0 — free to use, modify, and distribute. if you distribute a modified version, you must open-source your changes under the same license.
