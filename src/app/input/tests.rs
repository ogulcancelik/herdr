use super::{
    modal::{modal_action_from_buttons, open_new_tab_dialog, open_rename_active_tab, ModalAction},
    mouse::wheel_routing,
    navigate::{execute_navigate_action, handle_navigate_reserved_key, NavigateAction},
    settings::{open_settings, update_settings_state},
    *,
};
use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::{
    app::state::{
        AgentPanelScope, AppState, ContextMenuKind, ContextMenuState, DragTarget, MenuListState,
        Mode,
    },
    config::Config,
    detect::Agent,
    workspace::Workspace,
};
use crossterm::event::{KeyModifiers, MouseEvent};
use ratatui::layout::Rect;

fn state_with_workspaces(names: &[&str]) -> AppState {
    let mut state = AppState::test_new();
    state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
    if !state.workspaces.is_empty() {
        state.active = Some(0);
        state.selected = 0;
        state.mode = Mode::Navigate;
    }
    state
}

fn app_for_mouse_test() -> App {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        true,
        None,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.mode = Mode::Terminal;
    app.state.update_available = None;
    app.state.latest_release_notes_available = false;
    app.state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
    app.state.view.terminal_area = Rect::new(26, 0, 80, 20);
    app
}

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::empty(),
    }
}

fn numbered_lines_bytes(count: usize) -> Vec<u8> {
    (0..count)
        .map(|i| format!("{i:06}\r\n"))
        .collect::<String>()
        .into_bytes()
}

fn capture_snapshot(state: &AppState) -> crate::persist::SessionSnapshot {
    crate::persist::capture(
        &state.workspaces,
        state.active,
        state.selected,
        state.agent_panel_scope,
        state.sidebar_width,
        state.sidebar_section_split,
    )
}

fn unique_temp_path(name: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
}

fn wait_for_file(path: &Path) -> String {
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if let Ok(content) = fs::read_to_string(path) {
            return content;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {}", path.display());
}

fn root_layout_ratio(snapshot: &crate::persist::SessionSnapshot) -> Option<f32> {
    match &snapshot.workspaces.first()?.tabs.first()?.layout {
        crate::persist::LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
        crate::persist::LayoutSnapshot::Pane(_) => None,
    }
}

#[test]
fn custom_rename_key_enters_rename_mode() {
    let mut state = state_with_workspaces(&["test"]);
    state.keybinds.rename_workspace = (KeyCode::Char('g'), KeyModifiers::empty());
    state.keybinds.rename_workspace_label = "g".into();

    handle_navigate_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::RenameWorkspace);
    assert_eq!(state.name_input, "test");
}

#[test]
fn custom_new_workspace_key_requests_and_exits_navigate() {
    let mut state = state_with_workspaces(&["test"]);
    state.keybinds.new_workspace = (KeyCode::Char('g'), KeyModifiers::empty());
    state.keybinds.new_workspace_label = "g".into();

    handle_navigate_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
    );

    assert!(state.request_new_workspace);
    assert_eq!(state.mode, Mode::Terminal);
}

#[test]
fn custom_sidebar_toggle_key_toggles_and_exits_navigate() {
    let mut state = state_with_workspaces(&["test"]);
    state.keybinds.toggle_sidebar = (KeyCode::Char('g'), KeyModifiers::empty());
    state.keybinds.toggle_sidebar_label = "g".into();
    assert!(!state.sidebar_collapsed);

    handle_navigate_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
    );

    assert!(state.sidebar_collapsed);
    assert_eq!(state.mode, Mode::Terminal);
}

#[test]
fn custom_resize_key_enters_resize_mode() {
    let mut state = state_with_workspaces(&["test"]);
    state.keybinds.resize_mode = (KeyCode::Char('g'), KeyModifiers::empty());
    state.keybinds.resize_mode_label = "g".into();

    handle_navigate_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::Resize);
}

#[test]
fn movement_action_stays_in_navigate_mode() {
    let mut state = state_with_workspaces(&["a", "b"]);
    state.selected = 0;

    handle_navigate_key(
        &mut state,
        KeyEvent::new(KeyCode::Down, KeyModifiers::empty()),
    );

    assert_eq!(state.selected, 1);
    assert_eq!(state.mode, Mode::Navigate);
}

#[test]
fn terminal_direct_focus_pane_shortcut_maps_to_navigation_action() {
    let mut state = state_with_workspaces(&["test"]);
    state.keybinds.focus_pane_left = Some((KeyCode::Left, KeyModifiers::ALT));
    state.keybinds.focus_pane_left_label = Some("alt+left".into());

    let action =
        terminal_direct_navigation_action(&state, &KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));

    assert_eq!(action, Some(NavigateAction::FocusPaneLeft));
}

#[tokio::test]
async fn terminal_direct_focus_pane_shortcut_switches_focus_without_leaving_terminal_mode() {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        true,
        None,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("test")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.workspaces[0].test_split(Direction::Horizontal);
    app.state.view.pane_infos = app.state.workspaces[0]
        .active_tab()
        .unwrap()
        .layout
        .panes(Rect::new(0, 0, 80, 24));
    let focused_before = app.state.workspaces[0].layout.focused();
    app.state.keybinds.focus_pane_left = Some((KeyCode::Char('h'), KeyModifiers::ALT));
    app.state.keybinds.focus_pane_left_label = Some("alt+h".into());

    app.handle_terminal_key(TerminalKey::new(KeyCode::Char('h'), KeyModifiers::ALT))
        .await;

    assert_ne!(app.state.workspaces[0].layout.focused(), focused_before);
    assert_eq!(app.state.mode, Mode::Terminal);
}

