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

fn root_layout_ratio(snapshot: &crate::persist::SessionSnapshot) -> Option<f32> {
    match &snapshot.workspaces.first()?.tabs.first()?.layout {
        crate::persist::LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
        crate::persist::LayoutSnapshot::Pane(_) => None,
    }
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
