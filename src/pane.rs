use std::cell::Cell;
use std::io::{BufWriter, Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering},
    Arc, Mutex,
};

use bytes::Bytes;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use ratatui::{layout::Rect, Frame};
use tokio::sync::{mpsc, watch, Notify};
use tracing::{debug, error, info, warn};

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;

mod input;
mod osc;
mod state;
mod terminal;

use self::terminal::{GhosttyPaneTerminal, PaneTerminal};
pub use self::{
    state::PaneState,
    terminal::{InputState, ScrollMetrics, TerminalCursorState},
};
use crate::terminal::stabilize_agent_state;

const RELEASE_REACQUIRE_SUPPRESSION: std::time::Duration = std::time::Duration::from_secs(1);

use crate::platform::host::{default_login_shell, shell_command_runner};
const PANE_TERM: &str = "xterm-256color";
const PANE_COLORTERM: &str = "truecolor";

fn apply_pane_terminal_env(cmd: &mut CommandBuilder) {
    // Each pane is rendered by herdr's own terminal layer, not the outer terminal
    // that launched the app. Advertising the inherited TERM leaks the host terminal
    // identity into shells and across SSH, which breaks redraw and cursor movement
    // when the remote side lacks matching terminfo entries.
    cmd.env("TERM", PANE_TERM);
    cmd.env("COLORTERM", PANE_COLORTERM);
}

#[derive(Debug, Clone, Copy)]
struct PendingAgentRelease {
    agent: Agent,
    until: std::time::Instant,
}

fn active_pending_release(
    pending_release: &Mutex<Option<PendingAgentRelease>>,
    now: std::time::Instant,
) -> Option<Agent> {
    let mut pending_release = pending_release.lock().ok()?;
    match *pending_release {
        Some(pending) if now < pending.until => Some(pending.agent),
        Some(_) => {
            *pending_release = None;
            None
        }
        None => None,
    }
}

async fn publish_state_changed_event(
    state_events: mpsc::Sender<AppEvent>,
    pane_id: PaneId,
    agent: Option<Agent>,
    state: AgentState,
) {
    // This runs on the async detector task, not the PTY reader thread.
    // Waiting for queue space here preserves correctness-critical state transitions
    // without blocking pane I/O.
    if let Err(e) = state_events
        .send(AppEvent::StateChanged {
            pane_id,
            agent,
            state,
        })
        .await
    {
        warn!(
            pane = pane_id.raw(),
            err = %e,
            "failed to deliver StateChanged event"
        );
    }
}

const AGENT_MISS_CONFIRMATION_ATTEMPTS: u8 = 6;

#[derive(Debug, Clone, Copy)]
struct AgentDetectionPresence {
    current_agent: Option<Agent>,
    consecutive_misses: u8,
}

fn should_clear_agent_for_foreground_shell(
    previous_agent: Option<Agent>,
    new_agent: Option<Agent>,
    foreground_is_pane_shell: bool,
) -> bool {
    previous_agent.is_some() && new_agent.is_none() && foreground_is_pane_shell
}

impl AgentDetectionPresence {
    fn from_agent(current_agent: Option<Agent>) -> Self {
        Self {
            current_agent,
            consecutive_misses: 0,
        }
    }

    fn current_agent(&self) -> Option<Agent> {
        self.current_agent
    }

    fn clear_current_agent(&mut self) -> bool {
        if self.current_agent.is_none() {
            self.consecutive_misses = 0;
            return false;
        }
        self.current_agent = None;
        self.consecutive_misses = 0;
        true
    }

    fn observe_process_probe(&mut self, identified_agent: Option<Agent>) -> bool {
        match identified_agent {
            Some(agent) => {
                self.consecutive_misses = 0;
                if Some(agent) == self.current_agent {
                    return false;
                }
                self.current_agent = Some(agent);
                true
            }
            None => {
                if self.current_agent.is_none() {
                    self.consecutive_misses = 0;
                    return false;
                }
                self.consecutive_misses = self.consecutive_misses.saturating_add(1);
                if self.consecutive_misses < AGENT_MISS_CONFIRMATION_ATTEMPTS {
                    return false;
                }
                self.current_agent = None;
                self.consecutive_misses = 0;
                true
            }
        }
    }
}