#[tokio::test]
async fn custom_command_runs_from_prefix_key_in_navigate_mode() {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        true,
        None,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    app.state.workspaces = vec![Workspace::test_new("test")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;

    let output_path = unique_temp_path("custom-command-keybind");
    let command = format!(
        "printf '%s\\n%s\\n%s\\n' \"$HERDR_ACTIVE_WORKSPACE_ID\" \"$HERDR_ACTIVE_TAB_ID\" \"$HERDR_ACTIVE_PANE_ID\" > '{}'",
        output_path.display()
    );
    app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
        key: (KeyCode::Char('g'), KeyModifiers::empty()),
        label: "g".into(),
        command,
        action: crate::config::CustomCommandAction::Shell,
    }];

    app.handle_key(TerminalKey::new(
        app.state.prefix_code,
        app.state.prefix_mods,
    ))
    .await;
    assert_eq!(app.state.mode, Mode::Navigate);

    app.handle_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()))
        .await;

    let content = wait_for_file(&output_path);
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], app.state.workspaces[0].id);
    assert_eq!(lines[1], format!("{}:1", app.state.workspaces[0].id));
    assert_eq!(lines[2], format!("{}-1", app.state.workspaces[0].id));
    assert_eq!(app.state.mode, Mode::Terminal);

    let _ = fs::remove_file(output_path);
}

#[tokio::test]
async fn pane_overlay_command_opens_and_closes_after_exit() {
    let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut app = App::new(
        &Config::default(),
        true,
        None,
        None,
        api_rx,
        crate::api::EventHub::default(),
    );
    let workspace = Workspace::new(
        std::env::current_dir().unwrap_or_else(|_| "/".into()),
        24,
        80,
        app.state.pane_scrollback_limit_bytes,
        app.state.host_terminal_theme,
        app.event_tx.clone(),
        app.render_notify.clone(),
        app.render_dirty.clone(),
    )
    .expect("workspace should spawn");
    app.state.workspaces = vec![workspace];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;

    let output_path = unique_temp_path("custom-pane-command");
    let command = format!("printf done > '{}'", output_path.display());
    app.state.keybinds.custom_commands = vec![crate::config::CustomCommandKeybind {
        key: (KeyCode::Char('g'), KeyModifiers::empty()),
        label: "g".into(),
        command,
        action: crate::config::CustomCommandAction::Pane,
    }];

    app.handle_key(TerminalKey::new(
        app.state.prefix_code,
        app.state.prefix_mods,
    ))
    .await;
    app.handle_key(TerminalKey::new(KeyCode::Char('g'), KeyModifiers::empty()))
        .await;

    assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 2);
    assert!(app.state.workspaces[0].tabs[0].zoomed);

    let _ = wait_for_file(&output_path);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        if app.drain_internal_events() && app.state.workspaces[0].tabs[0].layout.pane_count() == 1 {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert_eq!(app.state.workspaces[0].tabs[0].layout.pane_count(), 1);
    assert!(!app.state.workspaces[0].tabs[0].zoomed);
    assert_eq!(app.state.mode, Mode::Terminal);
    let _ = fs::remove_file(output_path);
}

#[test]
fn fullscreen_action_exits_navigate_mode() {
    let mut state = state_with_workspaces(&["test"]);
    state.workspaces[0].test_split(Direction::Horizontal);
    state.keybinds.fullscreen = (KeyCode::Char('g'), KeyModifiers::empty());
    state.keybinds.fullscreen_label = "g".into();

    handle_navigate_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
    );

    assert!(state.workspaces[0].zoomed);
    assert_eq!(state.mode, Mode::Terminal);
}

#[test]
fn custom_resize_key_exits_resize_mode() {
    let mut state = state_with_workspaces(&["test"]);
    state.mode = Mode::Resize;
    state.keybinds.resize_mode = (KeyCode::Char('g'), KeyModifiers::empty());
    state.keybinds.resize_mode_label = "g".into();

    handle_resize_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('g'), KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::Terminal);
}

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
    assert!(state.sound.enabled);
    assert_eq!(state.mode, Mode::Settings);
}

#[test]
fn question_mark_opens_keybind_help_from_navigate() {
    let mut state = state_with_workspaces(&["test"]);

    handle_navigate_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
    );

    assert_eq!(state.mode, Mode::KeybindHelp);
}

#[test]
fn rename_modal_keyboard_and_mouse_share_actions() {
    let mut state = state_with_workspaces(&["test"]);
    state.mode = Mode::RenameWorkspace;
    state.name_input = "hello".into();

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    );
    assert!(state.name_input.is_empty());

    state.name_input = "renamed".into();
    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert_eq!(state.mode, Mode::Terminal);
    assert_eq!(state.workspaces[0].display_name(), "renamed");
    let snapshot = capture_snapshot(&state);
    assert_eq!(
        snapshot.workspaces[0].custom_name.as_deref(),
        Some("renamed")
    );

    state.view.sidebar_rect = Rect::new(0, 0, 26, 20);
    state.view.terminal_area = Rect::new(26, 0, 80, 20);
    state.mode = Mode::RenameWorkspace;
    state.name_input = "mouse".into();
    let inner = state.rename_modal_inner().unwrap();
    let (save, _, _) = crate::ui::rename_button_rects(inner);
    let action = modal_action_from_buttons(save.x, save.y, &[(save, ModalAction::Save)]);
    assert_eq!(action, Some(ModalAction::Save));
}

#[test]
fn tab_rename_updates_captured_snapshot() {
    let mut state = state_with_workspaces(&["test"]);
    state.mode = Mode::RenameTab;
    state.name_input = "logs".into();

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    let snapshot = capture_snapshot(&state);
    assert_eq!(
        snapshot.workspaces[0].tabs[0].custom_name.as_deref(),
        Some("logs")
    );
}

#[test]
fn rename_cancel_returns_to_terminal_when_workspace_is_active() {
    let mut state = state_with_workspaces(&["test"]);
    state.mode = Mode::RenameTab;
    state.name_input = "test".into();

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::Terminal);
    assert!(state.name_input.is_empty());
}

#[test]
fn rename_modal_replaces_prefilled_text_on_first_type() {
    let mut state = state_with_workspaces(&["test"]);
    state.mode = Mode::RenameTab;
    state.name_input = "2".into();
    state.name_input_replace_on_type = true;

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('n'), KeyModifiers::empty()),
    );
    assert_eq!(state.name_input, "n");
    assert!(!state.name_input_replace_on_type);

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('e'), KeyModifiers::empty()),
    );
    assert_eq!(state.name_input, "ne");
}

