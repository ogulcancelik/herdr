# herdr


<p align="center">
  <img src="assets/logo.png" alt="herdr" width="100" />
</p>

<p align="center">
  <a href="https://herdr.dev">herdr.dev</a> · <a href="#install">install</a> · <a href="#quick-start">quick start</a> · <a href="#supported-agents">supported agents</a> · <a href="https://herdr.dev/docs/">docs</a> · <a href="https://herdr.dev/docs/socket-api/">socket api</a> · <a href="#sponsors">sponsor</a>
</p>

<p align="center">
  <a href="https://trendshift.io/repositories/32084" target="_blank" rel="noopener noreferrer">
    <img src="https://trendshift.io/api/badge/repositories/32084" alt="herdr was #1 GitHub Trending repository of the day on Jun 30, 2026" width="250" height="55" />
  </a>
</p>

---

https://github.com/user-attachments/assets/043ec09f-4bdd-41d5-aee0-8fda6b83e267

**run all your coding agents in one terminal. see who's blocked, working, or done at a glance.**

run your agents where they already run; your machine, a server, anywhere you can ssh. each one gets its own real terminal, not an app's imitation of one, so even full-screen TUIs render right. click, drag, and split panes into workspaces and tabs, and watch each agent go blocked, working, done. close the laptop and nothing dies; reattach from another terminal, or from your phone over ssh. one local rust binary, not an app: no gui, no electron, no mac-only wrapper, no account, no telemetry. (if you've used tmux: it's that, rebuilt for agents.)

---

## what you get

- **a real terminal per agent.** you see each agent's own screen, not an app's imitation of one, so even full-screen TUIs render right.
- **agent state at a glance.** the sidebar rolls every agent up to 🔴 blocked, 🟡 working, 🔵 done, or 🟢 idle, so you always know who needs you. zero config, no hooks required.
- **workspaces, tabs, panes.** organize by repo or folder, click, drag, and split, mouse-native throughout.
- **nothing dies on detach.** a background server keeps panes and agents alive; detach and reattach from any terminal, including your phone over ssh.
- **runs anywhere.** single ~10MB rust binary, linux and macos (windows beta), no dependencies, runs inside the terminal you already use.
- **scriptable.** a local socket api and cli that agents can drive, plus plugins you can write in any language.

## how it compares

|                          | tmux | gui managers | herdr |
|--------------------------|------|--------------|-------|
| persistent sessions       | ✓    | —            | ✓     |
| detach / reattach        | ✓    | —            | ✓     |
| runs anywhere, over ssh  | ✓    | —            | ✓     |
| panes, tabs, workspaces  | ✓    | ✓            | ✓     |
| agent awareness          | —    | ✓            | ✓     |
| lives in your terminal   | ✓    | —            | ✓     |
| real terminal views      | ✓    | —            | ✓     |
| mouse-native            | —    | ✓            | ✓     |
| lightweight binary       | ✓    | —            | ✓     |
| agents can orchestrate   | ?    | ?            | ✓     |

tmux gives you persistence and panes, but it was built before agents existed. it has no idea which pane is blocked, working, or done; you can bolt a bell character and per-harness hooks onto it, but you wire each one yourself and still have no shared view of the fleet. the gui agent managers (conductor, cmux, emdash) do show agent state, so call that table stakes. the difference is everything around it. they are apps, often mac-only and closed, that redraw the terminal inside a wrapper. herdr is a single binary that runs in the terminal you already use, anywhere you can ssh, and shows each agent's real screen on a server that keeps it alive when you disconnect. see the [full comparison](https://herdr.dev/compare/) with tmux, zellij, cmux, warp, conductor, and more.

## install

```bash
curl -fsSL https://herdr.dev/install.sh | sh
```

