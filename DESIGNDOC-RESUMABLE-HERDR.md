# Resumable `herdr --remote` over SSH-authenticated QUIC

Status: design proposal grounded in repository revision `a0678a38b942` (`0.7.3`)

## Summary

Herdr should retain SSH for authentication, host verification, remote platform detection, binary bootstrap, and fallback, while preferring an authenticated QUIC connection for the live `herdr --remote` client protocol.

The remote Herdr server remains the authority for PTYs, terminal emulation, scrollback, workspaces, and rendered UI state. A sleeping or disconnected client must never backpressure a Fedora pane process merely because its network transport is blocked. When the client returns, it receives the newest complete display state rather than a replay of every intermediate render.

The intended user-visible contract is:

> Start `herdr --remote fedora`, change networks or close the laptop, and return to the same server-owned session automatically. If the original QUIC connection still exists, migrate or recover it. If it expired, open a fresh authenticated QUIC connection to the same Herdr server process and send a fresh complete frame. If UDP is unavailable, transparently use the existing SSH bridge.

This is not Mosh local prediction and not Eternal Terminal byte replay. Herdr already has the important Mosh-like primitive: authoritative server-side terminal state rendered into complete client frames.

## Priority requirements

In descending order:

1. **Follow demonstrated maintainer intent.** Existing server/runtime boundaries, newest-client foreground behavior, local-client behavior, rendering semantics, platform isolation, and repository conventions are the primary constraints. Re-read current code and recent relevant history before implementation; this document does not override newer maintainer decisions.
2. **No regressions outside `--remote`.** Users who never invoke `herdr --remote` should see optimally zero behavior change. Local Unix-socket clients, ordinary local sessions, direct terminal attach, server persistence, input, rendering, plugins, APIs, and existing SSH-to-host-then-`herdr` workflows must retain current behavior and performance. Shared refactors require characterization tests proving this.
3. **Perform well on documented 3G-quality networks.** Protect input latency, target 60 fps for workloads capable of producing meaningful 60 fps updates within the link budget, avoid stale-frame backlog, and make rendering encoding conditional on measured evidence.
4. **Handle jittery networks as well as possible.** Coalesce superseded display state, isolate control from blocked render/image transfers, adapt frame production to transport capacity, recover paths automatically, communicate stale/reconnecting state through existing Herdr modal/status patterns, and converge exactly after eventual delivery.
5. **Never make remote network health pane-process health.** A sleeping client or blocked transport must not backpressure Fedora applications.

If an implementation tradeoff conflicts with these requirements, maintainer intent and non-remote compatibility take priority. Raise the conflict rather than silently weakening it.

## Decisions

| Area | Decision |
| --- | --- |
| Authentication bootstrap | Continue using normal OpenSSH authentication and host verification. |
| QUIC server identity | Generate a self-signed TLS certificate and private key per Herdr server process. Keep the private key in memory. Send the certificate fingerprint over authenticated SSH and pin it in the client. No public CA is required. |
| Client authorization | SSH bootstrap mints a high-entropy, process-lifetime capability token scoped to the Unix user, named Herdr session, server instance, logical client, and launch mode. Send it only inside the pinned TLS connection. |
| Certificate/token persistence | Do not persist credentials to disk across an ordinary cold restart. Live self-update/handoff is a required compatibility interaction: the successor must receive enough ephemeral identity/token/endpoint state for transparent fresh QUIC attachment, or clients must be seamlessly rebootstrap without degrading today’s remote update UX. |
| 0-RTT | Out of scope. Use an ordinary TLS 1.3/QUIC handshake. No mutating terminal data may be sent as TLS early data. |
| QUIC endpoint | Bind an unprivileged UDP port from a configurable high port range and advertise it through SSH bootstrap. Fall back automatically to the current SSH bridge when UDP is blocked or unreachable. |
| Idle lifetime | Default on the order of 24 hours and configurable. Retain the original connection while both endpoints retain state; after expiry, make a fresh QUIC connection and state-sync. Do not attempt cross-connection input replay. |
| Input after transport loss | Input keeps flowing on the reliable control stream while the QUIC connection is alive, regardless of render staleness. Stop accepting pane input only when the connection is presumed dead/reconnecting. Do not silently buffer or replay uncertain keys across a dead connection; a final key such as Enter may be lost when it truly expires. |
| Live connection reliability | Let QUIC provide reliable ordered transport, retransmission, duplicate suppression, congestion control, and path migration. Do not build an application-level TCP analogue. |
| Streams | One long-lived bidirectional control/action stream; one replaceable ordered server-to-client render stream per render generation; separate reliable bulk streams for large Kitty image resources. Do not create a stream per frame or small control message. |
| Rendering | Retain the existing network-efficient server-diffed `TerminalAnsi` transport. Add recovery generations, client gap validation, and forced full redraws by resetting the existing `BlitEncoder`; do not invent semantic, compressed-semantic, delta, or hybrid wire formats unless measured ANSI behavior fails a requirement and the user explicitly reopens the decision. |
| Render recovery | Keep one in-flight render and a dirty/latest-state indication. Reset/stop obsolete render streams and send a fresh generation from current server state after recovery. |
| Application backpressure | PTY reads and libghostty parsing must never await network writes, client ACKs, frame serialization queues, or QUIC flow control. Only terminal-parser throughput may naturally backpressure the PTY. |
| Multi-client recovery | Newest client wins, consistent with current `ClientConnected` behavior. Reconnecting through a fresh QUIC connection is a new client activation and becomes foreground. Re-audit current maintainer behavior at implementation time because this area has received recent improvements. |
| Fallback | If initial QUIC path validation fails promptly, use SSH for that attach. If a live QUIC connection irrecoverably fails, try a fresh QUIC connection with the process-lifetime credential, then SSH rebootstrap, then SSH stream fallback. |
| Client process restart | A new local client reruns SSH bootstrap unless an in-memory process credential still exists. No crash-persistent journal of credentials or unacknowledged keystrokes is required. |
| Server restart and handoff | An ordinary cold restart invalidates the in-memory certificate and tokens and triggers SSH bootstrap. A live handoff/self-update must not be treated as an unrelated concern: preserve or transfer ephemeral reconnection authority, or prove automatic rebootstrap is at least as seamless as the current SSH bridge. |
| Protocol compatibility | Preserve current exact `PROTOCOL_VERSION` matching and deliberate bump policy. The bootstrap installs a matching remote binary, so QUIC does not require capability-bit negotiation. Introducing independently negotiable features is a separate maintainer philosophy decision and a blocker, not incidental plumbing. |
| Platform scope | Initial implementation follows current remote support: Unix clients and Linux/macOS remote hosts. Native Windows remote remains out of scope until the repository’s platform capability changes. |