#[test]
fn open_rename_active_tab_can_prefill_default_new_tab_name() {
    let mut state = state_with_workspaces(&["test"]);
    state.workspaces[0].test_add_tab(None);
    state.workspaces[0].switch_tab(1);

    open_rename_active_tab(&mut state, true);

    assert_eq!(state.mode, Mode::RenameTab);
    assert_eq!(state.name_input, "2");
    assert!(state.name_input_replace_on_type);
}

#[test]
fn new_tab_action_opens_dialog_without_creating_tab() {
    let mut state = state_with_workspaces(&["test"]);

    execute_navigate_action(&mut state, NavigateAction::NewTab);

    assert_eq!(state.mode, Mode::RenameTab);
    assert!(state.creating_new_tab);
    assert_eq!(state.name_input, "2");
    assert!(state.name_input_replace_on_type);
    assert!(!state.request_new_tab);
    assert_eq!(state.workspaces[0].tabs.len(), 1);
}

#[test]
fn cancel_new_tab_dialog_leaves_workspace_unchanged() {
    let mut state = state_with_workspaces(&["test"]);
    open_new_tab_dialog(&mut state);

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::Terminal);
    assert!(!state.creating_new_tab);
    assert!(!state.request_new_tab);
    assert!(state.requested_new_tab_name.is_none());
    assert_eq!(state.workspaces[0].tabs.len(), 1);
}

#[test]
fn saving_new_tab_dialog_requests_creation_with_name() {
    let mut state = state_with_workspaces(&["test"]);
    open_new_tab_dialog(&mut state);
    state.name_input = "logs".into();
    state.name_input_replace_on_type = false;

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::Terminal);
    assert!(!state.creating_new_tab);
    assert!(state.request_new_tab);
    assert_eq!(state.requested_new_tab_name.as_deref(), Some("logs"));
}

#[test]
fn saving_new_tab_dialog_with_default_name_keeps_tab_auto_named() {
    let mut state = state_with_workspaces(&["test"]);
    open_new_tab_dialog(&mut state);

    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::Terminal);
    assert!(!state.creating_new_tab);
    assert!(state.request_new_tab);
    assert!(state.requested_new_tab_name.is_none());
}

#[test]
fn closing_first_auto_tab_resets_remaining_auto_tab_and_next_prompt() {
    let mut state = state_with_workspaces(&["test"]);
    open_new_tab_dialog(&mut state);
    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    state.workspaces[0].test_add_tab(state.requested_new_tab_name.as_deref());
    state.request_new_tab = false;
    state.requested_new_tab_name = None;

    state.workspaces[0].close_tab(0);
    state.workspaces[0].switch_tab(0);

    assert_eq!(state.workspaces[0].tabs[0].display_name(), "1");
    assert!(state.workspaces[0].tabs[0].custom_name.is_none());

    open_new_tab_dialog(&mut state);
    assert_eq!(state.name_input, "2");
}

#[test]
fn renaming_auto_tab_to_its_default_number_keeps_it_auto_named() {
    let mut state = state_with_workspaces(&["test"]);
    state.workspaces[0].test_add_tab(None);
    state.workspaces[0].switch_tab(1);

    open_rename_active_tab(&mut state, false);
    handle_rename_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );

    assert_eq!(state.mode, Mode::Terminal);
    assert!(state.workspaces[0].tabs[1].custom_name.is_none());
    assert_eq!(state.workspaces[0].tabs[1].display_name(), "2");
}

#[test]
fn confirm_close_keyboard_actions_are_direct_not_focused() {
    let mut state = state_with_workspaces(&["a", "b"]);
    state.mode = Mode::ConfirmClose;
    state.selected = 1;

    handle_confirm_close_key(
        &mut state,
        KeyEvent::new(KeyCode::Esc, KeyModifiers::empty()),
    );
    assert_eq!(state.mode, Mode::Navigate);
    assert_eq!(state.workspaces.len(), 2);

    state.mode = Mode::ConfirmClose;
    handle_confirm_close_key(
        &mut state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert_eq!(state.workspaces.len(), 1);
}

#[test]
fn clicking_launcher_opens_global_menu() {
    let mut app = app_for_mouse_test();
    let rect = app.state.global_launcher_rect();

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        rect.x + rect.width.saturating_sub(1),
        rect.y,
    ));

    assert_eq!(app.state.mode, Mode::GlobalMenu);
}

#[test]
fn hovering_global_menu_updates_highlight() {
    let mut app = app_for_mouse_test();
    let launcher = app.state.global_launcher_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        launcher.x,
        launcher.y,
    ));

    let menu = app.state.global_menu_rect();
    app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

    assert_eq!(app.state.global_menu.highlighted, 1);
}

#[test]
fn clicking_keybinds_menu_item_opens_help() {
    let mut app = app_for_mouse_test();
    let launcher = app.state.global_launcher_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        launcher.x,
        launcher.y,
    ));

    let menu = app.state.global_menu_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        menu.x + 2,
        menu.y + 2,
    ));

    assert_eq!(app.state.mode, Mode::KeybindHelp);
}

#[test]
fn clicking_settings_menu_item_opens_settings() {
    let mut app = app_for_mouse_test();
    let launcher = app.state.global_launcher_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        launcher.x,
        launcher.y,
    ));

    let menu = app.state.global_menu_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        menu.x + 2,
        menu.y + 1,
    ));

    assert_eq!(app.state.mode, Mode::Settings);
}

#[test]
fn clicking_reload_keybinds_menu_item_requests_reload() {
    let mut app = app_for_mouse_test();
    let launcher = app.state.global_launcher_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        launcher.x,
        launcher.y,
    ));

    let menu = app.state.global_menu_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        menu.x + 2,
        menu.y + 3,
    ));

    assert!(app.state.request_reload_keybinds);
    assert_eq!(app.state.mode, Mode::Navigate);
}

#[test]
fn update_pending_menu_surfaces_update_ready_entry() {
    let mut app = app_for_mouse_test();
    app.state.update_available = Some("0.3.2".into());
    app.state.latest_release_notes_available = true;

    let launcher = app.state.global_launcher_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        launcher.x,
        launcher.y,
    ));

    assert_eq!(
        app.state.global_menu_labels(),
        vec![
            "settings",
            "keybinds",
            "reload keybinds",
            "update ready",
            "quit"
        ]
    );

    assert!(!app.state.should_quit);
}

