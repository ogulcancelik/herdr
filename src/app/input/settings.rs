use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;

use crate::{
    app::{
        state::{
            ordered_sidebar_agent_items, ordered_sidebar_space_items, AppState, SettingsSection,
            SidebarAgentItem, SidebarAgentPreferences, SidebarConfigGroup, SidebarLine,
            SidebarSpaceItem, SidebarSpacePreferences, SIDEBAR_AGENT_ITEMS, SIDEBAR_SPACE_ITEMS,
            THEME_NAMES,
        },
        App, Mode,
    },
    config::ToastDelivery,
};

#[derive(Debug, Clone, PartialEq, Eq)]
// The shared `Save` verb is semantic: these actions persist settings.
#[allow(clippy::enum_variant_names)]
pub(super) enum SettingsAction {
    SaveTheme(String),
    SaveSound(bool),
    SaveToastDelivery(ToastDelivery),
    SaveAgentBorderLabels(bool),
    SavePaneHistory(bool),
    SaveSidebarSpace {
        previous: SidebarSpacePreferences,
        preferences: SidebarSpacePreferences,
    },
    SaveSidebarAgent {
        previous: SidebarAgentPreferences,
        preferences: SidebarAgentPreferences,
    },
    InstallRecommendedIntegrations,
}

impl App {
    pub(crate) fn handle_settings_key(&mut self, key: KeyEvent) {
        let previous_section = self.state.settings.section;
        if let Some(action) = update_settings_state(&mut self.state, key) {
            match action {
                SettingsAction::SaveTheme(name) => self.save_theme(&name),
                SettingsAction::SaveSound(enabled) => self.save_sound(enabled),
                SettingsAction::SaveToastDelivery(delivery) => self.save_toast_delivery(delivery),
                SettingsAction::SaveAgentBorderLabels(enabled) => {
                    self.save_agent_border_labels(enabled)
                }
                SettingsAction::SavePaneHistory(enabled) => {
                    self.save_pane_history_persistence(enabled)
                }
                SettingsAction::SaveSidebarSpace {
                    previous,
                    preferences,
                } => {
                    if !self.save_sidebar_space_preferences(preferences) {
                        self.state.sidebar_space = previous;
                    }
                }
                SettingsAction::SaveSidebarAgent {
                    previous,
                    preferences,
                } => {
                    if !self.save_sidebar_agent_preferences(preferences) {
                        self.state.sidebar_agent = previous;
                    }
                }
                SettingsAction::InstallRecommendedIntegrations => {
                    self.install_recommended_integrations()
                }
            }
        }
        if previous_section != SettingsSection::Integrations
            && self.state.settings.section == SettingsSection::Integrations
        {
            self.refresh_integration_recommendations();
        }
    }
}

fn normalize_theme_name(name: &str) -> String {
    name.to_lowercase().replace([' ', '_'], "-")
}

fn current_theme_index(theme_name: &str) -> usize {
    let normalized = normalize_theme_name(theme_name);
    THEME_NAMES
        .iter()
        .position(|name| normalize_theme_name(name) == normalized)
        .unwrap_or(0)
}

fn toast_delivery_index(delivery: ToastDelivery) -> usize {
    match delivery {
        ToastDelivery::Off => 0,
        ToastDelivery::Herdr => 1,
        ToastDelivery::Terminal => 2,
        ToastDelivery::System => 3,
    }
}

fn toast_delivery_for_index(idx: usize) -> ToastDelivery {
    match idx {
        0 => ToastDelivery::Off,
        1 => ToastDelivery::Herdr,
        2 => ToastDelivery::Terminal,
        _ => ToastDelivery::System,
    }
}

fn sidebar_config_row_count(group: SidebarConfigGroup) -> usize {
    match group {
        SidebarConfigGroup::Spaces => SIDEBAR_SPACE_ITEMS.len(),
        SidebarConfigGroup::Agents => SIDEBAR_AGENT_ITEMS.len(),
    }
}

fn sidebar_space_settings_lines(
    preferences: &SidebarSpacePreferences,
) -> impl Iterator<Item = SidebarLine> {
    (0..SidebarConfigGroup::Spaces.settings_line_count(preferences.lines.len()))
        .map(SidebarLine::from_index)
}

fn sidebar_agent_settings_lines(
    preferences: &SidebarAgentPreferences,
) -> impl Iterator<Item = SidebarLine> {
    (0..SidebarConfigGroup::Agents.settings_line_count(preferences.lines.len()))
        .map(SidebarLine::from_index)
}

fn sidebar_config_row_offsets(state: &AppState) -> Vec<(usize, u16)> {
    let mut rows = Vec::new();
    let mut offset = 0;
    match state.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => {
            let ordered = ordered_sidebar_space_items(&state.sidebar_space);
            for line in sidebar_space_settings_lines(&state.sidebar_space) {
                offset += 1;
                let start_len = rows.len();
                for (idx, _) in ordered
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, item)| item.line(&state.sidebar_space) == line)
                {
                    rows.push((idx, offset));
                    offset += 1;
                }
                if rows.len() == start_len {
                    offset += 1;
                }
            }
        }
        SidebarConfigGroup::Agents => {
            let ordered = ordered_sidebar_agent_items(&state.sidebar_agent);
            for line in sidebar_agent_settings_lines(&state.sidebar_agent) {
                offset += 1;
                let start_len = rows.len();
                for (idx, _) in ordered
                    .iter()
                    .copied()
                    .enumerate()
                    .filter(|(_, item)| item.line(&state.sidebar_agent) == line)
                {
                    rows.push((idx, offset));
                    offset += 1;
                }
                if rows.len() == start_len {
                    offset += 1;
                }
            }
        }
    }
    rows
}