## Goals

1. Make ordinary network blackholes, NAT rebinding, client IP changes, and laptop sleep recover without terminating or blocking server-owned pane processes.
2. Preserve current OpenSSH authentication, host aliases, keys, `ssh-agent`, and remote binary bootstrap behavior.
3. Prefer direct QUIC when reachable and preserve the current SSH bridge as a zero-surprise fallback.
4. Reconstruct the client from authoritative server state after any render gap or transport replacement.
5. Prevent stale frames, large graphics, and reliable control queues from accumulating without bound.
6. Make input and terminal rendering responsive under packet loss and bandwidth limitation.
7. Prove that the recovered client display is exactly equivalent to a perfect-connectivity direct/SSH-only session at the same logical state.
8. Preserve current multi-client foreground intent unless maintainers have changed it when implementation begins.

## Non-goals

1. Mosh-style predictive local echo.
2. Replaying every output byte or every intermediate frame.
3. Recovering arbitrary pane processes after the Fedora host or Herdr server process dies.
4. Exactly-once replay of terminal input across a destroyed QUIC connection.
5. Public-CA certificate provisioning, ACME, or a public Herdr network service.
6. A privileged host-wide gateway in the initial implementation.
7. QUIC 0-RTT.
8. Mixing local and remote panes in one Herdr server session.
9. Replacing SSH authentication or supporting SSH port forwarding over QUIC.

## Current implementation

### Current `herdr --remote` flow

`src/remote/unix.rs::run_remote` currently:

1. Resolves the active named session.
2. Creates `RemoteSsh`, optionally with Herdr-managed OpenSSH keepalive configuration.
3. Detects the remote OS and architecture over SSH.
4. Locates or installs a matching Herdr executable.
5. Ensures a compatible remote Herdr server is running.
6. Starts `SshStdioBridge`, which executes the remote helper command.
7. Starts a local Herdr client connected to a temporary local Unix socket.

The remote command is built by `remote_bridge_command` and executes:

```text
<remote-herdr> [--session NAME] remote-client-bridge
```

`run_remote_client_bridge` runs on the remote host, connects to the remote Herdr server’s local client socket, and copies stdin/stdout bytes between that socket and SSH. The local bridge in `bridge_connection` copies bytes between SSH stdio and the local forwarded Unix socket.

The local thin client is launched by `run_client_process`. Current `herdr --remote` explicitly sets:

```text
HERDR_RENDER_ENCODING=terminal-ansi
```

Therefore current native remote does not use semantic frames by default. It asks the server to diff semantic server frames into `TerminalFrame { seq, width, height, full, bytes }`, then writes those ANSI bytes directly to the local terminal.

### Maintainer intent behind the thin-client transport

Commit `793f127` is titled `feat(remote): add network-efficient thin client transport`. Its commit message states the intended design directly: add SSH-stdio remote attach, negotiate Terminal-ANSI rendering, move blit encoding server-side for remote clients, and split control/render delivery for safer backpressure.

That implementation introduced:

- `RenderEncoding::{SemanticFrame, TerminalAnsi}` negotiation;
- `Hello.requested_encoding` and `Welcome.encoding`;
- server-side per-client `BlitEncoder`, frame baseline, dimensions, and sequence;
- `TerminalFrame { seq, width, height, full, bytes }`;
- a one-slot render queue separate from reliable control;
- deferred full rendering when the render slot is occupied;
- local thin-client direct writes of server-produced ANSI.

This saves bandwidth because the remote server compares complete semantic grids and sends only ANSI needed to change the host terminal from the client-specific prior baseline. Ordinary local clients continue to use semantic frames and perform ANSI encoding locally. Current HEAD preserves this design across `src/protocol/wire.rs`, `src/protocol/render_ansi.rs`, `src/server/render_stream.rs`, `src/server/client_transport.rs`, and `src/remote/unix.rs`.

It does not provide transport resumption: Terminal-ANSI sequencing is metadata only, the client does not validate gaps, render baseline advances on queue admission rather than client application, and the reliable SSH/TCP stream cannot abandon bytes already being written. The resumable design extends this maintainer-chosen optimization with generation fencing, stream supersession, and forced full redraw recovery rather than replacing it.

### Does current `herdr --remote` require Herdr on the server?

