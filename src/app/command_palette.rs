use crate::{
    app::{
        input::{can_execute_navigate_action, ActionContext, NavigateAction},
        state::{text_matches_query, AppState},
        Mode,
    },
    config::{ActionKeybinds, Keybinds},
    terminal::TerminalRuntimeRegistry,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum KeybindCommandGroup {
    Global,
    Navigation,
    WorkspacesTabs,
    Panes,
    Custom,
}

impl KeybindCommandGroup {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Navigation => "navigation",
            Self::WorkspacesTabs => "workspaces / tabs",
            Self::Panes => "panes",
            Self::Custom => "custom",
        }
    }

    fn order(self) -> u8 {
        match self {
            Self::Global => 0,
            Self::Navigation => 1,
            Self::WorkspacesTabs => 2,
            Self::Panes => 3,
            Self::Custom => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeybindCommand {
    Navigate(NavigateAction),
    CustomCommand(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct KeybindCommandEntry {
    pub group: KeybindCommandGroup,
    pub label: String,
    pub shortcuts: String,
    pub command: KeybindCommand,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct KeybindHelpEntry {
    pub group: KeybindCommandGroup,
    pub label: String,
    pub shortcuts: String,
    pub command: KeybindCommand,
}

#[derive(Debug, Clone)]
pub(crate) enum KeybindHelpRow {
    Spacer,
    Header(KeybindCommandGroup),
    Entry(usize),
}

#[derive(Debug, Clone)]
pub(crate) struct KeybindHelpModel {
    pub entries: Vec<KeybindHelpEntry>,
    pub rows: Vec<KeybindHelpRow>,
}

#[derive(Debug, Clone, Copy)]
struct NavigateCommandSpec {
    group: KeybindCommandGroup,
    label: &'static str,
    action: NavigateAction,
}

const NAVIGATE_COMMAND_SPECS: &[NavigateCommandSpec] = &[
    NavigateCommandSpec {
        group: KeybindCommandGroup::Global,
        label: "keybinds",
        action: NavigateAction::Help,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Global,
        label: "settings",
        action: NavigateAction::Settings,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Global,
        label: "reload config",
        action: NavigateAction::ReloadConfig,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Global,
        label: "open notification target",
        action: NavigateAction::OpenNotificationTarget,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Global,
        label: "detach",
        action: NavigateAction::Detach,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Navigation,
        label: "workspace navigation",
        action: NavigateAction::WorkspacePicker,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Navigation,
        label: "session navigator",
        action: NavigateAction::OpenNavigator,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "new workspace",
        action: NavigateAction::NewWorkspace,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "new worktree",
        action: NavigateAction::NewWorktree,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "open worktree",
        action: NavigateAction::OpenWorktree,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "delete worktree checkout",
        action: NavigateAction::RemoveWorktree,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "rename workspace",
        action: NavigateAction::RenameWorkspace,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "close workspace",
        action: NavigateAction::CloseWorkspace,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "previous workspace",
        action: NavigateAction::PreviousWorkspace,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "next workspace",
        action: NavigateAction::NextWorkspace,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "previous agent",
        action: NavigateAction::PreviousAgent,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "next agent",
        action: NavigateAction::NextAgent,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "new tab",
        action: NavigateAction::NewTab,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "rename tab",
        action: NavigateAction::RenameTab,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "previous tab",
        action: NavigateAction::PreviousTab,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "next tab",
        action: NavigateAction::NextTab,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::WorkspacesTabs,
        label: "close tab",
        action: NavigateAction::CloseTab,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "split vertical",
        action: NavigateAction::SplitVertical,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "split horizontal",
        action: NavigateAction::SplitHorizontal,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "close pane",
        action: NavigateAction::ClosePane,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "rename pane",
        action: NavigateAction::RenamePane,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "edit scrollback",
        action: NavigateAction::EditScrollback,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "copy mode",
        action: NavigateAction::CopyMode,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "zoom pane",
        action: NavigateAction::Zoom,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "resize mode",
        action: NavigateAction::EnterResizeMode,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "toggle sidebar",
        action: NavigateAction::ToggleSidebar,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "focus pane left",
        action: NavigateAction::FocusPaneLeft,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "focus pane down",
        action: NavigateAction::FocusPaneDown,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "focus pane up",
        action: NavigateAction::FocusPaneUp,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "focus pane right",
        action: NavigateAction::FocusPaneRight,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "cycle pane next",
        action: NavigateAction::CyclePaneNext,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "cycle pane previous",
        action: NavigateAction::CyclePanePrevious,
    },
    NavigateCommandSpec {
        group: KeybindCommandGroup::Panes,
        label: "last pane",
        action: NavigateAction::LastPane,
    },
];

pub(crate) fn keybind_help_action_context(state: &AppState) -> ActionContext {
    if state.keybind_help.origin_mode == Mode::Navigate {
        ActionContext::Navigate
    } else {
        ActionContext::Prefix
    }
}

pub(crate) fn build_keybind_help_model(state: &AppState) -> KeybindHelpModel {
    let query = state.keybind_help.query.trim().to_lowercase();
    let context = keybind_help_action_context(state);
    let mut entries: Vec<KeybindHelpEntry> = all_entries(state, context)
        .into_iter()
        .filter_map(|entry| {
            if !entry.enabled {
                return None;
            }
            let haystack = format!(
                "{} {} {}",
                entry.group.label(),
                entry.shortcuts.to_lowercase(),
                entry.label.to_lowercase()
            );
            if !query.is_empty() && !keybind_help_matches_query(&query, &haystack) {
                return None;
            }
            Some(KeybindHelpEntry {
                group: entry.group,
                label: entry.label,
                shortcuts: entry.shortcuts,
                command: entry.command,
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        a.group
            .order()
            .cmp(&b.group.order())
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });

    let mut rows = Vec::new();
    let mut last_group = None;
    for (idx, entry) in entries.iter().enumerate() {
        if last_group != Some(entry.group) {
            if last_group.is_some() {
                rows.push(KeybindHelpRow::Spacer);
            }
            rows.push(KeybindHelpRow::Header(entry.group));
            last_group = Some(entry.group);
        }
        rows.push(KeybindHelpRow::Entry(idx));
    }
    KeybindHelpModel { entries, rows }
}

fn keybind_help_matches_query(query: &str, haystack: &str) -> bool {
    if text_matches_query(query, haystack) {
        return true;
    }

    let compact_query: String = query.split_whitespace().collect();
    if compact_query.is_empty() {
        return false;
    }
    let compact_haystack: String = haystack.split_whitespace().collect();
    compact_haystack.contains(&compact_query)
}

pub(crate) fn normalize_keybind_help_selection(state: &mut AppState, model: &KeybindHelpModel) {
    if model.entries.is_empty() {
        state.keybind_help.selected = 0;
        return;
    }
    state.keybind_help.selected = state
        .keybind_help
        .selected
        .min(model.entries.len().saturating_sub(1));
}

pub(crate) fn keybind_help_selected_row(
    model: &KeybindHelpModel,
    selected: usize,
) -> Option<usize> {
    model.rows.iter().position(|row| match row {
        KeybindHelpRow::Entry(idx) => *idx == selected,
        KeybindHelpRow::Header(_) | KeybindHelpRow::Spacer => false,
    })
}

fn keybind_label(bindings: &ActionKeybinds) -> String {
    bindings.label().unwrap_or_else(|| "unset".to_string())
}

fn keybinds_for_navigate_action(kb: &Keybinds, action: NavigateAction) -> Option<&ActionKeybinds> {
    match action {
        NavigateAction::Help => Some(&kb.help),
        NavigateAction::Settings => Some(&kb.settings),
        NavigateAction::ReloadConfig => Some(&kb.reload_config),
        NavigateAction::OpenNotificationTarget => Some(&kb.open_notification_target),
        NavigateAction::Detach => Some(&kb.detach),
        NavigateAction::WorkspacePicker => Some(&kb.workspace_picker),
        NavigateAction::OpenNavigator => Some(&kb.goto),
        NavigateAction::NewWorkspace => Some(&kb.new_workspace),
        NavigateAction::NewWorktree => Some(&kb.new_worktree),
        NavigateAction::OpenWorktree => Some(&kb.open_worktree),
        NavigateAction::RemoveWorktree => Some(&kb.remove_worktree),
        NavigateAction::RenameWorkspace => Some(&kb.rename_workspace),
        NavigateAction::CloseWorkspace => Some(&kb.close_workspace),
        NavigateAction::PreviousWorkspace => Some(&kb.previous_workspace),
        NavigateAction::NextWorkspace => Some(&kb.next_workspace),
        NavigateAction::PreviousAgent => Some(&kb.previous_agent),
        NavigateAction::NextAgent => Some(&kb.next_agent),
        NavigateAction::NewTab => Some(&kb.new_tab),
        NavigateAction::RenameTab => Some(&kb.rename_tab),
        NavigateAction::PreviousTab => Some(&kb.previous_tab),
        NavigateAction::NextTab => Some(&kb.next_tab),
        NavigateAction::CloseTab => Some(&kb.close_tab),
        NavigateAction::SplitVertical => Some(&kb.split_vertical),
        NavigateAction::SplitHorizontal => Some(&kb.split_horizontal),
        NavigateAction::ClosePane => Some(&kb.close_pane),
        NavigateAction::RenamePane => Some(&kb.rename_pane),
        NavigateAction::EditScrollback => Some(&kb.edit_scrollback),
        NavigateAction::CopyMode => Some(&kb.copy_mode),
        NavigateAction::Zoom => Some(&kb.zoom),
        NavigateAction::EnterResizeMode => Some(&kb.resize_mode),
        NavigateAction::ToggleSidebar => Some(&kb.toggle_sidebar),
        NavigateAction::FocusPaneLeft => Some(&kb.focus_pane_left),
        NavigateAction::FocusPaneDown => Some(&kb.focus_pane_down),
        NavigateAction::FocusPaneUp => Some(&kb.focus_pane_up),
        NavigateAction::FocusPaneRight => Some(&kb.focus_pane_right),
        NavigateAction::CyclePaneNext => Some(&kb.cycle_pane_next),
        NavigateAction::CyclePanePrevious => Some(&kb.cycle_pane_previous),
        NavigateAction::LastPane => Some(&kb.last_pane),
        NavigateAction::SwitchWorkspace(_)
        | NavigateAction::SwitchTab(_)
        | NavigateAction::FocusAgent(_)
        | NavigateAction::SwapPaneLeft
        | NavigateAction::SwapPaneDown
        | NavigateAction::SwapPaneUp
        | NavigateAction::SwapPaneRight => None,
    }
}

fn all_entries(state: &AppState, context: ActionContext) -> Vec<KeybindCommandEntry> {
    let mut entries = Vec::new();
    for spec in NAVIGATE_COMMAND_SPECS {
        let Some(bindings) = keybinds_for_navigate_action(&state.keybinds, spec.action) else {
            continue;
        };
        let command = KeybindCommand::Navigate(spec.action);
        let (enabled, _) = can_execute_keybind_command(state, command, context, None);
        entries.push(KeybindCommandEntry {
            group: spec.group,
            label: spec.label.to_string(),
            shortcuts: keybind_label(bindings),
            command,
            enabled,
        });
    }

    for (idx, binding) in state.keybinds.custom_commands.iter().enumerate() {
        let command = KeybindCommand::CustomCommand(idx);
        let (enabled, _) = can_execute_keybind_command(state, command, context, None);
        entries.push(KeybindCommandEntry {
            group: KeybindCommandGroup::Custom,
            label: binding
                .description
                .clone()
                .unwrap_or_else(|| binding.command.clone()),
            shortcuts: keybind_label(&binding.bindings),
            command,
            enabled,
        });
    }

    entries
}

pub(crate) fn can_execute_keybind_command(
    state: &AppState,
    command: KeybindCommand,
    context: ActionContext,
    terminal_runtimes: Option<&TerminalRuntimeRegistry>,
) -> (bool, Option<String>) {
    match command {
        KeybindCommand::CustomCommand(_) => {
            if state.active.is_none() {
                (false, Some("no active workspace".to_string()))
            } else {
                (true, None)
            }
        }
        KeybindCommand::Navigate(action) => {
            can_execute_navigate_action(state, terminal_runtimes, action, context)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_pane_unavailable_without_workspace() {
        let state = AppState::test_new();
        let (enabled, reason) = can_execute_keybind_command(
            &state,
            KeybindCommand::Navigate(NavigateAction::ClosePane),
            ActionContext::Prefix,
            None,
        );
        assert!(!enabled);
        assert_eq!(reason.as_deref(), Some("no active workspace"));
    }
}