fn selected_sidebar_space_item(state: &AppState) -> Option<SidebarSpaceItem> {
    ordered_sidebar_space_items(&state.sidebar_space)
        .get(state.settings.list.selected)
        .copied()
}

fn selected_sidebar_agent_item(state: &AppState) -> Option<SidebarAgentItem> {
    ordered_sidebar_agent_items(&state.sidebar_agent)
        .get(state.settings.list.selected)
        .copied()
}

fn normalize_sidebar_space_orders(preferences: &mut SidebarSpacePreferences) {
    for line in sidebar_space_settings_lines(preferences) {
        let mut items: Vec<_> = SIDEBAR_SPACE_ITEMS
            .iter()
            .copied()
            .filter(|item| item.line(preferences) == line)
            .collect();
        items.sort_by_key(|item| (item.order(preferences), item.default_index()));
        for (order, item) in items.into_iter().enumerate() {
            item.set_order(preferences, order as u8);
        }
    }
}

fn normalize_sidebar_agent_orders(preferences: &mut SidebarAgentPreferences) {
    for line in sidebar_agent_settings_lines(preferences) {
        let mut items: Vec<_> = SIDEBAR_AGENT_ITEMS
            .iter()
            .copied()
            .filter(|item| item.line(preferences) == line)
            .collect();
        items.sort_by_key(|item| (item.order(preferences), item.default_index()));
        for (order, item) in items.into_iter().enumerate() {
            item.set_order(preferences, order as u8);
        }
    }
}

fn selected_sidebar_space_index(
    preferences: &SidebarSpacePreferences,
    selected: SidebarSpaceItem,
) -> usize {
    ordered_sidebar_space_items(preferences)
        .iter()
        .position(|item| *item == selected)
        .unwrap_or(0)
}

fn selected_sidebar_agent_index(
    preferences: &SidebarAgentPreferences,
    selected: SidebarAgentItem,
) -> usize {
    ordered_sidebar_agent_items(preferences)
        .iter()
        .position(|item| *item == selected)
        .unwrap_or(0)
}

fn sidebar_space_line_end_order(preferences: &SidebarSpacePreferences, line: SidebarLine) -> u8 {
    SIDEBAR_SPACE_ITEMS
        .iter()
        .copied()
        .filter(|item| item.line(preferences) == line)
        .filter_map(|item| item.order(preferences).checked_add(1))
        .max()
        .unwrap_or(0)
}

fn sidebar_space_item_at_order(
    preferences: &SidebarSpacePreferences,
    line: SidebarLine,
    order: u8,
) -> Option<SidebarSpaceItem> {
    SIDEBAR_SPACE_ITEMS
        .iter()
        .copied()
        .find(|item| item.line(preferences) == line && item.order(preferences) == order)
}

fn sidebar_agent_line_end_order(preferences: &SidebarAgentPreferences, line: SidebarLine) -> u8 {
    SIDEBAR_AGENT_ITEMS
        .iter()
        .copied()
        .filter(|item| item.line(preferences) == line)
        .filter_map(|item| item.order(preferences).checked_add(1))
        .max()
        .unwrap_or(0)
}

fn sidebar_agent_item_at_order(
    preferences: &SidebarAgentPreferences,
    line: SidebarLine,
    order: u8,
) -> Option<SidebarAgentItem> {
    SIDEBAR_AGENT_ITEMS
        .iter()
        .copied()
        .find(|item| item.line(preferences) == line && item.order(preferences) == order)
}

fn move_sidebar_space_item_to_line(
    preferences: &mut SidebarSpacePreferences,
    item: SidebarSpaceItem,
    line: SidebarLine,
    order: u8,
) {
    item.set_line(preferences, line);
    item.set_order(preferences, order);
}

fn move_sidebar_agent_item_to_line(
    preferences: &mut SidebarAgentPreferences,
    item: SidebarAgentItem,
    line: SidebarLine,
    order: u8,
) {
    item.set_line(preferences, line);
    item.set_order(preferences, order);
}

fn toggle_sidebar_config_item(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => {
            let item = selected_sidebar_space_item(state)?;
            let previous = state.sidebar_space.clone();
            let mut preferences = previous.clone();
            let enabled = !item.enabled(&preferences);
            item.set_enabled(&mut preferences, enabled);
            state.sidebar_space = preferences.clone();
            Some(SettingsAction::SaveSidebarSpace {
                previous,
                preferences,
            })
        }
        SidebarConfigGroup::Agents => {
            let item = selected_sidebar_agent_item(state)?;
            let previous = state.sidebar_agent.clone();
            let mut preferences = previous.clone();
            let enabled = !item.enabled(&preferences);
            item.set_enabled(&mut preferences, enabled);
            state.sidebar_agent = preferences.clone();
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences,
            })
        }
    }
}