#[test]
fn persistence_mode_menu_surfaces_detach_action() {
    let mut app = app_for_mouse_test();
    app.state.quit_detaches = true;

    let launcher = app.state.global_launcher_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        launcher.x,
        launcher.y,
    ));

    assert_eq!(
        app.state.global_menu_labels(),
        vec!["settings", "keybinds", "reload keybinds", "detach"]
    );

    let menu = app.state.global_menu_rect();
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        menu.x + 2,
        menu.y + 4,
    ));

    assert!(app.state.detach_requested);
    assert!(!app.state.should_quit);
}

#[test]
fn persistence_mode_navigate_q_detaches_instead_of_quitting_server() {
    let mut state = AppState::test_new();
    state.quit_detaches = true;

    assert!(handle_navigate_reserved_key(
        &mut state,
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::empty())
    ));
    assert!(state.detach_requested);
    assert!(!state.should_quit);
}

#[test]
fn whats_new_remains_in_menu_for_latest_installed_release_notes() {
    let mut app = app_for_mouse_test();
    app.state.latest_release_notes_available = true;

    assert_eq!(
        app.state.global_menu_labels(),
        vec![
            "settings",
            "keybinds",
            "reload keybinds",
            "what's new",
            "quit"
        ]
    );
}

#[test]
fn clicking_keybind_help_close_button_closes_overlay() {
    let mut app = app_for_mouse_test();
    app.state.mode = Mode::KeybindHelp;

    let rect = app.state.keybind_help_popup_rect();
    let inner = Rect::new(
        rect.x + 1,
        rect.y + 1,
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    );
    let close =
        crate::ui::release_notes_close_button_rect(Rect::new(inner.x, inner.y, inner.width, 1));
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        close.x,
        close.y,
    ));

    assert_eq!(app.state.mode, Mode::Navigate);
}

#[test]
fn hovering_context_menu_updates_highlight() {
    let mut app = app_for_mouse_test();
    app.state.context_menu = Some(ContextMenuState {
        kind: ContextMenuKind::Workspace { ws_idx: 0 },
        x: 2,
        y: 2,
        list: MenuListState::new(0),
    });
    app.state.mode = Mode::ContextMenu;

    let menu = app.state.context_menu_rect().unwrap();
    app.handle_mouse(mouse(MouseEventKind::Moved, menu.x + 2, menu.y + 2));

    assert_eq!(app.state.context_menu.unwrap().list.highlighted, 1);
}

#[test]
fn onboarding_hover_does_not_change_selection() {
    let mut app = app_for_mouse_test();
    app.state.mode = Mode::Onboarding;
    app.state.onboarding_step = 1;
    app.state.onboarding_list.select(1);

    let inner = app.state.onboarding_modal_inner(56, 14).unwrap();
    let content = crate::ui::modal_stack_areas(inner, 3, 0, 1, 1).content;
    app.handle_mouse(mouse(MouseEventKind::Moved, content.x + 2, content.y));

    assert_eq!(app.state.onboarding_list.selected, 1);
}

#[test]
fn onboarding_click_selects_notification_option() {
    let mut app = app_for_mouse_test();
    app.state.mode = Mode::Onboarding;
    app.state.onboarding_step = 1;
    app.state.onboarding_list.select(0);

    let inner = app.state.onboarding_modal_inner(56, 14).unwrap();
    let content = crate::ui::modal_stack_areas(inner, 3, 0, 1, 1).content;
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        content.x + 2,
        content.y + 2,
    ));

    assert_eq!(app.state.onboarding_list.selected, 2);
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
fn clicking_confirm_close_accepts_workspace_close() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
    app.state.active = Some(0);
    app.state.selected = 1;
    app.state.mode = Mode::ConfirmClose;

    let popup = app.state.confirm_close_rect();
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let (confirm, _) = crate::ui::confirm_close_button_rects(inner);

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        confirm.x,
        confirm.y,
    ));

    assert_eq!(app.state.workspaces.len(), 1);
    assert_eq!(app.state.mode, Mode::Terminal);
}

#[test]
fn clicking_confirm_close_accepts_after_workspace_context_menu_close() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;

    app.state.context_menu = Some(ContextMenuState {
        kind: ContextMenuKind::Workspace { ws_idx: 1 },
        x: 2,
        y: 2,
        list: MenuListState::new(1),
    });
    app.state.mode = Mode::ContextMenu;
    handle_context_menu_key(
        &mut app.state,
        KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()),
    );
    assert_eq!(app.state.mode, Mode::ConfirmClose);
    assert_eq!(app.state.selected, 1);

    let popup = app.state.confirm_close_rect();
    let inner = Rect::new(
        popup.x + 1,
        popup.y + 1,
        popup.width.saturating_sub(2),
        popup.height.saturating_sub(2),
    );
    let (confirm, _) = crate::ui::confirm_close_button_rects(inner);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        confirm.x + 1,
        confirm.y,
    ));

    assert_eq!(app.state.workspaces.len(), 1);
    assert_eq!(app.state.workspaces[0].display_name(), "a");
}

#[test]
fn clicking_agent_detail_row_switches_to_correct_tab_and_pane() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    ws.tabs[0].set_custom_name("main".into());
    let first_pane = ws.tabs[0].root_pane;
    ws.tabs[0]
        .panes
        .get_mut(&first_pane)
        .unwrap()
        .detected_agent = Some(Agent::Pi);
    let second_tab = ws.test_add_tab(Some("logs"));
    let second_pane = ws.tabs[second_tab].root_pane;
    ws.tabs[second_tab]
        .panes
        .get_mut(&second_pane)
        .unwrap()
        .detected_agent = Some(Agent::Claude);
    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 2, 16));

    assert_eq!(app.state.workspaces[0].active_tab, 1);
    assert_eq!(
        app.state.workspaces[0].tabs[1].layout.focused(),
        second_pane
    );
    assert_eq!(app.state.mode, Mode::Terminal);
    let snapshot = capture_snapshot(&app.state);
    assert_eq!(snapshot.workspaces[0].active_tab, second_tab);
    assert_eq!(
        snapshot.workspaces[0].tabs[second_tab].focused,
        Some(second_pane.raw())
    );
}

