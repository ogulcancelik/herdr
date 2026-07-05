use std::sync::{atomic::AtomicBool, Arc};

use bytes::Bytes;
use ratatui::{layout::Rect, Frame};
use tokio::sync::{mpsc, Notify};

use crate::events::AppEvent;
use crate::layout::PaneId;

/// Live runtime for a server-owned terminal.
///
/// The PTY implementation still delegates to the legacy pane runtime while the
/// migration proceeds, but production code now depends on this terminal-layer
/// type instead of the pane module's implementation detail.
pub struct TerminalRuntime(crate::pane::PaneRuntime);

impl TerminalRuntime {
    pub fn shutdown(self) {
        self.0.shutdown();
    }

    #[cfg(unix)]
    pub fn duplicate_handoff_fd(&self) -> std::io::Result<std::os::fd::RawFd> {
        self.0.duplicate_handoff_fd()
    }

    #[cfg(unix)]
    pub fn preserve_for_handoff(self) {
        self.0.preserve_for_handoff()
    }

    #[cfg(unix)]
    pub fn assume_handoff_ownership(&mut self) {
        self.0.assume_handoff_ownership();
    }

    #[cfg(unix)]
    pub fn set_handoff_reader_paused(&self, paused: bool) {
        self.0.set_handoff_reader_paused(paused);
    }

    #[cfg(unix)]
    pub fn pause_handoff_reader(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        self.0.pause_handoff_reader(timeout)
    }

    #[cfg(unix)]
    pub fn handoff_runtime_state(
        &self,
        pane_id: u32,
    ) -> crate::handoff_runtime::HandoffRuntimeState {
        self.0.handoff_runtime_state(pane_id)
    }

    #[cfg(unix)]
    pub fn handoff_history_ansi(&self) -> Option<String> {
        self.0.handoff_history_ansi()
    }

    #[cfg(unix)]
    pub fn from_handoff_fd(
        import: crate::handoff_runtime::ImportedHandoffRuntime,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        crate::pane::PaneRuntime::from_handoff_fd(
            import,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
        )
        .map(Self)
    }