It requires a compatible Herdr executable on the server **at runtime**, because the remote executable starts or attaches the server and runs `remote-client-bridge`. It does **not** require Herdr to have been installed before the user invokes `herdr --remote` interactively.

`prepare_remote_herdr` performs the following:

1. Search remote `PATH` and known direct, Homebrew, mise, and Nix locations for a matching version.
2. Check the default direct-install location at `~/.local/bin/herdr`.
3. If no matching executable exists, prompt the interactive user to install one.
4. Resolve the correct binary from `HERDR_REMOTE_BINARY`, the current local binary when platforms match, or the release manifest for the remote platform.
5. Stream the binary through SSH into a temporary path and atomically install it at `~/.local/bin/herdr`.
6. Verify that the installed executable reports the current client version.

A non-interactive invocation does not modify the host: `confirm_remote_install` fails with guidance to rerun from an interactive terminal. Thus the precise contract is:

> A matching server-side Herdr executable is required to run, but an interactive `herdr --remote` can bootstrap it automatically; preinstallation is optional.

The QUIC design preserves this behavior. SSH bootstrap must still locate/install the server executable before requesting a QUIC endpoint.

### Current wire protocol

`src/protocol/wire.rs` defines an exact-version bincode protocol over a four-byte little-endian length prefix. Current `PROTOCOL_VERSION` is 16. `Hello` carries terminal and cell dimensions, requested render encoding, client keybindings, and launch mode. `Welcome` accepts or rejects the exact version and selects an encoding.

Relevant render forms:

- `ServerMessage::Frame(FrameData)` is a complete semantic frame containing cells, dimensions, cursor, hyperlinks, and graphics bytes.
- `ServerMessage::Terminal(TerminalFrame)` contains server-diffed ANSI with a monotonic per-client sequence and `full` flag.
- `ServerMessage::Graphics` carries additional host Kitty bytes.

The current client writes `TerminalFrame.bytes` directly and does not inspect `seq` or `full`, because the existing local/SSH stream is reliable and ordered.

### Current client and writer lifecycle

`src/server/client_transport.rs` accepts a local socket, allocates an ephemeral `u64` connection ID, reads `Hello`, creates blocking reader/writer threads, and forwards events into the headless server.

`ClientWriterQueue` currently has:

- an unbounded `VecDeque<Vec<u8>>` for control messages;
- one optional pending render buffer;
- control priority when the writer asks for its next item.

This prevents multiple queued render frames, but it does not solve all network backpressure:

- once the writer dequeues a frame and blocks in `write_all`, it cannot send later control messages;
- the unbounded control queue can grow while the writer blocks;
- the render baseline is committed when a serialized frame enters the writer queue, not when the client applies it;
- bytes already handed to a TCP/SSH stream cannot be superseded;
- the client returns `ConnectionLost` rather than reconnecting.

These are explicit refactoring targets, not behavior to reproduce in QUIC.

### Current rendering and PTY separation

The current server already has the correct foundational separation:

- pane PTY actors continuously feed output into libghostty;
- PTY activity marks rendering dirty and notifies the render loop;
- `ClientRenderState` stores the last semantic frame or ANSI blit baseline;
- per-client render output is bounded to one pending item;
- a full render is deferred when that slot is occupied;
- a newly connected client starts with an empty baseline, producing a complete initial screen.

The headless server’s retained render path attempts dirty patches and falls back to full virtual rendering. `ClientConnection::request_full_redraw` already resets the rendering baseline and graphics surface state.

The new transport must preserve the PTY/network separation and move the “no client capacity” decision earlier so the server avoids repeatedly building and serializing frames for a sleeping client.

This separation is necessary but not sufficient to claim that pane applications never block today. Open issue `#1295` reports alt-screen full-viewport repaint writes stalling in the existing PTY parse/composite path. Resumable transport must prove that network health adds no additional backpressure, while avoiding an unrelated global rewrite of local PTY behavior unless maintainers choose to address that issue in the same work. Network fault tests must distinguish existing parser/compositor saturation from transport-induced blocking.

### Current multi-client intent

At the inspected revision:

- every non-direct app `ClientConnected` sets `foreground_client_id` to the new client;
- client interaction promotes that client through `promote_client_to_foreground`;
- when a foreground client disappears, `promote_latest_remaining_client` selects the latest remaining app client;
- foreground client size, focus, cell size, theme, and keybindings drive shared runtime state;
- foreground-driven resize forces a fresh frame for all clients.

The resumable design chooses “newest client wins” to align with this demonstrated intent. The implementing agent must inspect the then-current code and recent multi-client changes before changing this behavior. If maintainer intent has changed, that discrepancy is a product decision and must be raised before implementation.

## Proposed architecture

### Bootstrap sequence

```text
Mac client                        Fedora over SSH                   Fedora Herdr server
    |                                   |                                  |
    | ssh authenticated bootstrap      |                                  |
    |---------------------------------->| local API/bootstrap request      |
    |                                   |--------------------------------->|
    |                                   | endpoint, fingerprint, token     |
    |<----------------------------------|<---------------------------------|
    |                                                                      |
    | QUIC TLS 1.3, pinned fingerprint, ALPN herdr/1                       |
    |--------------------------------------------------------------------->|
    | capability token + Hello                                             |
    |--------------------------------------------------------------------->|
    | Welcome + initial semantic generation                                |
    |<---------------------------------------------------------------------|
```

A proposed internal command is:

```text
herdr [--session NAME] remote-quic-bootstrap
```

The exact private command name is not part of the public contract. It should communicate with the already-running local server over its protected Unix socket rather than independently opening another session authority.

