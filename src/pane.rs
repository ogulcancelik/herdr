use std::cell::Cell;
use std::io::{BufWriter, Read, Write};
use std::sync::{
    atomic::{AtomicBool, AtomicU16, AtomicU32, Ordering},
    Arc, RwLock,
};

use bytes::Bytes;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tokio::sync::{mpsc, Notify};
use tracing::{debug, error, info, warn};

use crate::detect::{Agent, AgentState};
use crate::events::AppEvent;
use crate::layout::PaneId;
use crate::pty_callbacks::PtyResponses;

const CLAUDE_WORKING_HOLD: std::time::Duration = std::time::Duration::from_millis(1200);

fn stabilize_agent_state(
    agent: Option<Agent>,
    previous: AgentState,
    raw: AgentState,
    now: std::time::Instant,
    last_claude_working_at: &mut Option<std::time::Instant>,
) -> AgentState {
    if agent != Some(Agent::Claude) {
        return raw;
    }

    match raw {
        AgentState::Working => {
            *last_claude_working_at = Some(now);
            AgentState::Working
        }
        AgentState::Blocked => AgentState::Blocked,
        AgentState::Idle if previous == AgentState::Working => {
            if last_claude_working_at
                .is_some_and(|last_working| now.duration_since(last_working) < CLAUDE_WORKING_HOLD)
            {
                AgentState::Working
            } else {
                AgentState::Idle
            }
        }
        _ => raw,
    }
}

// ---------------------------------------------------------------------------
// PaneState — pure data, constructable without PTYs, testable
// ---------------------------------------------------------------------------

/// Observable state for a single pane.
/// This is the only part of a pane that workspace logic and tests need.
pub struct PaneState {
    pub detected_agent: Option<Agent>,
    pub state: AgentState,
    /// Whether the user has seen this pane since its last state change to Idle.
    /// False = "Done" (agent finished while user was in another workspace).
    pub seen: bool,
}

impl PaneState {
    pub fn new() -> Self {
        Self {
            detected_agent: None,
            state: AgentState::Unknown,
            seen: true,
        }
    }
}

// ---------------------------------------------------------------------------
// PaneRuntime — PTY, parser, channels, background tasks
// ---------------------------------------------------------------------------