// ---------------------------------------------------------------------------
// PaneRuntime — PTY, parser, channels, background tasks
// ---------------------------------------------------------------------------

/// PTY runtime for a pane. Owns the terminal, I/O channels, and background tasks.
/// Dropping this shuts down all background tasks and closes the PTY.
pub struct PaneRuntime {
    pane_id: PaneId,
    terminal: Arc<PaneTerminal>,
    sender: mpsc::Sender<Bytes>,
    resize_tx: watch::Sender<(u16, u16, u32, u32)>,
    current_size: Cell<(u16, u16, u32, u32)>,
    child_pid: Arc<AtomicU32>,
    kitty_keyboard_flags: Arc<AtomicU16>,
    detect_reset_notify: Arc<Notify>,
    pending_release: Arc<Mutex<Option<PendingAgentRelease>>>,
    // Task handles for deterministic shutdown
    detect_handle: tokio::task::AbortHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WheelRouting {
    HostScroll,
    MouseReport,
    AlternateScroll,
}

impl Drop for PaneRuntime {
    fn drop(&mut self) {
        // Abort detection task immediately and terminate the owned session.
        // Reader/writer/resize tasks shut down naturally via channel close
        // and PTY EOF when the rest of PaneRuntime is dropped.
        self.detect_handle.abort();
        shutdown_pane_processes(self.pane_id, self.child_pid.load(Ordering::Acquire));
    }
}

fn wait_for_processes_to_exit(pids: &[u32], timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if pids
            .iter()
            .all(|pid| !crate::platform::process_exists(*pid))
        {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

fn shutdown_pane_processes(pane_id: PaneId, child_pid: u32) {
    if child_pid == 0 {
        return;
    }

    let mut pids = crate::platform::session_processes(child_pid);
    if pids.is_empty() {
        pids.push(child_pid);
    }
    pids.sort_unstable();
    pids.dedup();

    for (signal, grace) in [
        (
            crate::platform::Signal::Hangup,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Terminate,
            std::time::Duration::from_millis(250),
        ),
        (
            crate::platform::Signal::Kill,
            std::time::Duration::from_millis(250),
        ),
    ] {
        crate::platform::signal_processes(&pids, signal);
        if wait_for_processes_to_exit(&pids, grace) {
            info!(
                pane = pane_id.raw(),
                pid = child_pid,
                ?signal,
                "pane session terminated"
            );
            return;
        }
    }

    warn!(
        pane = pane_id.raw(),
        pid = child_pid,
        pids = ?pids,
        "pane session still alive after forced shutdown"
    );
}

impl PaneRuntime {
    pub fn shutdown(self) {
        self.detect_handle.abort();
        shutdown_pane_processes(self.pane_id, self.child_pid.load(Ordering::Acquire));
    }

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        self.terminal.apply_host_terminal_theme(theme);
    }

    pub fn spawn(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let shell = default_login_shell();
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);
        apply_pane_terminal_env(&mut cmd);
        crate::integration::apply_pane_env(&mut cmd, pane_id);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn shell",
        )
    }

