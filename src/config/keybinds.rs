#[cfg(test)]
use crossterm::event::KeyEvent;
use crossterm::event::{KeyCode, KeyModifiers};
use serde::{Deserialize, Serialize};
use tracing::warn;

use super::Config;
use crate::input::TerminalKey;

pub type KeyCombo = (KeyCode, KeyModifiers);

#[derive(Debug, Clone)]
pub struct LiveKeybindConfig {
    pub prefix: KeyCombo,
    pub keybinds: Keybinds,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum BindingConfig {
    One(String),
    Many(Vec<String>),
}

impl Default for BindingConfig {
    fn default() -> Self {
        Self::One(String::new())
    }
}

impl BindingConfig {
    pub fn one(value: impl Into<String>) -> Self {
        Self::One(value.into())
    }

    pub fn empty() -> Self {
        Self::One(String::new())
    }

    fn values(&self) -> Vec<&str> {
        match self {
            Self::One(value) => vec![value.as_str()],
            Self::Many(values) => values.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CommandKeybindType {
    #[default]
    Shell,
    Pane,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct CommandKeybindConfig {
    /// Key that runs a command. Use `prefix+g` for prefix mode or a modified chord for direct mode.
    pub key: BindingConfig,
    /// Command executed either in the background shell or inside a pane.
    pub command: String,
    /// Command execution mode. Default: "shell".
    #[serde(rename = "type")]
    pub action_type: CommandKeybindType,
    /// Optional user-defined description for this custom command.
    pub description: Option<String>,
}

impl Default for CommandKeybindConfig {
    fn default() -> Self {
        Self {
            key: BindingConfig::empty(),
            command: String::new(),
            action_type: CommandKeybindType::Shell,
            description: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomCommandAction {
    Shell,
    Pane,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingTrigger {
    Direct(KeyCombo),
    Prefix(KeyCombo),
}

impl BindingTrigger {
    pub fn combo(self) -> KeyCombo {
        match self {
            Self::Direct(combo) | Self::Prefix(combo) => combo,
        }
    }

    pub fn is_direct(self) -> bool {
        matches!(self, Self::Direct(_))
    }

    pub fn is_prefix(self) -> bool {
        matches!(self, Self::Prefix(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedBinding {
    pub trigger: BindingTrigger,
    pub label: String,
}

impl ResolvedBinding {
    #[cfg(test)]
    fn matches_key_event(&self, key: &KeyEvent) -> bool {
        key_event_matches_combo(key, self.trigger.combo())
    }

    fn matches_terminal_key(&self, key: TerminalKey) -> bool {
        terminal_key_matches_combo(key, self.trigger.combo())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActionKeybinds {
    pub bindings: Vec<ResolvedBinding>,
}

impl ActionKeybinds {
    #[cfg(test)]
    pub fn prefix(label: &str) -> Self {
        let raw = if label.starts_with("prefix+") {
            label.to_string()
        } else {
            format!("prefix+{label}")
        };
        let trigger = parse_binding_string(&raw)
            .and_then(|parsed| match parsed {
                ParsedBinding::Single(binding) => Some(binding),
                ParsedBinding::Range(_) => None,
            })
            .expect("prefix binding should parse");
        Self {
            bindings: vec![trigger],
        }
    }

    #[cfg(test)]
    pub fn direct(label: &str) -> Self {
        let trigger = parse_binding_string(label)
            .and_then(|parsed| match parsed {
                ParsedBinding::Single(binding) => Some(binding),
                ParsedBinding::Range(_) => None,
            })
            .expect("direct binding should parse");
        Self {
            bindings: vec![trigger],
        }
    }

    #[cfg(test)]
    pub fn matches_prefix(&self, key: &KeyEvent) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.trigger.is_prefix() && binding.matches_key_event(key))
    }

    pub fn matches_prefix_key(&self, key: TerminalKey) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.trigger.is_prefix() && binding.matches_terminal_key(key))
    }

    pub fn matches_direct_key(&self, key: TerminalKey) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.trigger.is_direct() && binding.matches_terminal_key(key))
    }

    pub fn labels(&self) -> Vec<String> {
        self.bindings
            .iter()
            .map(|binding| binding.label.clone())
            .collect()
    }

    pub fn label(&self) -> Option<String> {
        let labels = self.labels();
        if labels.is_empty() {
            None
        } else {
            Some(labels.join(" / "))
        }
    }

    pub fn prefix_rhs_label(&self) -> Option<String> {
        let labels: Vec<String> = self
            .bindings
            .iter()
            .filter(|binding| binding.trigger.is_prefix())
            .map(|binding| {
                binding
                    .label
                    .strip_prefix("prefix+")
                    .unwrap_or(&binding.label)
                    .to_string()
            })
            .collect();
        if labels.is_empty() {
            None
        } else {
            Some(labels.join(" / "))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedKeybind {
    pub trigger: BindingTrigger,
    pub label: String,
}

impl IndexedKeybind {
    pub fn matched_index(&self, key: TerminalKey) -> Option<usize> {
        let KeyCode::Char(c @ '1'..='9') = key.code else {
            return None;
        };
        if terminal_key_matches_combo(key, self.trigger.combo()) {
            Some((c as usize) - ('1' as usize))
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct CustomCommandKeybind {
    pub bindings: ActionKeybinds,
    pub label: String,
    pub command: String,
    pub action: CustomCommandAction,
    pub description: Option<String>,
}

/// Parsed keybinds for Herdr actions.
#[derive(Debug, Clone)]
pub struct NavigateKeybinds {
    pub workspace_up: ActionKeybinds,
    pub workspace_down: ActionKeybinds,
    pub pane_left: ActionKeybinds,
    pub pane_down: ActionKeybinds,
    pub pane_up: ActionKeybinds,
    pub pane_right: ActionKeybinds,
}

/// Parsed keybinds for Herdr actions.
#[derive(Debug, Clone)]
pub struct Keybinds {
    pub navigate: NavigateKeybinds,
    pub help: ActionKeybinds,
    pub settings: ActionKeybinds,
    pub new_workspace: ActionKeybinds,
    pub new_worktree: ActionKeybinds,
    pub branch_session: ActionKeybinds,
    pub toggle_collapse_all: ActionKeybinds,
    pub toggle_prompt_expand: ActionKeybinds,
    pub toggle_float: ActionKeybinds,
    pub kill_worktree: ActionKeybinds,
    pub focus_attention: ActionKeybinds,
    pub focus_attention_previous: ActionKeybinds,
    pub focus_attention_project: ActionKeybinds,
    pub focus_attention_project_previous: ActionKeybinds,
    pub open_worktree: ActionKeybinds,
    pub remove_worktree: ActionKeybinds,
    pub rename_workspace: ActionKeybinds,
    pub close_workspace: ActionKeybinds,
    pub workspace_picker: ActionKeybinds,
    pub goto: ActionKeybinds,
    pub detach: ActionKeybinds,
    pub reload_config: ActionKeybinds,
    pub open_notification_target: ActionKeybinds,
    pub previous_workspace: ActionKeybinds,
    pub next_workspace: ActionKeybinds,
    pub previous_agent: ActionKeybinds,
    pub next_agent: ActionKeybinds,
    pub focus_agent: Vec<IndexedKeybind>,
    pub new_tab: ActionKeybinds,
    pub rename_tab: ActionKeybinds,
    pub previous_tab: ActionKeybinds,
    pub next_tab: ActionKeybinds,
    pub switch_tab: Vec<IndexedKeybind>,
    pub switch_workspace: Vec<IndexedKeybind>,
    pub close_tab: ActionKeybinds,
    pub rename_pane: ActionKeybinds,
    pub edit_scrollback: ActionKeybinds,
    pub copy_mode: ActionKeybinds,
    pub focus_pane_left: ActionKeybinds,
    pub focus_pane_down: ActionKeybinds,
    pub focus_pane_up: ActionKeybinds,
    pub focus_pane_right: ActionKeybinds,
    pub cycle_pane_next: ActionKeybinds,
    pub cycle_pane_previous: ActionKeybinds,
    pub last_pane: ActionKeybinds,
    pub split_vertical: ActionKeybinds,
    pub split_horizontal: ActionKeybinds,
    pub close_pane: ActionKeybinds,
    pub zoom: ActionKeybinds,
    pub resize_mode: ActionKeybinds,
    pub toggle_sidebar: ActionKeybinds,
    pub custom_commands: Vec<CustomCommandKeybind>,
}

impl Default for Keybinds {
    fn default() -> Self {
        Config::default().keybinds()
    }
}

#[derive(Clone)]
enum ParsedBinding {
    Single(ResolvedBinding),
    Range(Vec<ResolvedBinding>),
}

struct BindingRegistry {
    prefix_combo: KeyCombo,
    direct: std::collections::HashMap<KeyCombo, String>,
    prefix: std::collections::HashMap<KeyCombo, String>,
}

impl BindingRegistry {
    fn new(prefix_combo: KeyCombo) -> Self {
        Self {
            prefix_combo: normalize_key_combo(prefix_combo),
            direct: std::collections::HashMap::new(),
            prefix: std::collections::HashMap::new(),
        }
    }

    fn reserve_direct(&mut self, combo: KeyCombo, field: &str) {
        self.direct
            .entry(normalize_key_combo(combo))
            .or_insert_with(|| field.to_string());
    }

    fn prefix_rhs_is_reserved(&self, combo: KeyCombo) -> bool {
        normalize_key_combo(combo) == self.prefix_combo
    }

    fn conflict(&self, binding: &ResolvedBinding) -> Option<&str> {
        match binding.trigger {
            BindingTrigger::Direct(combo) => self
                .direct
                .get(&normalize_key_combo(combo))
                .map(String::as_str),
            BindingTrigger::Prefix(combo) => self
                .prefix
                .get(&normalize_key_combo(combo))
                .map(String::as_str),
        }
    }

    fn register(&mut self, binding: &ResolvedBinding, field: &str) {
        match binding.trigger {
            BindingTrigger::Direct(combo) => {
                self.direct
                    .insert(normalize_key_combo(combo), field.to_string());
            }
            BindingTrigger::Prefix(combo) => {
                self.prefix
                    .insert(normalize_key_combo(combo), field.to_string());
            }
        }
    }
}

impl Config {
    pub(super) fn validated_keybinds(&self) -> (Option<String>, KeyCombo, Vec<String>, Keybinds) {
        let mut diagnostics = Vec::new();
        let (prefix, prefix_diag) = parse_key_combo_with_diagnostic(
            &self.keys.prefix,
            "keys.prefix",
            (KeyCode::Char('b'), KeyModifiers::CONTROL),
        );
        if let Some(diag) = &prefix_diag {
            warn!(message = %diag, "config diagnostic");
        }

        let mut registry = BindingRegistry::new(prefix);
        registry.reserve_direct(prefix, "keys.prefix");
        let mut navigate_registry = BindingRegistry::new(prefix);
        navigate_registry.reserve_direct(prefix, "keys.prefix");
        reserve_navigate_runtime_keys(&mut navigate_registry);

        macro_rules! action {
            ($field:literal, $config:expr) => {
                parse_action_bindings($field, $config, false, &mut registry, &mut diagnostics)
            };
        }
        macro_rules! indexed {
            ($field:literal, $config:expr) => {
                parse_indexed_bindings($field, $config, &mut registry, &mut diagnostics)
            };
        }

        let mut keybinds = Keybinds {
            navigate: NavigateKeybinds {
                workspace_up: parse_navigate_bindings(
                    "keys.navigate_workspace_up",
                    &self.keys.navigate_workspace_up,
                    &mut navigate_registry,
                    &mut diagnostics,
                ),
                workspace_down: parse_navigate_bindings(
                    "keys.navigate_workspace_down",
                    &self.keys.navigate_workspace_down,
                    &mut navigate_registry,
                    &mut diagnostics,
                ),
                pane_left: parse_navigate_bindings(
                    "keys.navigate_pane_left",
                    &self.keys.navigate_pane_left,
                    &mut navigate_registry,
                    &mut diagnostics,
                ),
                pane_down: parse_navigate_bindings(
                    "keys.navigate_pane_down",
                    &self.keys.navigate_pane_down,
                    &mut navigate_registry,
                    &mut diagnostics,
                ),
                pane_up: parse_navigate_bindings(
                    "keys.navigate_pane_up",
                    &self.keys.navigate_pane_up,
                    &mut navigate_registry,
                    &mut diagnostics,
                ),
                pane_right: parse_navigate_bindings(
                    "keys.navigate_pane_right",
                    &self.keys.navigate_pane_right,
                    &mut navigate_registry,
                    &mut diagnostics,
                ),
            },
            help: action!("keys.help", &self.keys.help),
            settings: action!("keys.settings", &self.keys.settings),
            new_workspace: action!("keys.new_workspace", &self.keys.new_workspace),
            new_worktree: action!("keys.new_worktree", &self.keys.new_worktree),
            branch_session: action!("keys.branch_session", &self.keys.branch_session),
            toggle_collapse_all: action!(
                "keys.toggle_collapse_all",
                &self.keys.toggle_collapse_all
            ),
            toggle_prompt_expand: action!(
                "keys.toggle_prompt_expand",
                &self.keys.toggle_prompt_expand
            ),
            toggle_float: action!("keys.toggle_float", &self.keys.toggle_float),
            kill_worktree: action!("keys.kill_worktree", &self.keys.kill_worktree),
            focus_attention: action!("keys.focus_attention", &self.keys.focus_attention),
            focus_attention_previous: action!(
                "keys.focus_attention_previous",
                &self.keys.focus_attention_previous
            ),
            focus_attention_project: action!(
                "keys.focus_attention_project",
                &self.keys.focus_attention_project
            ),
            focus_attention_project_previous: action!(
                "keys.focus_attention_project_previous",
                &self.keys.focus_attention_project_previous
            ),
            open_worktree: action!("keys.open_worktree", &self.keys.open_worktree),
            remove_worktree: action!("keys.remove_worktree", &self.keys.remove_worktree),
            rename_workspace: action!("keys.rename_workspace", &self.keys.rename_workspace),
            close_workspace: action!("keys.close_workspace", &self.keys.close_workspace),
            workspace_picker: action!("keys.workspace_picker", &self.keys.workspace_picker),
            goto: action!("keys.goto", &self.keys.goto),
            detach: action!("keys.detach", &self.keys.detach),
            reload_config: action!("keys.reload_config", &self.keys.reload_config),
            open_notification_target: action!(
                "keys.open_notification_target",
                &self.keys.open_notification_target
            ),
            previous_workspace: action!("keys.previous_workspace", &self.keys.previous_workspace),
            next_workspace: action!("keys.next_workspace", &self.keys.next_workspace),
            previous_agent: action!("keys.previous_agent", &self.keys.previous_agent),
            next_agent: action!("keys.next_agent", &self.keys.next_agent),
            focus_agent: indexed!("keys.focus_agent", &self.keys.focus_agent),
            new_tab: action!("keys.new_tab", &self.keys.new_tab),
            rename_tab: action!("keys.rename_tab", &self.keys.rename_tab),
            previous_tab: action!("keys.previous_tab", &self.keys.previous_tab),
            next_tab: action!("keys.next_tab", &self.keys.next_tab),
            switch_tab: indexed!("keys.switch_tab", &self.keys.switch_tab),
            switch_workspace: indexed!("keys.switch_workspace", &self.keys.switch_workspace),
            close_tab: action!("keys.close_tab", &self.keys.close_tab),
            rename_pane: action!("keys.rename_pane", &self.keys.rename_pane),
            edit_scrollback: action!("keys.edit_scrollback", &self.keys.edit_scrollback),
            copy_mode: action!("keys.copy_mode", &self.keys.copy_mode),
            focus_pane_left: action!("keys.focus_pane_left", &self.keys.focus_pane_left),
            focus_pane_down: action!("keys.focus_pane_down", &self.keys.focus_pane_down),
            focus_pane_up: action!("keys.focus_pane_up", &self.keys.focus_pane_up),
            focus_pane_right: action!("keys.focus_pane_right", &self.keys.focus_pane_right),
            last_pane: action!("keys.last_pane", &self.keys.last_pane),
            cycle_pane_next: action!("keys.cycle_pane_next", &self.keys.cycle_pane_next),
            cycle_pane_previous: action!(
                "keys.cycle_pane_previous",
                &self.keys.cycle_pane_previous
            ),
            split_vertical: action!("keys.split_vertical", &self.keys.split_vertical),
            split_horizontal: action!("keys.split_horizontal", &self.keys.split_horizontal),
            close_pane: action!("keys.close_pane", &self.keys.close_pane),
            zoom: action!("keys.zoom", &self.keys.zoom),
            resize_mode: action!("keys.resize_mode", &self.keys.resize_mode),
            toggle_sidebar: action!("keys.toggle_sidebar", &self.keys.toggle_sidebar),
            custom_commands: Vec::new(),
        };

        append_legacy_indexed_bindings(
            &mut keybinds.switch_tab,
            "keys.indexed.tabs",
            &self.keys.indexed.tabs,
            &mut registry,
            &mut diagnostics,
        );
        append_legacy_indexed_bindings(
            &mut keybinds.switch_workspace,
            "keys.indexed.workspaces",
            &self.keys.indexed.workspaces,
            &mut registry,
            &mut diagnostics,
        );
        append_legacy_indexed_bindings(
            &mut keybinds.focus_agent,
            "keys.indexed.agents",
            &self.keys.indexed.agents,
            &mut registry,
            &mut diagnostics,
        );

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

            let bindings = parse_action_bindings_owned(
                &key_field,
                &command.key,
                false,
                &mut registry,
                &mut diagnostics,
            );
            if bindings.bindings.is_empty() {
                continue;
            }

            let action = match command.action_type {
                CommandKeybindType::Shell => CustomCommandAction::Shell,
                CommandKeybindType::Pane => CustomCommandAction::Pane,
            };
            let label = bindings.label().unwrap_or_else(|| "unset".to_string());
            keybinds.custom_commands.push(CustomCommandKeybind {
                bindings,
                label,
                command: command.command.clone(),
                action,
                description: command.description.clone(),
            });
        }

        (prefix_diag, prefix, diagnostics, keybinds)
    }
}

fn reserve_navigate_runtime_keys(registry: &mut BindingRegistry) {
    for combo in [
        (KeyCode::Esc, KeyModifiers::empty()),
        (KeyCode::Enter, KeyModifiers::empty()),
        (KeyCode::Tab, KeyModifiers::empty()),
        (KeyCode::BackTab, KeyModifiers::empty()),
        (KeyCode::Tab, KeyModifiers::SHIFT),
        (KeyCode::Left, KeyModifiers::empty()),
        (KeyCode::Right, KeyModifiers::empty()),
    ] {
        registry.reserve_direct(combo, "navigate reserved keys");
    }

    for idx in '1'..='9' {
        registry.reserve_direct(
            (KeyCode::Char(idx), KeyModifiers::empty()),
            "navigate reserved keys",
        );
    }
}

fn parse_action_bindings(
    field: &'static str,
    config: &BindingConfig,
    allow_ranges: bool,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
) -> ActionKeybinds {
    parse_action_bindings_owned(field, config, allow_ranges, registry, diagnostics)
}

fn parse_action_bindings_owned(
    field: &str,
    config: &BindingConfig,
    allow_ranges: bool,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
) -> ActionKeybinds {
    let mut bindings = Vec::new();
    for raw in config.values() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match parse_binding_string(raw) {
            Some(ParsedBinding::Single(binding)) => {
                if reject_binding(field, &binding, registry, diagnostics) {
                    continue;
                }
                registry.register(&binding, field);
                bindings.push(binding);
            }
            Some(ParsedBinding::Range(_)) if !allow_ranges => {
                let diag = format!("range keybinding is only valid for indexed actions: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
            Some(ParsedBinding::Range(range)) => {
                for binding in range {
                    if reject_binding(field, &binding, registry, diagnostics) {
                        continue;
                    }
                    registry.register(&binding, field);
                    bindings.push(binding);
                }
            }
            None => {
                let diag = format!("invalid keybinding: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
        }
    }
    ActionKeybinds { bindings }
}

fn parse_navigate_bindings(
    field: &'static str,
    config: &BindingConfig,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
) -> ActionKeybinds {
    let mut bindings = Vec::new();
    for raw in config.values() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        match parse_binding_string(raw) {
            Some(ParsedBinding::Single(binding)) => {
                if reject_navigate_binding(field, &binding, registry, diagnostics) {
                    continue;
                }
                registry.register(&binding, field);
                bindings.push(binding);
            }
            Some(ParsedBinding::Range(_)) => {
                let diag = format!("range keybinding is only valid for indexed actions: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
            None => {
                let diag = format!("invalid keybinding: {field} = {raw:?}; disabling binding");
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
            }
        }
    }
    ActionKeybinds { bindings }
}

fn parse_indexed_bindings(
    field: &'static str,
    config: &BindingConfig,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
) -> Vec<IndexedKeybind> {
    parse_action_bindings(field, config, true, registry, diagnostics)
        .bindings
        .into_iter()
        .filter_map(|binding| {
            if matches!(binding.trigger.combo().0, KeyCode::Char('1'..='9')) {
                Some(IndexedKeybind {
                    trigger: binding.trigger,
                    label: binding.label,
                })
            } else {
                let diag = format!(
                    "indexed keybinding must use 1..9: {field} = {:?}; disabling binding",
                    binding.label
                );
                warn!(message = %diag, "config diagnostic");
                diagnostics.push(diag);
                None
            }
        })
        .collect()
}

fn append_legacy_indexed_bindings(
    target: &mut Vec<IndexedKeybind>,
    field: &'static str,
    configured_label: &str,
    registry: &mut BindingRegistry,
    diagnostics: &mut Vec<String>,
) {
    if configured_label.trim().is_empty() {
        return;
    }
    let Some(modifiers) = parse_modifier_combo(configured_label) else {
        let diag = format!(
            "invalid indexed keybinding: {field} = {configured_label:?}; disabling binding"
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return;
    };

    for idx in 1..=9 {
        let combo = (
            KeyCode::Char(char::from_digit(idx, 10).unwrap_or('1')),
            modifiers,
        );
        let binding = ResolvedBinding {
            trigger: BindingTrigger::Direct(combo),
            label: format!("{}+{idx}", configured_label.trim()),
        };
        if reject_binding(field, &binding, registry, diagnostics) {
            continue;
        }
        registry.register(&binding, field);
        target.push(IndexedKeybind {
            trigger: binding.trigger,
            label: binding.label,
        });
    }
}

fn reject_navigate_binding(
    field: &str,
    binding: &ResolvedBinding,
    registry: &BindingRegistry,
    diagnostics: &mut Vec<String>,
) -> bool {
    if binding.trigger.is_prefix() {
        let diag = format!(
            "navigate keybinding must not include prefix: {field} = {:?}; disabling binding",
            binding.label
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if matches!(normalize_key_combo(binding.trigger.combo()).0, KeyCode::Esc) {
        let diag = format!(
            "navigate keybinding cannot use esc: {field} = {:?}; disabling binding",
            binding.label
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if let Some(first_field) = registry.conflict(binding) {
        let diag = format!("{}: kept {first_field}, disabled {field}", binding.label);
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    false
}

fn reject_binding(
    field: &str,
    binding: &ResolvedBinding,
    registry: &BindingRegistry,
    diagnostics: &mut Vec<String>,
) -> bool {
    if binding.trigger.is_prefix() && registry.prefix_rhs_is_reserved(binding.trigger.combo()) {
        let diag = format!(
            "reserved keybinding: {field} = {:?} uses keys.prefix as the prefix-mode key; pressing the prefix twice sends a literal prefix key, so this binding is disabled",
            binding.label
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if let Some(first_field) = registry.conflict(binding) {
        let diag = format!("{}: kept {first_field}, disabled {field}", binding.label);
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    if binding.trigger.is_direct() && is_unmodified_printable(binding.trigger.combo()) {
        let suggestion = format!("prefix+{}", binding.label);
        let diag = format!(
            "unsafe direct keybinding: {field} = {:?} would intercept typing; use {:?} to require the prefix; disabling binding",
            binding.label, suggestion
        );
        warn!(message = %diag, "config diagnostic");
        diagnostics.push(diag);
        return true;
    }

    false
}

fn parse_binding_string(raw: &str) -> Option<ParsedBinding> {
    let trimmed = raw.trim();
    let (trigger_prefix, body) = if let Some(rest) = trimmed.strip_prefix("prefix+") {
        (true, rest)
    } else {
        (false, trimmed)
    };

    if let Some(range_modifiers) = parse_range_modifiers(body) {
        let bindings = (1..=9)
            .map(|idx| {
                let combo = (
                    KeyCode::Char(char::from_digit(idx, 10).unwrap_or('1')),
                    range_modifiers,
                );
                let key_label = format_key_combo(combo);
                ResolvedBinding {
                    trigger: if trigger_prefix {
                        BindingTrigger::Prefix(combo)
                    } else {
                        BindingTrigger::Direct(combo)
                    },
                    label: if trigger_prefix {
                        format!("prefix+{key_label}")
                    } else {
                        key_label
                    },
                }
            })
            .collect();
        return Some(ParsedBinding::Range(bindings));
    }

    let combo = parse_key_combo(body)?;
    let label = if trigger_prefix {
        format!("prefix+{}", format_key_combo(combo))
    } else {
        format_key_combo(combo)
    };
    Some(ParsedBinding::Single(ResolvedBinding {
        trigger: if trigger_prefix {
            BindingTrigger::Prefix(combo)
        } else {
            BindingTrigger::Direct(combo)
        },
        label,
    }))
}

pub fn format_key_combo(binding: KeyCombo) -> String {
    let (code, modifiers) = binding;
    let mut parts = Vec::new();
    if modifiers.contains(KeyModifiers::CONTROL) {
        parts.push("ctrl".to_string());
    }
    if modifiers.contains(KeyModifiers::ALT) {
        parts.push("alt".to_string());
    }
    if modifiers.contains(KeyModifiers::SHIFT) && !matches!(code, KeyCode::BackTab) {
        parts.push("shift".to_string());
    }
    if modifiers.contains(KeyModifiers::SUPER) {
        parts.push(super_modifier_label().to_string());
    }
    if modifiers.contains(KeyModifiers::HYPER) {
        parts.push("hyper".to_string());
    }
    if modifiers.contains(KeyModifiers::META) {
        parts.push("meta".to_string());
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
            format!("{}+{key}", parts.join("+"))
        };
    }

    parts.push(key);
    parts.join("+")
}

fn super_modifier_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "super"
    }
}

fn parse_modifier_token(token: &str) -> Option<KeyModifiers> {
    match token.to_lowercase().as_str() {
        "ctrl" | "control" => Some(KeyModifiers::CONTROL),
        "shift" => Some(KeyModifiers::SHIFT),
        "alt" | "option" | "meta" => Some(KeyModifiers::ALT),
        "cmd" | "command" | "super" => Some(KeyModifiers::SUPER),
        "hyper" => Some(KeyModifiers::HYPER),
        _ => None,
    }
}

fn parse_range_modifiers(s: &str) -> Option<KeyModifiers> {
    let mut modifiers = KeyModifiers::empty();
    let mut saw_range = false;
    for part in s.split('+') {
        let trimmed = part.trim();
        if trimmed == "1..9" {
            if saw_range {
                return None;
            }
            saw_range = true;
        } else {
            modifiers |= parse_modifier_token(trimmed)?;
        }
    }
    saw_range.then_some(modifiers)
}

fn parse_modifier_combo(s: &str) -> Option<KeyModifiers> {
    let mut modifiers = KeyModifiers::empty();
    let parts: Vec<&str> = s.split('+').collect();
    if parts.is_empty() {
        return None;
    }

    for part in &parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return None;
        }
        modifiers |= parse_modifier_token(trimmed)?;
    }

    if modifiers.is_empty() {
        None
    } else {
        Some(modifiers)
    }
}

pub(super) fn parse_key_combo(s: &str) -> Option<KeyCombo> {
    let parts: Vec<&str> = s.split('+').collect();
    let mut modifiers = KeyModifiers::empty();
    let mut key_str: Option<&str> = None;

    for part in &parts {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            return None;
        }
        if let Some(modifier) = parse_modifier_token(trimmed) {
            modifiers |= modifier;
        } else if key_str.is_some() {
            return None;
        } else {
            key_str = Some(trimmed);
        }
    }

    let key_str = key_str?;
    let single_char = single_key_char(key_str);
    let lower = key_str.to_lowercase();
    let code = match lower.as_str() {
        "space" | " " => KeyCode::Char(' '),
        "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Esc,
        "tab" if modifiers.contains(KeyModifiers::SHIFT) => {
            modifiers.remove(KeyModifiers::SHIFT);
            KeyCode::BackTab
        }
        "tab" => KeyCode::Tab,
        "backspace" | "bs" => KeyCode::Backspace,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "minus" => KeyCode::Char('-'),
        "comma" => KeyCode::Char(','),
        "period" => KeyCode::Char('.'),
        "slash" => KeyCode::Char('/'),
        "backslash" => KeyCode::Char('\\'),
        "quote" => KeyCode::Char('\''),
        "double_quote" | "double-quote" => KeyCode::Char('"'),
        "semicolon" => KeyCode::Char(';'),
        "colon" => KeyCode::Char(':'),
        "percent" => KeyCode::Char('%'),
        "ampersand" => KeyCode::Char('&'),
        "backtick" => KeyCode::Char('`'),
        "plus" => KeyCode::Char('+'),
        _ if single_char.is_some() => {
            let ch = single_char?;
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

    Some(normalize_key_combo((code, modifiers)))
}

fn single_key_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    let ch = chars.next()?;
    if chars.next().is_none() {
        Some(ch)
    } else {
        None
    }
}

fn parse_key_combo_with_diagnostic(
    s: &str,
    field: &str,
    fallback: KeyCombo,
) -> (KeyCombo, Option<String>) {
    match parse_key_combo(s) {
        Some(binding) => (binding, None),
        None => {
            let diag = format!("invalid keybinding: {field} = {s:?}; using fallback");
            warn!(message = %diag, "config diagnostic");
            (fallback, Some(diag))
        }
    }
}

pub fn normalize_key_combo((mut code, mut modifiers): KeyCombo) -> KeyCombo {
    if matches!(code, KeyCode::Tab) && modifiers.contains(KeyModifiers::SHIFT) {
        code = KeyCode::BackTab;
        modifiers.remove(KeyModifiers::SHIFT);
    } else if matches!(code, KeyCode::BackTab) {
        modifiers.remove(KeyModifiers::SHIFT);
    }
    (code, modifiers)
}

#[cfg(test)]
pub fn key_event_matches_combo(key: &KeyEvent, combo: KeyCombo) -> bool {
    key_parts_match_combo(key.code, key.modifiers, None, combo)
}

pub fn terminal_key_matches_combo(key: TerminalKey, combo: KeyCombo) -> bool {
    key_parts_match_combo(key.code, key.modifiers, key.shifted_codepoint, combo)
}

fn key_parts_match_combo(
    actual_code: KeyCode,
    actual_modifiers: KeyModifiers,
    shifted_codepoint: Option<u32>,
    combo: KeyCombo,
) -> bool {
    let (actual_code, actual_modifiers) = normalize_key_combo((actual_code, actual_modifiers));
    let (expected_code, expected_modifiers) = normalize_key_combo(combo);

    if actual_modifiers == expected_modifiers
        && key_codes_match(
            actual_code,
            actual_modifiers,
            expected_code,
            expected_modifiers,
            shifted_codepoint,
        )
    {
        return true;
    }

    let actual_without_shift = actual_modifiers.difference(KeyModifiers::SHIFT);
    actual_modifiers.contains(KeyModifiers::SHIFT)
        && actual_without_shift == expected_modifiers
        && shifted_char_matches_expected(actual_code, shifted_codepoint, expected_code)
        || legacy_shifted_ascii_letter_matches(
            actual_code,
            actual_modifiers,
            expected_code,
            expected_modifiers,
        )
}

fn key_codes_match(
    actual: KeyCode,
    actual_modifiers: KeyModifiers,
    expected: KeyCode,
    expected_modifiers: KeyModifiers,
    shifted_codepoint: Option<u32>,
) -> bool {
    match (actual, expected) {
        (KeyCode::Char(actual), KeyCode::Char(expected))
            if actual.is_ascii_alphabetic() && expected.is_ascii_alphabetic() =>
        {
            actual == expected
                || actual_modifiers.contains(KeyModifiers::SHIFT)
                    && expected_modifiers.contains(KeyModifiers::SHIFT)
                    && actual.eq_ignore_ascii_case(&expected)
        }
        (KeyCode::Char(actual), KeyCode::Char(expected)) => {
            actual == expected
                || shifted_char_matches_expected(
                    KeyCode::Char(actual),
                    shifted_codepoint,
                    KeyCode::Char(expected),
                )
        }
        (actual, expected) => actual == expected,
    }
}

fn legacy_shifted_ascii_letter_matches(
    actual_code: KeyCode,
    actual_modifiers: KeyModifiers,
    expected_code: KeyCode,
    expected_modifiers: KeyModifiers,
) -> bool {
    if actual_modifiers.contains(KeyModifiers::SHIFT) {
        return false;
    }
    let (KeyCode::Char(actual), KeyCode::Char(expected)) = (actual_code, expected_code) else {
        return false;
    };
    actual.is_ascii_uppercase()
        && expected.is_ascii_lowercase()
        && actual.to_ascii_lowercase() == expected
        && actual_modifiers | KeyModifiers::SHIFT == expected_modifiers
}

fn shifted_char_matches_expected(
    actual_code: KeyCode,
    shifted_codepoint: Option<u32>,
    expected_code: KeyCode,
) -> bool {
    let KeyCode::Char(expected) = expected_code else {
        return false;
    };
    if shifted_codepoint.and_then(char::from_u32) == Some(expected) {
        return true;
    }
    matches!(actual_code, KeyCode::Char(actual) if actual == expected && is_shifted_punctuation(expected))
}

fn is_shifted_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '!' | '@'
            | '#'
            | '$'
            | '%'
            | '^'
            | '&'
            | '*'
            | '('
            | ')'
            | '_'
            | '+'
            | '{'
            | '}'
            | '|'
            | ':'
            | '"'
            | '<'
            | '>'
            | '?'
            | '~'
    )
}

fn is_unmodified_printable(combo: KeyCombo) -> bool {
    matches!(combo.0, KeyCode::Char(ch) if !ch.is_control())
        && combo.1.difference(KeyModifiers::SHIFT).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{config::Config, input::TerminalKey};

    fn binding_triggers(bindings: &ActionKeybinds) -> Vec<BindingTrigger> {
        bindings
            .bindings
            .iter()
            .map(|binding| binding.trigger)
            .collect()
    }

    #[test]
    fn parse_simple_char_combo() {
        assert_eq!(
            parse_key_combo("v"),
            Some((KeyCode::Char('v'), KeyModifiers::empty()))
        );
    }

    #[test]
    fn parse_unicode_char_combo() {
        assert_eq!(
            parse_key_combo("ö"),
            Some((KeyCode::Char('ö'), KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("alt+é"),
            Some((KeyCode::Char('é'), KeyModifiers::ALT))
        );
    }

    #[test]
    fn unicode_prefix_config_is_valid() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ö"
"#,
        )
        .unwrap();
        assert_eq!(
            config.prefix_key(),
            (KeyCode::Char('ö'), KeyModifiers::empty())
        );
        assert!(config.collect_diagnostics().is_empty());
    }

    #[test]
    fn parse_shift_tab_as_backtab() {
        assert_eq!(
            parse_key_combo("shift+tab"),
            Some((KeyCode::BackTab, KeyModifiers::empty()))
        );
    }

    #[test]
    fn parse_named_punctuation() {
        assert_eq!(
            parse_key_combo("minus"),
            Some((KeyCode::Char('-'), KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("comma"),
            Some((KeyCode::Char(','), KeyModifiers::empty()))
        );
        assert_eq!(
            parse_key_combo("ampersand"),
            Some((KeyCode::Char('&'), KeyModifiers::empty()))
        );
    }

    #[test]
    fn prefix_binding_is_not_direct_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
next_tab = "prefix+n"
"#,
        )
        .unwrap();
        let kb = config.keybinds();
        assert_eq!(
            binding_triggers(&kb.next_tab),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('n'),
                KeyModifiers::empty()
            ))]
        );
    }

    #[test]
    fn new_worktree_defaults_to_prefix_shift_g() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.new_worktree),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('g'),
                KeyModifiers::SHIFT
            ))]
        );
    }

    #[test]
    fn goto_defaults_to_prefix_g() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.goto),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('g'),
                KeyModifiers::empty()
            ))]
        );
    }

    #[test]
    fn open_and_remove_worktree_keybinds_are_unset_by_default() {
        let kb = Config::default().keybinds();
        assert!(kb.open_worktree.bindings.is_empty());
        assert!(kb.remove_worktree.bindings.is_empty());
    }

    #[test]
    fn copy_mode_uses_tmux_prefix_bracket_by_default() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.copy_mode),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('['),
                KeyModifiers::empty()
            ))]
        );
    }

    #[test]
    fn back_and_forth_keybinds_are_unset_by_default() {
        let kb = Config::default().keybinds();
        assert!(kb.last_pane.bindings.is_empty());
    }

    #[test]
    fn array_bindings_allow_prefix_and_modified_direct() {
        let config: Config = toml::from_str(
            r#"
[keys]
next_tab = ["prefix+n", "ctrl+alt+]"]
"#,
        )
        .unwrap();
        let kb = config.keybinds();
        assert_eq!(
            binding_triggers(&kb.next_tab),
            vec![
                BindingTrigger::Prefix((KeyCode::Char('n'), KeyModifiers::empty())),
                BindingTrigger::Direct((
                    KeyCode::Char(']'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                )),
            ]
        );
        assert_eq!(kb.next_tab.prefix_rhs_label().as_deref(), Some("n"));
    }

    #[test]
    fn unsafe_direct_printable_binding_is_disabled_with_diagnostic() {
        let config: Config = toml::from_str(
            r#"
[keys]
new_tab = "c"
close_tab = "X"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        let keybinds = config.keybinds();
        assert!(keybinds.new_tab.bindings.is_empty());
        assert!(keybinds.close_tab.bindings.is_empty());
        assert!(
            diagnostics
                .iter()
                .any(|diag| diag.contains("unsafe direct keybinding")
                    && diag.contains("keys.new_tab"))
        );
        assert!(diagnostics.iter().any(
            |diag| diag.contains("unsafe direct keybinding") && diag.contains("keys.close_tab")
        ));
    }

    #[test]
    fn shifted_letter_binding_matches_uppercase_key_event() {
        let bindings = ActionKeybinds::prefix("shift+n");
        assert!(bindings.matches_prefix(&KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT)));
    }

    #[test]
    fn shifted_letter_binding_matches_legacy_uppercase_key_event() {
        let bindings = ActionKeybinds::prefix("shift+n");
        assert!(bindings
            .matches_prefix_key(TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty(),)));
    }

    #[test]
    fn shifted_letter_direct_binding_matches_legacy_uppercase_key_event() {
        let bindings = ActionKeybinds::direct("shift+n");
        assert!(bindings
            .matches_direct_key(TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty(),)));
    }

    #[test]
    fn shifted_letter_binding_matches_modern_modified_key_event() {
        let bindings = ActionKeybinds::direct("cmd+shift+j");
        assert!(bindings.matches_direct_key(TerminalKey::new(
            KeyCode::Char('J'),
            KeyModifiers::SUPER | KeyModifiers::SHIFT,
        )));
    }

    #[test]
    fn legacy_uppercase_key_event_does_not_match_unshifted_letter_binding() {
        let bindings = ActionKeybinds::prefix("n");
        assert!(!bindings
            .matches_prefix_key(TerminalKey::new(KeyCode::Char('N'), KeyModifiers::empty(),)));
    }

    #[test]
    fn legacy_uppercase_shift_fallback_is_limited_to_ascii_letters() {
        let shifted_number = ActionKeybinds::prefix("shift+1");
        assert!(!shifted_number
            .matches_prefix_key(TerminalKey::new(KeyCode::Char('!'), KeyModifiers::empty(),)));

        let shifted_non_ascii = ActionKeybinds::prefix("shift+ö");
        assert!(!shifted_non_ascii
            .matches_prefix_key(TerminalKey::new(KeyCode::Char('Ö'), KeyModifiers::empty(),)));
    }

    #[test]
    fn shifted_tab_inputs_match_backtab_canonical_binding() {
        let bindings = ActionKeybinds::prefix("shift+tab");
        assert!(
            bindings.matches_prefix_key(TerminalKey::new(KeyCode::BackTab, KeyModifiers::empty()))
        );
        assert!(
            bindings.matches_prefix_key(TerminalKey::new(KeyCode::BackTab, KeyModifiers::SHIFT))
        );
        assert!(bindings.matches_prefix_key(TerminalKey::new(KeyCode::Tab, KeyModifiers::SHIFT)));
        assert!(!ActionKeybinds::prefix("tab")
            .matches_prefix_key(TerminalKey::new(KeyCode::Tab, KeyModifiers::SHIFT)));
        assert_eq!(
            normalize_key_combo((KeyCode::Tab, KeyModifiers::CONTROL | KeyModifiers::SHIFT)),
            (KeyCode::BackTab, KeyModifiers::CONTROL)
        );
    }

    #[test]
    fn format_modified_backtab_keeps_shift_label() {
        assert_eq!(
            format_key_combo((KeyCode::BackTab, KeyModifiers::CONTROL)),
            "ctrl+shift+tab"
        );
        assert_eq!(
            format_key_combo((KeyCode::BackTab, KeyModifiers::CONTROL | KeyModifiers::ALT)),
            "ctrl+alt+shift+tab"
        );
    }

    #[test]
    fn shifted_punctuation_matches_enhanced_input() {
        let help = ActionKeybinds::prefix("?");
        assert!(help.matches_prefix_key(TerminalKey::new(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        assert!(help.matches_prefix_key(
            TerminalKey::new(KeyCode::Char('/'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('?' as u32)
        ));

        let bang = ActionKeybinds::prefix("!");
        assert!(bang.matches_prefix_key(
            TerminalKey::new(KeyCode::Char('1'), KeyModifiers::SHIFT)
                .with_shifted_codepoint('!' as u32)
        ));
    }

    #[test]
    fn prefix_rhs_equal_to_configured_prefix_is_rejected() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
help = "prefix+ctrl+a"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        assert!(config.keybinds().help.bindings.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("reserved keybinding")
                && diag.contains("keys.help")
                && diag.contains("keys.prefix")
        }));

        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
help = "prefix+ctrl+b"
"#,
        )
        .unwrap();
        assert!(!config.keybinds().help.bindings.is_empty());
    }

    #[test]
    fn navigate_bindings_allow_plain_keys_and_reject_local_conflicts() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_up = "j"
navigate_workspace_down = "j"
navigate_pane_down = "ctrl+j"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds
            .navigate
            .workspace_up
            .matches_direct_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty())));
        assert!(keybinds.navigate.workspace_down.bindings.is_empty());
        assert!(keybinds
            .navigate
            .pane_down
            .matches_direct_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::CONTROL)));
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.navigate_workspace_up")
                && diag.contains("disabled keys.navigate_workspace_down")
        }));
    }

    #[test]
    fn navigate_bindings_reject_runtime_reserved_keys() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_up = ["esc", "alt+esc", "enter", "1", "tab", "shift+tab", "left", "right"]
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds.navigate.workspace_up.bindings.is_empty());
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diag| {
                    (diag.contains("navigate reserved keys")
                        || diag.contains("navigate keybinding cannot use esc"))
                        && diag.contains("keys.navigate_workspace_up")
                })
                .count(),
            8
        );
    }

    #[test]
    fn navigate_bindings_can_reuse_navigate_mode_prefix_rhs_keys() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_workspace_down = ["n", "f"]