/// PTY runtime for a pane. Owns the terminal, I/O channels, and background tasks.
/// Dropping this shuts down all background tasks and closes the PTY.
pub struct PaneRuntime {
    pub parser: Arc<RwLock<vt100::Parser<PtyResponses>>>,
    pub sender: mpsc::Sender<Bytes>,
    resize_tx: mpsc::Sender<(u16, u16)>,
    current_size: Cell<(u16, u16)>,
    child_pid: Arc<AtomicU32>,
    pub kitty_keyboard_flags: Arc<AtomicU16>,
    /// Live screen content snapshot — updated by reader, read by detector.
    /// Decouples detection from parser viewport state (scrollback).
    /// Kept alive here so the Arc isn't dropped; tasks hold their own clones.
    #[allow(dead_code)]
    screen_content: Arc<RwLock<String>>,
    // Task handles for deterministic shutdown
    detect_handle: tokio::task::AbortHandle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollMetrics {
    pub offset_from_bottom: usize,
    pub max_offset_from_bottom: usize,
    pub viewport_rows: usize,
}

impl Drop for PaneRuntime {
    fn drop(&mut self) {
        // Abort detection task immediately.
        // Reader/writer/resize tasks shut down naturally via channel close
        // and PTY EOF when the rest of PaneRuntime is dropped.
        self.detect_handle.abort();
    }
}

fn trim_trailing_blank_rows(rows: &mut Vec<String>) {
    while rows.last().is_some_and(|row| row.trim().is_empty()) {
        rows.pop();
    }
}

fn max_scrollback(parser: &mut vt100::Parser<PtyResponses>) -> usize {
    let screen = parser.screen_mut();
    let original_scrollback = screen.scrollback();
    screen.set_scrollback(usize::MAX);
    let max_scrollback = screen.scrollback();
    screen.set_scrollback(original_scrollback);
    max_scrollback
}

fn recent_text_from_parser(parser: &mut vt100::Parser<PtyResponses>, lines: usize) -> String {
    let max_scrollback = max_scrollback(parser);
    let screen = parser.screen_mut();
    let original_scrollback = screen.scrollback();

    let (_, cols) = screen.size();
    screen.set_scrollback(0);
    let visible_rows: Vec<String> = screen
        .rows(0, cols)
        .map(|row| row.trim_end().to_string())
        .collect();
    let extra_rows = lines.saturating_sub(visible_rows.len()).min(max_scrollback);

    let mut rows = Vec::with_capacity(extra_rows + visible_rows.len());
    if extra_rows > 0 {
        for offset in (1..=extra_rows).rev() {
            screen.set_scrollback(offset);
            if let Some(row) = screen.rows(0, cols).next() {
                rows.push(row.trim_end().to_string());
            }
        }
    }

    screen.set_scrollback(original_scrollback);
    rows.extend(visible_rows);
    trim_trailing_blank_rows(&mut rows);

    let start = rows.len().saturating_sub(lines);
    let text = rows[start..].join("\n");
    if text.is_empty() {
        text
    } else {
        format!("{text}\n")
    }
}

impl PaneRuntime {
    pub fn spawn(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
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

        let responses = PtyResponses::new();
        let kitty_keyboard_flags = responses.kitty_keyboard_flags.clone();
        let parser = Arc::new(RwLock::new(vt100::Parser::new_with_callbacks(
            rows,
            cols,
            10000,
            responses.clone(),
        )));

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.cwd(cwd);
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);

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
                            info!(pane = pane_id.raw(), pid, "child spawned");
                        }
                        match child.wait() {
                            Ok(status) => info!(pane = pane_id.raw(), ?status, "child exited"),
                            Err(e) => error!(pane = pane_id.raw(), err = %e, "child wait failed"),
                        }
                    }
                    Err(e) => error!(pane = pane_id.raw(), err = %e, "failed to spawn shell"),
                }
                // Use blocking send — PaneDied is critical, must not be dropped
                if let Err(e) = rt.block_on(events.send(AppEvent::PaneDied { pane_id })) {
                    error!(pane = pane_id.raw(), err = %e, "failed to send PaneDied event");
                }
            });
        }

        // --- Writer channel ---
        let (input_tx, mut input_rx) = mpsc::channel::<Bytes>(32);

        // Live screen snapshot for detection (decoupled from parser scrollback)
        let screen_content = Arc::new(RwLock::new(String::new()));

        // --- Reader task: PTY → parser + screen snapshot + terminal query responses ---
        {
            let mut reader = reader;
            let parser = parser.clone();
            let screen_content = screen_content.clone();
            let response_writer = input_tx.clone();
            let render_notify = render_notify.clone();
            let render_dirty = render_dirty.clone();
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
                            if let Ok(mut p) = parser.write() {
                                p.process(&buf[..n]);
                                // Snapshot live screen content for detection.
                                // Always reads at scrollback 0 (current view),
                                // without touching the user's scroll position.
                                let scrollback = p.screen().scrollback();
                                if scrollback > 0 {
                                    p.screen_mut().set_scrollback(0);
                                }
                                let content = p.screen().contents();
                                if scrollback > 0 {
                                    p.screen_mut().set_scrollback(scrollback);
                                }
                                if let Ok(mut sc) = screen_content.write() {
                                    *sc = content;
                                }
                            } else {
                                error!(pane = pane_id.raw(), "parser lock poisoned in reader");
                                break;
                            }
                            let resp = responses.take();
                            if !resp.is_empty() {
                                if let Err(e) = response_writer.try_send(Bytes::from(resp)) {
                                    warn!(pane = pane_id.raw(), err = %e, "dropped terminal query response");
                                }
                            }
                            if !render_dirty.swap(true, Ordering::AcqRel) {
                                render_notify.notify_one();
                            }
                        }
                    }
                }
                debug!(pane = pane_id.raw(), "reader task exiting");
            });
        }

        // --- Detection task ---
        let detect_handle = {
            use crate::detect;
            use std::time::{Duration, Instant};

            const TICK_UNIDENTIFIED: Duration = Duration::from_millis(500);
            const TICK_IDENTIFIED: Duration = Duration::from_millis(300);
            const PROCESS_RECHECK: Duration = Duration::from_secs(5);

            let child_pid = child_pid.clone();
            let screen_content = screen_content.clone();
            let state_events = events.clone();

            let handle = tokio::spawn(async move {
                let mut agent: Option<Agent> = None;
                let mut state = AgentState::Unknown;
                let mut last_process_check = Instant::now();
                let mut last_claude_working_at = None;

                tokio::time::sleep(Duration::from_millis(50)).await;

                loop {
                    let tick = if agent.is_none() {
                        TICK_UNIDENTIFIED
                    } else {
                        TICK_IDENTIFIED
                    };
                    tokio::time::sleep(tick).await;

                    let now = Instant::now();
                    let should_check_process = agent.is_none()
                        || now.duration_since(last_process_check) >= PROCESS_RECHECK;

                    let mut agent_changed = false;
                    if should_check_process {
                        last_process_check = now;
                        let pid = child_pid.load(Ordering::Acquire);
                        if pid > 0 {
                            if let Some(job) = detect::foreground_job(pid) {
                                let identified = detect::identify_agent_in_job(&job);
                                let new_agent = identified.as_ref().map(|(agent, _)| *agent);
                                if new_agent != agent {
                                    if let Some((_, process_name)) = identified {
                                        info!(
                                            pane = pane_id.raw(),
                                            ?new_agent,
                                            process = %process_name,
                                            pgid = job.process_group_id,
                                            "agent changed"
                                        );
                                    } else {
                                        info!(
                                            pane = pane_id.raw(),
                                            ?new_agent,
                                            pgid = job.process_group_id,
                                            "agent changed"
                                        );
                                    }
                                    agent = new_agent;
                                    agent_changed = true;
                                }
                            }
                        }
                    }

                    let raw_state = if let Ok(content) = screen_content.read() {
                        detect::detect_state(agent, &content)
                    } else {
                        continue;
                    };
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
                        if let Err(e) = state_events.try_send(AppEvent::StateChanged {
                            pane_id,
                            agent,
                            state: new_state,
                        }) {
                            warn!(
                                pane = pane_id.raw(),
                                err = %e,
                                "dropped StateChanged event"
                            );
                        }
                    }
                }
            });
            handle.abort_handle()
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
        let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(4);
        {
            let master = pair.master;
            tokio::task::spawn_blocking(move || {
                let rt = tokio::runtime::Handle::current();
                while let Some((rows, cols)) = rt.block_on(resize_rx.recv()) {
                    if let Err(e) = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    }) {
                        warn!(pane = pane_id.raw(), err = %e, rows, cols, "pty resize failed");
                    }
                }
            });
        }

        Ok(Self {
            parser,
            sender: input_tx,
            resize_tx,
            current_size: Cell::new((rows, cols)),
            child_pid,
            kitty_keyboard_flags,
            screen_content,
            detect_handle,
        })
    }

    /// Resize if the dimensions actually changed.
    pub fn resize(&self, rows: u16, cols: u16) {
        let rows = rows.max(2);
        let cols = cols.max(4);
        if self.current_size.get() == (rows, cols) {
            return;
        }
        self.current_size.set((rows, cols));
        if let Ok(mut p) = self.parser.write() {
            p.screen_mut().set_size(rows, cols);
        }
        let _ = self.resize_tx.try_send((rows, cols));
    }

    /// Scroll up by N lines (into scrollback history).
    pub fn scroll_up(&self, lines: usize) {
        if let Ok(mut p) = self.parser.write() {
            let current = p.screen().scrollback();
            p.screen_mut().set_scrollback(current + lines);
        }
    }

    /// Scroll down by N lines (toward live output).
    pub fn scroll_down(&self, lines: usize) {
        if let Ok(mut p) = self.parser.write() {
            let current = p.screen().scrollback();
            p.screen_mut().set_scrollback(current.saturating_sub(lines));
        }
    }

    /// Reset scroll to live view (offset = 0).
    pub fn scroll_reset(&self) {
        if let Ok(mut p) = self.parser.write() {
            p.screen_mut().set_scrollback(0);
        }
    }

    /// Set scrollback offset measured from the live bottom of the terminal.
    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        if let Ok(mut p) = self.parser.write() {
            p.screen_mut().set_scrollback(lines);
        }
    }

    pub fn scroll_metrics(&self) -> Option<ScrollMetrics> {
        let Ok(mut parser) = self.parser.write() else {
            return None;
        };
        let max_offset_from_bottom = max_scrollback(&mut parser);
        let screen = parser.screen();
        let (viewport_rows, _) = screen.size();
        Some(ScrollMetrics {
            offset_from_bottom: screen.scrollback(),
            max_offset_from_bottom,
            viewport_rows: viewport_rows as usize,
        })
    }

    pub fn visible_text(&self) -> String {
        let Ok(content) = self.screen_content.read() else {
            return String::new();
        };
        let mut rows: Vec<String> = content
            .lines()
            .map(|line| line.trim_end().to_string())
            .collect();
        trim_trailing_blank_rows(&mut rows);
        let text = rows.join("\n");
        if text.is_empty() {
            text
        } else {
            format!("{text}\n")
        }
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.parser
            .write()
            .map(|mut parser| recent_text_from_parser(&mut parser, lines))
            .unwrap_or_default()
    }

    /// Get the current working directory of the child shell process.
    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        let pid = self.child_pid.load(Ordering::Relaxed);
        crate::platform::process_cwd(pid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recent_text_reconstructs_scrollback_tail() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(3, 10, 100, responses);
        parser.process(b"a\r\nb\r\nc\r\nd\r\ne");

        let recent = recent_text_from_parser(&mut parser, 4);
        assert_eq!(recent, "b\nc\nd\ne\n");
    }

    #[test]
    fn max_scrollback_reports_clamped_history_without_changing_position() {
        let responses = PtyResponses::new();
        let mut parser = vt100::Parser::new_with_callbacks(3, 10, 100, responses);
        parser.process(b"a\r\nb\r\nc\r\nd\r\ne");
        parser.screen_mut().set_scrollback(1);

        let max = max_scrollback(&mut parser);

        assert_eq!(max, 2);
        assert_eq!(parser.screen().scrollback(), 1);
    }

    #[test]
    fn trim_trailing_blank_rows_drops_empty_viewport_tail() {
        let mut rows = vec!["hello".to_string(), "".to_string(), "   ".to_string()];
        trim_trailing_blank_rows(&mut rows);
        assert_eq!(rows, vec!["hello".to_string()]);
    }

    #[test]
    fn claude_working_is_sticky_for_short_gap() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Idle,
            AgentState::Working,
            now,
            &mut last_working,
        );
        assert_eq!(working, AgentState::Working);

        let still_working = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + std::time::Duration::from_millis(400),
            &mut last_working,
        );
        assert_eq!(still_working, AgentState::Working);
    }

    #[test]
    fn claude_transitions_to_idle_after_hold_expires() {
        let now = std::time::Instant::now();
        let mut last_working = Some(now);

        let state = stabilize_agent_state(
            Some(Agent::Claude),
            AgentState::Working,
            AgentState::Idle,
            now + CLAUDE_WORKING_HOLD + std::time::Duration::from_millis(1),
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }

    #[test]
    fn non_claude_states_are_unchanged() {
        let now = std::time::Instant::now();
        let mut last_working = None;

        let state = stabilize_agent_state(
            Some(Agent::Codex),
            AgentState::Working,
            AgentState::Idle,
            now,
            &mut last_working,
        );
        assert_eq!(state, AgentState::Idle);
    }
}
