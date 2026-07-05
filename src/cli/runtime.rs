use crate::api::schema::{
    EmptyParams, Method, PaneFocusDirectionParams, PaneMoveParams, PaneRenameParams,
    PaneResizeParams, PaneSplitParams, PaneSwapParams, PaneTarget, PaneZoomParams, Request,
    TabCreateParams, TabListParams, TabRenameParams, TabTarget, WorkspaceCreateParams,
    WorkspaceRenameParams, WorkspaceTarget, WorktreeCreateParams, WorktreeListParams,
    WorktreeOpenParams, WorktreeRemoveParams,
};

fn print_method_response(id: &'static str, method: Method) -> std::io::Result<i32> {
    super::print_response(&super::send_request(&Request {
        id: id.into(),
        method,
    })?)
}

pub(super) fn workspace_list() -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:list",
        Method::WorkspaceList(EmptyParams::default()),
    )
}

pub(super) fn workspace_create(params: WorkspaceCreateParams) -> std::io::Result<i32> {
    print_method_response("cli:workspace:create", Method::WorkspaceCreate(params))
}

pub(super) fn workspace_get(workspace_id: String) -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:get",
        Method::WorkspaceGet(WorkspaceTarget { workspace_id }),
    )
}

pub(super) fn workspace_focus(workspace_id: String) -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:focus",
        Method::WorkspaceFocus(WorkspaceTarget { workspace_id }),
    )
}

pub(super) fn workspace_rename(params: WorkspaceRenameParams) -> std::io::Result<i32> {
    print_method_response("cli:workspace:rename", Method::WorkspaceRename(params))
}

pub(super) fn workspace_close(workspace_id: String) -> std::io::Result<i32> {
    print_method_response(
        "cli:workspace:close",
        Method::WorkspaceClose(WorkspaceTarget { workspace_id }),
    )
}

pub(super) fn tab_list(params: TabListParams) -> std::io::Result<i32> {
    print_method_response("cli:tab:list", Method::TabList(params))
}

pub(super) fn tab_create(params: TabCreateParams) -> std::io::Result<i32> {
    print_method_response("cli:tab:create", Method::TabCreate(params))
}

pub(super) fn tab_get(tab_id: String) -> std::io::Result<i32> {
    print_method_response("cli:tab:get", Method::TabGet(TabTarget { tab_id }))
}

pub(super) fn tab_focus(tab_id: String) -> std::io::Result<i32> {
    print_method_response("cli:tab:focus", Method::TabFocus(TabTarget { tab_id }))
}

pub(super) fn tab_rename(params: TabRenameParams) -> std::io::Result<i32> {
    print_method_response("cli:tab:rename", Method::TabRename(params))
}

pub(super) fn tab_close(tab_id: String) -> std::io::Result<i32> {
    print_method_response("cli:tab:close", Method::TabClose(TabTarget { tab_id }))
}

pub(super) fn worktree_list(params: WorktreeListParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:list", Method::WorktreeList(params))
}

pub(super) fn worktree_create(params: WorktreeCreateParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:create", Method::WorktreeCreate(params))
}

pub(super) fn worktree_open(params: WorktreeOpenParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:open", Method::WorktreeOpen(params))
}

pub(super) fn worktree_remove(params: WorktreeRemoveParams) -> std::io::Result<i32> {
    print_method_response("cli:worktree:remove", Method::WorktreeRemove(params))
}

pub(super) fn pane_focus(params: PaneFocusDirectionParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:focus", Method::PaneFocusDirection(params))
}

pub(super) fn pane_resize(params: PaneResizeParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:resize", Method::PaneResize(params))
}

pub(super) fn pane_zoom(params: PaneZoomParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:zoom", Method::PaneZoom(params))
}

pub(super) fn pane_rename(params: PaneRenameParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:rename", Method::PaneRename(params))
}

pub(super) fn pane_split(params: PaneSplitParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:split", Method::PaneSplit(params))
}

pub(super) fn pane_swap(params: PaneSwapParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:swap", Method::PaneSwap(params))
}

pub(super) fn pane_move(params: PaneMoveParams) -> std::io::Result<i32> {
    print_method_response("cli:pane:move", Method::PaneMove(params))
}

pub(super) fn pane_close(pane_id: String) -> std::io::Result<i32> {
    print_method_response("cli:pane:close", Method::PaneClose(PaneTarget { pane_id }))
}
