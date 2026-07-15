//! Local Unix-socket proxy that keeps the thin client alive while remote transports recover.

use std::io;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::debug;

use super::quic::{ConnectParams, QuicSession, ResourceCache, SessionExit};
use super::unix::{
    apply_managed_ssh_options, remote_bridge_command, remote_quic_candidates,
    request_remote_quic_bootstrap, ManagedSshOptions, RemoteHerdr,
};
use crate::config::{RemoteConfig, RemoteTransportConfig};
use crate::protocol::{
    ClientKeybindings, ClientLaunchMode, ClientMessage, RemoteBootstrapRecord,
    RemoteTransportStatus, RenderEncoding, ServerMessage, MAX_FRAME_SIZE, MAX_GRAPHICS_FRAME_SIZE,
    PROTOCOL_VERSION, REMOTE_QUIC_ID_BYTES,
};

const ACCEPT_POLL: Duration = Duration::from_millis(50);
const SOCKET_PERMISSION_MODE: u32 = 0o600;
const INPUT_QUEUE_ITEMS: usize = 64;
const OUTPUT_QUEUE_ITEMS: usize = 16;
const SSH_RECONNECT_DELAY: Duration = Duration::from_secs(1);
const SSH_REBOOTSTRAP_DEADLINE: Duration = Duration::from_secs(15);
const SSH_REBOOTSTRAP_RETRY_DELAY: Duration = Duration::from_millis(250);

pub(super) struct BridgeConfig {
    pub(super) target: String,
    pub(super) remote_herdr: RemoteHerdr,
    pub(super) local_socket: PathBuf,
    pub(super) session_name: String,
    pub(super) ssh_options: Option<ManagedSshOptions>,
    pub(super) remote_config: RemoteConfig,
    pub(super) logical_client_id: [u8; REMOTE_QUIC_ID_BYTES],
    pub(super) ssh_hostname: Option<String>,
    pub(super) bootstrap: Option<RemoteBootstrapRecord>,
    pub(super) bootstrap_error: Option<String>,
}