fn cycle_sidebar_config_item_color(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => {
            let item = selected_sidebar_space_item(state)?;
            let previous = state.sidebar_space.clone();
            let mut preferences = previous.clone();
            let next_color = item.color(&preferences).next();
            item.set_color(&mut preferences, next_color);
            state.sidebar_space = preferences.clone();
            Some(SettingsAction::SaveSidebarSpace {
                previous,
                preferences,
            })
        }
        SidebarConfigGroup::Agents => {
            let item = selected_sidebar_agent_item(state)?;
            let previous = state.sidebar_agent.clone();
            let mut preferences = previous.clone();
            let next_color = item.color(&preferences).next();
            item.set_color(&mut preferences, next_color);
            state.sidebar_agent = preferences.clone();
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences,
            })
        }
    }
}

fn reorder_selected_sidebar_item(state: &mut AppState, delta: i8) -> Option<SettingsAction> {
    match state.settings.sidebar_config_group {
        SidebarConfigGroup::Spaces => {
            let item = selected_sidebar_space_item(state)?;
            let previous = state.sidebar_space.clone();
            let mut preferences = previous.clone();
            let item_line = item.line(&preferences);
            let item_order = item.order(&preferences);
            let line_count =
                SidebarConfigGroup::Spaces.settings_line_count(preferences.lines.len());
            if delta < 0 && item_order > 0 {
                let target_order = item_order.saturating_sub(1);
                let target = sidebar_space_item_at_order(&preferences, item_line, target_order)?;
                item.set_order(&mut preferences, target_order);
                target.set_order(&mut preferences, item_order);
            } else if delta > 0
                && item_order + 1 < sidebar_space_line_end_order(&preferences, item_line)
            {
                let target_order = item_order.saturating_add(1);
                let target = sidebar_space_item_at_order(&preferences, item_line, target_order)?;
                item.set_order(&mut preferences, target_order);
                target.set_order(&mut preferences, item_order);
            } else if delta < 0 && item_line.index() > 0 {
                let target_line = SidebarLine::from_index(item_line.index() - 1);
                let insert_order = sidebar_space_line_end_order(&preferences, target_line);
                move_sidebar_space_item_to_line(&mut preferences, item, target_line, insert_order);
            } else if delta > 0 && item_line.index() + 1 < line_count {
                let target_line = SidebarLine::from_index(item_line.index() + 1);
                move_sidebar_space_item_to_line(&mut preferences, item, target_line, 0);
            } else {
                return None;
            }
            normalize_sidebar_space_orders(&mut preferences);
            state.settings.list.selected = selected_sidebar_space_index(&preferences, item);
            state.sidebar_space = preferences.clone();
            Some(SettingsAction::SaveSidebarSpace {
                previous,
                preferences,
            })
        }
        SidebarConfigGroup::Agents => {
            let item = selected_sidebar_agent_item(state)?;
            let previous = state.sidebar_agent.clone();
            let mut preferences = previous.clone();
            let item_line = item.line(&preferences);
            let item_order = item.order(&preferences);
            let line_count =
                SidebarConfigGroup::Agents.settings_line_count(preferences.lines.len());
            if delta < 0 && item_order > 0 {
                let target_order = item_order.saturating_sub(1);
                let target = sidebar_agent_item_at_order(&preferences, item_line, target_order)?;
                item.set_order(&mut preferences, target_order);
                target.set_order(&mut preferences, item_order);
            } else if delta > 0
                && item_order + 1 < sidebar_agent_line_end_order(&preferences, item_line)
            {
                let target_order = item_order.saturating_add(1);
                let target = sidebar_agent_item_at_order(&preferences, item_line, target_order)?;
                item.set_order(&mut preferences, target_order);
                target.set_order(&mut preferences, item_order);
            } else if delta < 0 && item_line.index() > 0 {
                let target_line = SidebarLine::from_index(item_line.index() - 1);
                let insert_order = sidebar_agent_line_end_order(&preferences, target_line);
                move_sidebar_agent_item_to_line(&mut preferences, item, target_line, insert_order);
            } else if delta > 0 && item_line.index() + 1 < line_count {
                let target_line = SidebarLine::from_index(item_line.index() + 1);
                move_sidebar_agent_item_to_line(&mut preferences, item, target_line, 0);
            } else {
                return None;
            }
            normalize_sidebar_agent_orders(&mut preferences);
            state.settings.list.selected = selected_sidebar_agent_index(&preferences, item);
            state.sidebar_agent = preferences.clone();
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences,
            })
        }
    }
}

fn preview_selected_theme(state: &mut AppState) {
    use crate::app::state::Palette;

    let name = THEME_NAMES[state.settings.list.selected];
    if let Some(palette) = Palette::from_name(name) {
        state.palette = palette;
        state.theme_name = name.to_string();
    }
}

fn cancel_settings(state: &mut AppState) {
    if let Some(palette) = state.settings.original_palette.take() {
        state.palette = palette;
    }
    if let Some(theme_name) = state.settings.original_theme.take() {
        state.theme_name = theme_name;
    }
    super::modal::leave_modal(state);
}

fn integrations_need_install(state: &AppState) -> bool {
    state
        .integration_recommendations
        .iter()
        .any(crate::integration::IntegrationRecommendation::needs_install)
}

