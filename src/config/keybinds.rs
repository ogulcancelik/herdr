use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;
use tracing::warn;

use super::Config;

#[derive(Debug)]
pub struct LiveKeybindConfig {
    pub prefix: (KeyCode, KeyModifiers),
    pub keybinds: Keybinds,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandKeybindType {
    #[default]
    Shell,
    Pane,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CommandKeybindConfig {
    /// Navigate-mode key that runs a command after pressing the prefix key.
    pub key: String,
    /// Command executed either in the background shell or inside a pane.
    pub command: String,
    /// Command execution mode. Default: "shell".
    #[serde(rename = "type")]
    pub action_type: CommandKeybindType,
}

impl Default for CommandKeybindConfig {
    fn default() -> Self {
        Self {
            key: String::new(),
            command: String::new(),
            action_type: CommandKeybindType::Shell,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomCommandAction {
    Shell,
    Pane,
}

#[derive(Debug, Clone)]
pub struct CustomCommandKeybind {
    pub key: (KeyCode, KeyModifiers),
    pub label: String,
    pub command: String,
    pub action: CustomCommandAction,
}

/// Parsed keybinds for navigate mode actions.
#[derive(Debug, Clone)]
pub struct Keybinds {
    pub new_workspace: (KeyCode, KeyModifiers),
    pub new_workspace_label: String,
    pub rename_workspace: (KeyCode, KeyModifiers),
    pub rename_workspace_label: String,
    pub close_workspace: (KeyCode, KeyModifiers),
    pub close_workspace_label: String,
    pub detach: Option<(KeyCode, KeyModifiers)>,
    pub detach_label: Option<String>,
    pub previous_workspace: Option<(KeyCode, KeyModifiers)>,
    pub previous_workspace_label: Option<String>,
    pub next_workspace: Option<(KeyCode, KeyModifiers)>,
    pub next_workspace_label: Option<String>,
    pub new_tab: (KeyCode, KeyModifiers),
    pub new_tab_label: String,
    pub rename_tab: Option<(KeyCode, KeyModifiers)>,
    pub rename_tab_label: Option<String>,
    pub previous_tab: Option<(KeyCode, KeyModifiers)>,
    pub previous_tab_label: Option<String>,
    pub next_tab: Option<(KeyCode, KeyModifiers)>,
    pub next_tab_label: Option<String>,
    pub close_tab: Option<(KeyCode, KeyModifiers)>,
    pub close_tab_label: Option<String>,
    pub focus_pane_left: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_left_label: Option<String>,
    pub focus_pane_down: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_down_label: Option<String>,
    pub focus_pane_up: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_up_label: Option<String>,
    pub focus_pane_right: Option<(KeyCode, KeyModifiers)>,
    pub focus_pane_right_label: Option<String>,
    pub split_vertical: (KeyCode, KeyModifiers),
    pub split_vertical_label: String,
    pub split_horizontal: (KeyCode, KeyModifiers),
    pub split_horizontal_label: String,
    pub close_pane: (KeyCode, KeyModifiers),
    pub close_pane_label: String,
    pub fullscreen: (KeyCode, KeyModifiers),
    pub fullscreen_label: String,
    pub resize_mode: (KeyCode, KeyModifiers),
    pub resize_mode_label: String,
    pub toggle_sidebar: (KeyCode, KeyModifiers),
    pub toggle_sidebar_label: String,
    pub custom_commands: Vec<CustomCommandKeybind>,
}

impl Config {
    pub(super) fn validated_keybinds(
        &self,
    ) -> (
        Option<String>,
        (KeyCode, KeyModifiers),
        Vec<String>,
        Keybinds,
    ) {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        enum BindingScope {
            Navigate,
            TerminalDirect,
        }

        #[derive(Clone)]
        struct RequiredBinding<'a> {
            scope: BindingScope,
            field: &'a str,
            label: String,
            default_label: &'a str,
            value: (KeyCode, KeyModifiers),
            default: (KeyCode, KeyModifiers),
        }

        struct OptionalBinding {
            scope: BindingScope,
            field: &'static str,
            value: Option<(KeyCode, KeyModifiers)>,
            label: Option<String>,
        }

        #[derive(Default)]
        struct BindingRegistry {
            seen: std::collections::HashMap<(BindingScope, KeyCode, KeyModifiers), String>,
        }

        impl BindingRegistry {
            fn register(
                &mut self,
                scope: BindingScope,
                binding: (KeyCode, KeyModifiers),
                field: &str,
            ) -> Option<String> {
                self.seen
                    .insert((scope, binding.0, binding.1), field.to_string())
            }

            fn conflict(
                &self,
                scope: BindingScope,
                binding: (KeyCode, KeyModifiers),
            ) -> Option<&str> {
                self.seen
                    .get(&(scope, binding.0, binding.1))
                    .map(String::as_str)
            }

            fn reserve_if_unbound(
                &mut self,
                scope: BindingScope,
                binding: (KeyCode, KeyModifiers),
                field: &str,
            ) {
                self.seen
                    .entry((scope, binding.0, binding.1))
                    .or_insert_with(|| field.to_string());
            }
        }

        fn required_binding<'a>(
            scope: BindingScope,
            field: &'a str,
            configured_label: &'a str,
            default_label: &'a str,
            default: (KeyCode, KeyModifiers),
            diagnostics: &mut Vec<String>,
        ) -> RequiredBinding<'a> {
            let (value, diag) = parse_key_combo_with_diagnostic(configured_label, field, default);
            let label = if let Some(diag) = diag {
                diagnostics.push(diag);
                default_label.to_string()
            } else {
                configured_label.to_string()
            };
            RequiredBinding {
                scope,
                field,
                label,
                default_label,
                value,
                default,
            }
        }

        fn optional_binding(
            scope: BindingScope,
            field: &'static str,
            configured_label: &str,
            diagnostics: &mut Vec<String>,
        ) -> OptionalBinding {
            if configured_label.trim().is_empty() {
                return OptionalBinding {
                    scope,
                    field,
                    value: None,
                    label: None,
                };
            }
            match parse_key_combo(configured_label) {
                Some(value) => OptionalBinding {
                    scope,
                    field,
                    value: Some(value),
                    label: Some(configured_label.to_string()),
                },
                None => {
                    let diag = format!(
                        "invalid keybinding: {field} = {:?}; disabling binding",
                        configured_label
                    );
                    warn!(message = %diag, "config diagnostic");
                    diagnostics.push(diag);
                    OptionalBinding {
                        scope,
                        field,
                        value: None,
                        label: None,
                    }
                }
            }
        }

        let mut diagnostics = Vec::new();
        let (prefix, prefix_diag) = parse_key_combo_with_diagnostic(
            &self.keys.prefix,
            "keys.prefix",
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        if let Some(diag) = &prefix_diag {
            warn!(message = %diag, "config diagnostic");
        }

        let mut bindings = vec![
            required_binding(
                BindingScope::Navigate,
                "keys.new_workspace",
                &self.keys.new_workspace,
                "n",
                (KeyCode::Char('n'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.rename_workspace",
                &self.keys.rename_workspace,
                "shift+n",
                (KeyCode::Char('n'), KeyModifiers::SHIFT),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.close_workspace",
                &self.keys.close_workspace,
                "shift+d",
                (KeyCode::Char('d'), KeyModifiers::SHIFT),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.new_tab",
                &self.keys.new_tab,
                "c",
                (KeyCode::Char('c'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.split_vertical",
                &self.keys.split_vertical,
                "v",
                (KeyCode::Char('v'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.split_horizontal",
                &self.keys.split_horizontal,
                "-",
                (KeyCode::Char('-'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.close_pane",
                &self.keys.close_pane,
                "x",
                (KeyCode::Char('x'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.fullscreen",
                &self.keys.fullscreen,
                "f",
                (KeyCode::Char('f'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.resize_mode",
                &self.keys.resize_mode,
                "r",
                (KeyCode::Char('r'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
            required_binding(
                BindingScope::Navigate,
                "keys.toggle_sidebar",
                &self.keys.toggle_sidebar,
                "b",
                (KeyCode::Char('b'), KeyModifiers::empty()),
                &mut diagnostics,
            ),
        ];

        let mut optional_bindings = vec![
            optional_binding(
                BindingScope::Navigate,
                "keys.detach",
                &self.keys.detach,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::Navigate,
                "keys.previous_workspace",
                &self.keys.previous_workspace,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::Navigate,
                "keys.next_workspace",
                &self.keys.next_workspace,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::Navigate,
                "keys.rename_tab",
                &self.keys.rename_tab,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::Navigate,
                "keys.previous_tab",
                &self.keys.previous_tab,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::Navigate,
                "keys.next_tab",
                &self.keys.next_tab,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::Navigate,
                "keys.close_tab",
                &self.keys.close_tab,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::TerminalDirect,
                "keys.focus_pane_left",
                &self.keys.focus_pane_left,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::TerminalDirect,
                "keys.focus_pane_down",
                &self.keys.focus_pane_down,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::TerminalDirect,
                "keys.focus_pane_up",
                &self.keys.focus_pane_up,
                &mut diagnostics,
            ),
            optional_binding(
                BindingScope::TerminalDirect,
                "keys.focus_pane_right",
                &self.keys.focus_pane_right,
                &mut diagnostics,
            ),
        ];

        let mut registry = BindingRegistry::default();
        for binding in &mut bindings {
            if let Some(first_field) = registry.conflict(binding.scope, binding.value) {
                let diag = format!(
                    "duplicate keybinding: {} conflicts with {}; using default {}",
                    binding.field, first_field, binding.default_label
                );
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
                binding.value = binding.default;
                binding.label = binding.default_label.to_string();
            }
            registry.register(binding.scope, binding.value, binding.field);
        }

        for binding in &mut optional_bindings {
            let Some(value) = binding.value else {
                continue;
            };
            if let Some(first_field) = registry.conflict(binding.scope, value) {
                let diag = format!(
                    "duplicate keybinding: {} conflicts with {}; disabling binding",
                    binding.field, first_field
                );
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
                binding.value = None;
                binding.label = None;
                continue;
            }
            registry.register(binding.scope, value, binding.field);
        }

        registry.reserve_if_unbound(BindingScope::Navigate, prefix, "keys.prefix");
        for (field, binding) in [
            ("navigate.quit", (KeyCode::Char('q'), KeyModifiers::empty())),
            (
                "navigate.open_workspace",
                (KeyCode::Enter, KeyModifiers::empty()),
            ),
            (
                "navigate.settings",
                (KeyCode::Char('s'), KeyModifiers::empty()),
            ),
            (
                "navigate.keybind_help",
                (KeyCode::Char('?'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_up",
                (KeyCode::Up, KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_down",
                (KeyCode::Down, KeyModifiers::empty()),
            ),
            (
                "navigate.focus_left",
                (KeyCode::Char('h'), KeyModifiers::empty()),
            ),
            (
                "navigate.focus_down",
                (KeyCode::Char('j'), KeyModifiers::empty()),
            ),
            (
                "navigate.focus_up",
                (KeyCode::Char('k'), KeyModifiers::empty()),
            ),
            (
                "navigate.focus_right",
                (KeyCode::Char('l'), KeyModifiers::empty()),
            ),
            (
                "navigate.arrow_left",
                (KeyCode::Left, KeyModifiers::empty()),
            ),
            (
                "navigate.arrow_right",
                (KeyCode::Right, KeyModifiers::empty()),
            ),
            ("navigate.tab_next", (KeyCode::Tab, KeyModifiers::empty())),
            (
                "navigate.tab_prev",
                (KeyCode::BackTab, KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_1",
                (KeyCode::Char('1'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_2",
                (KeyCode::Char('2'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_3",
                (KeyCode::Char('3'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_4",
                (KeyCode::Char('4'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_5",
                (KeyCode::Char('5'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_6",
                (KeyCode::Char('6'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_7",
                (KeyCode::Char('7'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_8",
                (KeyCode::Char('8'), KeyModifiers::empty()),
            ),
            (
                "navigate.workspace_9",
                (KeyCode::Char('9'), KeyModifiers::empty()),
            ),
            ("navigate.back", (KeyCode::Esc, KeyModifiers::empty())),
        ] {
            registry.reserve_if_unbound(BindingScope::Navigate, binding, field);
        }

        let mut custom_commands = Vec::new();
        for (index, command) in self.keys.command.iter().enumerate() {
            let key_field = format!("keys.command[{index}].key");
            let command_field = format!("keys.command[{index}].command");

            if command.command.trim().is_empty() {
                let diag =
                    format!("empty custom command: {command_field}; disabling custom command");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
                continue;
            }

            let Some(binding) = parse_key_combo(&command.key) else {
                let diag = format!(
                    "invalid keybinding: {} = {:?}; disabling custom command",
                    key_field, command.key
                );
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
                continue;
            };

            if let Some(first_field) = registry.conflict(BindingScope::Navigate, binding) {
                let diag = format!(
                    "duplicate custom keybinding: {} conflicts with {}; disabling custom command",
                    key_field, first_field
                );
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
                continue;
            }

            registry.register(BindingScope::Navigate, binding, &key_field);
            let action = match command.action_type {
                CommandKeybindType::Shell => CustomCommandAction::Shell,
                CommandKeybindType::Pane => CustomCommandAction::Pane,
            };
            custom_commands.push(CustomCommandKeybind {
                key: binding,
                label: format_key_combo(binding),
                command: command.command.clone(),
                action,
            });
        }

        let keybinds = Keybinds {
            new_workspace: bindings[0].value,
            new_workspace_label: bindings[0].label.clone(),
            rename_workspace: bindings[1].value,
            rename_workspace_label: bindings[1].label.clone(),
            close_workspace: bindings[2].value,
            close_workspace_label: bindings[2].label.clone(),
            detach: optional_bindings[0].value,
            detach_label: optional_bindings[0].label.clone(),
            previous_workspace: optional_bindings[1].value,
            previous_workspace_label: optional_bindings[1].label.clone(),
            next_workspace: optional_bindings[2].value,
            next_workspace_label: optional_bindings[2].label.clone(),
            new_tab: bindings[3].value,
            new_tab_label: bindings[3].label.clone(),
            rename_tab: optional_bindings[3].value,
            rename_tab_label: optional_bindings[3].label.clone(),
            previous_tab: optional_bindings[4].value,
            previous_tab_label: optional_bindings[4].label.clone(),
            next_tab: optional_bindings[5].value,
            next_tab_label: optional_bindings[5].label.clone(),
            close_tab: optional_bindings[6].value,
            close_tab_label: optional_bindings[6].label.clone(),
            focus_pane_left: optional_bindings[7].value,
            focus_pane_left_label: optional_bindings[7].label.clone(),
            focus_pane_down: optional_bindings[8].value,
            focus_pane_down_label: optional_bindings[8].label.clone(),
            focus_pane_up: optional_bindings[9].value,
            focus_pane_up_label: optional_bindings[9].label.clone(),
            focus_pane_right: optional_bindings[10].value,
            focus_pane_right_label: optional_bindings[10].label.clone(),
            split_vertical: bindings[4].value,
            split_vertical_label: bindings[4].label.clone(),
            split_horizontal: bindings[5].value,
            split_horizontal_label: bindings[5].label.clone(),
            close_pane: bindings[6].value,
            close_pane_label: bindings[6].label.clone(),
            fullscreen: bindings[7].value,
            fullscreen_label: bindings[7].label.clone(),
            resize_mode: bindings[8].value,
            resize_mode_label: bindings[8].label.clone(),
            toggle_sidebar: bindings[9].value,
            toggle_sidebar_label: bindings[9].label.clone(),
            custom_commands,
        };

        (prefix_diag, prefix, diagnostics, keybinds)
    }
}

pub fn format_key_combo(binding: (KeyCode, KeyModifiers)) -> String {
    let (code, modifiers) = binding;
    let mut parts = Vec::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if modifiers.contains(KeyModifiers::SHIFT) {
        parts.push("shift".to_string());
    }

    let key = match code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "shift+tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        _ => format!("{:?}", code).to_lowercase(),
    };

    if matches!(code, KeyCode::BackTab) {
        return if parts.is_empty() {
            key
        } else {
            format!("{}+tab", parts.join("+"))
        };
    }

    parts.push(key);
    parts.join("+")
}

pub(super) fn parse_key_combo(s: &str) -> Option<(KeyCode, KeyModifiers)> {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::empty();
    let mut key_str: Option<&str> = None;

    for part in &parts {
        let trimmed = part.trim();
        match trimmed.to_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "alt" | "meta" => modifiers |= KeyModifiers::ALT,
            _ if trimmed.is_empty() => return None,
            _ => {
                if key_str.is_some() {
                    return None;
                }
                key_str = Some(trimmed);
            }
        }
    }

    let key_str = key_str?;

    let lower = key_str.to_lowercase();
    let code = match lower.as_str() {
        "space" | " " => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        s if s.len() == 1 => {
            let ch = key_str.chars().next().unwrap();
            if ch.is_ascii_uppercase() {
                modifiers |= KeyModifiers::SHIFT;
                KeyCode::Char(ch.to_ascii_lowercase())
            } else {
                KeyCode::Char(ch)
            }
        }
        s if s.starts_with('f') => s[1..].parse::<u8>().ok().map(KeyCode::F)?,
        _ => return None,
    };

    Some((code, modifiers))
}

fn parse_key_combo_with_diagnostic(
    s: &str,
    field: &str,
    fallback: (KeyCode, KeyModifiers),
) -> ((KeyCode, KeyModifiers), Option<String>) {
    match parse_key_combo(s) {
        Some(binding) => (binding, None),
        None => {
            let diag = format!("invalid keybinding: {field} = {s:?}; using fallback");
            warn!(message = %diag, "config diagnostic");
            (fallback, Some(diag))
        }
    }
}
