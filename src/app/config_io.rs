use super::App;
use crate::app::state::{AgentViewItem, AgentViewPreferences, SpaceViewItem, SpaceViewPreferences};
use crate::config::{AgentViewField, SpaceViewField, ViewColorPreset, ViewItem};

impl App {
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

    pub(super) fn save_pane_history_persistence(&mut self, enabled: bool) {
        if self.update_config_file("pane screen history", |content| {
            crate::config::upsert_section_bool(content, "experimental", "pane_history", enabled)
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_space_view_preferences(&mut self, preferences: SpaceViewPreferences) {
        if self.update_config_file("space sidebar preferences", |content| {
            crate::config::upsert_section_body(
                content,
                "ui.sidebar.spaces",
                &space_view_config_body(&preferences),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_view_preferences(&mut self, preferences: AgentViewPreferences) {
        if self.update_config_file("agent sidebar preferences", |content| {
            crate::config::upsert_section_body(
                content,
                "ui.sidebar.agents",
                &agent_view_config_body(&preferences),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }

    pub(super) fn save_agent_panel_scope(&mut self, scope: crate::app::state::AgentPanelScope) {
        let value = match scope {
            crate::app::state::AgentPanelScope::CurrentWorkspace => {
                crate::config::AgentPanelScopeConfig::Current.as_str()
            }
            crate::app::state::AgentPanelScope::AllWorkspaces => {
                crate::config::AgentPanelScopeConfig::All.as_str()
            }
        };
        if self.update_config_file("agent panel scope", |content| {
            crate::config::upsert_section_value(
                content,
                "ui",
                "agent_panel_scope",
                &format!("\"{value}\""),
            )
        }) {
            self.apply_config_from_disk(false);
        }
    }
}

fn space_view_config_body(preferences: &SpaceViewPreferences) -> String {
    view_item_lines_array(&preferences.lines, space_view_field_name)
}

fn agent_view_config_body(preferences: &AgentViewPreferences) -> String {
    view_item_lines_array(&preferences.lines, agent_view_field_name)
}

fn view_item_lines_array<F: Copy>(
    lines: &[Vec<ViewItem<F>>],
    field_name: fn(F) -> &'static str,
) -> String {
    let mut body = String::new();
    body.push_str("lines = [\n");
    for line in lines {
        body.push_str("  [\n");
        for item in line {
            let color = if ViewColorPreset::is_default(&item.color) {
                String::new()
            } else {
                format!(", color = \"{}\"", item.color.as_str())
            };
            body.push_str(&format!(
                "    {{ field = \"{}\", show = {}{} }},\n",
                field_name(item.field),
                item.show,
                color
            ));
        }
        body.push_str("  ],\n");
    }
    body.push_str("]\n");
    body
}

fn space_view_field_name(field: SpaceViewItem) -> &'static str {
    match field {
        SpaceViewField::Status => "status",
        SpaceViewField::Name => "name",
        SpaceViewField::Branch => "branch",
        SpaceViewField::BranchStatus => "branch_status",
    }
}

fn agent_view_field_name(field: AgentViewItem) -> &'static str {
    match field {
        AgentViewField::AgentStatus => "agent_status",
        AgentViewField::PaneName => "pane_name",
        AgentViewField::TabName => "tab_name",
        AgentViewField::SpaceName => "space_name",
        AgentViewField::Status => "status",
        AgentViewField::Time => "time",
        AgentViewField::CustomStatus => "custom_status",
        AgentViewField::AgentName => "agent_name",
        AgentViewField::RightAlignment => "right_alignment",
    }
}