Bootstrap returns a versioned record containing at least:

```text
protocol version
server instance ID
UDP candidate endpoints
certificate SHA-256 fingerprint
capability token
token expiry
SSH fallback availability
```

Do not print the token in ordinary logs or diagnostics.

### TLS identity

QUIC includes TLS 1.3. The server process generates one self-signed certificate and key pair when enabling its QUIC endpoint. The key remains in memory and dies with the process.

The authenticated SSH bootstrap carries the exact certificate fingerprint to the client. The client uses a custom verifier that accepts only that fingerprint. Public trust roots, DNS validation, and ACME are unnecessary.

After TLS authenticates the server, the client presents the high-entropy capability token inside the encrypted connection. The server stores only a hash where practical and validates scope, server instance, session, client mode, and expiry.

Use a dedicated ALPN such as `herdr/1`.

No 0-RTT data is required. Specifically, keys, paste, approvals, pane operations, takeover, detach, and other mutations must never be accepted as TLS early data.

### UDP endpoint and fallback

Add a configurable unprivileged port range. Tentative configuration names, subject to repository conventions:

```toml
[remote]
transport = "auto"           # auto | quic | ssh
quic_port_range = "48000-48100"
quic_idle_timeout_seconds = 86400
ssh_fallback = true
```

The server binds one port from the range and advertises reachable address candidates through SSH. The client performs IPv6/IPv4 path attempts with a short bounded deadline. If no candidate validates, it immediately uses the current SSH bridge.

ProxyJump, private hosts, cloud firewalls, and networks that block arbitrary UDP are normal fallback cases, not fatal errors. A host-level privileged gateway is explicitly deferred.

After a live QUIC failure:

1. Attempt recovery/path migration on the original connection while it remains valid.
2. If the connection is dead, open a fresh QUIC connection using the process-lifetime fingerprint and token.
3. If credentials or endpoint are stale, rerun SSH bootstrap.
4. If UDP remains unavailable, attach through the SSH stream.
5. Initially avoid rapid automatic transport oscillation after fallback. Do not foreclose a lazy background QUIC re-probe and in-place upgrade after the network stabilizes; its interval, hysteresis, and handover semantics remain a later measured decision.

### QUIC stack selection gate

Quinn is an obvious Rust candidate, but library selection is not made by this document. Before committing to a stack, the implementing agent must demonstrate on supported platforms:

- NAT rebinding with the same connection ID;
- client local-address/socket change or a documented transparent fresh-connection fallback;
- configurable long idle timeout;
- stream reset and `STOP_SENDING` behavior;
- independent progress between streams;
- bounded memory under a non-reading peer;
- rustls certificate pinning hooks;
- behavior on macOS sleep/wake and Linux servers.

If the selected library cannot provide required path migration on macOS, this is a blocker to raise, not a reason to silently redefine “resumable” as repeated SSH attachment.

### Connection and session lifetimes

Distinguish three concepts:

1. QUIC connection migration: same cryptographic connection and stream state, new network path.
2. TLS session resumption: a faster new cryptographic connection; not old stream restoration. Not required initially.
3. Herdr session reattachment: a new transport connects to the same server-owned PTYs and state.

While the original QUIC connection remains alive, QUIC owns reliability and retransmission. A source IP/port change does not require an application reattach if path migration succeeds.

When the original connection expires, Herdr opens a new one and attaches to current server state. It does not reconstruct old stream offsets or replay uncertain input. The client briefly reports that it reconnected and input during the interruption may have been lost.

The process-lifetime token permits fresh QUIC connections without SSH as long as the same remote Herdr server process remains alive. A changed certificate, server instance, rejected token, or expired token triggers automatic SSH bootstrap.

### Control stream

Use one long-lived bidirectional control/action stream for small ordered messages:

Client to server:

- input received while connected and current;
- resize and cell-size state;
- focus state;
- attach/observe/control requests;
- scroll commands;
- clipboard image metadata or references;
- explicit detach;
- `SyncRequest` after wake/recovery.

Server to client:

- welcome/status;
- render-stream generation announcements;
- mouse-capture state;
- window title;
- local configuration refresh;
- bounded notification summaries;
- shutdown/close reason.

A bidirectional QUIC stream has independent ordered byte sequences in each direction, so stalled server output does not block client input. Large payloads must not be placed on this stream.

Do not reproduce the current unbounded reliable control queue. Classify output as current state or ephemeral event:

| Output | Stalled-client policy |
| --- | --- |
| Window title | Keep latest. |
| Mouse-capture mode | Keep latest. |
| Prefix input-source state | Keep latest. |
| Agent status | Reconstruct from current server state. |
| Sound | Drop while disconnected. |
| Toast | Drop or summarize current attention state. |
| OSC 52 clipboard write | Drop while disconnected; replay hours later is surprising. |
| Shutdown | Close transport. |

Every queue must have a documented bound and overflow policy.

### Rendering streams and generations

Retain the existing `TerminalFrame`/Terminal-ANSI representation. A render generation owns one ordered unidirectional QUIC stream. Its stream header carries:

```text
connection generation
render generation
```

The stream then carries contiguous records:

```text
server state revision
TerminalFrame { seq, width, height, full, bytes }
```

The first record in every generation is a full redraw. Steady-state records use the existing server-side `BlitEncoder` diff and must arrive in contiguous `seq` order because each diff depends on the preceding display state. The client validates connection generation, render generation, and frame sequence before writing bytes; this replaces today’s unconditional byte write without creating another render representation.

