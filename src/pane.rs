use std::cell::Cell;
use std::io::{Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering},
    Arc, Mutex,
};

use bytes::Bytes;
use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};
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

const RELEASE_REACQUIRE_SUPPRESSION: std::time::Duration = std::time::Duration::from_secs(1);
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

#[derive(Clone, Copy, Default)]
struct SpawnInitialState<'a> {
    detected_agent: Option<Agent>,
    history_ansi: Option<&'a str>,
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
    visible_blocker: bool,
    visible_idle: bool,
    visible_working: bool,
    process_exited: bool,
    observed_at: std::time::Instant,
) {
    // This runs on the async detector task, not the PTY reader thread.
    // Waiting for queue space here preserves correctness-critical state transitions
    // without blocking pane I/O.
    if let Err(e) = state_events
        .send(AppEvent::StateChanged {
            pane_id,
            agent,
            state,
            visible_blocker,
            visible_idle,
            visible_working,
            process_exited,
            observed_at,
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
const PROCESS_RECHECK_IDENTIFIED: std::time::Duration = std::time::Duration::from_secs(5);
const PROCESS_RECHECK_UNIDENTIFIED: std::time::Duration = std::time::Duration::from_secs(30);
const PROCESS_ACQUISITION_WINDOW: std::time::Duration = std::time::Duration::from_secs(8);
const PROCESS_ACQUISITION_FAST_WINDOW: std::time::Duration = std::time::Duration::from_millis(1500);
const PROCESS_ACQUISITION_FAST_RECHECK: std::time::Duration = std::time::Duration::from_millis(500);
const PROCESS_ACQUISITION_SLOW_RECHECK: std::time::Duration = std::time::Duration::from_secs(2);

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ForegroundShellAgentAction {
    ObserveProbe,
    ReportProcessExit,
    ClearAgent,
}

fn foreground_shell_agent_action(
    previous_agent: Option<Agent>,
    new_agent: Option<Agent>,
    foreground_is_pane_shell: bool,
    process_exit_reported: bool,
) -> ForegroundShellAgentAction {
    if !should_clear_agent_for_foreground_shell(previous_agent, new_agent, foreground_is_pane_shell)
    {
        return ForegroundShellAgentAction::ObserveProbe;
    }

    // Do not clear identity immediately. First publish an idle process-exit
    // transition for the previous agent so notifications and wait-agent callers
    // observe completion before the pane becomes unknown.
    if process_exit_reported {
        ForegroundShellAgentAction::ClearAgent
    } else {
        ForegroundShellAgentAction::ReportProcessExit
    }
}

#[derive(Debug, Clone, Copy)]
struct ProcessProbeInput {
    current_agent: Option<Agent>,
    suppressed_agent: Option<Agent>,
    foreground_pgid: Option<u32>,
    last_foreground_pgid: Option<u32>,
    has_process_probe: bool,
    acquisition_age: Option<std::time::Duration>,
    pending_foreground_shell_clear: bool,
    pending_restore_probe: bool,
    elapsed_since_process_check: std::time::Duration,
}

fn foreground_group_changed(
    foreground_pgid: Option<u32>,
    last_foreground_pgid: Option<u32>,
) -> bool {
    foreground_pgid != last_foreground_pgid
        && (foreground_pgid.is_some() || last_foreground_pgid.is_some())
}

fn should_probe_foreground_job(input: ProcessProbeInput) -> bool {
    if input.pending_foreground_shell_clear || input.pending_restore_probe {
        return true;
    }

    let foreground_group_changed =
        foreground_group_changed(input.foreground_pgid, input.last_foreground_pgid);

    if input.suppressed_agent.is_some() {
        return !input.has_process_probe || foreground_group_changed;
    }

    if let Some(acquisition_age) = input.acquisition_age {
        let acquisition_interval = if acquisition_age <= PROCESS_ACQUISITION_FAST_WINDOW {
            PROCESS_ACQUISITION_FAST_RECHECK
        } else {
            PROCESS_ACQUISITION_SLOW_RECHECK
        };
        if acquisition_age <= PROCESS_ACQUISITION_WINDOW
            && input.elapsed_since_process_check >= acquisition_interval
        {
            return true;
        }
    }

    if input.current_agent.is_none() {
        return !input.has_process_probe
            || foreground_group_changed
            || input.elapsed_since_process_check >= PROCESS_RECHECK_UNIDENTIFIED;
    }

    foreground_group_changed || input.elapsed_since_process_check >= PROCESS_RECHECK_IDENTIFIED
}

#[derive(Debug, Clone)]
struct ProcessProbeResult {
    process_group_id: Option<u32>,
    foreground_is_pane_shell: bool,
    agent: Option<Agent>,
    process_name: Option<String>,
}

fn probe_foreground_process(pid: u32, foreground_pgid: Option<u32>) -> ProcessProbeResult {
    if let Some(job) = foreground_pgid.and_then(crate::detect::foreground_group_leader_job) {
        if let Some((agent, process_name)) = crate::detect::identify_agent_in_job(&job) {
            return ProcessProbeResult {
                process_group_id: Some(job.process_group_id),
                foreground_is_pane_shell: job.processes.iter().any(|p| p.pid == pid),
                agent: Some(agent),
                process_name: Some(process_name),
            };
        }
    }

    if let Some(job) = crate::detect::foreground_job(pid) {
        let identified = crate::detect::identify_agent_in_job(&job);
        return ProcessProbeResult {
            process_group_id: Some(job.process_group_id),
            foreground_is_pane_shell: job.processes.iter().any(|p| p.pid == pid),
            agent: identified.as_ref().map(|(agent, _)| *agent),
            process_name: identified.map(|(_, process_name)| process_name),
        };
    }

    ProcessProbeResult {
        process_group_id: foreground_pgid,
        foreground_is_pane_shell: false,
        agent: None,
        process_name: None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DetectionPublishState {
    state: AgentState,
    visible_blocker: bool,
    visible_idle: bool,
    visible_working: bool,
}

fn should_publish_detection_update(
    previous: DetectionPublishState,
    next: DetectionPublishState,
    agent_changed: bool,
    process_exited: bool,
) -> bool {
    next.state != previous.state
        || next.visible_blocker != previous.visible_blocker
        || next.visible_idle != previous.visible_idle
        || next.visible_working != previous.visible_working
        || agent_changed
        || process_exited
        || (next.visible_idle && previous.visible_idle)
}

fn spawn_basic_detection_task(
    pane_id: PaneId,
    child_pid: Arc<AtomicU32>,
    terminal: Arc<PaneTerminal>,
    state_events: mpsc::Sender<AppEvent>,
) -> (
    tokio::task::AbortHandle,
    Arc<Notify>,
    Arc<Mutex<Option<PendingAgentRelease>>>,
) {
    let detect_reset_notify = Arc::new(Notify::new());
    let detect_reset = detect_reset_notify.clone();
    let pending_release = Arc::new(Mutex::new(None));
    let pending_release_for_task = pending_release.clone();

    let handle = tokio::spawn(async move {
        let mut agent_presence = AgentDetectionPresence::from_agent(None);
        let mut state = AgentState::Unknown;
        let mut last_visible_blocker = false;
        let mut last_visible_idle = false;
        let mut last_visible_working = false;
        let mut last_process_check = std::time::Instant::now();
        let mut last_foreground_pgid = None;
        let mut has_process_probe = false;
        let mut acquisition_started_at = None;
        let mut release_was_active = false;

        loop {
            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => {}
                _ = detect_reset.notified() => {
                    agent_presence = AgentDetectionPresence::from_agent(None);
                    state = AgentState::Unknown;
                    last_visible_blocker = false;
                    last_visible_idle = false;
                    last_visible_working = false;
                    last_process_check = std::time::Instant::now();
                    last_foreground_pgid = None;
                    has_process_probe = false;
                    acquisition_started_at = None;
                    release_was_active = false;
                }
            }

            let now = std::time::Instant::now();
            let suppressed_agent = active_pending_release(&pending_release_for_task, now);
            if suppressed_agent.is_none() && release_was_active {
                has_process_probe = false;
                acquisition_started_at = None;
            }
            release_was_active = suppressed_agent.is_some();
            let pid = child_pid.load(Ordering::Acquire);
            let mut agent_changed = false;
            let mut agent = agent_presence.current_agent();
            let content = terminal.detection_text();

            let foreground_pgid = (pid > 0)
                .then(|| crate::detect::foreground_process_group_id(pid))
                .flatten();
            let process_group_changed =
                foreground_group_changed(foreground_pgid, last_foreground_pgid);
            let should_check_process = pid > 0
                && should_probe_foreground_job(ProcessProbeInput {
                    current_agent: agent_presence.current_agent(),
                    suppressed_agent,
                    foreground_pgid,
                    last_foreground_pgid,
                    has_process_probe,
                    acquisition_age: acquisition_started_at
                        .map(|started| now.duration_since(started)),
                    pending_foreground_shell_clear: false,
                    pending_restore_probe: false,
                    elapsed_since_process_check: now.duration_since(last_process_check),
                });

            if should_check_process {
                last_process_check = now;
                let had_process_probe = has_process_probe;
                has_process_probe = true;
                let probe = probe_foreground_process(pid, foreground_pgid);
                let mut new_agent = probe.agent;
                if let Some(suppressed_agent) = suppressed_agent {
                    if new_agent == Some(suppressed_agent) {
                        new_agent = None;
                    } else if let Ok(mut pending_release) = pending_release_for_task.lock() {
                        *pending_release = None;
                    }
                }
                if new_agent.is_none() {
                    last_foreground_pgid = probe.process_group_id.or(foreground_pgid);
                    if had_process_probe && process_group_changed {
                        acquisition_started_at = Some(now);
                    }
                } else {
                    last_foreground_pgid = probe.process_group_id.or(foreground_pgid);
                    acquisition_started_at = None;
                }
                let previous_agent = agent_presence.current_agent();
                if agent_presence.observe_process_probe(new_agent) {
                    agent = agent_presence.current_agent();
                    agent_changed = previous_agent != agent;
                }
            }

            let detection = crate::detect::detect_agent(agent, &content);
            let new_state = detection.state;
            let visible_blocker = detection.visible_blocker && new_state == AgentState::Blocked;
            let visible_idle = detection.visible_idle && new_state == AgentState::Idle;
            let visible_working = detection.visible_working && new_state == AgentState::Working;

            if should_publish_detection_update(
                DetectionPublishState {
                    state,
                    visible_blocker: last_visible_blocker,
                    visible_idle: last_visible_idle,
                    visible_working: last_visible_working,
                },
                DetectionPublishState {
                    state: new_state,
                    visible_blocker,
                    visible_idle,
                    visible_working,
                },
                agent_changed,
                false,
            ) {
                state = new_state;
                last_visible_blocker = visible_blocker;
                last_visible_idle = visible_idle;
                last_visible_working = visible_working;
                publish_state_changed_event(
                    state_events.clone(),
                    pane_id,
                    agent,
                    new_state,
                    visible_blocker,
                    visible_idle,
                    visible_working,
                    false,
                    now,
                )
                .await;
            }
        }
    });

    (handle.abort_handle(), detect_reset_notify, pending_release)
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
    pty_master: Option<Box<dyn MasterPty + Send>>,
    raw_master_fd: Option<std::os::fd::RawFd>,
    force_resize_fd: Option<std::os::fd::RawFd>,
    io_stop: Arc<AtomicBool>,
    reader_paused: Arc<AtomicBool>,
    reader_pause_ack: Arc<AtomicBool>,
    reader_stopped_rx: Option<std::sync::mpsc::Receiver<()>>,
    kitty_keyboard_flags: Arc<AtomicU16>,
    detect_reset_notify: Arc<Notify>,
    pending_release: Arc<Mutex<Option<PendingAgentRelease>>>,
    preserve_processes_on_drop: bool,
    // Task handles for deterministic shutdown
    detect_handle: tokio::task::AbortHandle,
}

#[cfg(unix)]
#[derive(Debug)]
pub struct PaneRuntimeImport {
    pub pane_id: PaneId,
    pub master_fd: std::os::fd::RawFd,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub cell_width_px: u32,
    pub cell_height_px: u32,
    pub initial_history_ansi: Option<String>,
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
        self.io_stop.store(true, Ordering::Release);
        if !self.preserve_processes_on_drop {
            shutdown_pane_processes(self.pane_id, self.child_pid.load(Ordering::Acquire));
        }
        if let Some(fd) = self.raw_master_fd.take() {
            let _ = unsafe { libc::close(fd) };
        }
        if let Some(fd) = self.force_resize_fd.take() {
            let _ = unsafe { libc::close(fd) };
        }
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

#[cfg(unix)]
fn truncate_handoff_history(history: String, max_bytes: usize) -> String {
    if history.len() <= max_bytes {
        return history;
    }
    let mut start = history.len().saturating_sub(max_bytes);
    while !history.is_char_boundary(start) {
        start += 1;
    }
    let Some(newline_offset) = history[start..].find('\n') else {
        return String::new();
    };
    start += newline_offset + 1;
    history[start..].to_owned()
}

fn pane_shell(configured_shell: &str) -> String {
    pane_shell_from(configured_shell, std::env::var("SHELL").ok())
}

fn pane_shell_from(configured_shell: &str, env_shell: Option<String>) -> String {
    let configured_shell = configured_shell.trim();
    if !configured_shell.is_empty() {
        return configured_shell.to_string();
    }

    env_shell
        .map(|shell| shell.trim().to_string())
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

#[cfg(unix)]
fn duplicate_fd(fd: std::os::fd::RawFd) -> std::io::Result<std::os::fd::RawFd> {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(duplicated)
}

#[cfg(unix)]
fn set_cloexec(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn set_nonblocking(fd: std::os::fd::RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
fn duplicate_cloexec_fd(fd: std::os::fd::RawFd) -> std::io::Result<std::os::fd::RawFd> {
    let duplicated = duplicate_fd(fd)?;
    if let Err(err) = set_cloexec(duplicated) {
        let _ = unsafe { libc::close(duplicated) };
        return Err(err);
    }
    Ok(duplicated)
}

#[cfg(unix)]
fn file_from_duplicated_fd(fd: std::os::fd::RawFd) -> std::io::Result<std::fs::File> {
    use std::os::fd::FromRawFd;

    let duplicated = duplicate_cloexec_fd(fd)?;
    Ok(unsafe { std::fs::File::from_raw_fd(duplicated) })
}

#[cfg(unix)]
fn poll_read_ready(fd: std::os::fd::RawFd, timeout_ms: i32) -> std::io::Result<bool> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(result > 0 && (poll_fd.revents & (libc::POLLIN | libc::POLLHUP)) != 0);
    }
}

#[cfg(unix)]
fn poll_write_ready(fd: std::os::fd::RawFd, timeout_ms: i32) -> std::io::Result<bool> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(result > 0 && (poll_fd.revents & (libc::POLLOUT | libc::POLLHUP)) != 0);
    }
}

#[cfg(unix)]
fn write_all_nonblocking(
    writer: &mut std::fs::File,
    fd: std::os::fd::RawFd,
    mut bytes: &[u8],
    io_stop: &AtomicBool,
) -> std::io::Result<()> {
    while !bytes.is_empty() {
        if io_stop.load(Ordering::Acquire) {
            return Ok(());
        }
        match writer.write(bytes) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "pty write returned zero bytes",
                ));
            }
            Ok(written) => bytes = &bytes[written..],
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                let _ = poll_write_ready(fd, 50)?;
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn resize_pty_fd(
    fd: std::os::fd::RawFd,
    rows: u16,
    cols: u16,
    cell_width_px: u32,
    cell_height_px: u32,
) -> std::io::Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: (cols as u32)
            .saturating_mul(cell_width_px)
            .min(u16::MAX as u32) as u16,
        ws_ypixel: (rows as u32)
            .saturating_mul(cell_height_px)
            .min(u16::MAX as u32) as u16,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

impl PaneRuntime {
    #[cfg(unix)]
    fn master_fd(&self) -> Option<std::os::fd::RawFd> {
        self.raw_master_fd.or_else(|| {
            self.pty_master
                .as_ref()
                .and_then(|master| master.as_raw_fd())
        })
    }

    pub fn shutdown(self) {
        self.detect_handle.abort();
        shutdown_pane_processes(self.pane_id, self.child_pid.load(Ordering::Acquire));
    }

    #[cfg(unix)]
    pub fn duplicate_handoff_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        let master_fd = self
            .master_fd()
            .ok_or_else(|| std::io::Error::other("runtime has no PTY master fd"))?;
        duplicate_cloexec_fd(master_fd)
    }

    #[cfg(unix)]
    pub fn preserve_for_handoff(mut self) {
        self.io_stop.store(true, Ordering::Release);
        if let Some(reader_stopped_rx) = self.reader_stopped_rx.take() {
            let _ = reader_stopped_rx.recv_timeout(std::time::Duration::from_millis(500));
        }
        self.detect_handle.abort();
        self.preserve_processes_on_drop = true;
    }

    #[cfg(unix)]
    pub fn assume_handoff_ownership(&mut self) {
        self.preserve_processes_on_drop = false;
    }

    #[cfg(unix)]
    pub fn set_handoff_reader_paused(&self, paused: bool) {
        self.reader_paused.store(paused, Ordering::Release);
        if !paused {
            self.reader_pause_ack.store(false, Ordering::Release);
        }
    }

    #[cfg(unix)]
    pub fn pause_handoff_reader(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.reader_pause_ack.store(false, Ordering::Release);
        self.reader_paused.store(true, Ordering::Release);
        let deadline = std::time::Instant::now() + timeout;
        while std::time::Instant::now() < deadline {
            if self.reader_pause_ack.load(Ordering::Acquire) || self.io_stop.load(Ordering::Acquire)
            {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "timed out waiting for pane reader to pause for handoff",
        ))
    }

    #[cfg(unix)]
    pub fn handoff_pane(&self, pane_id: u32) -> crate::server::handoff::HandoffPane {
        let child_pid = self.child_pid.load(Ordering::Acquire);
        let (rows, cols, cell_width_px, cell_height_px) = self.current_size.get();
        crate::server::handoff::HandoffPane {
            pane_id,
            child_pid,
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            initial_history_ansi: None,
        }
    }

    #[cfg(unix)]
    pub fn handoff_history_ansi(&self) -> Option<String> {
        if self
            .terminal
            .input_state()
            .is_some_and(|input_state| input_state.alternate_screen)
        {
            return None;
        }
        self.snapshot_history().map(|history| {
            truncate_handoff_history(history, crate::server::handoff::MAX_REPLAY_BYTES_PER_PANE)
        })
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
        default_shell: &str,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        Self::spawn_with_initial_history(
            pane_id,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            default_shell,
            None,
            events,
            render_notify,
            render_dirty,
        )
    }

    pub(crate) fn spawn_with_initial_history(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        default_shell: &str,
        initial_history_ansi: Option<&str>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let shell = pane_shell(default_shell);
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
            SpawnInitialState {
                detected_agent: None,
                history_ansi: initial_history_ansi,
            },
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
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
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
            SpawnInitialState::default(),
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
            SpawnInitialState::default(),
        )
    }

    pub fn spawn_agent_restore(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        launch: crate::agent_resume::AgentResumeLaunch<'_>,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let Some((program, args)) = launch.plan.argv.split_first() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "restore argv must not be empty",
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
            "failed to spawn agent restore pane",
            SpawnInitialState {
                detected_agent: crate::detect::parse_agent_label(&launch.plan.agent),
                history_ansi: launch.initial_history_ansi,
            },
        )
    }

    #[cfg(unix)]
    pub fn from_handoff_fd(
        import: PaneRuntimeImport,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        let PaneRuntimeImport {
            pane_id,
            master_fd,
            child_pid,
            rows,
            cols,
            cell_width_px,
            cell_height_px,
            initial_history_ansi,
        } = import;
        use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};

        let master_fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(master_fd) };
        set_cloexec(master_fd.as_raw_fd())?;
        set_nonblocking(master_fd.as_raw_fd())?;
        let reader = file_from_duplicated_fd(master_fd.as_raw_fd())?;
        let writer = file_from_duplicated_fd(master_fd.as_raw_fd())?;
        let force_resize_fd = duplicate_cloexec_fd(master_fd.as_raw_fd())?;
        let resize_fd = unsafe {
            std::os::fd::OwnedFd::from_raw_fd(duplicate_cloexec_fd(master_fd.as_raw_fd())?)
        };
        let io_stop = Arc::new(AtomicBool::new(false));
        let reader_paused = Arc::new(AtomicBool::new(true));
        let reader_pause_ack = Arc::new(AtomicBool::new(false));

        let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(32);
        let mut terminal = crate::ghostty::Terminal::new(cols, rows, scrollback_limit_bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
        if crate::kitty_graphics::is_enabled() {
            terminal
                .enable_kitty_graphics()
                .map_err(|e| std::io::Error::other(e.to_string()))?;
        }
        let pane_terminal = GhosttyPaneTerminal::new(terminal, input_tx.clone())?;
        pane_terminal.apply_host_terminal_theme(host_terminal_theme);
        if let Some(ansi) = initial_history_ansi.as_deref() {
            pane_terminal.seed_history_ansi(ansi);
        }
        let terminal = Arc::new(PaneTerminal::new(pane_terminal));
        let child_pid = Arc::new(AtomicU32::new(child_pid));
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(0));
        let (reader_stopped_tx, reader_stopped_rx) = std::sync::mpsc::channel();

        {
            use std::os::fd::AsRawFd;

            let mut reader = reader;
            let reader_fd = reader.as_raw_fd();
            let terminal = terminal.clone();
            let response_writer = input_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let child_pid = child_pid.clone();
            let events = events.clone();
            let io_stop = io_stop.clone();
            let reader_paused = reader_paused.clone();
            let reader_pause_ack = reader_pause_ack.clone();
            let rt = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 8192];
                loop {
                    if io_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if reader_paused.load(Ordering::Acquire) {
                        reader_pause_ack.store(true, Ordering::Release);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    reader_pause_ack.store(false, Ordering::Release);
                    match poll_read_ready(reader_fd, 50) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(e) => {
                            debug!(pane = pane_id.raw(), err = %e, "handoff pty reader poll failed");
                            break;
                        }
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                        Err(e) => {
                            debug!(pane = pane_id.raw(), err = %e, "handoff pty reader closed");
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
                                let _ =
                                    rt.block_on(events.send(AppEvent::ClipboardWrite { content }));
                            }
                        }
                    }
                }
                let _ = reader_stopped_tx.send(());
                let _ = rt.block_on(events.send(AppEvent::PaneDied { pane_id }));
                debug!(pane = pane_id.raw(), "handoff reader task exiting");
            });
        }

        {
            use std::os::fd::AsRawFd;

            let mut writer = writer;
            let writer_fd = writer.as_raw_fd();
            let io_stop = io_stop.clone();
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                while let Some(bytes) = rt.block_on(input_rx.recv()) {
                    if io_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(e) = write_all_nonblocking(&mut writer, writer_fd, &bytes, &io_stop)
                    {
                        warn!(pane = pane_id.raw(), err = %e, "handoff pty write failed");
                        break;
                    }
                    if let Err(e) = writer.flush() {
                        warn!(pane = pane_id.raw(), err = %e, "handoff pty flush failed");
                        break;
                    }
                }
                debug!(pane = pane_id.raw(), "handoff writer task exiting");
            });
        }

        let (resize_tx, mut resize_rx) =
            watch::channel::<(u16, u16, u32, u32)>((rows, cols, cell_width_px, cell_height_px));
        {
            let io_stop = io_stop.clone();
            let resize_fd = resize_fd.into_raw_fd();
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                let mut last_size = (rows, cols, cell_width_px, cell_height_px);
                while rt.block_on(resize_rx.changed()).is_ok() {
                    if io_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let (rows, cols, cell_width_px, cell_height_px) =
                        *resize_rx.borrow_and_update();
                    if (rows, cols, cell_width_px, cell_height_px) == last_size {
                        continue;
                    }
                    last_size = (rows, cols, cell_width_px, cell_height_px);
                    if let Err(e) =
                        resize_pty_fd(resize_fd, rows, cols, cell_width_px, cell_height_px)
                    {
                        warn!(pane = pane_id.raw(), err = %e, rows, cols, "handoff pty resize failed");
                    }
                }
                let _ = unsafe { libc::close(resize_fd) };
            });
        }

        let (detect_handle, detect_reset_notify, pending_release) =
            spawn_basic_detection_task(pane_id, child_pid.clone(), terminal.clone(), events);

        Ok(Self {
            pane_id,
            terminal,
            sender: input_tx,
            resize_tx,
            current_size: Cell::new((rows, cols, cell_width_px, cell_height_px)),
            child_pid,
            pty_master: None,
            raw_master_fd: Some(master_fd.into_raw_fd()),
            force_resize_fd: Some(force_resize_fd),
            io_stop,
            reader_paused,
            reader_pause_ack,
            reader_stopped_rx: Some(reader_stopped_rx),
            kitty_keyboard_flags,
            detect_reset_notify,
            pending_release,
            preserve_processes_on_drop: true,
            detect_handle,
        })
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
        initial_state: SpawnInitialState<'_>,
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
        if let Some(ansi) = initial_state.history_ansi {
            pane_terminal.seed_history_ansi(ansi);
        }
        let terminal = Arc::new(PaneTerminal::new(pane_terminal));
        let kitty_keyboard_flags = Arc::new(AtomicU16::new(0));

        let master_fd = pair
            .master
            .as_raw_fd()
            .ok_or_else(|| std::io::Error::other("pty master fd is unavailable"))?;
        set_nonblocking(master_fd)?;
        let reader = file_from_duplicated_fd(master_fd)?;
        let writer = file_from_duplicated_fd(master_fd)?;
        let force_resize_fd = duplicate_cloexec_fd(master_fd)?;
        let resize_fd = duplicate_cloexec_fd(master_fd)?;
        let io_stop = Arc::new(AtomicBool::new(false));
        let reader_paused = Arc::new(AtomicBool::new(false));
        let reader_pause_ack = Arc::new(AtomicBool::new(false));
        let (reader_stopped_tx, reader_stopped_rx) = std::sync::mpsc::channel();

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
            use std::os::fd::AsRawFd;

            let mut reader = reader;
            let reader_fd = reader.as_raw_fd();
            let terminal = terminal.clone();
            let response_writer = input_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
            let child_pid = child_pid.clone();
            let events = events.clone();
            let io_stop = io_stop.clone();
            let reader_paused = reader_paused.clone();
            let reader_pause_ack = reader_pause_ack.clone();
            let rt = tokio::runtime::Handle::current();
            tokio::task::spawn_blocking(move || {
                let mut buf = [0u8; 8192];
                loop {
                    if io_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if reader_paused.load(Ordering::Acquire) {
                        reader_pause_ack.store(true, Ordering::Release);
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    reader_pause_ack.store(false, Ordering::Release);
                    match poll_read_ready(reader_fd, 50) {
                        Ok(true) => {}
                        Ok(false) => continue,
                        Err(e) => {
                            debug!(pane = pane_id.raw(), err = %e, "pty reader poll failed");
                            break;
                        }
                    }
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
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
                let _ = reader_stopped_tx.send(());
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
                let mut agent_presence =
                    AgentDetectionPresence::from_agent(initial_state.detected_agent);
                let mut state = if initial_state.detected_agent.is_some() {
                    AgentState::Idle
                } else {
                    AgentState::Unknown
                };
                let mut last_process_check = Instant::now();
                let mut last_foreground_pgid = None;
                let mut has_process_probe = false;
                let mut acquisition_started_at = None;
                let mut pending_foreground_shell_clear = false;
                let mut foreground_shell_exit_reported = false;
                let mut release_was_active = false;
                let mut pending_restore_probe = initial_state.detected_agent.is_some();
                let mut last_claude_working_at = None;
                let mut last_visible_blocker = false;
                let mut last_visible_idle = false;
                let mut last_visible_working = false;

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
                            has_process_probe = false;
                            acquisition_started_at = None;
                            pending_foreground_shell_clear = false;
                            foreground_shell_exit_reported = false;
                            release_was_active = false;
                            pending_restore_probe = false;
                            last_claude_working_at = None;
                            last_visible_blocker = false;
                            last_visible_idle = false;
                            last_visible_working = false;
                        }
                    }

                    let now = Instant::now();
                    let suppressed_agent = active_pending_release(&pending_release_for_task, now);
                    if suppressed_agent.is_none() && release_was_active {
                        has_process_probe = false;
                        acquisition_started_at = None;
                    }
                    release_was_active = suppressed_agent.is_some();
                    let pid = child_pid.load(Ordering::Acquire);
                    let content = terminal.detection_text();
                    let foreground_pgid = (pid > 0)
                        .then(|| detect::foreground_process_group_id(pid))
                        .flatten();
                    let process_group_changed =
                        foreground_group_changed(foreground_pgid, last_foreground_pgid);
                    let should_check_process = pid > 0
                        && should_probe_foreground_job(ProcessProbeInput {
                            current_agent: agent_presence.current_agent(),
                            suppressed_agent,
                            foreground_pgid,
                            last_foreground_pgid,
                            has_process_probe,
                            acquisition_age: acquisition_started_at
                                .map(|started| now.duration_since(started)),
                            pending_foreground_shell_clear,
                            pending_restore_probe,
                            elapsed_since_process_check: now.duration_since(last_process_check),
                        });

                    let mut agent_changed = false;
                    let mut agent = agent_presence.current_agent();
                    if should_check_process {
                        last_process_check = now;
                        let had_process_probe = has_process_probe;
                        has_process_probe = true;
                        if pid > 0 {
                            let probe = probe_foreground_process(pid, foreground_pgid);
                            let process_name = probe.process_name;
                            let process_group_id = probe.process_group_id;
                            let foreground_is_pane_shell = probe.foreground_is_pane_shell;
                            let mut new_agent = probe.agent;

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
                            let changed = match foreground_shell_agent_action(
                                previous_agent,
                                new_agent,
                                foreground_is_pane_shell,
                                foreground_shell_exit_reported,
                            ) {
                                ForegroundShellAgentAction::ReportProcessExit => {
                                    pending_foreground_shell_clear = true;
                                    false
                                }
                                ForegroundShellAgentAction::ClearAgent => {
                                    pending_foreground_shell_clear = false;
                                    foreground_shell_exit_reported = false;
                                    agent_presence.clear_current_agent()
                                }
                                ForegroundShellAgentAction::ObserveProbe => {
                                    pending_foreground_shell_clear = false;
                                    foreground_shell_exit_reported = false;
                                    agent_presence.observe_process_probe(new_agent)
                                }
                            };
                            if new_agent.is_some() {
                                last_foreground_pgid = process_group_id;
                                acquisition_started_at = None;
                                pending_restore_probe = false;
                            } else if agent_presence.current_agent().is_none() {
                                last_foreground_pgid = process_group_id.or(foreground_pgid);
                                if had_process_probe && process_group_changed {
                                    acquisition_started_at = Some(now);
                                }
                                pending_restore_probe = false;
                            } else {
                                last_foreground_pgid = process_group_id.or(foreground_pgid);
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

                    let process_exited = pending_foreground_shell_clear
                        && agent.is_some()
                        && !foreground_shell_exit_reported;
                    let detection = if process_exited {
                        detect::AgentDetection {
                            state: AgentState::Idle,
                            visible_blocker: false,
                            visible_idle: false,
                            visible_working: false,
                        }
                    } else {
                        detect::detect_agent(agent, &content)
                    };
                    let raw_state = detection.state;
                    let new_state = crate::terminal::state::stabilize_agent_detection(
                        agent,
                        state,
                        detection,
                        process_exited,
                        now,
                        &mut last_claude_working_at,
                    );
                    let visible_blocker =
                        detection.visible_blocker && new_state == AgentState::Blocked;
                    let visible_idle = detection.visible_idle && new_state == AgentState::Idle;
                    let visible_working =
                        detection.visible_working && new_state == AgentState::Working;

                    if should_publish_detection_update(
                        DetectionPublishState {
                            state,
                            visible_blocker: last_visible_blocker,
                            visible_idle: last_visible_idle,
                            visible_working: last_visible_working,
                        },
                        DetectionPublishState {
                            state: new_state,
                            visible_blocker,
                            visible_idle,
                            visible_working,
                        },
                        agent_changed,
                        process_exited,
                    ) {
                        debug!(
                            pane = pane_id.raw(),
                            ?state,
                            ?raw_state,
                            ?new_state,
                            ?agent,
                            "state changed"
                        );
                        state = new_state;
                        last_visible_blocker = visible_blocker;
                        last_visible_idle = visible_idle;
                        last_visible_working = visible_working;
                        publish_state_changed_event(
                            state_events.clone(),
                            pane_id,
                            agent,
                            new_state,
                            visible_blocker,
                            visible_idle,
                            visible_working,
                            process_exited,
                            now,
                        )
                        .await;
                        if process_exited {
                            foreground_shell_exit_reported = true;
                        }
                    }
                }
            });
            (handle.abort_handle(), detect_reset_notify, pending_release)
        };

        // --- Writer task: channel → PTY ---
        {
            use std::os::fd::AsRawFd;

            let mut writer = writer;
            let writer_fd = writer.as_raw_fd();
            let io_stop = io_stop.clone();
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                while let Some(bytes) = rt.block_on(input_rx.recv()) {
                    if io_stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Err(e) = write_all_nonblocking(&mut writer, writer_fd, &bytes, &io_stop)
                    {
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
            let io_stop = io_stop.clone();
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                let mut last_size = (rows, cols, 0, 0);
                while rt.block_on(resize_rx.changed()).is_ok() {
                    if io_stop.load(Ordering::Acquire) {
                        break;
                    }
                    let (rows, cols, cell_width_px, cell_height_px) =
                        *resize_rx.borrow_and_update();
                    if (rows, cols, cell_width_px, cell_height_px) == last_size {
                        continue;
                    }
                    last_size = (rows, cols, cell_width_px, cell_height_px);
                    if let Err(e) =
                        resize_pty_fd(resize_fd, rows, cols, cell_width_px, cell_height_px)
                    {
                        warn!(pane = pane_id.raw(), err = %e, rows, cols, "pty resize failed");
                    }
                }
                let _ = unsafe { libc::close(resize_fd) };
            });
        }

        Ok(Self {
            pane_id,
            terminal,
            sender: input_tx,
            resize_tx,
            current_size: Cell::new((rows, cols, 0, 0)),
            child_pid,
            pty_master: Some(pair.master),
            raw_master_fd: None,
            force_resize_fd: Some(force_resize_fd),
            io_stop,
            reader_paused,
            reader_pause_ack,
            reader_stopped_rx: Some(reader_stopped_rx),
            kitty_keyboard_flags,
            detect_reset_notify,
            pending_release,
            preserve_processes_on_drop: false,
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

    pub fn nudge_child_redraw_after_handoff(&self) {
        let Some(fd) = self.force_resize_fd else {
            return;
        };
        let (rows, cols, cell_width_px, cell_height_px) = self.current_size.get();
        let nudge = if rows > 2 {
            (rows - 1, cols, cell_width_px, cell_height_px)
        } else {
            (
                rows,
                cols.saturating_sub(1).max(4),
                cell_width_px,
                cell_height_px,
            )
        };
        if nudge == (rows, cols, cell_width_px, cell_height_px) {
            return;
        }

        let Ok(fd) = duplicate_cloexec_fd(fd) else {
            return;
        };
        std::thread::spawn(move || {
            let _ = resize_pty_fd(fd, nudge.0, nudge.1, nudge.2, nudge.3);
            std::thread::sleep(std::time::Duration::from_millis(30));
            let _ = resize_pty_fd(fd, rows, cols, cell_width_px, cell_height_px);
            let _ = unsafe { libc::close(fd) };
        });
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

    pub fn osc_title(&self) -> String {
        self.terminal.osc_title()
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

    pub fn snapshot_history(&self) -> Option<String> {
        let ansi = self.recent_unwrapped_ansi(usize::MAX);
        (!ansi.trim().is_empty()).then_some(ansi)
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

    pub(crate) fn test_with_channel_and_scrollback_bytes(
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
                pty_master: None,
                raw_master_fd: None,
                force_resize_fd: None,
                io_stop: Arc::new(AtomicBool::new(false)),
                reader_paused: Arc::new(AtomicBool::new(false)),
                reader_pause_ack: Arc::new(AtomicBool::new(false)),
                reader_stopped_rx: None,
                kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
                detect_reset_notify: Arc::new(Notify::new()),
                pending_release: Arc::new(Mutex::new(None)),
                preserve_processes_on_drop: true,
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
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
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
    fn pane_shell_prefers_configured_shell() {
        assert_eq!(
            pane_shell_from("/usr/bin/nu", Some("/bin/bash".to_string())),
            "/usr/bin/nu"
        );
    }

    #[test]
    fn pane_shell_falls_back_to_shell_env() {
        assert_eq!(
            pane_shell_from("", Some("/bin/bash".to_string())),
            "/bin/bash"
        );
    }

    #[test]
    fn pane_shell_ignores_empty_values() {
        assert_eq!(pane_shell_from("   ", Some("  ".to_string())), "/bin/sh");
        assert_eq!(pane_shell_from("", None), "/bin/sh");
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
    async fn handoff_history_ansi_captures_primary_screen() {
        let runtime =
            PaneRuntime::test_with_scrollback_bytes(40, 5, 4096, b"handoff-primary-history\r\n");

        let history = runtime.handoff_history_ansi().unwrap();

        assert!(history.contains("handoff-primary-history"));
    }

    #[tokio::test]
    async fn handoff_history_ansi_skips_alternate_screen() {
        let runtime = PaneRuntime::test_with_scrollback_bytes(
            40,
            5,
            4096,
            b"primary\r\n\x1b[?1049halt-screen",
        );

        assert!(runtime.handoff_history_ansi().is_none());
    }

    #[test]
    fn truncate_handoff_history_keeps_recent_utf8_boundary() {
        let history = format!("old\n{}\nrecent\n", "é".repeat(8));

        let truncated = truncate_handoff_history(history, 20);

        assert_eq!(truncated, "recent\n");
        assert!(truncated.is_char_boundary(0));
    }

    #[test]
    fn truncate_handoff_history_drops_partial_long_line() {
        let history = format!("old\n{}", "x".repeat(64));

        let truncated = truncate_handoff_history(history, 12);

        assert!(truncated.is_empty());
    }

    fn process_command_name(pid: u32) -> Option<String> {
        let output = std::process::Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "comm="])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let command = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!command.is_empty()).then_some(command)
    }

    async fn wait_for_child_pid(runtime: &PaneRuntime) -> u32 {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            let pid = runtime.child_pid.load(Ordering::Acquire);
            if pid != 0 {
                return pid;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("child pid was not published");
    }

    #[tokio::test]
    async fn spawn_agent_restore_uses_restore_command_as_pane_child() {
        let (events, _event_rx) = mpsc::channel(4);
        let plan = crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/cat".into()],
            dedupe_key: "test".into(),
        };
        let runtime = PaneRuntime::spawn_agent_restore(
            PaneId::from_raw(7),
            24,
            80,
            std::env::current_dir().unwrap(),
            crate::agent_resume::AgentResumeLaunch {
                plan: &plan,
                initial_history_ansi: None,
            },
            0,
            crate::terminal_theme::TerminalTheme::default(),
            events,
            Arc::new(Notify::new()),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        let pid = wait_for_child_pid(&runtime).await;
        let command = process_command_name(pid).expect("child process should be visible to ps");

        assert!(
            command.ends_with("cat"),
            "restore command should be the pane child, got {command:?}"
        );
        assert!(
            !command.ends_with("sh"),
            "restore must not keep a shell wrapper as the pane child"
        );

        runtime.shutdown();
    }

    #[tokio::test]
    async fn spawn_agent_restore_reports_pane_death_after_early_failure() {
        let (events, mut event_rx) = mpsc::channel(8);
        let plan = crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["/bin/sh".into(), "-c".into(), "exit 7".into()],
            dedupe_key: "test".into(),
        };
        let runtime = PaneRuntime::spawn_agent_restore(
            PaneId::from_raw(7),
            24,
            80,
            std::env::current_dir().unwrap(),
            crate::agent_resume::AgentResumeLaunch {
                plan: &plan,
                initial_history_ansi: None,
            },
            0,
            crate::terminal_theme::TerminalTheme::default(),
            events,
            Arc::new(Notify::new()),
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut died = false;
        while tokio::time::Instant::now() < deadline {
            let Some(event) = tokio::time::timeout(
                deadline.saturating_duration_since(tokio::time::Instant::now()),
                event_rx.recv(),
            )
            .await
            .expect("pane death event should arrive") else {
                break;
            };
            if matches!(event, AppEvent::PaneDied { pane_id } if pane_id == PaneId::from_raw(7)) {
                died = true;
                break;
            }
        }

        assert!(died, "failed direct agent restore should report pane death");
        runtime.shutdown();
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
            pty_master: None,
            raw_master_fd: None,
            force_resize_fd: None,
            io_stop: Arc::new(AtomicBool::new(false)),
            reader_paused: Arc::new(AtomicBool::new(false)),
            reader_pause_ack: Arc::new(AtomicBool::new(false)),
            reader_stopped_rx: None,
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            preserve_processes_on_drop: true,
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
            pty_master: None,
            raw_master_fd: None,
            force_resize_fd: None,
            io_stop: Arc::new(AtomicBool::new(false)),
            reader_paused: Arc::new(AtomicBool::new(false)),
            reader_pause_ack: Arc::new(AtomicBool::new(false)),
            reader_stopped_rx: None,
            kitty_keyboard_flags: Arc::new(AtomicU16::new(0)),
            detect_reset_notify: Arc::new(Notify::new()),
            pending_release: Arc::new(Mutex::new(None)),
            preserve_processes_on_drop: true,
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
    fn foreground_shell_reports_process_exit_before_clearing_agent() {
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Codex), None, true, false),
            ForegroundShellAgentAction::ReportProcessExit
        );
        assert_eq!(
            foreground_shell_agent_action(Some(Agent::Codex), None, true, true),
            ForegroundShellAgentAction::ClearAgent
        );
    }

    #[test]
    fn stable_visible_idle_republishes_for_stale_hook_deadline() {
        let previous = DetectionPublishState {
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: true,
            visible_working: false,
        };

        assert!(should_publish_detection_update(
            previous, previous, false, false
        ));
    }

    #[test]
    fn stable_plain_idle_does_not_republish() {
        let previous = DetectionPublishState {
            state: AgentState::Idle,
            visible_blocker: false,
            visible_idle: false,
            visible_working: false,
        };

        assert!(!should_publish_detection_update(
            previous, previous, false, false
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

    fn process_probe_input() -> ProcessProbeInput {
        ProcessProbeInput {
            current_agent: None,
            suppressed_agent: None,
            foreground_pgid: Some(42),
            last_foreground_pgid: Some(42),
            has_process_probe: true,
            acquisition_age: None,
            pending_foreground_shell_clear: false,
            pending_restore_probe: false,
            elapsed_since_process_check: std::time::Duration::from_secs(1),
        }
    }

    #[test]
    fn unchanged_unidentified_foreground_group_skips_full_process_probe() {
        assert!(!should_probe_foreground_job(process_probe_input()));
    }

    #[test]
    fn unidentified_foreground_group_change_runs_full_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: Some(43),
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_gets_initial_and_safety_process_probes() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            has_process_probe: false,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            elapsed_since_process_check: PROCESS_RECHECK_UNIDENTIFIED,
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_without_foreground_group_uses_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: None,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_UNIDENTIFIED,
            ..process_probe_input()
        }));
    }

    #[test]
    fn unidentified_pane_probes_when_foreground_group_disappears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            foreground_pgid: None,
            last_foreground_pgid: Some(42),
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_shell_clear_and_restore_force_process_probes() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            pending_foreground_shell_clear: true,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            pending_restore_probe: true,
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_forces_initial_process_probe() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            suppressed_agent: Some(Agent::Codex),
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_forces_process_probe_after_runtime_identity_clears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_skips_repeated_probe_when_foreground_group_is_stable() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            ..process_probe_input()
        }));
    }

    #[test]
    fn pending_release_probes_when_foreground_group_changes() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            suppressed_agent: Some(Agent::Codex),
            foreground_pgid: Some(43),
            ..process_probe_input()
        }));
    }

    #[test]
    fn acquisition_window_catches_delayed_same_group_wrapper_startup() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_millis(1250)),
            elapsed_since_process_check: PROCESS_ACQUISITION_FAST_RECHECK
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_millis(1250)),
            elapsed_since_process_check: PROCESS_ACQUISITION_FAST_RECHECK,
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(std::time::Duration::from_secs(5)),
            elapsed_since_process_check: PROCESS_ACQUISITION_SLOW_RECHECK,
            ..process_probe_input()
        }));
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            acquisition_age: Some(PROCESS_ACQUISITION_WINDOW + std::time::Duration::from_millis(1),),
            elapsed_since_process_check: PROCESS_ACQUISITION_SLOW_RECHECK,
            ..process_probe_input()
        }));
    }

    #[test]
    fn release_expiry_can_force_reacquire_probe_by_resetting_probe_state() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: None,
            has_process_probe: false,
            ..process_probe_input()
        }));
    }

    #[test]
    fn identified_agent_uses_shorter_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        }));
    }

    #[test]
    fn identified_agent_probes_when_foreground_group_disappears() {
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: Some(42),
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
    }

    #[test]
    fn stable_missing_foreground_group_uses_safety_process_probe() {
        assert!(!should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED
                - std::time::Duration::from_millis(1),
            ..process_probe_input()
        }));
        assert!(should_probe_foreground_job(ProcessProbeInput {
            current_agent: Some(Agent::Codex),
            foreground_pgid: None,
            last_foreground_pgid: None,
            elapsed_since_process_check: PROCESS_RECHECK_IDENTIFIED,
            ..process_probe_input()
        }));
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

        let publish = publish_state_changed_event(
            tx.clone(),
            pane_id,
            Some(Agent::Pi),
            AgentState::Idle,
            false,
            false,
            false,
            false,
            std::time::Instant::now(),
        );
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
                visible_blocker: false,
                visible_idle: false,
                visible_working: false,
                process_exited: false,
                observed_at: _,
            } if delivered_pane == pane_id
        ));
    }
}