    pub fn spawn_shell_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        command: &str,
        extra_env: &[(String, String)],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let (shell, c_flag) = shell_command_runner();
        let mut cmd = CommandBuilder::new(shell);
        cmd.arg(c_flag);
        cmd.arg(command);
        cmd.cwd(cwd);
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);
        apply_pane_terminal_env(&mut cmd);
        crate::integration::apply_pane_env(&mut cmd, pane_id);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn command pane",
        )
    }

    pub fn spawn_argv_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        argv: &[String],
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let Some((program, args)) = argv.split_first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "argv must not be empty",
            ));
        };
        let mut cmd = CommandBuilder::new(program);
        for arg in args {
            cmd.arg(arg);
        }
        cmd.cwd(cwd);
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);
        apply_pane_terminal_env(&mut cmd);
        crate::integration::apply_pane_env(&mut cmd, pane_id);
        Self::spawn_command_builder(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
            cmd,
            "failed to spawn argv command pane",
        )
    }

    fn spawn_command_builder(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
        cmd: CommandBuilder,
        spawn_error_message: &'static str,
    ) -> std::io::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // --- Writer channel ---
        let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(32);

        crate::logging::pane_spawn_started(pane_id.raw(), rows, cols, scrollback_limit_bytes);

        let mut terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if crate::kitty_graphics::is_enabled() {
            terminal
                .enable_kitty_graphics()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        let pane_terminal = GhosttyPaneTerminal::new(terminal, input_tx.clone())?;
        pane_terminal.apply_host_terminal_theme(host_terminal_theme);
        let terminal = Arc::new(PaneTerminal::new(pane_terminal));
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(0));

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        // --- Child watcher task ---
        let child_pid = Arc::new(AtomicU32::new(0));
        {
            let child_pid = child_pid.clone();
            let slave = pair.slave;
            let events = events.clone();
            let rt = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                match slave.spawn_command(cmd) {
                    Ok(mut child) => {
                        if let Some(pid) = child.process_id() {
                            child_pid.store(pid, Ordering::Release);
                            crate::logging::pane_spawned(pane_id.raw(), pid);
                        }
                        match child.wait() {
                            Ok(status) => {
                                let status_text = format!("{status:?}");
                                crate::logging::pane_exited(pane_id.raw(), &status_text);
                            }
                            Err(e) => {
                                crate::logging::pane_exit_failed(pane_id.raw(), &e.to_string())
                            }
                        }
                    }
                    Err(e) => error!(pane = pane_id.raw(), err = %e, "{spawn_error_message}"),
                }
                // Use blocking send — PaneDied is critical, must not be dropped
                if let Err(e) = rt.block_on(events.send(AppEvent::PaneDied { pane_id })) {
                    error!(pane = pane_id.raw(), err = %e, "failed to send PaneDied event");
                }
            });
        }

        // --- Reader task: PTY → terminal backend + screen snapshot + terminal query responses ---
        {
            let mut reader = reader;
            let terminal = terminal.clone();
            let response_writer = input_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let child_pid = child_pid.clone();
            let events = events.clone();
            let rt = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 8192];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Err(e) => {
                            debug!(pane = pane_id.raw(), err = %e, "pty reader closed");
                            break;
                        }
                        Ok(n) => {
                            let shell_pid = child_pid.load(Ordering::Acquire);
                            let result = terminal.process_pty_bytes(
                                pane_id,
                                shell_pid,
                                &buf[..n],
                                &response_writer,
                            );
                            if result.request_render && !render_dirty.swap(true, Ordering::AcqRel) {
                                render_notify.notify_one();
                            }
                            if let Some(delay) = result.render_delay {
                                let render_notify = render_notify.clone();
                                let render_dirty = render_dirty.clone();
                                rt.spawn(async move {
                                    tokio::time::sleep(delay).await;
                                    if !render_dirty.swap(true, Ordering::AcqRel) {
                                        render_notify.notify_one();
                                    }
                                });
                            }
                            for content in result.clipboard_writes {
                                if let Err(err) =
                                    rt.block_on(events.send(AppEvent::ClipboardWrite { content }))
                                {
                                    warn!(
                                        pane = pane_id.raw(),
                                        err = %err,
                                        "failed to send OSC 52 clipboard write"
                                    );
                                }
                            }
                        }
                    }
                }
                debug!(pane = pane_id.raw(), "reader task exiting");
            });
        }

        // --- Detection task ---
        let (detect_handle, detect_reset_notify, pending_release) = {
            use crate::detect;
            use std::time::{Duration, Instant};

            const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
            const TICK_IDENTIFIED: Duration = Duration::from_millis(300);
            const TICK_PENDING_RELEASE: Duration = Duration::from_millis(50);
            const PROCESS_RECHECK: Duration = Duration::from_secs(5);

            let child_pid = child_pid.clone();
            let terminal = terminal.clone();
            let state_events = events.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let detect_reset_notify = Arc::new(Notify::new());
            let detect_reset = detect_reset_notify.clone();
            let pending_release = Arc::new(Mutex::new(None));
            let pending_release_for_task = pending_release.clone();

            let handle = tokio::spawn(async move {
                let mut agent_presence = AgentDetectionPresence::from_agent(None);
                let mut state = AgentState::Unknown;
                let mut last_process_check = Instant::now();
                let mut last_foreground_pgid = None;
                let mut pending_foreground_shell_clear = false;
                let mut last_claude_working_at = None;

                tokio::time::sleep(Duration::from_millis(50)).await;

                loop {
                    let tick = if active_pending_release(&pending_release_for_task, Instant::now())
                        .is_some()
                        || terminal.has_transient_default_color_override()
                    {
                        TICK_PENDING_RELEASE
                    } else if agent_presence.current_agent().is_none() {
                        TICK_UNIDENTIFIED
                    } else {
                        TICK_IDENTIFIED
                    };
                    tokio::select! {
                        _ = tokio::time::sleep(tick) => {}
                        _ = detect_reset.notified() => {
                            agent_presence = AgentDetectionPresence::from_agent(None);
                            state = AgentState::Unknown;
                            last_foreground_pgid = None;
                            pending_foreground_shell_clear = false;
                            last_claude_working_at = None;
                        }
                    }

                    let now = Instant::now();
                    let suppressed_agent = active_pending_release(&pending_release_for_task, now);
                    let pid = child_pid.load(Ordering::Acquire);
                    let foreground_pgid = (pid > 0 && agent_presence.current_agent().is_some())
                        .then(|| detect::foreground_process_group_id(pid))
                        .flatten();
                    let foreground_group_changed = foreground_pgid.is_some()
                        && last_foreground_pgid.is_some()
                        && foreground_pgid != last_foreground_pgid;
                    let should_check_process = suppressed_agent.is_some()
                        || agent_presence.current_agent().is_none()
                        || foreground_group_changed
                        || pending_foreground_shell_clear
                        || now.duration_since(last_process_check) >= PROCESS_RECHECK;

                    let mut agent_changed = false;
                    let mut agent = agent_presence.current_agent();
                    if should_check_process {
                        last_process_check = now;
                        if pid > 0 {
                            let mut process_name = None;
                            let mut process_group_id = None;
                            let mut foreground_is_pane_shell = false;
                            let mut new_agent = None;

                            if let Some(job) = detect::foreground_job(pid) {
                                process_group_id = Some(job.process_group_id);
                                last_foreground_pgid = Some(job.process_group_id);
                                foreground_is_pane_shell =
                                    job.processes.iter().any(|p| p.pid == pid);
                                let identified = detect::identify_agent_in_job(&job);
                                process_name = identified
                                    .as_ref()
                                    .map(|(_, process_name)| process_name.clone());
                                new_agent = identified.as_ref().map(|(agent, _)| *agent);
                            } else if foreground_pgid.is_some() {
                                process_group_id = foreground_pgid;
                                last_foreground_pgid = foreground_pgid;
                            }

                            if let Some(suppressed_agent) = suppressed_agent {
                                if new_agent == Some(suppressed_agent) {
                                    new_agent = None;
                                } else if let Ok(mut pending_release) =
                                    pending_release_for_task.lock()
                                {
                                    *pending_release = None;
                                }
                            }

                            let previous_agent = agent_presence.current_agent();
                            let changed = if should_clear_agent_for_foreground_shell(
                                previous_agent,
                                new_agent,
                                foreground_is_pane_shell,
                            ) {
                                if state == AgentState::Idle {
                                    pending_foreground_shell_clear = false;
                                    agent_presence.clear_current_agent()
                                } else {
                                    pending_foreground_shell_clear = true;
                                    false
                                }
                            } else {
                                pending_foreground_shell_clear = false;
                                agent_presence.observe_process_probe(new_agent)
                            };
                            if new_agent.is_some() {
                                last_foreground_pgid = process_group_id;
                            } else if agent_presence.current_agent().is_none() {
                                last_foreground_pgid = None;
                            }
                            if changed {
                                agent = agent_presence.current_agent();
                                if let Some(process_name) = process_name {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        process = %process_name,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                } else {
                                    info!(
                                        pane = pane_id.raw(),
                                        previous_agent = ?previous_agent,
                                        ?agent,
                                        pgid = ?process_group_id,
                                        "agent changed"
                                    );
                                }
                                agent_changed = true;
                            }
                        }
                    }

                    let pid = child_pid.load(Ordering::Acquire);
                    // Keep the terminal restore side effect separate from render notification state.
                    #[allow(clippy::collapsible_if)]
                    if pid > 0 && terminal.maybe_restore_host_terminal_theme(pane_id, pid) {
                        if !render_dirty.swap(true, Ordering::AcqRel) {
                            render_notify.notify_one();
                        }
                    }

                    let content = terminal.detection_text();
                    let raw_state = detect::detect_state(agent, &content);
                    let new_state = stabilize_agent_state(
                        agent,
                        state,
                        raw_state,
                        now,
                        &mut last_claude_working_at,
                    );

                    if new_state != state || agent_changed {
                        debug!(
                            pane = pane_id.raw(),
                            ?state,
                            ?raw_state,
                            ?new_state,
                            ?agent,
                            "state changed"
                        );
                        state = new_state;
                        publish_state_changed_event(
                            state_events.clone(),
                            pane_id,
                            agent,
                            new_state,
                        )
                        .await;
                    }
                }
            });
            (handle.abort_handle(), detect_reset_notify, pending_release)
        };

        // --- Writer task: channel → PTY ---
        {
            let mut writer = BufWriter::new(writer);
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                while let Some(bytes) = rt.block_on(input_rx.recv()) {
                    if let Err(e) = writer.write_all(&bytes) {
                        warn!(pane = pane_id.raw(), err = %e, "pty write failed");
                        break;
                    }
                    if let Err(e) = writer.flush() {
                        warn!(pane = pane_id.raw(), err = %e, "pty flush failed");
                        break;
                    }
                }
                debug!(pane = pane_id.raw(), "writer task exiting");
            });
        }

        // --- Resize task ---
        let (resize_tx, mut resize_rx) = watch::channel::<(u16, u16, u32, u32)>((rows, cols, 0, 0));
        {
            let master = pair.master;
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                let mut last_size = (rows, cols, 0, 0);
                while rt.block_on(resize_rx.changed()).is_ok() {
                    let (rows, cols, cell_width_px, cell_height_px) =
                        *resize_rx.borrow_and_update();
                    if (rows, cols, cell_width_px, cell_height_px) == last_size {
                        continue;
                    }
                    last_size = (rows, cols, cell_width_px, cell_height_px);
                    if let Err(e) = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: (cols as u32)
                            .saturating_mul(cell_width_px)
                            .min(u16::MAX as u32) as u16,
                        pixel_height: (rows as u32)
                            .saturating_mul(cell_height_px)
                            .min(u16::MAX as u32) as u16,
                    }) {
                        warn!(pane = pane_id.raw(), err = %e, rows, cols, "pty resize failed");
                    }
                }
            });
        }

        Ok(Self {
            pane_id,
            terminal,
            sender: input_tx,
            resize_tx,
            current_size: Cell::new((rows, cols, 0, 0)),
            child_pid,
            kitty_keyboard_flags,
            detect_reset_notify,
            pending_release,
            detect_handle,
        })
    }

    pub fn begin_graceful_release(&self, agent: Agent) {
        if let Ok(mut pending_release) = self.pending_release.lock() {
            *pending_release = Some(PendingAgentRelease {
                agent,
                until: std::time::Instant::now() + RELEASE_REACQUIRE_SUPPRESSION,
            });
        }
        self.detect_reset_notify.notify_one();
    }

    #[cfg(test)]
    pub(crate) fn current_size(&self) -> (u16, u16) {
        let (rows, cols, _, _) = self.current_size.get();
        (rows, cols)
    }

    /// Resize if the dimensions actually changed.
    pub fn resize(&self, rows: u16, cols: u16, cell_width_px: u32, cell_height_px: u32) {
        let rows = rows.max(2);
        let cols = cols.max(4);
        let size = (rows, cols, cell_width_px, cell_height_px);
        if self.current_size.get() == size {
            return;
        }
        self.current_size.set(size);
        self.terminal
            .resize(rows, cols, cell_width_px, cell_height_px);
        let _ = self.resize_tx.send(size);
    }

    /// Scroll up by N lines (into scrollback history).
    pub fn scroll_up(&self, lines: usize) {
        self.terminal.scroll_up(lines);
    }

    /// Scroll down by N lines (toward live output).
    pub fn scroll_down(&self, lines: usize) {
        self.terminal.scroll_down(lines);
    }

    /// Reset scroll to live view (offset = 0).
    pub fn scroll_reset(&self) {
        self.terminal.scroll_reset();
    }

    /// Set scrollback offset measured from the live bottom of the terminal.
    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        self.terminal.set_scroll_offset_from_bottom(lines);
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        self.terminal.scroll_metrics()
    }

    pub fn input_state(&self) -> Option<InputState> {
        self.terminal.input_state()
    }

    pub fn cursor_state(&self, area: Rect, show_cursor: bool) -> Option<TerminalCursorState> {
        if !show_cursor {
            return None;
        }
        let cursor = self.terminal.cursor_state()?;
        if cursor.x >= area.width || cursor.y >= area.height {
            return None;
        }
        Some(TerminalCursorState {
            x: area.x + cursor.x,
            y: area.y + cursor.y,
            visible: cursor.visible,
            shape: cursor.shape,
        })
    }

    pub fn visible_text(&self) -> String {
        self.terminal.visible_text()
    }

    pub fn visible_ansi(&self) -> String {
        self.terminal.visible_ansi()
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.terminal.recent_text(lines)
    }

    pub fn recent_ansi(&self, lines: usize) -> String {
        self.terminal.recent_ansi(lines)
    }

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.terminal.recent_unwrapped_text(lines)
    }

    pub fn recent_unwrapped_ansi(&self, lines: usize) -> String {
        self.terminal.recent_unwrapped_ansi(lines)
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.terminal.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.terminal.render(frame, area, show_cursor);
    }

    pub fn visible_hyperlinks(&self, area: Rect) -> Vec<((u16, u16), String, String)> {
        self.terminal.visible_hyperlinks(area)
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        needs_data: F,
    ) -> Vec<crate::ghostty::KittyImagePlacement>
    where
        F: FnMut(crate::ghostty::KittyImageDescriptor) -> bool,
    {
        self.terminal
            .kitty_image_placements_with_data_filter(needs_data)
    }

    pub fn keyboard_protocol(&self) -> crate::input::KeyboardProtocol {
        let fallback = crate::input::KeyboardProtocol::from_kitty_flags(
            self.kitty_keyboard_flags.load(Ordering::Relaxed),
        );
        self.terminal.keyboard_protocol(fallback)
    }

    pub fn encode_terminal_key(&self, key: crate::input::TerminalKey) -> Vec<u8> {
        self.terminal
            .encode_terminal_key(key, self.keyboard_protocol())
    }

    pub async fn send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        self.sender.send(bytes).await
    }

    pub fn try_send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        self.sender.try_send(bytes)
    }

    pub async fn send_paste(&self, text: String) -> Result<(), mpsc::error::SendError<Bytes>> {
        let bracketed = self
            .input_state()
            .map(|state| state.bracketed_paste)
            .unwrap_or(false);
        let payload = if bracketed {
            format!("\x1b[200~{text}\x1b[201~")
        } else {
            text
        };
        self.send_bytes(Bytes::from(payload)).await
    }

    pub fn try_send_focus_event(&self, event: crate::ghostty::FocusEvent) -> bool {
        if !self
            .input_state()
            .map(|state| state.focus_reporting)
            .unwrap_or(false)
        {
            return false;
        }

        let Ok(bytes) = crate::ghostty::encode_focus(event) else {
            return false;
        };
        if let Err(err) = self.try_send_bytes(Bytes::from(bytes)) {
            warn!(err = %err, ?event, "failed to forward pane focus event");
        }
        true
    }

    pub fn wheel_routing(&self) -> Option<WheelRouting> {
        let input_state = self.input_state()?;
        Some(if input_state.mouse_reporting_enabled() {
            WheelRouting::MouseReport
        } else if input_state.alternate_screen && input_state.mouse_alternate_scroll {
            WheelRouting::AlternateScroll
        } else {
            WheelRouting::HostScroll
        })
    }

    pub fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if !self.input_state()?.mouse_protocol_mode.reporting_enabled() {
            return None;
        }
        self.terminal
            .encode_mouse_button(kind, column, row, modifiers)
    }

    pub fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        if self.wheel_routing()? != WheelRouting::MouseReport {
            return None;
        }
        self.terminal
            .encode_mouse_wheel(kind, column, row, modifiers)
    }

    pub fn encode_alternate_scroll(
        &self,
        kind: crossterm::event::MouseEventKind,
    ) -> Option<Vec<u8>> {
        self.input_state()?;
        if self.wheel_routing()? != WheelRouting::AlternateScroll {
            return None;
        }
        let key = match kind {
            crossterm::event::MouseEventKind::ScrollUp => crossterm::event::KeyCode::Up,
            crossterm::event::MouseEventKind::ScrollDown => crossterm::event::KeyCode::Down,
            _ => return None,
        };
        Some(self.encode_terminal_key(crate::input::TerminalKey::new(
            key,
            crossterm::event::KeyModifiers::empty(),
        )))
    }

    /// Get the current working directory of the child shell process.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.child_pid.load(Ordering::Relaxed);
        crate::platform::process_cwd(pid)
    }
}

