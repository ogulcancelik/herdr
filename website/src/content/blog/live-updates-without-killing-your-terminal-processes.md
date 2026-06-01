---
title: Live updates without killing your terminal processes
description: Herdr can replace its server during an update while keeping pane processes alive.
date: 2026-05-27
---

Live handoff shipped in Herdr 0.6.3 as an experimental, opt-in update path. See the [live handoff docs](/docs/session-state/#live-handoff) for the user-facing behavior.

Terminal multiplexers are supposed to protect long-running work from interruption. You start a server, run shells and tools inside panes, detach clients, reconnect later, and the work keeps going.

Updating the multiplexer itself usually breaks that.

That is normal for tools like tmux and Zellij. They are mature, stable, and their core job changes slowly. If the server/client protocol changes, the usual answer is to keep using the old server until you can restart it. That is annoying, but often acceptable.

Herdr has a different problem. It is a terminal multiplexer, sure, but it is also an AI development workspace. It is new. I released it a few months ago, and we are at 2.6k stars on GitHub. It has more than 15k downloads according to GitHub release stats.

It gets frequent fixes for agent detection (isn't it cool that we have all the labs building their own CLIs, plugins, and hooks in their own broken way?), terminal behavior, remote workflows, worktrees, mobile use, update flows, and integrations. Users should not have to choose between getting those fixes and killing every pane in a running session.

That's a problem: Herdr needed to update like an app while preserving processes like a terminal multiplexer.

## The update problem

A persistent terminal multiplexer usually has at least two sides.

The server owns the session. It owns pane state, PTYs, readers, writers, layout, buffers, sockets, and child process lifetime. The client is mostly a view and input path into that server.

When a new binary changes the client/server protocol, the new client may not be able to talk to the old server. Restarting the server fixes that, but it also risks killing the pane processes. For a terminal multiplexer, that is the worst possible update path.

This is not only a Herdr problem. I looked around because I assumed somebody had already solved it.

Zellij has multiple long-running issues about upgrade pain: sessions appearing lost after upgrade, users being unable to attach to sessions from a previous version, requests for an `upgrade-session` flow, and requests to import sessions from previous versions. See [zellij#1255](https://github.com/zellij-org/zellij/issues/1255), [zellij#2943](https://github.com/zellij-org/zellij/issues/2943), [zellij#3371](https://github.com/zellij-org/zellij/issues/3371), [zellij#3420](https://github.com/zellij-org/zellij/issues/3420), [zellij#3449](https://github.com/zellij-org/zellij/issues/3449), and [zellij#4305](https://github.com/zellij-org/zellij/issues/4305). Zellij 0.44 added forward-compatible client/server support, which helps future clients connect to older running sessions, but that is still not replacing the running server while preserving live PTYs.

Tmux has the same class of version-skew behavior. Users hit protocol mismatch errors, and the practical answer is to restart tmux, delay the update, or attach with the old binary through `/proc/<pid>/exe` where that exists. See [tmux#99](https://github.com/tmux/tmux/issues/99), [tmux#2189](https://github.com/tmux/tmux/issues/2189), [tmux#4356](https://github.com/tmux/tmux/issues/4356), and [tmux#4890](https://github.com/tmux/tmux/issues/4890).

As far as I can tell, mainstream terminal multiplexers do not do in-place server binary replacement while keeping arbitrary live pane processes attached.

So, that is a small breakthrough, but a real one.

## The important detail is PTY ownership

At first, the problem sounds like process migration. But no, that would be much harder.

Herdr does not move the child processes. It moves ownership of the terminals those processes are already attached to.

Each pane process talks to the slave side of a PTY. Herdr's server owns the master side. If the final master FD disappears, the child can lose I/O, receive HUP, or exit. But if the master FD stays alive and a new server takes over reading and writing it, the child process does not need to know the server changed.

That changes the problem from “move a process” to “transfer open file descriptors and rebuild runtime ownership around them.”

On Unix, that is possible with `SCM_RIGHTS`: one process can send open file descriptors to another process over a Unix socket.

## The handoff

The final shape is a two-server transaction.

```text
old server: pause PTY readers
old server: snapshot pure app state
old server: duplicate PTY master FDs
old server: spawn new Herdr in import mode
handoff:    send manifest + FDs over a private Unix socket
new server: restore state and rebuild pane runtimes
new server: bind public sockets and report ready
old server: commit and exit without killing pane process groups
new server: unpause readers and continue
```

The old server does not just dump state and hope. It enters handoff mode, rejects new mutations, disconnects clients, and pauses every transferred PTY reader. That pause acknowledgement matters because only one server may actively read from a PTY at a time. If both servers read from the same master, output can be stolen by the wrong runtime.

After readers are paused, the old server captures Herdr's normal session snapshot. That works because Herdr already separates pure application state from runtime ownership. `AppState` can be serialized. Pane runtimes, PTY handles, readers, writers, and detector loops are separate runtime objects.

Then the old server creates a handoff manifest and sends the duplicated PTY master FDs to the new server.

```text
manifest = app snapshot + pane metadata + optional replay bytes
FDs      = duplicated PTY master file descriptors
transport = private Unix socket + SCM_RIGHTS
```

The new server validates the manifest, checks the expected protocol and version, receives the FDs, restores `AppState`, constructs imported pane runtimes around the received PTY masters, binds the normal Herdr sockets, and reports ready.

Only then does the old server commit. Before commit, any failure rolls back to the old server. After commit, the new server owns the panes and the old server exits without signaling the pane process groups.

That commit boundary is the safety line.

## Why Herdr could do it

Herdr had the pieces already, mostly because of earlier architecture choices.

The state model is separated from runtime ownership. Workspaces, tabs, panes, labels, focus, layout, and agent metadata live in serializable state. PTYs, child handles, reader threads, writer paths, resize state, and detectors live in runtime objects.

The server/client boundary is explicit. Herdr already has a persistent server, reconnecting clients, local sockets, named sessions, remote attach flows, and documented [session state paths](/docs/session-state/). That made it possible to add a hidden import mode instead of rewriting the app around live upgrades.

The terminal engine also helped with visual continuity. Herdr uses `libghostty-vt` for pane terminal state. For normal primary-screen panes, the old server can export recent ANSI history and the new server can seed that history before it starts reading future PTY bytes. That is not required for process survival, but it makes shells and dev servers look continuous after reconnect.

Full-screen terminal apps are different. Herdr does not fake replay for alternate-screen panes like `nvim`, `btop`, or `htop`. Their processes survive the handoff, but the visual state is best effort until the app redraws. That is the honest trade-off.

## The invariants

The hard part was not sending FDs. The hard part was making the transaction safe.

These were the rules:

- exactly one server may read each PTY at a time
- every transferred reader must acknowledge pause before snapshot and replay capture
- before commit, failure leaves the old server alive and owning the panes
- after commit, the old server must not destructively drop transferred runtimes
- no failure path may close the final PTY master FD
- agent restore must not run during [live handoff](/docs/session-state/#live-handoff)
- alternate-screen panes must stay live without pretending their screen was perfectly serialized

The implementation was tested with shell panes, a Python HTTP server, `btop`, and `nvim` on Linux and macOS. The HTTP server kept serving requests across handoff. Shell panes accepted input after handoff. Full-screen TUIs stayed alive and redrew on a best-effort basis.

## What this is not

This is not checkpoint/restore. It does not survive a reboot. It does not migrate processes across machines. It does not preserve arbitrary process memory. It does not make every future internal state change automatically compatible.

It is a same-machine Unix handoff. The running pane process stays attached to its PTY slave. The old Herdr server transfers the PTY master to the new Herdr server. The server binary changes; the pane process keeps running.

That is the useful boundary.

## Why this is cool

Herdr users often keep the exact kind of work open that should not be casually restarted: coding agents, dev servers, shells, REPLs, SSH sessions, debugging sessions, long builds, and local experiments.

A terminal multiplexer should make that work safer, not make updates feel dangerous.

The interesting part is not that Herdr remembers your layout. The interesting part is that the process inside the pane does not have to know an update happened.