Recovery calls `ClientRenderState::reset_baseline`, causing the existing encoder to emit `full=true`, `CSI 2J`, cursor-home, and all cells. Raw ANSI byte equality is not required; canonical displayed state is.

Maintain at most:

- one active render-generation stream;
- one serialized/in-flight frame on that stream;
- one dirty/current-state indication.

Do not open/reset a stream for every PTY update while the client is asleep. When a generation makes no useful progress and stale diffs would delay current state, abandon the entire generation, not an individual dependent diff. Stop building frames and leave the client dirty until recovery.

Recovery on the same QUIC connection:

1. Client sends `SyncRequest` after path recovery, focus/wake detection, or excessive frame staleness.
2. Server resets the obsolete generation stream or client sends `STOP_SENDING`.
3. Server resets the ANSI baseline and increments render generation.
4. Server opens one new ordered render stream.
5. Server sends the current state as the generation’s first `full=true` frame.
6. Client clears/rebuilds its host display from that full frame, then accepts contiguous diffs.
7. Client ignores any record or stream from an older generation that completes late.

Recovery on a fresh QUIC or SSH connection follows the same logical steps, starting with an empty render baseline.

QUIC packet number gaps are not an application concern. QUIC retransmits stream ranges in new packets and retains accepted reliable bytes until acknowledged, reset, or connection close. If its send buffer fills, the QUIC write waits; only the network render task may wait.

### Mandatory Terminal-ANSI performance validation and escalation gate

Current `herdr --remote` deliberately uses server-diffed `TerminalAnsi` for network efficiency. Keep it. The initial implementation benchmarks this existing representation over SSH and QUIC under the required network profiles; it does not implement speculative semantic, compressed-semantic, delta, or hybrid transports for comparison.

Before the QUIC protocol is finalized, validate current server-diffed ANSI, including forced full-redraw recovery. If it misses a requirement, first measure whether the problem is transport scheduling, stale-frame coalescing, BlitEncoder output, graphics interleaving, or QUIC overhead. Do not respond by silently creating a second wire format.

Representative workloads must include:

- typing in a shell;
- cursor-only movement;
- an agent spinner/status update;
- moderate scrolling output;
- a full-screen agent/editor repaint;
- split panes and sidebar updates;
- Kitty image placement separately from image bytes.

Network profiles must include a documented 3G-quality profile with bandwidth, RTT, jitter, and loss. The benchmark must report:

- delivered frames per second;
- bytes per rendered frame and per second;
- p50/p95/p99 input-to-visible latency;
- frame serialization and client application CPU;
- memory held by stalled/superseded frames;
- recovery keyframe latency.

The performance target is 60 frames per second for workloads where the source produces 60 meaningful frames per second on the agreed 3G profile, without input/control starvation. Full-screen random-cell churn may be physically bandwidth-limited and must be reported separately rather than hidden.

If existing Terminal ANSI cannot reasonably meet the 60 fps/input-latency target or cannot recover exactly after a forced full redraw, the implementing agent must stop, present the measurements and root cause, and ask the user whether to optimize the existing path or reopen alternative render encodings. Correct recovery remains mandatory.

### PTY independence and no-app-blocking invariant

The critical invariant is:

> PTY consumption and terminal emulation never wait for an attached client or network transport.

The dataflow must remain:

```text
pane process -> PTY actor -> libghostty terminal state -> dirty revision
                                                       -> optional render scheduler
                                                       -> QUIC/SSH publisher
```

There must be no network backpressure edge from the publisher to the PTY actor.

While a client is asleep:

1. Continue draining PTY bytes.
2. Feed every byte into libghostty; terminal escape streams are stateful and cannot be sampled safely.
3. Maintain current primary/alternate screen, modes, cursor, bounded scrollback, and bounded image storage.
4. Increment/mark a terminal or render revision dirty.
5. Do not repeatedly build or serialize frames without client capacity.
6. Drop/coalesce client-only transient events according to explicit policy.

The only acceptable pane-process blocking is natural PTY backpressure if the application emits bytes faster than Herdr/libghostty can parse them. A sleeping Mac, full QUIC send window, blocked render stream, or client-side frame application must not block the process.

A watch/latest-value primitive or revision counter is preferable to an event queue. If terminal state changes while a frame is being built, leave dirty set and schedule one later render when capacity exists.

### Input semantics

Within a live QUIC connection, use QUIC’s reliable ordered control stream. Do not add TCP-like sequence windows or retransmission logic.

Render staleness alone does not stop input. While the QUIC connection remains alive, input continues on the independent reliable control direction even if a render stream is stalled, delayed, or awaiting a fresh generation. The client should indicate stale rendering without pretending the control path is dead.

Only when the connection is presumed dead or the client has entered reconnecting state:

- stop forwarding pane input;
- preserve only local detach/quit/reconnect controls;
- communicate stale/reconnecting state using existing Herdr modal/status language and components, not a transport-specific one-off screen;
- resume input after a fresh connection and complete frame are established.

When a QUIC connection expires, do not replay uncertain input on the new connection. This intentionally permits a final pre-expiry key to be lost. It prevents destructive duplicate Enter, paste, approval, or Ctrl-C events and matches the stated product preference for long outages.

If future requirements demand seamless input commit across destroyed connections, that is a separate application-level idempotency protocol and requires a new decision.

### Foreground and multi-client behavior

A fresh/recovered client becomes newest and therefore foreground, matching current connection behavior. It drives effective size, host cell size, theme, local keybindings, and focus state.

The old connection must be fenced when a replacement is accepted so a partitioned path cannot later send input concurrently. A monotonically increasing connection generation associated with the process-lifetime capability is sufficient; messages from older generations are rejected.

