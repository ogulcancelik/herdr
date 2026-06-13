use std::path::PathBuf;

use super::App;

/// Decide where in-app edits should land.
///
/// When the base config is a real, writable regular file we edit it in
/// place; when it is a symlink (the read-only nix/HM case) or absent we
/// route edits into the overlay (`config.local.toml`). We treat symlinks
/// as read-only even when the target underneath is writable -- nix store
/// paths *look* writable in symlink targets but actually live in
/// `/nix/store` and reject writes.
///
/// On first overlay use we materialise an empty file with a header
/// comment so the editor opens onto something instead of a brand-new
/// buffer. The parent directory is created when needed.
pub fn resolve_write_target() -> std::io::Result<PathBuf> {
    let base = crate::config::config_path();
    let base_is_writable_file = match std::fs::symlink_metadata(&base) {
        Ok(meta) => !meta.file_type().is_symlink() && meta.file_type().is_file(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
        Err(err) => return Err(err),
    };
    if base_is_writable_file {
        return Ok(base);
    }

    let overlay = crate::config::config_overlay_path();
    if let Some(parent) = overlay.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if !overlay.exists() {
        std::fs::write(
            &overlay,
            "# herdr overlay -- read after config.toml, adds scalar keys\n\
             # the base config does not declare. See `herdr config edit`.\n",
        )?;
    }
    Ok(overlay)
}

impl App {
    /// Open the editable config target in `${EDITOR:-vi}` hosted by an
    /// overlay pane (mirrors `launch_focused_scrollback_editor`). On the
    /// pane's PaneDied we reload the config; on a Failed apply we restore
    /// the pre-edit contents and surface the diagnostics via toast.
    pub(crate) fn launch_config_editor(&mut self) {
        let previous_toast = self.state.toast.clone();
        match self.open_config_editor() {
            Ok(()) => self.sync_toast_deadline(previous_toast),
            Err(err) => {
                self.state.toast = Some(crate::app::state::ToastNotification {
                    kind: crate::app::state::ToastKind::NeedsAttention,
                    title: "edit config failed".to_string(),
                    context: err.to_string(),
                    target: None,
                });
                self.sync_toast_deadline(previous_toast);
            }
        }
    }

    fn open_config_editor(&mut self) -> std::io::Result<()> {
        let target = resolve_write_target()?;
        let current = std::fs::read_to_string(&target).unwrap_or_default();

        // Build two temp paths: one the editor opens against, one we
        // keep as the pre-edit backup. Both live under temp_dir so the
        // overlay-cleanup pass reaps them when the pane exits.
        let workspace_temp = unique_config_edit_path("edit");
        let backup = unique_config_edit_path("backup");
        std::fs::write(&workspace_temp, &current)?;
        std::fs::write(&backup, &current)?;

        let quoted_tmp = shell_single_quote(&workspace_temp.display().to_string());
        let quoted_target = shell_single_quote(&target.display().to_string());
        // The editor edits the temp, then we cp back. Keep the temp on
        // disk -- the overlay cleanup pass removes it.
        let command = format!(
            r#"tmp={quoted_tmp}; target={quoted_target}; eval "${{EDITOR:-vi}} \"\$tmp\"" && cp "$tmp" "$target""#
        );

        let pane_id =
            self.spawn_pane_command(&command, vec![workspace_temp.clone(), backup.clone()])?;
        if let Some(entry) = self.overlay_panes.get_mut(&pane_id) {
            entry.post_exit = Some(crate::app::OverlayPostExit::ConfigEdit { target, backup });
        }
        Ok(())
    }

    /// Transient, verbatim feedback for an action that could not run
    /// (wrong target, nothing to do). Auto-expires after a few seconds.
    pub(crate) fn show_action_notice(&mut self, message: impl Into<String>) {
        self.state.action_notice = Some(message.into());
        self.action_notice_deadline =
            Some(std::time::Instant::now() + std::time::Duration::from_secs(4));
    }

    pub(super) fn update_config_file<F>(&mut self, error_context: &str, update: F) -> bool
    where
        F: FnOnce(&str) -> String,
    {
        #[cfg(test)]
        if std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR).is_none() {
            return false;
        }

        let path = crate::config::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                crate::logging::config_write_failed(&path, error_context, &err.to_string());
                self.state.config_diagnostic =
                    Some(format!("failed to save {error_context}: {err}"));
                self.config_diagnostic_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                return false;
            }
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = update(&content);
        if let Err(err) = std::fs::write(&path, new_content) {
            crate::logging::config_write_failed(&path, error_context, &err.to_string());
            self.state.config_diagnostic = Some(format!("failed to save {error_context}: {err}"));
            self.config_diagnostic_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
            return false;
        }

        true
    }

    pub(super) fn mark_onboarding_complete(&mut self) {
        self.update_config_file("onboarding setting", |content| {
            crate::config::upsert_top_level_bool(content, "onboarding", false)
        });
    }

    pub(super) fn save_theme(&mut self, name: &str) {
        if self.update_config_file("theme", |content| {
            crate::config::upsert_section_value(content, "theme", "name", &format!("\"{name}\""))
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_sound(&mut self, enabled: bool) {
        if self.update_config_file("sound setting", |content| {
            crate::config::upsert_section_bool(content, "ui.sound", "enabled", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_toast_delivery(&mut self, delivery: crate::config::ToastDelivery) {
        let value = match delivery {
            crate::config::ToastDelivery::Off => "\"off\"",
            crate::config::ToastDelivery::Herdr => "\"herdr\"",
            crate::config::ToastDelivery::Terminal => "\"terminal\"",
            crate::config::ToastDelivery::System => "\"system\"",
        };
        if self.update_config_file("toast setting", |content| {
            let content =
                crate::config::upsert_section_value(content, "ui.toast", "delivery", value);
            crate::config::remove_section_key(&content, "ui.toast", "enabled")
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_border_labels(&mut self, enabled: bool) {
        if self.update_config_file("agent border labels", |content| {
            crate::config::upsert_section_bool(
                content,
                "ui",
                "show_agent_labels_on_pane_borders",
                enabled,
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_sidebar_row_gap(&mut self, gap: u16) {
        if self.update_config_file("sidebar row gap", |content| {
            crate::config::upsert_section_value(content, "ui", "sidebar_row_gap", &gap.to_string())
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_sidebar_pane_gap(&mut self, gap: u16) {
        if self.update_config_file("sidebar pane gap", |content| {
            crate::config::upsert_section_value(content, "ui", "sidebar_pane_gap", &gap.to_string())
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_pane_history_persistence(&mut self, enabled: bool) {
        if self.update_config_file("pane screen history", |content| {
            crate::config::upsert_section_bool(content, "experimental", "pane_history", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_switch_ascii_input_source_in_prefix(&mut self, enabled: bool) {
        if self.update_config_file("prefix ascii input source", |content| {
            crate::config::upsert_section_bool(
                content,
                "experimental",
                "switch_ascii_input_source_in_prefix",
                enabled,
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_panel_scope(&mut self, scope: crate::app::state::AgentPanelScope) {
        let value = match scope {
            crate::app::state::AgentPanelScope::CurrentWorkspace => {
                crate::config::PanelScopeConfig::Current
            }
            crate::app::state::AgentPanelScope::AllWorkspaces => {
                crate::config::PanelScopeConfig::All
            }
        };
        self.save_ui_panel_scope("agent panel scope", "agent_panel_scope", value);
    }

    pub(super) fn save_servers_panel_scope(&mut self, scope: crate::app::state::PanelScope) {
        self.save_ui_panel_scope(
            "servers panel scope",
            "servers_panel_scope",
            panel_scope_config(scope),
        );
    }

    pub(super) fn save_spaces_panel_scope(&mut self, scope: crate::app::state::PanelScope) {
        self.save_ui_panel_scope(
            "spaces panel scope",
            "spaces_panel_scope",
            panel_scope_config(scope),
        );
    }

    fn save_ui_panel_scope(
        &mut self,
        label: &str,
        key: &str,
        scope: crate::config::PanelScopeConfig,
    ) {
        let value = scope.as_str();
        if self.update_config_file(label, |content| {
            crate::config::upsert_section_value(content, "ui", key, &format!("\"{value}\""))
        }) {
            self.apply_config_from_disk(false);
        }
    }
}

/// Quote `value` for safe inclusion inside a `sh -c` command. Paths from
/// `std::env::temp_dir()` and `config_path()` are well-behaved on macOS
/// and Linux, but defensive single-quoting keeps spaces and shell
/// metacharacters from breaking the eval below.
fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn unique_config_edit_path(label: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "herdr-config-edit-{label}-{}-{nanos}.toml",
        std::process::id()
    ))
}

fn panel_scope_config(scope: crate::app::state::PanelScope) -> crate::config::PanelScopeConfig {
    match scope {
        crate::app::state::PanelScope::Current => crate::config::PanelScopeConfig::Current,
        crate::app::state::PanelScope::All => crate::config::PanelScopeConfig::All,
    }
}

#[cfg(test)]
mod edit_config_tests {
    use super::*;
    use crate::app::App;
    use crate::config::Config;
    use crate::workspace::Workspace;

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("{label}-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::config::test_config_env_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    #[tokio::test]
    async fn launch_config_editor_registers_post_exit_hook_and_temp_files() {
        let _lock = lock_env();
        let dir = unique_dir("herdr-edit-launch");
        let base = dir.join("config.toml");
        std::fs::write(&base, "[ui]\nsidebar_row_gap = 1\n").unwrap();

        let previous = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &base);

        // No-op editor: just exit successfully. The pane spawn still
        // happens; we check the registration, not the editor side
        // effects (which a real PaneDied would drive).
        let previous_editor = std::env::var_os("EDITOR");
        std::env::set_var("EDITOR", "true");

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = Workspace::test_new("test");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.selected = 0;

        app.launch_config_editor();

        // Exactly one overlay should have been registered, with a
        // ConfigEdit hook pointing at the live base config.
        assert_eq!(app.overlay_panes.len(), 1, "expected a single overlay");
        let (_pane_id, overlay) = app.overlay_panes.iter().next().unwrap();
        let Some(crate::app::OverlayPostExit::ConfigEdit { target, backup }) =
            overlay.post_exit.as_ref()
        else {
            panic!(
                "expected ConfigEdit post_exit hook, got {:?}",
                overlay.post_exit
            );
        };
        assert_eq!(target, &base);
        assert!(backup.exists(), "backup file should have been written");
        // The temp_files list owns BOTH the temp the editor edits and
        // the backup -- they get reaped on PaneDied.
        assert_eq!(overlay.temp_files.len(), 2);

        // Cleanup: drain runtimes so the PTY threads stop.
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_file(&base);
        let _ = std::fs::remove_dir_all(&dir);

        match previous_editor {
            Some(value) => std::env::set_var("EDITOR", value),
            None => std::env::remove_var("EDITOR"),
        }
        match previous {
            Some(value) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, value),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }
    }

    #[tokio::test]
    async fn config_edit_rolls_back_when_edit_produces_invalid_toml() {
        let _lock = lock_env();
        let dir = unique_dir("herdr-edit-rollback");
        let base = dir.join("config.toml");
        let backup = dir.join("backup.toml");
        let good = "[ui]\nsidebar_row_gap = 1\n";
        let bad = "[ui\nsidebar_row_gap = 1\n"; // unterminated header -> parse error
                                                // The flow is: backup holds the pre-edit (good) contents; the
                                                // target was overwritten with bad contents by the editor's cp.
        std::fs::write(&backup, good).unwrap();
        std::fs::write(&base, bad).unwrap();

        let previous = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &base);

        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        let workspace = Workspace::test_new("test");
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);

        app.finish_config_edit(base.clone(), backup.clone());

        // Rollback: base should now match the good pre-edit contents.
        let restored = std::fs::read_to_string(&base).unwrap();
        assert_eq!(restored, good, "base config was not rolled back");
        // Backup gets cleaned up.
        assert!(!backup.exists(), "backup should be removed after rollback");
        // A NeedsAttention toast should describe the failure.
        let toast = app
            .state
            .toast
            .as_ref()
            .expect("rollback should surface a toast");
        assert_eq!(toast.title, "config rolled back");

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_file(&base);
        let _ = std::fs::remove_dir_all(&dir);

        match previous {
            Some(value) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, value),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }
    }
}

#[cfg(test)]
mod write_target_tests {
    use super::*;

    fn unique_dir(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("{label}-{}-{}", std::process::id(), nanos));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        // Tolerate a previous test's panic that left this lock poisoned --
        // the lock is here for serialization, not for state invariants.
        crate::config::test_config_env_lock()
            .lock()
            .unwrap_or_else(|err| err.into_inner())
    }

    #[test]
    fn write_target_uses_overlay_when_base_is_symlink() {
        let _lock = lock_env();
        let dir = unique_dir("herdr-write-target");

        // Pretend the base config is a read-only symlink to a nix-store-ish
        // path that already has content. The exact target doesn't matter --
        // the test only cares that resolve_write_target detects the symlink
        // and routes writes to the overlay file instead.
        let real_target = dir.join("config-real.toml");
        std::fs::write(&real_target, "[ui]\nsidebar_row_gap = 1\n").unwrap();
        let base = dir.join("config.toml");
        std::os::unix::fs::symlink(&real_target, &base).unwrap();

        let previous = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &base);

        let target = resolve_write_target().expect("write target resolves");
        // Resolve the expected overlay path while the env var is still
        // pointing at the test base config -- both functions key off the
        // same env, so restoring it before this read would compute the
        // real `~/.config/...` path.
        let expected = crate::config::config_overlay_path();

        match previous {
            Some(value) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, value),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }

        assert_eq!(
            target,
            expected,
            "expected the overlay path, got {}",
            target.display()
        );
        // The overlay file was materialised with a header comment so the
        // editor opens onto something explanatory.
        let content = std::fs::read_to_string(&target).unwrap();
        assert!(content.starts_with("# herdr overlay"));
    }

    #[test]
    fn write_target_uses_base_when_base_is_real_file() {
        let _lock = lock_env();
        let dir = unique_dir("herdr-write-target-base");
        let base = dir.join("config.toml");
        std::fs::write(&base, "[ui]\nsidebar_row_gap = 1\n").unwrap();

        let previous = std::env::var_os(crate::config::CONFIG_PATH_ENV_VAR);
        std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, &base);

        let target = resolve_write_target().expect("write target resolves");

        match previous {
            Some(value) => std::env::set_var(crate::config::CONFIG_PATH_ENV_VAR, value),
            None => std::env::remove_var(crate::config::CONFIG_PATH_ENV_VAR),
        }

        assert_eq!(target, base);
    }
}
