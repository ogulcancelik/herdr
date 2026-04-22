use super::App;

impl App {
    pub(super) fn update_config_file<F>(&mut self, error_context: &str, update: F)
    where
        F: FnOnce(&str) -> String,
    {
        let path = crate::config::config_path();
        if let Some(parent) = path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent) {
                self.state.config_diagnostic =
                    Some(format!("failed to save {error_context}: {err}"));
                self.config_diagnostic_deadline =
                    Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
                return;
            }
        }

        let content = std::fs::read_to_string(&path).unwrap_or_default();
        let new_content = update(&content);
        if let Err(err) = std::fs::write(&path, new_content) {
            self.state.config_diagnostic = Some(format!("failed to save {error_context}: {err}"));
            self.config_diagnostic_deadline =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(5));
        }
    }

    pub(super) fn save_theme(&mut self, name: &str) {
        self.update_config_file("theme", |content| {
            crate::config::upsert_section_value(content, "theme", "name", &format!("\"{name}\""))
        });
    }

    pub(super) fn save_sound(&mut self, enabled: bool) {
        self.update_config_file("sound setting", |content| {
            crate::config::upsert_section_bool(content, "ui.sound", "enabled", enabled)
        });
    }

    pub(super) fn save_toast(&mut self, enabled: bool) {
        self.update_config_file("toast setting", |content| {
            crate::config::upsert_section_bool(content, "ui.toast", "enabled", enabled)
        });
    }
}