    pub fn spawn(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &crate::pane::PaneLaunchEnv,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        crate::pane::PaneRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            shell_config,
            launch_env,
            events,
            render_notify,
            render_dirty,
        )
        .map(Self)
    }

    // Wrapper mirrors pane runtime construction arguments.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_with_initial_history(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        shell_config: crate::pane::PaneShellConfig<'_>,
        launch_env: &crate::pane::PaneLaunchEnv,
        initial_history_ansi: Option<&str>,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        crate::pane::PaneRuntime::spawn_with_initial_history(
            pane_id,
            rows,
            cols,
            cwd,
            scrollback_limit_bytes,
            host_terminal_theme,
            shell_config,
            launch_env,
            initial_history_ansi,
            events,
            render_notify,
            render_dirty,
        )
        .map(Self)
    }

    // Wrapper mirrors pane runtime construction arguments.
    #[allow(clippy::too_many_arguments)]
    pub fn spawn_shell_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        command: &str,
        launch_env: &crate::pane::PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        crate::pane::PaneRuntime::spawn_shell_command(
            pane_id,
            rows,
            cols,
            cwd,
            command,
            launch_env,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
        )
        .map(Self)
    }

    pub fn spawn_argv_command(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        cwd: std::path::PathBuf,
        argv: &[String],
        launch_env: &crate::pane::PaneLaunchEnv,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        events: mpsc::Sender<AppEvent>,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        crate::pane::PaneRuntime::spawn_argv_command(
            pane_id,
            rows,
            cols,
            cwd,
            argv,
            launch_env,
            scrollback_limit_bytes,
            host_terminal_theme,
            events,
            render_notify,
            render_dirty,
        )
        .map(Self)
    }

    pub fn apply_host_terminal_theme(&self, theme: crate::terminal_theme::TerminalTheme) {
        self.0.apply_host_terminal_theme(theme);
    }

    /// Constructs a PTY-less local emulator for an adopted remote pane
    /// (Task 9b). See `PaneRuntime::spawn_remote_fed`'s doc comment for the
    /// full rationale; this wrapper only threads the call through like
    /// every other constructor on this type. Registers into
    /// `TerminalRuntimeRegistry` exactly like a PTY-backed `TerminalRuntime`.
    ///
    /// Only called from tests today; the visibility-hook integration is the
    /// real caller.
    #[allow(dead_code)]
    pub(crate) fn spawn_remote_fed(
        pane_id: PaneId,
        rows: u16,
        cols: u16,
        scrollback_limit_bytes: usize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        render_notify: Arc<Notify>,
        render_dirty: Arc<AtomicBool>,
    ) -> std::io::Result<Self> {
        crate::pane::PaneRuntime::spawn_remote_fed(
            pane_id,
            rows,
            cols,
            scrollback_limit_bytes,
            host_terminal_theme,
            render_notify,
            render_dirty,
        )
        .map(Self)
    }

    /// Feeds display bytes from an adopted remote pane into this runtime's
    /// ghostty vt core. See `PaneRuntime::feed_remote_bytes`'s doc comment
    /// for why `terminal_responses` are dropped and `shell_pid` is `0`.
    ///
    /// Only reachable on a runtime built by `spawn_remote_fed`. Production
    /// calls this from `HeadlessServer::feed_remote_pane_terminal_bytes`
    /// (`#[cfg(unix)]` only, since multi-host is unix-only), so on a
    /// non-unix build nothing outside tests calls this -- the `allow` stays
    /// for that target.
    #[allow(dead_code)]
    pub(crate) fn feed_remote_bytes(&self, bytes: &[u8]) {
        self.0.feed_remote_bytes(bytes);
    }

    pub fn begin_graceful_release(&self, agent: crate::detect::Agent) {
        self.0.begin_graceful_release(agent);
    }

    pub fn reset_agent_detection(&self) {
        self.0.reset_agent_detection();
    }

    #[cfg(test)]
    pub(crate) fn agent_detection_reset_notify_for_test(
        &self,
    ) -> std::sync::Arc<tokio::sync::Notify> {
        self.0.agent_detection_reset_notify_for_test()
    }

    pub fn set_full_lifecycle_authority_active(&self, active: bool) {
        self.0.set_full_lifecycle_authority_active(active);
    }

    pub fn resize(&self, rows: u16, cols: u16, cell_width_px: u32, cell_height_px: u32) {
        self.0.resize(rows, cols, cell_width_px, cell_height_px);
    }

    #[cfg(unix)]
    pub fn nudge_child_redraw_after_handoff(&self) {
        self.0.nudge_child_redraw_after_handoff();
    }

    pub fn scroll_up(&self, lines: usize) {
        self.0.scroll_up(lines);
    }

    pub fn scroll_down(&self, lines: usize) {
        self.0.scroll_down(lines);
    }

    pub fn scroll_reset(&self) {
        self.0.scroll_reset();
    }

    pub fn set_scroll_offset_from_bottom(&self, lines: usize) {
        self.0.set_scroll_offset_from_bottom(lines);
    }

    pub fn scroll_metrics(&self) -> Option<crate::pane::ScrollMetrics> {
        self.0.scroll_metrics()
    }

    pub fn input_state(&self) -> Option<crate::pane::InputState> {
        self.0.input_state()
    }

    pub fn cursor_state(
        &self,
        area: Rect,
        show_cursor: bool,
    ) -> Option<crate::pane::TerminalCursorState> {
        self.0.cursor_state(area, show_cursor)
    }

    pub fn synchronized_output_active(&self) -> bool {
        self.0.synchronized_output_active()
    }

    pub fn visible_text(&self) -> String {
        self.0.visible_text()
    }

    pub fn visible_ansi(&self) -> String {
        self.0.visible_ansi()
    }

    pub fn detection_text(&self) -> String {
        self.0.detection_text()
    }

    pub fn agent_osc_title(&self) -> String {
        self.0.agent_osc_title()
    }

    pub fn agent_osc_progress(&self) -> String {
        self.0.agent_osc_progress()
    }

    pub fn recent_text(&self, lines: usize) -> String {
        self.0.recent_text(lines)
    }

    pub fn recent_ansi(&self, lines: usize) -> String {
        self.0.recent_ansi(lines)
    }

    pub fn recent_unwrapped_text(&self, lines: usize) -> String {
        self.0.recent_unwrapped_text(lines)
    }

    pub fn recent_unwrapped_ansi(&self, lines: usize) -> String {
        self.0.recent_unwrapped_ansi(lines)
    }

    pub fn snapshot_history(&self) -> Option<String> {
        self.0.snapshot_history()
    }

    pub fn extract_selection(&self, selection: &crate::selection::Selection) -> Option<String> {
        self.0.extract_selection(selection)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, show_cursor: bool) {
        self.0.render(frame, area, show_cursor);
    }

    pub(crate) fn collect_dirty_patch(
        &self,
        area_width: u16,
        area_height: u16,
    ) -> crate::pane::TerminalDirtyPatchOutcome {
        self.0.collect_dirty_patch(area_width, area_height)
    }

    pub fn visible_hyperlinks(&self, area: Rect) -> Vec<((u16, u16), String, String)> {
        self.0.visible_hyperlinks(area)
    }

    pub fn kitty_image_placements_with_data_filter<F>(
        &self,
        needs_data: F,
    ) -> Vec<crate::ghostty::KittyImagePlacement>
    where
        F: FnMut(crate::ghostty::KittyImageDescriptor) -> bool,
    {
        self.0.kitty_image_placements_with_data_filter(needs_data)
    }

    pub fn keyboard_protocol(&self) -> crate::input::KeyboardProtocol {
        self.0.keyboard_protocol()
    }

    pub fn encode_terminal_key(&self, key: crate::input::TerminalKey) -> Vec<u8> {
        self.0.encode_terminal_key(key)
    }

    pub async fn send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::SendError<Bytes>> {
        self.0.send_bytes(bytes).await
    }

    pub fn try_send_bytes(&self, bytes: Bytes) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        self.0.try_send_bytes(bytes)
    }

    pub async fn send_paste(&self, text: String) -> Result<(), mpsc::error::SendError<Bytes>> {
        self.0.send_paste(text).await
    }

    pub fn try_send_focus_event(&self, event: crate::ghostty::FocusEvent) -> bool {
        self.0.try_send_focus_event(event)
    }

    pub fn wheel_routing(&self) -> Option<crate::pane::WheelRouting> {
        self.0.wheel_routing()
    }

    pub fn encode_mouse_button(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.0.encode_mouse_button(kind, column, row, modifiers)
    }

    pub fn encode_mouse_motion(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.0.encode_mouse_motion(kind, column, row, modifiers)
    }

    pub fn encode_mouse_wheel(
        &self,
        kind: crossterm::event::MouseEventKind,
        column: u16,
        row: u16,
        modifiers: crossterm::event::KeyModifiers,
    ) -> Option<Vec<u8>> {
        self.0.encode_mouse_wheel(kind, column, row, modifiers)
    }

    pub fn encode_alternate_scroll(
        &self,
        kind: crossterm::event::MouseEventKind,
    ) -> Option<Vec<u8>> {
        self.0.encode_alternate_scroll(kind)
    }

    pub fn cwd(&self) -> Option<std::path::PathBuf> {
        self.0.cwd()
    }

    pub fn foreground_cwd(&self) -> Option<std::path::PathBuf> {
        self.0.foreground_cwd()
    }

    pub fn child_pid(&self) -> Option<u32> {
        self.0.child_pid()
    }

    pub(crate) fn current_size(&self) -> (u16, u16) {
        self.0.current_size()
    }
}