pub(super) struct ResumableRemoteBridge {
    local_socket: PathBuf,
    should_stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ResumableRemoteBridge {
    pub(super) fn start(config: BridgeConfig) -> io::Result<Self> {
        let _ = std::fs::remove_file(&config.local_socket);
        let listener = UnixListener::bind(&config.local_socket)?;
        crate::ipc::restrict_socket_permissions(&config.local_socket, SOCKET_PERMISSION_MODE)?;
        listener.set_nonblocking(true)?;

        let local_socket = config.local_socket.clone();
        let should_stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&should_stop);
        let thread = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        if let Err(error) = stream.set_nonblocking(false) {
                            eprintln!(
                                "herdr: remote proxy failed to prepare client socket: {error}"
                            );
                            continue;
                        }
                        if let Err(error) = bridge_connection(stream, &config, &thread_stop) {
                            eprintln!("herdr: remote proxy failed: {error}");
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        thread::sleep(ACCEPT_POLL);
                    }
                    Err(error) => {
                        eprintln!("herdr: remote proxy listener failed: {error}");
                        break;
                    }
                }
            }
        });

        Ok(Self {
            local_socket,
            should_stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ResumableRemoteBridge {
    fn drop(&mut self) {
        self.should_stop.store(true, Ordering::Release);
        let _ = std::fs::remove_file(&self.local_socket);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
struct HelloState {
    cols: u16,
    rows: u16,
    cell_width_px: u32,
    cell_height_px: u32,
    requested_encoding: RenderEncoding,
    keybindings: ClientKeybindings,
}

impl HelloState {
    fn from_message(message: ClientMessage) -> Result<Self, String> {
        let ClientMessage::Hello {
            version,
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            requested_encoding,
            keybindings,
            launch_mode,
        } = message
        else {
            return Err("expected local client Hello as first message".to_owned());
        };
        if version != PROTOCOL_VERSION {
            return Err(format!(
                "local client protocol {version} does not match proxy protocol {PROTOCOL_VERSION}"
            ));
        }
        if launch_mode != ClientLaunchMode::App {
            return Err("remote proxy accepts full app clients only".to_owned());
        }
        if requested_encoding != RenderEncoding::TerminalAnsi {
            return Err("remote proxy requires Terminal-ANSI rendering".to_owned());
        }
        Ok(Self {
            cols,
            rows,
            cell_width_px,
            cell_height_px,
            requested_encoding,
            keybindings,
        })
    }

    fn message(&self) -> ClientMessage {
        ClientMessage::Hello {
            version: PROTOCOL_VERSION,
            cols: self.cols,
            rows: self.rows,
            cell_width_px: self.cell_width_px,
            cell_height_px: self.cell_height_px,
            requested_encoding: self.requested_encoding,
            keybindings: self.keybindings.clone(),
            launch_mode: ClientLaunchMode::App,
        }
    }

    fn apply_resize(&mut self, message: &ClientMessage) {
        if let ClientMessage::Resize {
            cols,
            rows,
            cell_width_px,
            cell_height_px,
        } = message
        {
            self.cols = *cols;
            self.rows = *rows;
            self.cell_width_px = *cell_width_px;
            self.cell_height_px = *cell_height_px;
        }
    }
}

struct InputRouter {
    state: Mutex<InputRouterState>,
    detached: AtomicBool,
}

struct InputRouterState {
    hello: HelloState,
    active: Option<mpsc::Sender<ClientMessage>>,
}

impl InputRouter {
    fn new(hello: HelloState) -> Self {
        Self {
            state: Mutex::new(InputRouterState {
                hello,
                active: None,
            }),
            detached: AtomicBool::new(false),
        }
    }

    fn hello(&self) -> HelloState {
        lock(&self.state).hello.clone()
    }

    fn set_active(&self, sender: mpsc::Sender<ClientMessage>) {
        let resize = {
            let mut state = lock(&self.state);
            state.active = Some(sender.clone());
            ClientMessage::Resize {
                cols: state.hello.cols,
                rows: state.hello.rows,
                cell_width_px: state.hello.cell_width_px,
                cell_height_px: state.hello.cell_height_px,
            }
        };
        let _ = sender.try_send(resize);
    }

    fn clear_active(&self) {
        lock(&self.state).active = None;
    }

    fn route(&self, message: ClientMessage) {
        if matches!(message, ClientMessage::Detach) {
            self.detached.store(true, Ordering::Release);
        }
        let sender = {
            let mut state = lock(&self.state);
            state.hello.apply_resize(&message);
            state.active.clone()
        };
        let Some(sender) = sender else {
            return;
        };
        match sender.try_send(message) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(
                ClientMessage::Input { .. }
                | ClientMessage::InputEvents { .. }
                | ClientMessage::ClipboardImage { .. },
            )) => {
                // Never replay pane input that accumulated behind a degraded path.
            }
            Err(mpsc::error::TrySendError::Full(message)) => {
                if sender.blocking_send(message).is_err() {
                    self.clear_active();
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => self.clear_active(),
        }
    }

    fn mark_closed(&self) {
        self.detached.store(true, Ordering::Release);
        self.clear_active();
    }

    fn is_detached(&self) -> bool {
        self.detached.load(Ordering::Acquire)
    }
}

fn bridge_connection(
    mut stream: UnixStream,
    config: &BridgeConfig,
    should_stop: &AtomicBool,
) -> io::Result<()> {
    let hello_message: ClientMessage = crate::protocol::read_message(&mut stream, MAX_FRAME_SIZE)
        .map_err(|error| io::Error::other(error.to_string()))?;
    let hello = HelloState::from_message(hello_message).map_err(io::Error::other)?;
    let router = Arc::new(InputRouter::new(hello));

    let (output_tx, mut output_rx) = mpsc::channel::<ServerMessage>(OUTPUT_QUEUE_ITEMS);
    let mut output_stream = stream.try_clone()?;
    let output_router = Arc::clone(&router);
    let output_thread = thread::spawn(move || {
        while let Some(message) = output_rx.blocking_recv() {
            if crate::protocol::write_message(&mut output_stream, &message).is_err() {
                break;
            }
        }
        output_router.mark_closed();
        let _ = output_stream.shutdown(Shutdown::Both);
    });

    let mut input_stream = stream;
    let input_router = Arc::clone(&router);
    let input_thread = thread::spawn(move || {
        while let Ok(message) = crate::protocol::read_message::<_, ClientMessage>(
            &mut input_stream,
            MAX_GRAPHICS_FRAME_SIZE,
        ) {
            let detached = matches!(message, ClientMessage::Detach);
            input_router.route(message);
            if detached || input_router.is_detached() {
                break;
            }
        }
    });

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .thread_name("herdr-remote-quic")
        .build()?;
    let mut welcome_forwarded = false;
    let mut use_ssh = config.remote_config.transport == RemoteTransportConfig::Ssh;

    if !use_ssh {
        match run_quic(
            &runtime,
            config,
            Arc::clone(&router),
            output_tx.clone(),
            &mut welcome_forwarded,
        ) {
            QuicOutcome::Detached => {}
            QuicOutcome::Fallback(detail) => {
                if config.remote_config.ssh_fallback {
                    if welcome_forwarded {
                        let _ = output_tx.blocking_send(ServerMessage::TransportStatus {
                            status: RemoteTransportStatus::SshFallbackConnecting,
                            detail: Some(detail),
                        });
                    }
                    use_ssh = true;
                } else {
                    send_proxy_error(&output_tx, &mut welcome_forwarded, detail);
                }
            }
        }
    }

    if use_ssh && !router.is_detached() && !should_stop.load(Ordering::Acquire) {
        run_ssh_reconnect_loop(
            config,
            Arc::clone(&router),
            output_tx.clone(),
            &mut welcome_forwarded,
            should_stop,
        );
    }

    router.mark_closed();
    drop(output_tx);
    let _ = output_thread.join();
    let _ = input_thread.join();
    Ok(())
}

enum QuicOutcome {
    Detached,
    Fallback(String),
}

fn run_quic(
    runtime: &tokio::runtime::Runtime,
    config: &BridgeConfig,
    router: Arc<InputRouter>,
    output: mpsc::Sender<ServerMessage>,
    welcome_forwarded: &mut bool,
) -> QuicOutcome {
    let Some(mut bootstrap) = config.bootstrap.clone() else {
        return QuicOutcome::Fallback(
            config
                .bootstrap_error
                .clone()
                .unwrap_or_else(|| "SSH bootstrap did not enable QUIC".to_owned()),
        );
    };
    let Some(hostname) = config.ssh_hostname.as_deref() else {
        return QuicOutcome::Fallback("SSH target hostname could not be resolved".to_owned());
    };
    let resource_cache = Arc::new(Mutex::new(ResourceCache::default()));
    let mut connection_generation = 0u64;
    let mut reconnecting = false;
    let mut fresh_attempted = false;
    let mut rebootstrapped = false;

    loop {
        if router.is_detached() {
            return QuicOutcome::Detached;
        }
        let candidates = match remote_quic_candidates(hostname, bootstrap.port) {
            Ok(candidates) => candidates,
            Err(error) => return QuicOutcome::Fallback(error.to_string()),
        };
        connection_generation = connection_generation.saturating_add(1);
        if reconnecting
            && output
                .blocking_send(ServerMessage::TransportStatus {
                    status: RemoteTransportStatus::FreshQuicConnecting,
                    detail: None,
                })
                .is_err()
        {
            return QuicOutcome::Detached;
        }
        let hello = router.hello();
        let connect = runtime.block_on(QuicSession::connect(
            ConnectParams {
                bootstrap: bootstrap.clone(),
                candidates,
                logical_client_id: config.logical_client_id,
                connection_generation,
                cols: hello.cols,
                rows: hello.rows,
                cell_width_px: hello.cell_width_px,
                cell_height_px: hello.cell_height_px,
                keybindings: hello.keybindings,
            },
            Arc::clone(&resource_cache),
        ));
        let (session, welcome) = match connect {
            Ok(connected) => connected,
            Err(error) if !*welcome_forwarded => return QuicOutcome::Fallback(error),
            Err(error) if !fresh_attempted => {
                fresh_attempted = true;
                reconnecting = true;
                debug!(%error, "fresh remote QUIC connection failed");
                continue;
            }
            Err(error) if !rebootstrapped => {
                rebootstrapped = true;
                if output
                    .blocking_send(ServerMessage::TransportStatus {
                        status: RemoteTransportStatus::SshRebootstrap,
                        detail: Some(error),
                    })
                    .is_err()
                {
                    return QuicOutcome::Detached;
                }
                match rebootstrap(config) {
                    Ok(record) => {
                        bootstrap = record;
                        continue;
                    }
                    Err(error) => return QuicOutcome::Fallback(error.to_string()),
                }
            }
            Err(error) => return QuicOutcome::Fallback(error),
        };

        fresh_attempted = false;
        rebootstrapped = false;
        if !*welcome_forwarded {
            if output.blocking_send(welcome).is_err() {
                return QuicOutcome::Detached;
            }
            *welcome_forwarded = true;
        }
        let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE_ITEMS);
        router.set_active(input_tx);
        let exit = runtime.block_on(session.run(input_rx, output.clone(), reconnecting));
        router.clear_active();
        match exit {
            SessionExit::Detached => return QuicOutcome::Detached,
            SessionExit::RetryFresh(detail) => {
                reconnecting = true;
                if !fresh_attempted {
                    fresh_attempted = true;
                    debug!(%detail, "remote QUIC connection lost; trying fresh QUIC");
                    continue;
                }
                if !rebootstrapped {
                    rebootstrapped = true;
                    let _ = output.blocking_send(ServerMessage::TransportStatus {
                        status: RemoteTransportStatus::SshRebootstrap,
                        detail: Some(detail),
                    });
                    match rebootstrap(config) {
                        Ok(record) => {
                            bootstrap = record;
                            continue;
                        }
                        Err(error) => return QuicOutcome::Fallback(error.to_string()),
                    }
                }
                return QuicOutcome::Fallback(detail);
            }
            SessionExit::Rebootstrap(detail) => {
                reconnecting = true;
                rebootstrapped = true;
                let _ = output.blocking_send(ServerMessage::TransportStatus {
                    status: RemoteTransportStatus::SshRebootstrap,
                    detail: Some(detail),
                });
                match rebootstrap(config) {
                    Ok(record) => {
                        bootstrap = record;
                        fresh_attempted = false;
                    }
                    Err(error) => return QuicOutcome::Fallback(error.to_string()),
                }
            }
        }
    }
}

fn rebootstrap(config: &BridgeConfig) -> io::Result<RemoteBootstrapRecord> {
    let deadline = Instant::now() + SSH_REBOOTSTRAP_DEADLINE;
    loop {
        match request_remote_quic_bootstrap(
            &config.target,
            &config.remote_herdr,
            &config.session_name,
            &config.logical_client_id,
            config.ssh_options.as_ref(),
        ) {
            Ok(record) => return Ok(record),
            Err(error) if Instant::now() < deadline => {
                debug!(%error, "SSH QUIC rebootstrap not ready; retrying");
                thread::sleep(SSH_REBOOTSTRAP_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
    }
}

fn run_ssh_reconnect_loop(
    config: &BridgeConfig,
    router: Arc<InputRouter>,
    output: mpsc::Sender<ServerMessage>,
    welcome_forwarded: &mut bool,
    should_stop: &AtomicBool,
) {
    let mut reconnecting = *welcome_forwarded;
    while !router.is_detached() && !should_stop.load(Ordering::Acquire) {
        if reconnecting
            && output
                .blocking_send(ServerMessage::TransportStatus {
                    status: RemoteTransportStatus::SshFallbackConnecting,
                    detail: None,
                })
                .is_err()
        {
            return;
        }
        match run_one_ssh_session(config, Arc::clone(&router), &output, welcome_forwarded) {
            SshExit::Detached => return,
            SshExit::Reconnect(detail) => {
                reconnecting = true;
                debug!(%detail, "remote SSH bridge disconnected; reconnecting");
                thread::sleep(SSH_RECONNECT_DELAY);
            }
            SshExit::Fatal(detail) => {
                send_proxy_error(&output, welcome_forwarded, detail);
                return;
            }
        }
    }
}

enum SshExit {
    Detached,
    Reconnect(String),
    Fatal(String),
}

fn run_one_ssh_session(
    config: &BridgeConfig,
    router: Arc<InputRouter>,
    output: &mpsc::Sender<ServerMessage>,
    welcome_forwarded: &mut bool,
) -> SshExit {
    let mut command = Command::new("ssh");
    apply_managed_ssh_options(&mut command, config.ssh_options.as_ref());
    command
        .arg("-T")
        .arg(&config.target)
        .arg(remote_bridge_command(
            &config.remote_herdr,
            &config.session_name,
        ))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return SshExit::Reconnect(format!("failed to start SSH bridge: {error}")),
    };
    let mut child_stdin = match child.stdin.take() {
        Some(stdin) => stdin,
        None => return SshExit::Fatal("SSH bridge stdin is unavailable".to_owned()),
    };
    let mut child_stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => return SshExit::Fatal("SSH bridge stdout is unavailable".to_owned()),
    };

    let hello = router.hello();
    if let Err(error) = crate::protocol::write_message(&mut child_stdin, &hello.message()) {
        let _ = child.kill();
        return SshExit::Reconnect(format!("failed to send SSH bridge hello: {error}"));
    }
    let welcome: ServerMessage =
        match crate::protocol::read_message(&mut child_stdout, MAX_FRAME_SIZE) {
            Ok(welcome @ ServerMessage::Welcome { error: None, .. }) => welcome,
            Ok(ServerMessage::Welcome {
                error: Some(error), ..
            }) => {
                let _ = child.kill();
                return SshExit::Fatal(error);
            }
            Ok(_) => {
                let _ = child.kill();
                return SshExit::Reconnect("SSH bridge sent an invalid welcome".to_owned());
            }
            Err(error) => {
                let _ = child.kill();
                return SshExit::Reconnect(format!("failed to read SSH bridge welcome: {error}"));
            }
        };
    if !*welcome_forwarded {
        if output.blocking_send(welcome).is_err() {
            let _ = child.kill();
            return SshExit::Detached;
        }
        *welcome_forwarded = true;
    }

    let (input_tx, mut input_rx) = mpsc::channel::<ClientMessage>(INPUT_QUEUE_ITEMS);
    router.set_active(input_tx);
    let writer = thread::spawn(move || {
        while let Some(message) = input_rx.blocking_recv() {
            if crate::protocol::write_message(&mut child_stdin, &message).is_err() {
                break;
            }
            if matches!(message, ClientMessage::Detach) {
                break;
            }
        }
    });

    let result = loop {
        match crate::protocol::read_message::<_, ServerMessage>(
            &mut child_stdout,
            MAX_GRAPHICS_FRAME_SIZE,
        ) {
            Ok(ServerMessage::ServerShutdown { reason }) if !router.is_detached() => {
                break SshExit::Reconnect(
                    reason.unwrap_or_else(|| "remote server restarted".to_owned()),
                );
            }
            Ok(message) => {
                if output.blocking_send(message).is_err() {
                    break SshExit::Detached;
                }
            }
            Err(error) => {
                if router.is_detached() {
                    break SshExit::Detached;
                }
                break SshExit::Reconnect(error.to_string());
            }
        }
    };
    router.clear_active();
    let _ = child.kill();
    let _ = child.wait();
    let _ = writer.join();
    result
}

fn send_proxy_error(
    output: &mpsc::Sender<ServerMessage>,
    welcome_forwarded: &mut bool,
    detail: String,
) {
    let message = if *welcome_forwarded {
        ServerMessage::ServerShutdown {
            reason: Some(detail),
        }
    } else {
        *welcome_forwarded = true;
        ServerMessage::Welcome {
            version: PROTOCOL_VERSION,
            encoding: RenderEncoding::TerminalAnsi,
            error: Some(detail),
        }
    };
    let _ = output.blocking_send(message);
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hello() -> HelloState {
        HelloState {
            cols: 80,
            rows: 24,
            cell_width_px: 8,
            cell_height_px: 16,
            requested_encoding: RenderEncoding::TerminalAnsi,
            keybindings: ClientKeybindings::Server,
        }
    }

    #[test]
    fn disconnected_router_drops_input_and_keeps_latest_geometry() {
        let router = InputRouter::new(hello());
        router.route(ClientMessage::Input {
            data: b"must not replay".to_vec(),
        });
        router.route(ClientMessage::Resize {
            cols: 120,
            rows: 40,
            cell_width_px: 9,
            cell_height_px: 18,
        });
        let current = router.hello();
        assert_eq!((current.cols, current.rows), (120, 40));
    }

    #[test]
    fn detach_is_local_even_without_an_active_transport() {
        let router = InputRouter::new(hello());
        router.route(ClientMessage::Detach);
        assert!(router.is_detached());
    }
}