This connection generation is a fencing mechanism, not a retransmission sequence.

The implementing agent must re-read the current foreground-client code and tests immediately before implementation. Recent maintainer work in this area takes precedence over this snapshot. Any conflict between “newest client wins” and demonstrated current intent must be raised to the user as a blocker.

### Kitty graphics

Current `FrameData` can contain up to 32 MiB when graphics are present, and current Terminal-ANSI preparation inserts graphics bytes into the frame stream. That is unsuitable for control responsiveness under loss.

Use separate reliable unidirectional streams for large image resources. Frames reference content-addressed image IDs/hashes and placement state. Client and server exchange a bounded cache inventory after reconnect, retransmitting only missing visible resources.

An interrupted or obsolete image stream can be reset without delaying text/control rendering. Placement changes must not retransmit unchanged PNG content. Existing host image ID isolation, clipping, scrolling, and cache behavior in `src/kitty_graphics.rs` remains the semantic reference.

Text recovery must work before all image resources arrive; the client then converges to the exact image placement once required resources complete.

## Implementation shape

### Protocol changes

`src/protocol/wire.rs` needs transport-neutral envelopes for:

- QUIC/bootstrap record and exact-version handshake;
- server instance and connection generation;
- semantic render generation and server state revision;
- `SyncRequest`;
- render stream headers;
- image resource inventory/chunks;
- explicit transport close/recovery reason.

Preserve the repository’s exact-version `PROTOCOL_VERSION` policy. The implementation must compare with the latest release before bumping and follow the documented deliberate bump rule. Existing render-encoding selection remains part of that exact-version schema; do not generalize it into capability bits. If rolling feature negotiation becomes necessary for deployment, graphics resources, or fallback, stop and obtain an explicit maintainer/user decision because that changes the project’s protocol-evolution philosophy.

### Transport abstraction

Refactor `src/server/client_transport.rs` so the headless server receives neutral connection events rather than owning cloneable blocking `LocalStream` assumptions. Preserve the local Unix-socket implementation for local clients and SSH bridge fallback.

The abstraction must expose separate operations for:

- receive control;
- send bounded/coalesced control;
- publish/supersede render generation;
- transfer/cancel bulk resource;
- report connected/degraded/closed state.

Avoid a god object. Keep network transport, logical client state, render scheduling, and PTY runtime separate in accordance with `AGENTS.md`.

### Client changes

`src/client/mod.rs` currently exits on `ServerDisconnected`. Introduce a reconnect state machine:

```text
Connected
-> PathRecovering
-> FreshQuicConnecting
-> SshRebootstrap
-> SshFallbackConnecting
-> Connected
```

While recovering, retain the last complete frame and communicate recovery through existing Herdr modal/status patterns. Do not invent a transport-specific screen, clear the terminal to blank, or accept pane input. Explicit detach exits without reconnecting.

Client state must track current connection/render generation and reject late obsolete frames.

### Server client state

Extend `ClientConnection` or add a transport-neutral logical-client layer with:

- server instance and connection generation;
- transport state;
- current render generation/revision;
- one in-flight render and dirty flag;
- bounded/coalesced control state;
- QUIC credential association;
- newest-client foreground semantics.

Do not persist this state to disk across an ordinary cold restart. Live handoff/self-update is different: inspect the existing handoff contract and transfer the ephemeral certificate/key, token/fencing state, endpoint metadata, and logical client state needed for fresh attachment where feasible. A QUIC library’s live connection object need not survive process replacement, but remote clients must reconnect automatically without an extra visible/manual disruption. If this cannot match current SSH-remote update behavior, treat it as a blocker.

### Dependencies

Likely additions include a Rust QUIC/TLS stack and certificate generation. Quinn/rustls/rcgen are candidates, not decisions. Enable only required Tokio network/I/O features. Any new dependency requires a repository-appropriate justification and license/security review.

## Testing strategy

### Principles

Use deterministic tests at three layers:

1. In-process protocol/transport tests for precise, stream-aware scheduling.
2. Linux network-namespace tests for real UDP loss, delay, reordering, bandwidth, MTU, blackholes, and address changes.
3. End-to-end terminal-state equivalence tests against a perfect-connectivity direct/SSH-only baseline.

Do not rely on wall-clock sleeps when a test hook, barrier, or virtual clock can express the condition deterministically. Wall-clock adverse-network tests should have generous bounded deadlines and emit transport diagnostics on failure.

### Non-remote compatibility gate

Before adding QUIC, preserve characterization tests for every shared component touched by the refactor:

- ordinary local `herdr` client over the Unix socket still negotiates its current default encoding and renders byte/cell-equivalently;
- `ssh fedora` followed by remote-host `herdr` is unchanged;
- direct terminal attach remains unchanged;
- local multi-client foreground, resize, focus, and keybinding behavior remain unchanged;
- server APIs/plugins and local control sockets retain protocol and permissions;
- local idle CPU, PTY throughput, frame latency, and memory do not regress;
- builds without invoking `--remote` do not bind UDP, create TLS material, open firewall-visible listeners, or start reconnect tasks.

QUIC-specific behavior should be behind the remote transport boundary or inactive feature path. Shared transport abstractions must preserve current local code paths rather than routing local clients through QUIC-shaped queues merely for architectural uniformity.

### Unit and in-process integration tests

Cover:

- certificate fingerprint pin success/failure;
- wrong, expired, revoked, and cross-session tokens;
- no token or private key in logs;
- old connection generation fenced after newest-client replacement;
- `SyncRequest` resets old render baseline and forces a complete generation;
- late completion of an older render generation is ignored;
- control queues remain bounded and coalesce state;
- sounds/toasts/OSC 52 follow disconnected-client policy;
- explicit detach does not reconnect;
- connection expiry does not replay uncertain input;
- fallback transition retains the last complete screen;
- full-frame and graphics size limits apply before and after decompression;
- exact protocol-version mismatch gives an actionable error and matching-binary bootstrap path.

### Testing one stream stalled while another continues

Docker/Podman network shaping alone cannot reliably target a QUIC stream. QUIC stream IDs and frames are encrypted and multiple streams may share the same UDP packet and five-tuple. `tc netem`, nftables, and ordinary UDP proxies can impair a whole connection but cannot say “delay render stream 7 while delivering control stream 3.”

Use two deterministic methods:

1. Add a test-only transport gate around stream send/receive. Hold the render stream for ten seconds of virtual or controlled time while allowing the control stream to progress. Assert that a notification/control message is received and that client input reaches the server while render remains blocked.
2. In a real loopback QUIC test, deliberately stop polling/reading one render stream until its stream flow-control window is exhausted while continuing to poll the control stream. Assert continued control progress and bounded sender memory. Resume the render reader after ten seconds and verify eventual completion or intentional reset.

The test must include both outcomes:

- delayed render eventually arrives ten seconds after control and is applied if still current;
- delayed render is superseded, reset, and ignored after a newer generation arrives.

### Podman/Linux network mayhem

The available container engine is Podman on `agent@black-lodge`; Docker is not available on the current macOS workstation. Run real network-shaping tests there or in Linux CI.

Podman provides sufficient Linux network namespaces for whole-connection adverse behavior when using a dedicated router container or namespace with `CAP_NET_ADMIN` and `tc netem`. Rootless Podman may not permit the necessary qdisc/nftables operations; use rootful Podman, an approved privileged router container, or direct Linux network namespaces in CI.

Proposed topology:

```text
client container -- client network -- router namespace -- server network -- Fedora server container
```

The router enables forwarding and applies independent ingress/egress qdiscs. Test profiles include:

- latency and jitter;
- random and burst packet loss;
- packet duplication and reordering;
- bandwidth limits approximating documented 3G quality;
- UDP blackhole followed by restoration;
- asymmetric loss and bandwidth;
- MTU reduction and PMTU blackhole behavior;
- client network disconnect/reconnect with a new IP and source port;
- server endpoint unreachable while SSH remains reachable;
- UDP blocked from the beginning to prove prompt SSH fallback;
- pause the client container longer than the QUIC flow-control/send buffers while the pane emits output;
- connection idle timeout followed by fresh QUIC attachment;
- server process restart followed by SSH rebootstrap and new certificate pin;
- live self-update/handoff with attached QUIC and SSH-fallback clients, asserting no manual reattach and no loss of pane state.

`tc netem delay 10s` can test an entire path arriving ten seconds late, but not one encrypted QUIC stream independently. Stream-selective delay remains an in-process transport test.

Container tests must record seed, qdisc configuration, endpoint addresses, QUIC connection IDs/generations, frame generations, and timing so failures are reproducible.

### Proving the Fedora pane does not block

Create a deterministic fixture program that:

1. Writes substantially more output than the configured QUIC stream/connection flow-control windows and application send buffers.
2. Records progress to a side-channel file or local server API that does not depend on the client transport.
3. Exits or reaches a known marker while the client container is paused or its UDP path is blackholed.

Assertions:

- the fixture reaches the marker before client recovery;
- the Herdr PTY reader continues advancing its terminal revision;
- server memory remains within documented scrollback, image, render, and control bounds;
- no network task owns a lock needed by PTY parsing;
- after recovery, one fresh frame represents final current state;
- old output beyond configured scrollback may be discarded, but the current display and retained scrollback are correct.

Also run a parser-saturation control test. If output exceeds libghostty parsing capacity, PTY backpressure is expected; distinguish that from network-induced blocking.

## Exact end-to-end screen equivalence

### Required invariant

After recovery or eventual delivery, at the same server state revision and client geometry:

> The QUIC client’s displayed terminal state is exactly identical to the displayed state of an equivalent direct/local-socket or current SSH-only Herdr client under perfect connectivity.

Do not compare raw ANSI byte sequences. A full redraw and a minimal diff can be byte-different while displaying identically.

### Deterministic fixture

Build a fixture pane application with scripted milestones covering:

- plain output and scrolling;
- Unicode, combining characters, wide glyphs, and grapheme clusters;
- indexed/default/truecolor foreground and background;
- bold, italic, strike, inverse, and underline variants;
- cursor visibility, position, and shape;
- primary/alternate screen entry and exit;
- erase, insert/delete, wrapping, and resize;
- OSC 8 hyperlinks;
- OSC 52 behavior according to disconnected policy;
- mouse/focus/bracketed-paste modes;
- multiple panes, sidebar, popups, and foreground-client resize;
- Kitty images, placements, clipping, deletion, and tab/workspace switches when graphics are enabled.

Remove timestamps, random IDs, and nondeterministic process output. Drive identical input scripts and terminal sizes into isolated named sessions using the same Herdr build and configuration.

### Baselines

For each fixture milestone, collect:

1. Perfect direct/local-socket semantic client output.
2. Perfect current SSH-only/`herdr --remote` Terminal-ANSI output.
3. QUIC output under each adverse-network scenario.