[[keys.command]]
key = "prefix+f"
command = "echo hi"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds
            .navigate
            .workspace_down
            .matches_direct_key(TerminalKey::new(KeyCode::Char('n'), KeyModifiers::empty())));
        assert!(keybinds
            .navigate
            .workspace_down
            .matches_direct_key(TerminalKey::new(KeyCode::Char('f'), KeyModifiers::empty())));
        assert!(!keybinds.custom_commands.is_empty());
        assert!(!diagnostics.iter().any(|diag| {
            diag.contains("disabled keys.navigate_workspace_down")
                && (diag.contains("keys.next_tab") || diag.contains("keys.command"))
        }));
    }

    #[test]
    fn navigate_bindings_do_not_conflict_with_general_focus_pane_bindings() {
        let config: Config = toml::from_str(
            r#"
[keys]
navigate_pane_down = "j"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();

        assert!(keybinds
            .navigate
            .pane_down
            .matches_direct_key(TerminalKey::new(KeyCode::Char('j'), KeyModifiers::empty())));
    }

    #[test]
    fn navigate_bindings_reject_prefix_syntax_and_prefix_key() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+a"
navigate_workspace_up = "prefix+j"
navigate_workspace_down = "ctrl+a"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        let diagnostics = config.collect_diagnostics();

        assert!(keybinds.navigate.workspace_up.bindings.is_empty());
        assert!(keybinds.navigate.workspace_down.bindings.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("navigate keybinding must not include prefix")
                && diag.contains("keys.navigate_workspace_up")
        }));
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.prefix") && diag.contains("keys.navigate_workspace_down")
        }));
    }

    #[test]
    fn custom_command_prefix_rhs_equal_to_configured_prefix_is_rejected() {
        let config: Config = toml::from_str(
            r#"
[keys]
prefix = "ctrl+b"

[[keys.command]]
key = "prefix+ctrl+b"
command = "echo no"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        assert!(config.keybinds().custom_commands.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("reserved keybinding") && diag.contains("keys.command[0].key")
        }));
    }

    #[test]
    fn direct_custom_printable_binding_is_rejected_as_unsafe() {
        let config: Config = toml::from_str(
            r#"
[keys]

[[keys.command]]
key = "g"
command = "echo no"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        assert!(config.keybinds().custom_commands.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("unsafe direct keybinding") && diag.contains("keys.command[0].key")
        }));
    }

    #[test]
    fn direct_custom_binding_conflicting_with_builtin_is_disabled() {
        let config: Config = toml::from_str(
            r#"
[keys]
new_tab = "ctrl+alt+g"

[[keys.command]]
key = "ctrl+alt+g"
command = "echo no"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        let keybinds = config.keybinds();
        assert!(!keybinds.new_tab.bindings.is_empty());
        assert!(keybinds.custom_commands.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.new_tab") && diag.contains("disabled keys.command[0].key")
        }));
    }

    #[test]
    fn prefixed_indexed_bindings_support_modifiers() {
        let config: Config = toml::from_str(
            r#"
[keys]
switch_workspace = "prefix+shift+1..9"
"#,
        )
        .unwrap();
        let kb = config.keybinds();
        assert_eq!(kb.switch_workspace.len(), 9);
        assert_eq!(
            kb.switch_workspace[0].trigger,
            BindingTrigger::Prefix((KeyCode::Char('1'), KeyModifiers::SHIFT))
        );
        assert_eq!(kb.switch_workspace[0].label, "prefix+shift+1");
    }

    #[test]
    fn default_keymap_is_prefix_first_and_tab_centered() {
        let kb = Config::default().keybinds();
        assert_eq!(
            binding_triggers(&kb.next_tab),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('n'),
                KeyModifiers::empty()
            ))]
        );
        assert_eq!(
            binding_triggers(&kb.previous_tab),
            vec![BindingTrigger::Prefix((
                KeyCode::Char('p'),
                KeyModifiers::empty()
            ))]
        );
        assert_eq!(kb.switch_tab.len(), 9);
        assert!(kb
            .switch_tab
            .iter()
            .all(|binding| binding.trigger.is_prefix()));
        assert!(kb
            .new_tab
            .bindings
            .iter()
            .all(|binding| binding.trigger.is_prefix()));
    }

    #[test]
    fn duplicate_prefix_binding_disables_later_binding() {
        let config: Config = toml::from_str(
            r#"
[keys]
next_tab = "prefix+n"
new_workspace = "prefix+n"
"#,
        )
        .unwrap();
        let diagnostics = config.collect_diagnostics();
        let kb = config.keybinds();
        assert!(kb.next_tab.bindings.is_empty() || kb.new_workspace.bindings.is_empty());
        assert!(diagnostics.iter().any(|diag| {
            diag.contains("kept keys.new_workspace") && diag.contains("disabled keys.next_tab")
        }));
    }

    #[test]
    fn custom_command_with_description_parses() {
        let config: Config = toml::from_str(
            r#"
[[keys.command]]
key = "prefix+y"
command = "echo hello"
description = "say hello"
"#,
        )
        .unwrap();
        let keybinds = config.keybinds();
        assert_eq!(keybinds.custom_commands.len(), 1);
        assert_eq!(
            keybinds.custom_commands[0].description,
            Some("say hello".to_string())
        );
    }
    #[test]
    fn branch_session_parses_from_keys_table() {
        let config: Config = toml::from_str(
            r#"
[keys]
branch_session = "prefix+y"

[worktrees]
directory = "/tmp/x"
"#,
        )
        .unwrap();
        let kb = config.keybinds();
        assert_eq!(kb.branch_session.label().as_deref(), Some("prefix+y"));
    }
}