#[test]
fn clicking_agent_panel_toggle_switches_scope() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![Workspace::test_new("test")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.agent_panel_scroll = 3;

    let (_, detail_area) = crate::ui::expanded_sidebar_sections(
        app.state.view.sidebar_rect,
        app.state.sidebar_section_split,
    );
    let toggle = crate::ui::agent_panel_toggle_rect(detail_area, app.state.agent_panel_scope);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        toggle.x,
        toggle.y,
    ));

    assert_eq!(app.state.agent_panel_scope, AgentPanelScope::AllWorkspaces);
    assert_eq!(app.state.agent_panel_scroll, 0);
    let snapshot = capture_snapshot(&app.state);
    assert_eq!(snapshot.agent_panel_scope, AgentPanelScope::AllWorkspaces);
}

#[test]
fn clicking_all_workspaces_agent_row_switches_to_correct_workspace() {
    let mut app = app_for_mouse_test();
    let mut first = Workspace::test_new("one");
    let first_pane = first.tabs[0].root_pane;
    first.tabs[0]
        .panes
        .get_mut(&first_pane)
        .unwrap()
        .detected_agent = Some(Agent::Pi);

    let mut second = Workspace::test_new("two");
    let second_pane = second.tabs[0].root_pane;
    second.tabs[0]
        .panes
        .get_mut(&second_pane)
        .unwrap()
        .detected_agent = Some(Agent::Claude);

    app.state.workspaces = vec![first, second];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.agent_panel_scope = AgentPanelScope::AllWorkspaces;

    let (_, detail_area) = crate::ui::expanded_sidebar_sections(
        app.state.view.sidebar_rect,
        app.state.sidebar_section_split,
    );
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        detail_area.x + 2,
        detail_area.y + 6,
    ));

    assert_eq!(app.state.active, Some(1));
    assert_eq!(app.state.selected, 1);
    assert_eq!(app.state.workspaces[1].active_tab, 0);
    assert_eq!(
        app.state.workspaces[1].tabs[0].layout.focused(),
        second_pane
    );
}

#[test]
fn scrolling_agent_panel_with_wheel_updates_agent_panel_scroll() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let first_pane = ws.tabs[0].root_pane;
    ws.tabs[0]
        .panes
        .get_mut(&first_pane)
        .unwrap()
        .detected_agent = Some(Agent::Pi);

    for (tab_name, agent) in [
        ("logs", Agent::Claude),
        ("review", Agent::Codex),
        ("ops", Agent::Gemini),
    ] {
        let tab_idx = ws.test_add_tab(Some(tab_name));
        let pane_id = ws.tabs[tab_idx].root_pane;
        ws.tabs[tab_idx]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .detected_agent = Some(agent);
    }

    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;

    let detail_area = app.state.agent_panel_rect();
    assert!(crate::ui::should_show_scrollbar(
        crate::ui::agent_panel_scroll_metrics(&app.state, detail_area)
    ));

    app.handle_mouse(mouse(
        MouseEventKind::ScrollDown,
        detail_area.x + 1,
        detail_area.y + 4,
    ));

    assert_eq!(app.state.agent_panel_scroll, 1);
    assert_eq!(app.state.selected, 0);
}

#[test]
fn clicking_scrolled_agent_detail_row_switches_to_correct_tab_and_pane() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let first_pane = ws.tabs[0].root_pane;
    ws.tabs[0]
        .panes
        .get_mut(&first_pane)
        .unwrap()
        .detected_agent = Some(Agent::Pi);

    let second_tab = ws.test_add_tab(Some("logs"));
    let second_pane = ws.tabs[second_tab].root_pane;
    ws.tabs[second_tab]
        .panes
        .get_mut(&second_pane)
        .unwrap()
        .detected_agent = Some(Agent::Claude);

    for (tab_name, agent) in [("review", Agent::Codex), ("ops", Agent::Gemini)] {
        let tab_idx = ws.test_add_tab(Some(tab_name));
        let pane_id = ws.tabs[tab_idx].root_pane;
        ws.tabs[tab_idx]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .detected_agent = Some(agent);
    }

    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.agent_panel_scroll = 1;

    let detail_area = app.state.agent_panel_rect();
    let body = crate::ui::agent_panel_body_rect(detail_area, true);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        body.x + 1,
        body.y,
    ));

    assert_eq!(app.state.workspaces[0].active_tab, second_tab);
    assert_eq!(
        app.state.workspaces[0].tabs[second_tab].layout.focused(),
        second_pane
    );
    assert_eq!(app.state.mode, Mode::Terminal);
}

#[test]
fn clicking_collapsed_agent_row_switches_to_correct_tab_and_pane() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let first_pane = ws.tabs[0].root_pane;
    ws.tabs[0]
        .panes
        .get_mut(&first_pane)
        .unwrap()
        .detected_agent = Some(Agent::Pi);
    let second_tab = ws.test_add_tab(Some("logs"));
    let second_pane = ws.tabs[second_tab].root_pane;
    ws.tabs[second_tab]
        .panes
        .get_mut(&second_pane)
        .unwrap()
        .detected_agent = Some(Agent::Claude);
    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.sidebar_collapsed = true;
    app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
    app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

    let (_, _, detail_area) = crate::ui::collapsed_sidebar_sections(app.state.view.sidebar_rect);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        detail_area.x,
        detail_area.y + 1,
    ));

    assert_eq!(app.state.workspaces[0].active_tab, 1);
    assert_eq!(
        app.state.workspaces[0].tabs[1].layout.focused(),
        second_pane
    );
    assert_eq!(app.state.mode, Mode::Terminal);
}

#[test]
fn clicking_collapsed_sidebar_toggle_expands_sidebar() {
    let mut app = app_for_mouse_test();
    app.state.sidebar_collapsed = true;
    app.state.view.sidebar_rect = Rect::new(0, 0, 4, 20);
    app.state.view.terminal_area = Rect::new(4, 0, 80, 20);

    let toggle = crate::ui::collapsed_sidebar_toggle_rect(app.state.view.sidebar_rect);
    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        toggle.x,
        toggle.y,
    ));

    assert!(!app.state.sidebar_collapsed);
}