Use the same columns, rows, cell pixel size, theme reports, keybindings, graphics setting, and foreground-client ordering.

### Canonical comparison

Apply each client’s emitted output to a fresh host-terminal model initialized identically. Because the target frontend in the motivating setup is Ghostty and Herdr vendors libghostty-vt, a libghostty-based capture is an appropriate primary display oracle. Avoid relying only on that shared parser by also comparing the server’s semantic `FrameData` at the target state revision.

Compare exactly:

- grid dimensions and every displayed grapheme/cell;
- foreground/background colors and modifiers;
- wide-cell continuation/skip state;
- hyperlinks;
- cursor position, visibility, and shape;
- current primary/alternate screen result;
- window title and host mouse-capture state where applicable;
- Kitty image content hashes, placements, crop, z-index, and visible geometry;
- every retained scrollback viewport after deterministic server-side scroll commands.

For graphics, add a Ghostty-driven screenshot comparison or deterministic image-surface hash as a release-level check; text-grid equality alone is insufficient.

### State barrier

Do not use “sleep and hope the frame arrived.” Each deterministic server update increments a test-visible server state revision. Render generations include the revision they represent. The test client exposes the latest fully applied revision through a test sink or test-only diagnostic interface.

The comparison runs only after:

```text
fixture reached milestone revision R
AND baseline client applied a frame at or after R
AND faulted client recovered and applied a complete frame at or after R
AND required visible image resources for that generation completed
```

If server state changes after frame construction, the dirty revision remains set and the barrier waits for the next complete generation.

### Adverse equivalence cases

At minimum prove exact convergence after:

- 30-second full UDP blackhole;
- client sleep/pause while output exceeds transport buffers;
- client source IP/port change;
- delayed render stream with control arriving ten seconds earlier;
- obsolete render reset and newer generation delivery;
- random loss/reordering/jitter;
- bandwidth-constrained 3G profile;
- QUIC idle expiry and fresh connection;
- UDP failure followed by SSH fallback;
- interrupted Kitty image transfer followed by resource recovery.

Any test that only proves “the client reconnected” without exact canonical display equality is insufficient.

## Performance and resource acceptance

The implementation must establish explicit bounds for:

- dormant QUIC connections per server;
- per-connection send/receive memory;
- control queue bytes/items;
- one in-flight render and serialized frame size;
- image transfer concurrency and cache size;
- bootstrap/token count and lifetime;
- reconnect backoff;
- render CPU while a client is stalled.

Acceptance includes:

- no repeated frame construction for a transport with no capacity;
- no unbounded control or image queue;
- input/control progress independent of a stalled render stream;
- prompt SSH fallback when UDP is blocked;
- no pane-process slowdown attributable to sleeping clients;
- Terminal-ANSI performance gate completed before QUIC protocol lock-in;
- exact eventual screen equivalence under all required fault profiles.

## Rollout order

1. Add deterministic frame/state revision and canonical equivalence harnesses before transport changes.
2. Benchmark existing Terminal ANSI over SSH and QUIC, including forced full-redraw recovery; raise measured failures before considering another encoding.
3. Refactor current local/SSH transport behind bounded control/render abstractions without changing behavior.
4. Add automatic reconnect over the existing SSH bridge and fresh full-screen recovery.
5. Spike candidate QUIC stacks for macOS path migration, stream isolation, TLS pinning, and bounded backpressure; raise blockers.
6. Add per-server-process TLS identity, capability issuance, UDP port-range listener, and SSH bootstrap response.
7. Add QUIC control transport and fresh-connection session attachment.
8. Add replaceable render generations and stalled-client dirty-state scheduling.
9. Add graphics resource streams and exact graphics convergence tests.
10. Run Podman/netem mayhem on black-lodge and Linux CI, then manual Mac sleep/network-switch validation.

## Design gaps and implementation blockers

The implementing agent must raise, not silently decide, the following if encountered:

1. **Encoding performance:** existing Terminal ANSI fails the 60 fps/3G-quality gate, materially worsens input latency, or cannot converge exactly after full redraw recovery. Present measurements and ask before implementing any alternative encoding.
2. **QUIC migration support:** selected Rust stack cannot preserve or transparently replace connections across macOS interface changes.
3. **Maintainer multi-client intent:** current code no longer follows newest-client-wins semantics.
4. **UDP operations:** port-range deployment is unacceptable for common target hosts and a stable gateway becomes necessary.
5. **Exact graphics equivalence:** current graphics representation cannot reconstruct visible Ghostty output after stream reset without embedding large resources in frames.
6. **PTY isolation:** any proposed channel, lock, or queue allows network backpressure into PTY parsing.
7. **Fallback compatibility:** protocol changes would prevent current SSH remote operation during rollout or version skew.
8. **Resource bounds:** selected QUIC stack cannot expose/enforce bounded buffering for non-reading peers.
9. **Live handoff/update continuity:** per-process TLS identity or QUIC endpoint ownership makes self-update visibly worse than the current SSH bridge. Resolve transfer versus coordinated rebootstrap before shipping.
10. **Protocol evolution philosophy:** implementation appears to require capability-bit or rolling feature negotiation instead of the current exact-version match and bootstrap-to-matching-binary policy. Raise the concrete need and compatibility tradeoff before changing it.

## Remaining intentional uncertainties

These are implementation details, not unresolved product behavior, unless evidence forces escalation:

- exact QUIC crate and version;
- exact high UDP port range and configuration field names;
- test-only state-revision exposure mechanism;
- image content hash and cache protocol;

Every choice must preserve the invariants and validation contract above.