#[cfg(test)]
impl TerminalRuntime {
    pub(crate) fn test_with_channel(cols: u16, rows: u16) -> (Self, mpsc::Receiver<Bytes>) {
        let (runtime, rx) = crate::pane::PaneRuntime::test_with_channel(cols, rows);
        (Self(runtime), rx)
    }

    pub(crate) fn test_with_channel_capacity(
        cols: u16,
        rows: u16,
        capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        let (runtime, rx) =
            crate::pane::PaneRuntime::test_with_channel_capacity(cols, rows, capacity);
        (Self(runtime), rx)
    }

    pub(crate) fn test_with_screen_bytes(cols: u16, rows: u16, bytes: &[u8]) -> Self {
        Self(crate::pane::PaneRuntime::test_with_screen_bytes(
            cols, rows, bytes,
        ))
    }

    pub(crate) fn test_process_pty_bytes(&self, bytes: &[u8]) {
        self.0.test_process_pty_bytes(bytes);
    }

    pub(crate) fn test_with_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
    ) -> Self {
        Self(crate::pane::PaneRuntime::test_with_scrollback_bytes(
            cols,
            rows,
            scrollback_limit_bytes,
            bytes,
        ))
    }

    pub(crate) fn test_with_channel_and_scrollback_bytes(
        cols: u16,
        rows: u16,
        scrollback_limit_bytes: usize,
        bytes: &[u8],
        channel_capacity: usize,
    ) -> (Self, mpsc::Receiver<Bytes>) {
        let (runtime, rx) = crate::pane::PaneRuntime::test_with_channel_and_scrollback_bytes(
            cols,
            rows,
            scrollback_limit_bytes,
            bytes,
            channel_capacity,
        );
        (Self(runtime), rx)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use super::*;
    use crate::terminal::{TerminalId, TerminalRuntimeRegistry};

    /// End-to-end check that a PTY-less remote-fed runtime works through the
    /// same registry/render/teardown surface as any other `TerminalRuntime`:
    /// `TerminalRuntimeRegistry::insert`/`get`/`remove`, `render`, and
    /// `collect_dirty_patch` all run completely unmodified. Byte-level
    /// coverage of the ghostty grid itself (ANSI parsing, resize,
    /// PaneRuntimeIo::Remote's match arms) lives beside `PaneRuntime` in
    /// `src/pane.rs`; this test only proves the registry round trip.
    #[tokio::test]
    async fn remote_fed_runtime_registers_renders_and_tears_down_through_the_registry() {
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(AtomicBool::new(false));
        let runtime = TerminalRuntime::spawn_remote_fed(
            PaneId::from_raw(1),
            5,
            20,
            0,
            crate::terminal_theme::TerminalTheme::default(),
            render_notify,
            render_dirty.clone(),
        )
        .expect("remote-fed terminal runtime should construct without a PTY");

        let mut registry = TerminalRuntimeRegistry::new();
        let terminal_id = TerminalId::alloc();
        assert!(registry.insert(terminal_id.clone(), runtime).is_none());
        assert_eq!(registry.len(), 1);

        let backend = ratatui::backend::TestBackend::new(20, 5);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|frame| {
            registry.get(&terminal_id).expect("just inserted").render(
                frame,
                Rect::new(0, 0, 20, 5),
                false,
            )
        })
        .unwrap();

        registry
            .get(&terminal_id)
            .expect("still registered")
            .feed_remote_bytes(b"remote output");
        assert!(
            render_dirty.load(Ordering::Acquire),
            "feeding bytes through the registry-held runtime should mark the render loop dirty"
        );

        let patch = match registry
            .get(&terminal_id)
            .expect("still registered")
            .collect_dirty_patch(20, 5)
        {
            crate::pane::TerminalDirtyPatchOutcome::Patch(patch) => patch,
            other => panic!("expected a dirty patch, got {other:?}"),
        };
        let row0 = &patch
            .rows
            .iter()
            .find(|(row, _)| *row == 0)
            .expect("row 0 should be dirty")
            .1;
        let text: String = row0[..13].iter().map(|cell| cell.symbol.as_str()).collect();
        assert_eq!(text, "remote output");

        // Teardown: removing from the registry drops the runtime. There is
        // no PTY or child process to reap, so this must not hang or panic.
        let removed = registry.remove(&terminal_id).expect("was registered");
        removed.shutdown();
        assert!(registry.get(&terminal_id).is_none());
    }
}