#[tokio::test]
async fn dragging_selection_above_pane_autoscrolls_and_extends_into_scrollback() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let pane_id = ws.tabs[0].root_pane;
    let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
    let info = pane_infos[0].clone();
    ws.tabs[0].runtimes.insert(
        pane_id,
        crate::pane::PaneRuntime::test_with_scrollback_bytes(
            info.inner_rect.width,
            info.inner_rect.height,
            16 * 1024,
            &numbered_lines_bytes(64),
        ),
    );

    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.view.pane_infos = pane_infos;

    let start_metrics = app.state.workspaces[0]
        .runtime(pane_id)
        .and_then(crate::pane::PaneRuntime::scroll_metrics)
        .expect("initial scroll metrics");
    let start_row = info.inner_rect.y;
    let start_col = info.inner_rect.x + 2;

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        start_col,
        start_row,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        start_col,
        info.inner_rect.y.saturating_sub(1),
    ));

    let end_metrics = app.state.workspaces[0]
        .runtime(pane_id)
        .and_then(crate::pane::PaneRuntime::scroll_metrics)
        .expect("scroll metrics after drag");
    assert_eq!(
        end_metrics.offset_from_bottom,
        start_metrics.offset_from_bottom + 3
    );

    let selection = app.state.selection.as_ref().expect("selection after drag");
    assert!(selection.is_visible());
    assert_eq!(
        selection.ordered_cells(),
        (
            (
                (start_metrics.max_offset_from_bottom - end_metrics.offset_from_bottom) as u32,
                2,
            ),
            (start_metrics.max_offset_from_bottom as u32, 2),
        )
    );
}

#[tokio::test]
async fn releasing_dragged_selection_clears_highlight_after_copy() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let pane_id = ws.tabs[0].root_pane;
    let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
    let info = pane_infos[0].clone();
    ws.tabs[0].runtimes.insert(
        pane_id,
        crate::pane::PaneRuntime::test_with_scrollback_bytes(
            info.inner_rect.width,
            info.inner_rect.height,
            16 * 1024,
            &numbered_lines_bytes(64),
        ),
    );

    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.view.pane_infos = pane_infos;

    let row = info.inner_rect.y;
    let start_col = info.inner_rect.x + 1;
    let end_col = info.inner_rect.x + 4;

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        start_col,
        row,
    ));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), end_col, row));
    assert!(app.state.selection.is_some());

    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), end_col, row));

    assert!(app.state.selection.is_none());
}

#[tokio::test]
async fn wheel_scroll_keeps_in_progress_selection_and_extends_it() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let pane_id = ws.tabs[0].root_pane;
    let pane_infos = ws.tabs[0].layout.panes(Rect::new(26, 2, 80, 18));
    let info = pane_infos[0].clone();
    ws.tabs[0].runtimes.insert(
        pane_id,
        crate::pane::PaneRuntime::test_with_scrollback_bytes(
            info.inner_rect.width,
            info.inner_rect.height,
            16 * 1024,
            &numbered_lines_bytes(64),
        ),
    );

    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.view.pane_infos = pane_infos;

    let start_metrics = app.state.workspaces[0]
        .runtime(pane_id)
        .and_then(crate::pane::PaneRuntime::scroll_metrics)
        .expect("initial scroll metrics");
    let top_row = info.inner_rect.y;
    let col = info.inner_rect.x + 2;

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), col, top_row));
    app.handle_mouse(mouse(MouseEventKind::ScrollUp, col, top_row));

    let end_metrics = app.state.workspaces[0]
        .runtime(pane_id)
        .and_then(crate::pane::PaneRuntime::scroll_metrics)
        .expect("scroll metrics after wheel");
    assert_eq!(
        end_metrics.offset_from_bottom,
        start_metrics.offset_from_bottom + 3
    );

    let selection = app.state.selection.as_ref().expect("selection after wheel");
    assert!(selection.is_visible());
    assert_eq!(
        selection.ordered_cells(),
        (
            (
                (start_metrics.max_offset_from_bottom - end_metrics.offset_from_bottom) as u32,
                2,
            ),
            (start_metrics.max_offset_from_bottom as u32, 2),
        )
    );
}

#[test]
fn clicking_workspace_switches_on_mouse_up() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
    app.state.active = Some(0);
    app.state.selected = 0;
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
    let target_row = app.state.view.workspace_card_areas[1].rect.y;

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        2,
        target_row,
    ));
    assert_eq!(app.state.active, Some(0));
    assert!(app.state.workspace_press.is_some());

    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));
    assert_eq!(app.state.active, Some(1));
    assert_eq!(app.state.selected, 1);
    assert!(app.state.workspace_press.is_none());
    let snapshot = capture_snapshot(&app.state);
    assert_eq!(snapshot.active, Some(1));
    assert_eq!(snapshot.selected, 1);
}

#[test]
fn dragging_workspace_reorders_without_changing_identity() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![
        Workspace::test_new("a"),
        Workspace::test_new("b"),
        Workspace::test_new("c"),
    ];
    let active_id = app.state.workspaces[1].id.clone();
    let selected_id = app.state.workspaces[2].id.clone();
    app.state.active = Some(1);
    app.state.selected = 2;
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
    let source_row = app.state.view.workspace_card_areas[1].rect.y;
    let target_row = crate::ui::workspace_drop_indicator_row(
        &app.state.view.workspace_card_areas,
        app.state.workspace_list_rect(),
        0,
    )
    .unwrap();

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        2,
        source_row,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        2,
        target_row,
    ));
    assert!(matches!(
        app.state.drag.as_ref().map(|drag| &drag.target),
        Some(DragTarget::WorkspaceReorder {
            source_ws_idx: 1,
            insert_idx: Some(0),
        })
    ));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 2, target_row));

    let names: Vec<_> = app
        .state
        .workspaces
        .iter()
        .map(|ws| ws.display_name())
        .collect();
    assert_eq!(names, vec!["b", "a", "c"]);
    assert_eq!(app.state.active, Some(0));
    assert_eq!(app.state.selected, 2);
    assert_eq!(app.state.workspaces[0].id, active_id);
    assert_eq!(app.state.workspaces[2].id, selected_id);
    let snapshot = capture_snapshot(&app.state);
    let captured_names: Vec<_> = snapshot
        .workspaces
        .iter()
        .map(|ws| ws.custom_name.clone().unwrap())
        .collect();
    assert_eq!(captured_names, vec!["b", "a", "c"]);
}