windows preview beta:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://herdr.dev/install.ps1 | iex"
```

also available with `brew install herdr`, `mise use -g herdr`, `nix run github:ogulcancelik/herdr`, or as a stable Linux/macOS binary from [releases](https://github.com/ogulcancelik/herdr/releases).

`herdr update` upgrades an installer-managed install; Homebrew, mise, and Nix update through their own package managers. channel, preview, restart, and restore details are in the [install docs](https://herdr.dev/docs/install/).

## quick start

```bash
herdr
```

herdr starts or attaches to a background server and opens a workspace. run an agent in the pane.

herdr is mouse-native, so clicking and dragging panes, tabs, and split borders gets you everywhere without a single keybinding. for the keyboard, `ctrl+b` is the prefix: press it, release, then press the action key, so `ctrl+b` then `c` makes a tab. one reserved key keeps herdr out of your shell's way.

- `ctrl+b` then `shift+n` for a new workspace
- `ctrl+b` then `v` or `minus` to split panes
- `ctrl+b` then `c` for a new tab
- `ctrl+b` then `w` to switch workspaces
- `ctrl+b` then `q` to detach; agents keep running, run `herdr` again to reattach

press `ctrl+b` then `?` for every binding. the [keyboard guide](https://herdr.dev/docs/keyboard/) explains the prefix model and how to go prefix-free; the full keymap, copy mode, and config syntax live in the [configuration docs](https://herdr.dev/docs/configuration/).

## remote

run herdr on a VPS and reach it from your local terminal. `herdr --remote` makes your local terminal the client of the remote server, so pasting images into your agents keeps working, the thing plain `ssh` + `tmux` breaks.

```bash
herdr --remote workbox
herdr --remote ssh://you@yourserver:2222
```

see the [persistence and remote docs](https://herdr.dev/docs/persistence-remote/) for named sessions, keepalives, direct attach, and handoff.

## supported agents

detection works out of the box with process-name matching plus terminal-output heuristics.

| agent | idle / done | working | blocked |
|-------|-------------|---------|---------|
| [pi](https://pi.dev) | ✓ | ✓ | partial |
| [claude code](https://docs.anthropic.com/en/docs/claude-code) | ✓ | ✓ | ✓ |
| [codex](https://github.com/openai/codex) | ✓ | ✓ | ✓ |
| [droid](https://factory.ai) | ✓ | ✓ | ✓ |
| [amp](https://ampcode.com) | ✓ | ✓ | ✓ |
| [opencode](https://github.com/anomalyco/opencode) | ✓ | ✓ | ✓ |
| [grok cli](https://x.ai/grok) | ✓ | ✓ | ✓ |
| [hermes agent](https://github.com/NousResearch/hermes-agent) | ✓ | ✓ | ✓ |
| [kilo code cli](https://kilo.ai/) | ✓ | ✓ | ✓ |
| [devin cli](https://docs.devin.ai/cli) | ✓ | ✓ | ✓ |
| cursor agent | ✓ | ✓ | ✓ |
| antigravity cli | ✓ | ✓ | ✓ |
| kimi code cli | ✓ | ✓ | ✓ |
| [github copilot cli](https://github.com/features/copilot) | ✓ | ✓ | ✓ |
| [qodercli](https://qoder.com/cli) | ✓ | ✓ | ✓ |
| [kiro cli](https://kiro.dev/docs/cli/) | ✓ | ✓ | — |

detected but not fully tested: gemini cli, cline. any other agent still works; herdr runs it as a terminal multiplexer, and custom integrations can report labels and state over the socket api.

official integrations add native session restore, and some report semantic state directly. install one with `herdr integration install <agent>`, available for pi, omp, claude, codex, copilot, devin, droid, kimi, opencode, kilo, hermes, qodercli, and cursor. see the [integrations docs](https://herdr.dev/docs/integrations/).

## agents can use herdr too

the local Unix socket lets agents create workspaces, split or zoom panes, spawn helpers, read output, and subscribe to state changes instead of polling. install the reusable skill with:

```bash
npx skills add ogulcancelik/herdr --skill herdr -g
```

start with the [agent skill docs](https://herdr.dev/docs/agent-skill/), [socket API docs](https://herdr.dev/docs/socket-api/), and [`SKILL.md`](./SKILL.md).

## docs

- [quick start](https://herdr.dev/docs/quick-start/): first session, panes, copy, and named sessions
- [concepts](https://herdr.dev/docs/concepts/): server and client, workspaces, tabs, and panes
- [install](https://herdr.dev/docs/install/): install, update, channels, Homebrew, mise, and Nix
- [session state](https://herdr.dev/docs/session-state/): detach, restart restore, agent restore, and live handoff
- [configuration](https://herdr.dev/docs/configuration/): keybindings, copy mode, themes, notifications, environment variables
- [integrations](https://herdr.dev/docs/integrations/): native session restore and semantic state per agent
- [socket api](https://herdr.dev/docs/socket-api/): socket protocol and cli reference
- [`SKILL.md`](./SKILL.md): reusable agent skill

## agent instructions

if you are an ai agent helping with this repository, read [`AGENTS.md`](./AGENTS.md) before making changes and read [`CONTRIBUTING.md`](./CONTRIBUTING.md) before opening issues or PRs.

## development

```bash
git clone https://github.com/ogulcancelik/herdr
cd herdr
cargo build --release
./target/release/herdr

just test        # unit tests
just check       # formatting, tests, and maintenance checks
```

## sponsors

herdr is built full-time, in the open, with no revenue behind it. sponsoring directly funds development, stability, and the path to a real agent runtime.

[**→ become a sponsor**](https://github.com/sponsors/ogulcancelik) · enterprise / partnership: hey@herdr.dev · see [SPONSORS.md](./SPONSORS.md) for tiers. thank you 🐑

## license

Herdr is dual-licensed:

1. Open source: GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later).
2. Commercial: commercial licenses are available for organizations that cannot comply with AGPL.

Contact: hey@herdr.dev

## mandatory star history

<a href="https://www.star-history.com/?repos=ogulcancelik%2Fherdr&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/chart?repos=ogulcancelik/herdr&type=date&theme=dark&legend=top-left&v=2026-05-19" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/chart?repos=ogulcancelik/herdr&type=date&legend=top-left&v=2026-05-19" />
   <img alt="star history chart" src="https://api.star-history.com/chart?repos=ogulcancelik/herdr&type=date&legend=top-left&v=2026-05-19" />
 </picture>
</a>
