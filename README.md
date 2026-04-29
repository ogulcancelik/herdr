# herdr


<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#supported-agents">supported agents</a> · <a href="./INTEGRATIONS.md">integrations</a> · <a href="./CONFIGURATION.md">configuration</a> · <a href="./SOCKET_API.md">socket api</a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**agent multiplexer that lives in your terminal.**

workspaces, tabs, panes. mouse-native: click, drag, split. every agent at a glance: blocked, working, done. detach and reattach, agents keep running. no gui app, no electron, no mac-only native wrapper. you see the agent's own terminal, not someone's interpretation of it.

---

## install

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

or download the binary from [releases](https://github.com/ogulcancelik/herdr/releases). requires linux or macos.

### update

herdr notifies you when a new version is available. run manually to update:

```bash
herdr update
```

## quick start

```bash
herdr
```

by default herdr launches or attaches to one background session server. `ctrl+b q` detaches the client. agents keep running. use `herdr server stop` to stop the default server. use `--no-session` for the old single-process mode.

named sessions are runtime/socket namespaces for separate persistent herdr servers. they do not replace workspaces; each named session has its own panes, tabs, workspaces, sockets, and session state while sharing the same global config file.

```bash
herdr session list
herdr session attach work
herdr session attach side-project
herdr session stop work
herdr session delete side-project
```

1. press `n` to create a workspace
2. run an agent in the root pane
3. press `ctrl+b` to enter navigate mode
4. use `v` or `-` to split panes, or `c` to create a new tab
5. watch the sidebar for blocked, working, and done states

on first run herdr opens a short onboarding flow. after that, restored sessions land in terminal mode; fresh sessions start in **navigate mode**.

## how it compares

|                          | tmux | gui managers | herdr |
|--------------------------|------|--------------|-------|
| persistent sessions       | ✓    | —            | ✓     |
| detach / reattach        | ✓    | —            | ✓     |
| panes, tabs, workspaces  | ✓    | ✓            | ✓     |
| agent awareness          | —    | ✓            | ✓     |
| lives in your terminal   | ✓    | —            | ✓     |
| real terminal views      | ✓    | —            | ✓     |
| mouse-native            | —    | ✓            | ✓     |
| lightweight binary       | ✓    | —            | ✓     |
| agents can orchestrate   | ?    | ?            | ✓     |

tmux gives you persistence and panes, but it was built before agents existed. gui managers show agent state, but they make you leave your terminal and use their wrapped view. herdr is persistence and awareness in one tool that stays out of your way.

## persistence

start herdr on your desktop or server. run your agents, split panes, do your work. press `ctrl+b q` to detach. close your terminal, close your laptop; your agents keep running. open a new terminal, run `herdr`, you're back. same session, same panes, same agents.

### from anywhere

need to check on your agents from your phone? just ssh in and run herdr. any ssh client works. no app to download, no account to create.

```
ssh you@yourserver
herdr
```

same session, same agents, same state.

## agent awareness

the sidebar shows which agents are blocked, working, or done. workspaces roll up to their most urgent state so you can scan the full list at a glance.

states:

- 🔴 **blocked** — agent needs input or approval
- 🟡 **working** — agent is actively running
- 🔵 **done** — work finished, you have not looked at it yet
- 🟢 **idle** — done and seen

detection works by reading foreground process and terminal output. zero config, no hooks required. for agents that expose hooks, the socket api integration gives more robust state reporting.

## lives in your terminal

not a gui window, not a web dashboard, not electron. herdr runs inside whatever terminal you already use. single rust binary, no dependencies. works inside tmux.

## what you get

- **workspaces** — organized around git repos or folder names, each with its own tabs and panes
- **tabs** — first-class in the socket api and cli
- **mouse-native** — click panes, drag borders, select text to copy; not keyboard-only
- **notifications** — sounds and toasts for background events; tab-aware suppression
- **9 built-in themes** — catppuccin (default), tokyo night, dracula, nord, gruvbox, one dark, solarized, kanagawa, rosé pine
- **session persistence** — pane processes survive client detach; sessions restore after full restart

## agents can use herdr too

the local unix socket lets agents create workspaces, split panes, spawn helpers, read output, and wait for state changes.

```bash
# create a workspace and tab
herdr workspace create --cwd ~/project --label "api"
herdr tab create --label "logs"

# split a pane and run
herdr pane split 1-1 --direction right
herdr pane run 1-2 "npm test"

# wait for an agent
herdr wait agent-status 1-1 --status done

# read output
herdr pane read 1-2 --source recent --lines 50
```

full reference: [`SOCKET_API.md`](./SOCKET_API.md) and [`SKILL.md`](./SKILL.md).

## supported agents

automatic detection works out of the box. process name matching plus terminal output heuristics.

| agent | idle / done | working | blocked |
|-------|-------------|---------|---------|
| [pi](https://pi.dev) | ✓ | ✓ | partial |
| [claude code](https://docs.anthropic.com/en/docs/claude-code) | ✓ | ✓ | ✓ |
| [codex](https://github.com/openai/codex) | ✓ | ✓ | ✓ |
| [droid](https://factory.ai) | ✓ | ✓ | ✓ |
| [amp](https://ampcode.com) | ✓ | ✓ | ✓ |
| [opencode](https://github.com/anomalyco/opencode) | ✓ | ✓ | ✓ |

detected but not fully tested: gemini cli, cursor agent, cline, kimi, github copilot cli.

for agents outside the built-in list, herdr still works as a terminal multiplexer with workspaces, panes, and tiling. custom integrations can report agent labels over the socket api. see [`SOCKET_API.md`](./SOCKET_API.md).

### direct integrations

the built-in pi, claude code, codex, and opencode integrations forward semantic state to herdr over the socket api. install with:

```bash
herdr integration install pi
herdr integration install claude
herdr integration install codex
herdr integration install opencode
```

see [`INTEGRATIONS.md`](./INTEGRATIONS.md) for setup details.

## keybindings

press `ctrl+b` to enter navigate mode.

| key | action |
|-----|--------|
| `n` | new workspace |
| `shift+n` | rename workspace |
| `shift+d` | close workspace |
| `c` | new tab |
| `v` / `-` | split pane |
| `x` | close pane |
| `b` | toggle sidebar |
| `f` | fullscreen pane |
| `r` | resize mode |
| `q` | detach (quit client) |

resize mode: `h`/`l` resize width, `j`/`k` resize height, `esc` exit.

mouse is supported throughout. full reference: [`CONFIGURATION.md`](./CONFIGURATION.md).

## configuration

config file: `~/.config/herdr/config.toml`

```bash
herdr --default-config   # print full default config
```

in-app settings screen for theme, sound, and toast preferences. full reference: [`CONFIGURATION.md`](./CONFIGURATION.md).

## logs

herdr writes logs under `~/.config/herdr/`.

common files:

```text
~/.config/herdr/herdr.log
~/.config/herdr/herdr-client.log
~/.config/herdr/herdr-server.log
```

in persistent session mode, the client and server logs are usually the useful ones. logs rotate automatically and keep a few older files like `.1` and `.2`.

for issue reports, include the relevant current log plus rotated siblings if they exist. default logs are metadata-focused and avoid pane contents by default.

use a higher log level only when needed:

```bash
HERDR_LOG=herdr=debug herdr
```

full logging and environment variable details: [`CONFIGURATION.md`](./CONFIGURATION.md).

## docs

- [`CONFIGURATION.md`](./CONFIGURATION.md) — keybindings, themes, notifications, environment variables
- [`INTEGRATIONS.md`](./INTEGRATIONS.md) — pi, claude code, codex, opencode integrations
- [`SKILL.md`](./SKILL.md) — reusable agent skill
- [`SOCKET_API.md`](./SOCKET_API.md) — socket protocol and cli reference

## building from source

```bash
git clone https://github.com/ogulcancelik/herdr
cd herdr
cargo build --release
./target/release/herdr
```

## testing

```bash
just test        # unit tests
just test-all   # full local test suite
```

## license

AGPL-3.0: free to use, modify, and distribute. modified versions must be open-sourced under the same license.