#[test]
fn clicking_tab_scroll_button_reveals_hidden_tabs_without_renaming() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    ws.test_add_tab(Some("logs"));
    ws.test_add_tab(Some("review"));
    ws.test_add_tab(Some("ops"));
    ws.test_add_tab(Some("notes"));
    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 52, 20));

    let right = app.state.view.tab_scroll_right_hit_area;
    assert!(right.width > 0);

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        right.x + 1,
        right.y,
    ));

    assert_eq!(app.state.tab_scroll, 1);
    assert!(!app.state.tab_scroll_follow_active);
    assert_eq!(app.state.workspaces[0].active_tab, 0);
    assert_eq!(app.state.view.tab_hit_areas[0].width, 0);
    assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
    assert_eq!(
        app.state.workspaces[0].tabs[1].custom_name.as_deref(),
        Some("logs")
    );
}

#[test]
fn clicking_last_visible_tab_at_right_edge_does_not_overscroll() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    for name in [
        "one", "two", "three", "four", "five", "six", "seven", "eight",
    ] {
        ws.test_add_tab(Some(name));
    }
    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.tab_scroll = usize::MAX;
    app.state.tab_scroll_follow_active = false;
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 52, 20));

    let last_idx = app.state.workspaces[0].tabs.len() - 1;
    let target = app.state.view.tab_hit_areas[last_idx];
    let clamped_scroll = app.state.tab_scroll;
    assert!(target.width > 0, "last tab should already be visible");

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        target.x + 1,
        target.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        target.x + 1,
        target.y,
    ));

    assert_eq!(app.state.workspaces[0].active_tab, last_idx);
    assert_eq!(app.state.tab_scroll, clamped_scroll);
    assert!(app.state.view.tab_hit_areas[last_idx].width > 0);
}

#[test]
fn dragging_tab_reorders_auto_and_custom_names_without_materializing_numbers() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    ws.test_add_tab(Some("foo"));
    ws.test_add_tab(None);
    let moved_root = ws.tabs[0].root_pane;
    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

    let source = app.state.view.tab_hit_areas[0];
    let last = app.state.view.tab_hit_areas[2];
    let drop_col = last.x + last.width;

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        source.x + 1,
        source.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        drop_col,
        source.y,
    ));
    assert!(matches!(
        app.state.drag.as_ref().map(|drag| &drag.target),
        Some(DragTarget::TabReorder {
            ws_idx: 0,
            source_tab_idx: 0,
            insert_idx: Some(3),
        })
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Up(MouseButton::Left),
        drop_col,
        source.y,
    ));

    let labels: Vec<_> = app.state.workspaces[0]
        .tabs
        .iter()
        .map(|tab| tab.display_name())
        .collect();
    assert_eq!(labels, vec!["foo", "2", "3"]);
    assert_eq!(
        app.state.workspaces[0].tabs[0].custom_name.as_deref(),
        Some("foo")
    );
    assert!(app.state.workspaces[0].tabs[1].custom_name.is_none());
    assert!(app.state.workspaces[0].tabs[2].custom_name.is_none());
    assert_eq!(app.state.workspaces[0].tabs[2].root_pane, moved_root);
    assert_eq!(app.state.workspaces[0].active_tab, 2);
}

#[test]
fn top_drop_slot_is_distinct_from_gap_below_first_workspace() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![Workspace::test_new("a"), Workspace::test_new("b")];
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

    assert_eq!(app.state.workspace_drop_index_at_row(0), Some(0));
    assert_eq!(app.state.workspace_drop_index_at_row(1), Some(0));
    assert_eq!(app.state.workspace_drop_index_at_row(2), Some(0));
    assert_eq!(app.state.workspace_drop_index_at_row(3), Some(1));
}

#[test]
fn bottom_drop_slot_stays_below_last_workspace_not_footer() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![
        Workspace::test_new("a"),
        Workspace::test_new("b"),
        Workspace::test_new("c"),
    ];
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));

    let cards = &app.state.view.workspace_card_areas;
    let bottom_slot = crate::ui::workspace_drop_indicator_row(
        cards,
        app.state.workspace_list_rect(),
        cards.len(),
    )
    .unwrap();

    let last = cards.last().unwrap().rect;
    assert_eq!(bottom_slot, last.y + last.height);
    assert!(bottom_slot < app.state.sidebar_footer_rect().y.saturating_sub(1));
}

#[test]
fn dragging_sidebar_divider_sets_manual_width() {
    let mut app = app_for_mouse_test();

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
    app.handle_mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 30, 5));

    assert_eq!(app.state.sidebar_width, 31);
    let snapshot = capture_snapshot(&app.state);
    assert_eq!(snapshot.sidebar_width, Some(31));
}

#[test]
fn dragging_sidebar_section_divider_sets_split_ratio() {
    let mut app = app_for_mouse_test();
    let divider = crate::ui::sidebar_section_divider_rect(
        app.state.view.sidebar_rect,
        app.state.sidebar_section_split,
    );

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        divider.x + 1,
        divider.y,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        divider.x + 1,
        divider.y + 4,
    ));

    assert!(app.state.sidebar_section_split > 0.5);
    let snapshot = capture_snapshot(&app.state);
    assert_eq!(
        snapshot.sidebar_section_split,
        Some(app.state.sidebar_section_split)
    );
}

#[test]
fn dragging_pane_split_updates_captured_layout_ratio() {
    let mut app = app_for_mouse_test();
    app.state.workspaces = vec![Workspace::test_new("test")];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.workspaces[0].test_split(Direction::Horizontal);
    crate::ui::compute_view(&mut app.state, Rect::new(0, 0, 106, 20));
    let border = app.state.view.split_borders[0].clone();
    let before = capture_snapshot(&app.state);
    let drag_row = border.area.y.saturating_add(1);

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        border.pos,
        drag_row,
    ));
    app.handle_mouse(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        border.pos.saturating_add(6),
        drag_row,
    ));

    let after = capture_snapshot(&app.state);
    assert_ne!(root_layout_ratio(&before), root_layout_ratio(&after));
}