#[cfg(test)]
impl PaneRuntime {
    pub(crate) fn test_with_channel(cols: u16, rows: u16) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[], 4)
    }

    pub(crate) fn test_with_channel_capacity(
        cols: u16,
        rows: u16,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, 0, &[], capacity)
    }

    pub(crate) fn test_with_screen_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Self {
        Self::test_with_scrollback_bytes(cols, rows, 0, bytes)
    }

    pub(crate) fn test_with_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> Self {
        Self::test_with_channel_and_scrollback_bytes(cols, rows, scrollback_limit_bytes, bytes, 4).0
    }

    fn test_with_channel_and_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
        channel_capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(channel_capacity);
        let (resize_tx, _resize_rx) = watch::channel((rows, cols, 0, 0));
        let mut terminal =
            crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes).unwrap();
        terminal.write(bytes);

        (
            Self {
                pane_id: PaneId::from_raw(0),
                terminal: Arc::new(PaneTerminal::new(
                    GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
                )),
                sender: tx,
                resize_tx,
                current_size: Cell::new((rows, cols, 0, 0)),
                child_pid: Arc::new(AtomicU32::new(0)),
                kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
                detect_reset_notify: Arc::new(Notify::new()),
                pending_release: Arc::new(Mutex::new(None)),
                detect_handle: tokio::spawn(async {}).abort_handle(),
            },
            rx,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture_shell_output(command: &str, extra_env: &[(&str, &str)]) -> String {
        let pair = native_pty_system()
            .openpty(PtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let output_path = std::env::temp_dir().join(format!(
            "herdr-pane-term-test-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let (shell, c_flag) = shell_command_runner();
        let mut cmd = CommandBuilder::new(shell);
        cmd.arg(c_flag);
        cmd.arg(format!("{command} > '{}'", output_path.display()));
        cmd.cwd(std::env::current_dir().unwrap());
        cmd.env("TERM", "xterm-ghostty");
        cmd.env("COLORTERM", "falsecolor");
        apply_pane_terminal_env(&mut cmd);
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let mut child = pair.slave.spawn_command(cmd).unwrap();
        let status = child.wait().unwrap();
        assert!(status.success(), "shell command failed: {status:?}");

        let output = std::fs::read_to_string(&output_path).unwrap();
        let _ = std::fs::remove_file(output_path);
        output
    }

    #[test]
    fn pane_terminal_identity_overrides_outer_terminal_env() {
        let output = capture_shell_output("printf '%s\\n%s\\n' \"$TERM\" \"$COLORTERM\"", &[]);
        assert_eq!(output, "xterm-256color\ntruecolor\n");
    }

    #[test]
    fn pane_terminal_identity_allows_explicit_override() {
        let output = capture_shell_output(
            "printf '%s\\n%s\\n' \"$TERM\" \"$COLORTERM\"",
            &[("TERM", "vt100"), ("COLORTERM", "24bit")],
        );
        assert_eq!(output, "vt100\n24bit\n");
    }

    #[tokio::test]
    async fn focus_events_are_forwarded_when_enabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = watch::channel((80, 24, 0, 0));
        let mut terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        terminal
            .mode_set(crate::ghostty::MODE_FOCUS_EVENT, true)
            .unwrap();
        let runtime = PaneRuntime {
            pane_id: PaneId::from_raw(0),
            terminal: Arc::new(PaneTerminal::new(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
            sender: tx,
            resize_tx,
            current_size: Cell::new((80, 24, 0, 0)),
            child_pid: Arc::new(AtomicU32::new(0)),
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            detect_handle: tokio::spawn(async {}).abort_handle(),
        };

        assert!(runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert_eq!(rx.recv().await.unwrap(), Bytes::from_static(b"\x1b[I"));
    }

    #[tokio::test]
    async fn focus_events_are_suppressed_when_disabled() {
        let (tx, mut rx) = mpsc::channel(4);
        let (resize_tx, _resize_rx) = watch::channel((80, 24, 0, 0));
        let terminal = crate::ghostty::Terminal::new(80, 24, 0).unwrap();
        let runtime = PaneRuntime {
            pane_id: PaneId::from_raw(0),
            terminal: Arc::new(PaneTerminal::new(
                GhosttyPaneTerminal::new(terminal, tx.clone()).unwrap(),
            )),
            sender: tx,
            resize_tx,
            current_size: Cell::new((80, 24, 0, 0)),
            child_pid: Arc::new(AtomicU32::new(0)),
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            detect_handle: tokio::spawn(async {}).abort_handle(),
        };

        assert!(!runtime.try_send_focus_event(crate::ghostty::FocusEvent::Gained));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), rx.recv())
                .await
                .is_err()
        );
    }

    #[test]
    fn foreground_shell_without_agent_is_immediate_clear_signal() {
        assert!(should_clear_agent_for_foreground_shell(
            Some(Agent::Claude),
            None,
            true
        ));
    }

    #[test]
    fn unknown_non_shell_foreground_job_is_not_immediate_clear_signal() {
        assert!(!should_clear_agent_for_foreground_shell(
            Some(Agent::Claude),
            None,
            false
        ));
    }

    #[test]
    fn foreground_agent_job_is_not_clear_signal() {
        assert!(!should_clear_agent_for_foreground_shell(
            Some(Agent::Claude),
            Some(Agent::OpenCode),
            true
        ));
    }

    #[test]
    fn transient_process_miss_keeps_current_agent_detected() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        let changed = presence.observe_process_probe(None);

        assert!(!changed, "one miss should not clear the detected agent");
        assert_eq!(presence.current_agent(), Some(Agent::Pi));
    }

    #[test]
    fn agent_only_clears_after_confirmation_misses() {
        let mut presence = AgentDetectionPresence::from_agent(Some(Agent::Pi));

        for attempt in 1..AGENT_MISS_CONFIRMATION_ATTEMPTS {
            let changed = presence.observe_process_probe(None);
            assert!(
                !changed,
                "miss {attempt} should stay in the confirmation window"
            );
            assert_eq!(presence.current_agent(), Some(Agent::Pi));
        }

        let changed = presence.observe_process_probe(None);
        assert!(changed, "last confirmation miss should clear the agent");
        assert_eq!(presence.current_agent(), None);
    }

    #[tokio::test]
    async fn state_changed_event_waits_for_queue_space_instead_of_dropping() {
        let (tx, mut rx) = mpsc::channel(1);
        let pane_id = PaneId::from_raw(42);

        tx.try_send(AppEvent::UpdateReady {
            version: "9.9.9".into(),
            install_command: "herdr update".into(),
        })
        .unwrap();

        let publish =
            publish_state_changed_event(tx.clone(), pane_id, Some(Agent::Pi), AgentState::Idle);
        tokio::pin!(publish);

        let blocked = tokio::time::timeout(std::time::Duration::from_millis(20), async {
            (&mut publish).await;
        })
        .await;
        assert!(
            blocked.is_err(),
            "publisher should wait for queue space instead of dropping StateChanged"
        );

        let first = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield first event")
            .expect("sender still alive");
        assert!(matches!(first, AppEvent::UpdateReady { .. }));

        tokio::time::timeout(std::time::Duration::from_millis(50), async {
            (&mut publish).await;
        })
        .await
        .expect("publisher should complete once queue space is available");

        let second = tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv())
            .await
            .expect("queue should yield second event")
            .expect("sender still alive");
        assert!(matches!(
            second,
            AppEvent::StateChanged {
                pane_id: delivered_pane,
                agent: Some(Agent::Pi),
                state: AgentState::Idle,
            } if delivered_pane == pane_id
        ));
    }
}