fn apply_settings(state: &mut AppState) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Theme => {
            let theme_name = state.theme_name.clone();
            state.settings.original_palette = None;
            state.settings.original_theme = None;
            super::modal::leave_modal(state);
            Some(SettingsAction::SaveTheme(theme_name))
        }
        SettingsSection::Integrations if integrations_need_install(state) => {
            Some(SettingsAction::InstallRecommendedIntegrations)
        }
        SettingsSection::Integrations => None,
        _ => {
            super::modal::leave_modal(state);
            None
        }
    }
}

pub(super) fn update_settings_state(state: &mut AppState, key: KeyEvent) -> Option<SettingsAction> {
    match state.settings.section {
        SettingsSection::Theme => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                let previous = state.settings.list.selected;
                state.settings.list.move_prev();
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let previous = state.settings.list.selected;
                state.settings.list.move_next(THEME_NAMES.len());
                if state.settings.list.selected != previous {
                    preview_selected_theme(state);
                }
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Experiments;
                state.settings.list.selected = 0;
            }
            _ => match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
                Some(super::modal::ModalAction::Apply) => return apply_settings(state),
                Some(super::modal::ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
        SettingsSection::Sound => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                return Some(SettingsAction::SaveSound(enabled));
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.list.selected = toast_delivery_index(state.toast_delivery());
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Toast => match key.code {
            KeyCode::Up | KeyCode::Char('k') => state.settings.list.move_prev(),
            KeyCode::Down | KeyCode::Char('j') => state.settings.list.move_next(4),
            KeyCode::Enter | KeyCode::Char(' ') => {
                let delivery = toast_delivery_for_index(state.settings.list.selected);
                return Some(SettingsAction::SaveToastDelivery(delivery));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Sound;
                state.settings.list.selected = usize::from(!state.sound_enabled());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::PaneLabels;
                state.settings.list.selected = usize::from(!state.agent_border_labels_enabled());
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::PaneLabels => match key.code {
            KeyCode::Up | KeyCode::Char('k') | KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.selected = 1 - state.settings.list.selected.min(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let enabled = state.settings.list.selected == 0;
                return Some(SettingsAction::SaveAgentBorderLabels(enabled));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Toast;
                state.settings.list.selected = toast_delivery_index(state.toast_delivery());
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Sidebar;
                state.settings.list.selected = 0;
                state.settings.sidebar_config_group = SidebarConfigGroup::Spaces;
                state.settings.sidebar_config_editing = false;
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Sidebar => match key.code {
            KeyCode::BackTab => {
                state.settings.section = SettingsSection::PaneLabels;
                state.settings.list.selected = usize::from(!state.agent_border_labels_enabled());
                state.settings.sidebar_config_editing = false;
            }
            KeyCode::Tab => {
                state.settings.section = SettingsSection::Integrations;
                state.settings.list.selected = 0;
                state.settings.sidebar_config_editing = false;
            }
            KeyCode::Enter if state.settings.sidebar_config_editing => {
                state.settings.sidebar_config_editing = false;
            }
            KeyCode::Up | KeyCode::Char('k') if state.settings.sidebar_config_editing => {
                return reorder_selected_sidebar_item(state, -1);
            }
            KeyCode::Down | KeyCode::Char('j') if state.settings.sidebar_config_editing => {
                return reorder_selected_sidebar_item(state, 1);
            }
            KeyCode::Char(' ') if state.settings.sidebar_config_editing => {}
            KeyCode::Char('c') => {
                return cycle_sidebar_config_item_color(state);
            }
            KeyCode::Up | KeyCode::Char('k') => state.settings.list.move_prev(),
            KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.move_next(sidebar_config_row_count(
                    state.settings.sidebar_config_group,
                ));
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Right | KeyCode::Char('l')
                if state.settings.sidebar_config_editing => {}
            KeyCode::Left | KeyCode::Char('h') => {
                state.settings.sidebar_config_group =
                    state.settings.sidebar_config_group.previous();
                state.settings.list.selected = 0;
                state.settings.sidebar_config_editing = false;
            }
            KeyCode::Right | KeyCode::Char('l') => {
                state.settings.sidebar_config_group = state.settings.sidebar_config_group.next();
                state.settings.list.selected = 0;
                state.settings.sidebar_config_editing = false;
            }
            KeyCode::Enter => match state.settings.sidebar_config_group {
                SidebarConfigGroup::Spaces => {
                    if selected_sidebar_space_item(state).is_some() {
                        state.settings.sidebar_config_editing = true;
                    }
                }
                SidebarConfigGroup::Agents => {
                    if selected_sidebar_agent_item(state).is_some() {
                        state.settings.sidebar_config_editing = true;
                    }
                }
            },
            KeyCode::Char(' ') => {
                return toggle_sidebar_config_item(state);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Experiments => match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                state.settings.list.move_prev();
            }
            KeyCode::Down | KeyCode::Char('j') => {
                state.settings.list.move_next(1);
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                return Some(SettingsAction::SavePaneHistory(
                    !state.pane_history_persistence_enabled(),
                ));
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Integrations;
                state.settings.list.selected = 0;
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Theme;
                state.settings.list.selected = current_theme_index(&state.theme_name);
            }
            _ => {
                if let Some(super::modal::ModalAction::Close) =
                    super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS)
                {
                    cancel_settings(state);
                }
            }
        },
        SettingsSection::Integrations => match key.code {
            KeyCode::Enter | KeyCode::Char(' ') if integrations_need_install(state) => {
                return Some(SettingsAction::InstallRecommendedIntegrations);
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
                state.settings.section = SettingsSection::Sidebar;
                state.settings.list.selected = 0;
                state.settings.sidebar_config_group = SidebarConfigGroup::Spaces;
                state.settings.sidebar_config_editing = false;
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                state.settings.section = SettingsSection::Experiments;
                state.settings.list.selected = 0;
            }
            _ => match super::modal::modal_action_from_key(&key, super::modal::SETTINGS_ACTIONS) {
                Some(super::modal::ModalAction::Apply) => return apply_settings(state),
                Some(super::modal::ModalAction::Close) => cancel_settings(state),
                _ => {}
            },
        },
    }

    None
}

pub(crate) fn open_settings(state: &mut AppState) {
    open_settings_at(state, SettingsSection::Theme);
}

pub(crate) fn open_settings_at(state: &mut AppState, section: SettingsSection) {
    state.settings.original_palette = Some(state.palette.clone());
    state.settings.original_theme = Some(state.theme_name.clone());
    state.settings.section = section;
    state.settings.list.selected = match section {
        SettingsSection::Theme => current_theme_index(&state.theme_name),
        SettingsSection::Sound => usize::from(!state.sound_enabled()),
        SettingsSection::Toast => toast_delivery_index(state.toast_delivery()),
        SettingsSection::PaneLabels => usize::from(!state.agent_border_labels_enabled()),
        SettingsSection::Sidebar => {
            state.settings.sidebar_config_group = SidebarConfigGroup::Spaces;
            0
        }
        SettingsSection::Experiments => 0,
        SettingsSection::Integrations => 0,
    };
    state.settings.sidebar_config_editing = false;
    state.mode = Mode::Settings;
}

impl AppState {
    fn settings_popup_rect(&self) -> Rect {
        crate::ui::centered_popup_rect(
            self.screen_rect(),
            crate::ui::SETTINGS_POPUP_WIDTH,
            crate::ui::SETTINGS_POPUP_HEIGHT,
        )
        .unwrap_or_default()
    }

    fn settings_inner_rect(&self) -> Rect {
        let popup = self.settings_popup_rect();
        Rect::new(
            popup.x + 1,
            popup.y + 1,
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    }

    fn settings_tab_at(&self, col: u16, row: u16) -> Option<SettingsSection> {
        let inner = self.settings_inner_rect();
        let tab_y = inner.y + 1;
        if row != tab_y {
            return None;
        }
        let mut x = inner.x;
        for section in SettingsSection::ALL {
            let badge_width = if self.settings_section_has_badge(*section) {
                2
            } else {
                0
            };
            let width = section.label().len() as u16 + 2 + badge_width;
            if col >= x && col < x + width {
                return Some(*section);
            }
            x += width + 1;
        }
        None
    }

    pub(crate) fn settings_content_rect(&self) -> Rect {
        let inner = self.settings_inner_rect();
        crate::ui::modal_stack_areas(inner, 3, 2, 0, 1).content
    }

    fn settings_list_index_at(&self, col: u16, row: u16) -> Option<usize> {
        let area = self.settings_content_rect();
        if row < area.y || row >= area.y + area.height || col < area.x || col >= area.x + area.width
        {
            return None;
        }

        match self.settings.section {
            SettingsSection::Theme => {
                let max_visible = area.height as usize;
                let scroll = if self.settings.list.selected >= max_visible {
                    self.settings.list.selected - max_visible + 1
                } else {
                    0
                };
                let idx = scroll + (row - area.y) as usize;
                (idx < THEME_NAMES.len()).then_some(idx)
            }
            SettingsSection::Sound => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Toast => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 8 {
                    Some(((row - list_y) / 2) as usize)
                } else {
                    None
                }
            }
            SettingsSection::PaneLabels => {
                let list_y = area.y + 3;
                if row >= list_y && row < list_y + 2 {
                    Some((row - list_y) as usize)
                } else {
                    None
                }
            }
            SettingsSection::Sidebar => {
                let list_y = area.y + 3;
                let offset = row.checked_sub(list_y)?;
                sidebar_config_row_offsets(self)
                    .into_iter()
                    .find_map(|(idx, row_offset)| (row_offset == offset).then_some(idx))
            }
            SettingsSection::Experiments => {
                let list_y = area.y + 3;
                (row == list_y).then_some(0)
            }
            SettingsSection::Integrations => None,
        }
    }

    pub(super) fn handle_settings_mouse(&mut self, mouse: MouseEvent) -> Option<SettingsAction> {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(section) = self.settings_tab_at(mouse.column, mouse.row) {
                    self.settings.section = section;
                    self.settings.list.select(match section {
                        SettingsSection::Theme => current_theme_index(&self.theme_name),
                        SettingsSection::Sound => usize::from(!self.sound_enabled()),
                        SettingsSection::Toast => toast_delivery_index(self.toast_delivery()),
                        SettingsSection::PaneLabels => {
                            usize::from(!self.agent_border_labels_enabled())
                        }
                        SettingsSection::Sidebar => {
                            self.settings.sidebar_config_group = SidebarConfigGroup::Spaces;
                            self.settings.sidebar_config_editing = false;
                            0
                        }
                        SettingsSection::Experiments => 0,
                        SettingsSection::Integrations => 0,
                    });
                    return None;
                }
                if let Some(idx) = self.settings_list_index_at(mouse.column, mouse.row) {
                    self.settings.list.select(idx);
                    return match self.settings.section {
                        SettingsSection::Theme => {
                            preview_selected_theme(self);
                            None
                        }
                        SettingsSection::Sound => {
                            let enabled = idx == 0;
                            Some(SettingsAction::SaveSound(enabled))
                        }
                        SettingsSection::Toast => {
                            let delivery = toast_delivery_for_index(idx);
                            Some(SettingsAction::SaveToastDelivery(delivery))
                        }
                        SettingsSection::PaneLabels => {
                            let enabled = idx == 0;
                            Some(SettingsAction::SaveAgentBorderLabels(enabled))
                        }
                        SettingsSection::Sidebar => {
                            self.settings.sidebar_config_editing = false;
                            toggle_sidebar_config_item(self)
                        }
                        SettingsSection::Experiments => Some(SettingsAction::SavePaneHistory(
                            !self.pane_history_persistence_enabled(),
                        )),
                        SettingsSection::Integrations => None,
                    };
                }

                let inner = self.settings_inner_rect();
                let show_primary = crate::ui::settings_show_primary_action(self);
                let (apply, close) =
                    crate::ui::settings_button_rects(inner, self.settings.section, show_primary);
                let mut buttons = vec![(close, super::modal::ModalAction::Close)];
                if let Some(apply) = apply {
                    buttons.insert(0, (apply, super::modal::ModalAction::Apply));
                }
                match super::modal::modal_action_from_buttons(mouse.column, mouse.row, &buttons) {
                    Some(super::modal::ModalAction::Apply) => apply_settings(self),
                    Some(super::modal::ModalAction::Close) => {
                        cancel_settings(self);
                        None
                    }
                    _ => {
                        cancel_settings(self);
                        None
                    }
                }
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEventKind};

    use super::super::{app_for_mouse_test, mouse, state_with_workspaces};
    use super::*;

    #[test]
    fn settings_cancel_restores_previewed_theme_from_other_sections() {
        let mut state = state_with_workspaces(&["test"]);
        let original_palette = state.palette.clone();
        let original_theme = state.theme_name.clone();

        open_settings(&mut state);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_ne!(state.theme_name, original_theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(
            state.settings.section,
            crate::app::state::SettingsSection::Sound
        );

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
        );

        assert_eq!(state.mode, Mode::Terminal);
        assert_eq!(state.theme_name, original_theme);
        assert_eq!(state.palette.accent, original_palette.accent);
        assert_eq!(state.palette.panel_bg, original_palette.panel_bg);
    }

    #[test]
    fn settings_sound_toggle_returns_save_action() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings(&mut state);
        state.settings.section = crate::app::state::SettingsSection::Sound;
        state.settings.list.selected = 0;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SaveSound(true)));
        assert!(!state.sound.enabled);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_experiments_toggles_pane_history() {
        let mut state = state_with_workspaces(&["test"]);
        state.pane_history_persistence = false;
        open_settings_at(&mut state, SettingsSection::Experiments);

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, Some(SettingsAction::SavePaneHistory(true)));
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_popup_rect_uses_wide_settings_modal() {
        let mut state = state_with_workspaces(&["test"]);
        state.view.sidebar_rect = Rect::new(0, 0, 30, 40);
        state.view.terminal_area = Rect::new(30, 0, 90, 40);

        let popup = state.settings_popup_rect();

        assert_eq!(popup.width, crate::ui::SETTINGS_POPUP_WIDTH);
        assert_eq!(popup.height, crate::ui::SETTINGS_POPUP_HEIGHT);
    }

    #[test]
    fn settings_sidebar_config_switches_groups_and_toggles_agents_time() {
        let mut state = state_with_workspaces(&["test"]);
        crate::app::state::SidebarAgentItem::Time.set_enabled(&mut state.sidebar_agent, true);
        open_settings_at(&mut state, SettingsSection::Sidebar);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        assert_eq!(
            state.settings.sidebar_config_group,
            crate::app::state::SidebarConfigGroup::Agents
        );

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.list.selected, 1);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.list.selected, 2);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.list.selected, 3);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.list.selected, 4);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.list.selected, 5);

        let previous = state.sidebar_agent.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarAgentItem::Time.set_enabled(&mut expected, false);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_agent, expected);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_sidebar_config_toggles_spaces_status() {
        let mut state = state_with_workspaces(&["test"]);
        crate::app::state::SidebarSpaceItem::Status.set_enabled(&mut state.sidebar_space, false);
        open_settings_at(&mut state, SettingsSection::Sidebar);

        let previous = state.sidebar_space.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarSpaceItem::Status.set_enabled(&mut expected, true);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarSpace {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_space, expected);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_sidebar_config_c_cycles_selected_item_color() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);

        let previous = state.sidebar_space.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarSpaceItem::Status
            .set_color(&mut expected, crate::config::SidebarColorPreset::Muted);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarSpace {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_space, expected);
    }

    #[test]
    fn settings_sidebar_enter_starts_and_stops_item_editing() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, None);
        assert!(state.settings.sidebar_config_editing);

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );

        assert_eq!(action, None);
        assert!(!state.settings.sidebar_config_editing);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_sidebar_edit_space_does_not_toggle_selected_item() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let expected = state.sidebar_space.clone();

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );

        assert_eq!(action, None);
        assert_eq!(state.sidebar_space, expected);
        assert!(state.settings.sidebar_config_editing);
    }

    #[test]
    fn settings_sidebar_edit_left_right_does_not_change_item_line() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        state.settings.list.selected = 1;

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );

        assert_eq!(action, None);
        assert_eq!(
            crate::app::state::SidebarAgentItem::PaneName.line(&state.sidebar_agent),
            crate::app::state::SidebarLine::First
        );
        assert!(state.settings.sidebar_config_editing);
    }

    #[test]
    fn settings_sidebar_edit_up_down_reorders_agents_within_line() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        state.settings.list.selected = 2;

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let previous = state.sidebar_agent.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarAgentItem::TabName.set_order(&mut expected, 1);
        crate::app::state::SidebarAgentItem::PaneName.set_order(&mut expected, 2);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_agent, expected);
        assert_eq!(state.mode, Mode::Settings);
    }

    #[test]
    fn settings_sidebar_edit_down_moves_agent_item_between_line_groups() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        state.settings.list.selected = 3;

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let previous = state.sidebar_agent.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarAgentItem::SpaceName
            .set_line(&mut expected, crate::app::state::SidebarLine::Second);
        crate::app::state::SidebarAgentItem::SpaceName.set_order(&mut expected, 0);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_agent, expected);
        assert!(state.settings.sidebar_config_editing);
    }

    #[test]
    fn settings_sidebar_edit_up_moves_agent_item_to_previous_line_without_swap() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        state.settings.list.selected = 4;

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let previous = state.sidebar_agent.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarAgentItem::Status
            .set_line(&mut expected, crate::app::state::SidebarLine::First);
        crate::app::state::SidebarAgentItem::Status.set_order(&mut expected, 4);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_agent, expected);
        assert!(state.settings.sidebar_config_editing);
    }

    #[test]
    fn settings_sidebar_edit_down_moves_agent_item_to_third_line() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        state.settings.list.selected = 8;

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let previous = state.sidebar_agent.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarAgentItem::AgentName
            .set_line(&mut expected, crate::app::state::SidebarLine::Extra(2));
        crate::app::state::SidebarAgentItem::AgentName.set_order(&mut expected, 0);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_agent, expected);
        assert!(state.settings.sidebar_config_editing);
    }

    #[test]
    fn settings_sidebar_edit_down_moves_space_item_into_empty_next_line() {
        let mut state = state_with_workspaces(&["test"]);
        state.sidebar_space.lines = vec![
            crate::app::state::SIDEBAR_SPACE_ITEMS
                .into_iter()
                .map(crate::config::SidebarItem::visible)
                .collect(),
            Vec::new(),
        ];
        open_settings_at(&mut state, SettingsSection::Sidebar);
        state.settings.list.selected = 3;

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let previous = state.sidebar_space.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarSpaceItem::BranchStatus
            .set_line(&mut expected, crate::app::state::SidebarLine::Second);
        crate::app::state::SidebarSpaceItem::BranchStatus.set_order(&mut expected, 0);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarSpace {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_space, expected);
        assert!(state.settings.sidebar_config_editing);
    }

    #[test]
    fn settings_sidebar_edit_up_moves_agent_item_into_empty_previous_line() {
        let mut state = state_with_workspaces(&["test"]);
        for item in crate::app::state::SIDEBAR_AGENT_ITEMS {
            item.set_line(
                &mut state.sidebar_agent,
                crate::app::state::SidebarLine::Second,
            );
        }
        open_settings_at(&mut state, SettingsSection::Sidebar);
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Right, KeyModifiers::empty()),
        );
        state.settings.list.selected = 0;

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let previous = state.sidebar_agent.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarAgentItem::AgentStatus
            .set_line(&mut expected, crate::app::state::SidebarLine::First);
        crate::app::state::SidebarAgentItem::AgentStatus.set_order(&mut expected, 0);
        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
        );

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(state.sidebar_agent, expected);
        assert!(state.settings.sidebar_config_editing);
    }

    #[test]
    fn settings_sidebar_edit_up_on_first_item_is_noop() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        // selected = 0 (first space item, line 0, order 0).
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let before_space = state.sidebar_space.clone();
        let before_agent = state.sidebar_agent.clone();
        let before_selected = state.settings.list.selected;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Up, KeyModifiers::empty()),
        );

        assert_eq!(action, None, "Up on first item must be a no-op");
        assert_eq!(state.sidebar_space, before_space);
        assert_eq!(state.sidebar_agent, before_agent);
        assert_eq!(state.settings.list.selected, before_selected);
    }

    #[test]
    fn settings_sidebar_edit_down_on_last_item_is_noop() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Sidebar);
        let last_index = sidebar_config_row_count(crate::app::state::SidebarConfigGroup::Spaces)
            .saturating_sub(1);
        state.settings.list.selected = last_index;
        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        let before_space = state.sidebar_space.clone();
        let before_agent = state.sidebar_agent.clone();
        let before_selected = state.settings.list.selected;

        let action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
        );

        assert_eq!(action, None, "Down on last item must be a no-op");
        assert_eq!(state.sidebar_space, before_space);
        assert_eq!(state.sidebar_agent, before_agent);
        assert_eq!(state.settings.list.selected, before_selected);
    }

    #[test]
    fn settings_tab_cycle_places_experiments_last() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::PaneLabels);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Sidebar);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Integrations);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Experiments);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Theme);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Experiments);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Integrations);

        update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()),
        );
        assert_eq!(state.settings.section, SettingsSection::Sidebar);
    }

    #[test]
    fn integrations_enter_does_nothing_when_nothing_needs_install() {
        let mut state = state_with_workspaces(&["test"]);
        open_settings_at(&mut state, SettingsSection::Integrations);

        let enter_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
        );
        assert_eq!(enter_action, None);

        let space_action = update_settings_state(
            &mut state,
            KeyEvent::new(KeyCode::Char(' '), KeyModifiers::empty()),
        );
        assert_eq!(space_action, None);
    }

    #[test]
    fn settings_hover_does_not_change_selection() {
        let mut app = app_for_mouse_test();
        open_settings(&mut app.state);
        app.state.settings.list.select(0);

        let area = app.state.settings_content_rect();
        app.handle_mouse(mouse(MouseEventKind::Moved, area.x + 2, area.y + 2));

        assert_eq!(app.state.settings.list.selected, 0);
    }

    #[test]
    fn settings_mouse_click_toggles_pane_history() {
        let mut app = app_for_mouse_test();
        app.state.pane_history_persistence = false;
        open_settings_at(&mut app.state, SettingsSection::Experiments);

        let area = app.state.settings_content_rect();
        let action = app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            area.x + 2,
            area.y + 3,
        ));

        assert_eq!(action, Some(SettingsAction::SavePaneHistory(true)));
        assert_eq!(app.state.settings.list.selected, 0);
    }

    #[test]
    fn settings_mouse_click_toggles_sidebar_agent_right_align_row() {
        let mut app = app_for_mouse_test();
        app.state.view.sidebar_rect = Rect::new(0, 0, 26, 40);
        app.state.view.terminal_area = Rect::new(26, 0, 100, 40);
        crate::app::state::SidebarAgentItem::RightAlignment
            .set_enabled(&mut app.state.sidebar_agent, true);
        open_settings_at(&mut app.state, SettingsSection::Sidebar);
        app.state.settings.sidebar_config_group = crate::app::state::SidebarConfigGroup::Agents;

        let area = app.state.settings_content_rect();
        let previous = app.state.sidebar_agent.clone();
        let mut expected = previous.clone();
        crate::app::state::SidebarAgentItem::RightAlignment.set_enabled(&mut expected, false);
        let action = app.state.handle_settings_mouse(mouse(
            MouseEventKind::Down(crossterm::event::MouseButton::Left),
            area.x + 2,
            area.y + 12,
        ));

        assert_eq!(
            action,
            Some(SettingsAction::SaveSidebarAgent {
                previous,
                preferences: expected.clone(),
            })
        );
        assert_eq!(app.state.sidebar_agent, expected);
        assert_eq!(app.state.settings.list.selected, 7);
    }

    #[test]
    fn integration_update_badge_only_tracks_outdated_recommendations() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::NotInstalled,
            false,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Current,
            true,
        )];
        assert!(!state.integration_updates_available());

        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        assert!(state.integration_updates_available());
    }

    #[test]
    fn settings_tab_hit_area_includes_integration_update_badge() {
        let mut state = state_with_workspaces(&["test"]);
        state.integration_recommendations = vec![integration_recommendation(
            crate::integration::IntegrationStatusKind::Outdated,
            true,
        )];
        open_settings(&mut state);

        let inner = state.settings_inner_rect();
        let tab_y = inner.y + 1;
        let integrations_idx = SettingsSection::ALL
            .iter()
            .position(|section| *section == SettingsSection::Integrations)
            .expect("integrations section should be present");
        let integrations_x = inner.x
            + SettingsSection::ALL[..integrations_idx]
                .iter()
                .map(|section| {
                    let badge_width = if state.settings_section_has_badge(*section) {
                        2
                    } else {
                        0
                    };
                    section.label().len() as u16 + 3 + badge_width
                })
                .sum::<u16>();
        let dotted_width = SettingsSection::Integrations.label().len() as u16 + 4;

        assert_eq!(
            state.settings_tab_at(integrations_x + dotted_width - 1, tab_y),
            Some(SettingsSection::Integrations)
        );
    }

    fn integration_recommendation(
        state: crate::integration::IntegrationStatusKind,
        available: bool,
    ) -> crate::integration::IntegrationRecommendation {
        crate::integration::IntegrationRecommendation {
            target: crate::api::schema::IntegrationTarget::Claude,
            label: "claude",
            command: "claude",
            available,
            path: std::path::PathBuf::from("/tmp/herdr-test-integration"),
            state,
        }
    }
}