#[test]
fn double_clicking_sidebar_divider_resets_default_width() {
    let mut app = app_for_mouse_test();
    app.state.default_sidebar_width = 26;
    app.state.sidebar_width = 30;

    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));
    app.handle_mouse(mouse(MouseEventKind::Up(MouseButton::Left), 25, 5));
    app.handle_mouse(mouse(MouseEventKind::Down(MouseButton::Left), 25, 5));

    assert_eq!(app.state.sidebar_width, 26);
    assert!(app.state.drag.is_none());
    let snapshot = capture_snapshot(&app.state);
    assert_eq!(snapshot.sidebar_width, Some(26));
}

#[test]
fn wheel_routing_prefers_mouse_reporting() {
    let input_state = crate::pane::InputState {
        alternate_screen: true,
        application_cursor: false,
        bracketed_paste: false,
        focus_reporting: false,
        mouse_protocol_mode: crate::input::MouseProtocolMode::ButtonMotion,
        mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Sgr,
        mouse_alternate_scroll: true,
    };

    assert_eq!(wheel_routing(input_state), WheelRouting::MouseReport);
}

#[test]
fn wheel_routing_uses_alternate_scroll_in_fullscreen_without_mouse_reporting() {
    let input_state = crate::pane::InputState {
        alternate_screen: true,
        application_cursor: false,
        bracketed_paste: false,
        focus_reporting: false,
        mouse_protocol_mode: crate::input::MouseProtocolMode::None,
        mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
        mouse_alternate_scroll: true,
    };

    assert_eq!(wheel_routing(input_state), WheelRouting::AlternateScroll);
}

#[test]
fn wheel_routing_falls_back_to_host_scrollback() {
    let input_state = crate::pane::InputState {
        alternate_screen: false,
        application_cursor: false,
        bracketed_paste: false,
        focus_reporting: false,
        mouse_protocol_mode: crate::input::MouseProtocolMode::None,
        mouse_protocol_encoding: crate::input::MouseProtocolEncoding::Default,
        mouse_alternate_scroll: true,
    };

    assert_eq!(wheel_routing(input_state), WheelRouting::HostScroll);
}

#[tokio::test]
async fn clicking_unfocused_pane_with_mouse_reporting_focuses_it_via_left_button() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let first_pane = ws.tabs[0].root_pane;
    let second_pane = ws.test_split(ratatui::layout::Direction::Vertical);

    let terminal_area = Rect::new(26, 2, 80, 18);
    let pane_infos = ws.tabs[0].layout.panes(terminal_area);
    let first_info = pane_infos
        .iter()
        .find(|p| p.id == first_pane)
        .expect("first pane info")
        .clone();
    let second_info = pane_infos
        .iter()
        .find(|p| p.id == second_pane)
        .expect("second pane info")
        .clone();

    ws.tabs[0].runtimes.insert(
        first_pane,
        crate::pane::PaneRuntime::test_with_screen_bytes(
            first_info.inner_rect.width.max(1),
            first_info.inner_rect.height.max(1),
            b"",
        ),
    );
    ws.tabs[0].runtimes.insert(
        second_pane,
        crate::pane::PaneRuntime::test_with_screen_bytes(
            second_info.inner_rect.width.max(1),
            second_info.inner_rect.height.max(1),
            b"\x1b[?1002h",
        ),
    );

    ws.tabs[0].layout.focus_pane(first_pane);

    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.view.pane_infos = pane_infos;

    assert_eq!(
        app.state.workspaces[0].tabs[0].layout.focused(),
        first_pane,
        "first pane should be focused before click"
    );

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Left),
        second_info.inner_rect.x + 2,
        second_info.inner_rect.y + 2,
    ));

    assert_eq!(
        app.state.workspaces[0].tabs[0].layout.focused(),
        second_pane,
        "left-clicking a pane with mouse reporting should move focus to it"
    );
    assert_eq!(app.state.mode, Mode::Terminal);
}

#[tokio::test]
async fn clicking_unfocused_pane_with_mouse_reporting_focuses_it_via_right_button() {
    let mut app = app_for_mouse_test();
    let mut ws = Workspace::test_new("test");
    let first_pane = ws.tabs[0].root_pane;
    let second_pane = ws.test_split(ratatui::layout::Direction::Vertical);

    let terminal_area = Rect::new(26, 2, 80, 18);
    let pane_infos = ws.tabs[0].layout.panes(terminal_area);
    let first_info = pane_infos
        .iter()
        .find(|p| p.id == first_pane)
        .expect("first pane info")
        .clone();
    let second_info = pane_infos
        .iter()
        .find(|p| p.id == second_pane)
        .expect("second pane info")
        .clone();

    ws.tabs[0].runtimes.insert(
        first_pane,
        crate::pane::PaneRuntime::test_with_screen_bytes(
            first_info.inner_rect.width.max(1),
            first_info.inner_rect.height.max(1),
            b"",
        ),
    );
    ws.tabs[0].runtimes.insert(
        second_pane,
        crate::pane::PaneRuntime::test_with_screen_bytes(
            second_info.inner_rect.width.max(1),
            second_info.inner_rect.height.max(1),
            b"\x1b[?1002h",
        ),
    );

    ws.tabs[0].layout.focus_pane(first_pane);

    app.state.workspaces = vec![ws];
    app.state.active = Some(0);
    app.state.selected = 0;
    app.state.mode = Mode::Terminal;
    app.state.view.pane_infos = pane_infos;

    assert_eq!(
        app.state.workspaces[0].tabs[0].layout.focused(),
        first_pane,
        "first pane should be focused before click"
    );

    app.handle_mouse(mouse(
        MouseEventKind::Down(MouseButton::Right),
        second_info.inner_rect.x + 2,
        second_info.inner_rect.y + 2,
    ));

    assert_eq!(
        app.state.workspaces[0].tabs[0].layout.focused(),
        second_pane,
        "right-clicking a pane with mouse reporting should move focus to it"
    );
    assert_eq!(
        app.state.mode,
        Mode::ContextMenu,
        "right-click should enter ContextMenu mode"
    );
    assert!(
        app.state.context_menu.is_some(),
        "right-click should populate context_menu state"
    );
}
