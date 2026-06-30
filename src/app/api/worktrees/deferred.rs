use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, Request, ResponseResult, WorktreeCreateParams,
    WorktreeRemoveParams,
};
use crate::app::App;
use crate::events::{ApiWorktreeAddRequest, ApiWorktreeRemoveRequest, AppEvent};

use super::super::responses::{encode_error, encode_success};
use super::{absolute_user_path, WorktreeSource};

impl App {
    pub(crate) fn handle_deferred_worktree_api_request(
        &mut self,
        request: Request,
        respond_to: std::sync::mpsc::Sender<String>,
    ) -> bool {
        match request.method {
            crate::api::schema::Method::WorktreeCreate(params) => {
                self.start_api_worktree_create(request.id, params, respond_to);
                true
            }
            crate::api::schema::Method::WorktreeRemove(params) => {
                self.start_api_worktree_remove(request.id, params, respond_to);
                true
            }
            _ => false,
        }
    }

    fn send_api_response(respond_to: std::sync::mpsc::Sender<String>, response: String) {
        let _ = respond_to.send(response);
    }

    fn next_api_worktree_operation_id(&mut self) -> u64 {
        let id = self.next_api_worktree_operation_id;
        self.next_api_worktree_operation_id = self.next_api_worktree_operation_id.saturating_add(1);
        id
    }

    fn api_create_source_workspace_idx(&self, api: &ApiWorktreeAddRequest) -> Option<usize> {
        let Some(source_workspace_id) = api.source_workspace_id.as_ref() else {
            return self.find_parent_workspace_by_key(&api.repo_key);
        };
        let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|ws| &ws.id == source_workspace_id)
        else {
            return self.find_parent_workspace_by_key(&api.repo_key);
        };
        let workspace = &self.state.workspaces[ws_idx];
        if let Some(expected) = api.source_existing_membership.as_ref() {
            if workspace.worktree_space() == Some(expected) {
                return Some(ws_idx);
            }
            return self.find_parent_workspace_by_key(&api.repo_key);
        }

        if let Some(current) = workspace.worktree_space() {
            let expected = crate::workspace::WorktreeSpaceMembership {
                key: api.repo_key.clone(),
                label: api.repo_name.clone(),
                repo_root: api.source_repo_root.clone(),
                checkout_path: api.source_checkout_path.clone(),
                is_linked_worktree: false,
            };
            if current == &expected {
                return Some(ws_idx);
            }
            return self.find_parent_workspace_by_key(&api.repo_key);
        }
        let git_space = workspace.git_space().cloned().or_else(|| {
            workspace
                .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                .as_deref()
                .and_then(crate::workspace::git_space_metadata)
        });
        if git_space.is_some_and(|space| {
            !space.is_linked_worktree
                && space.key == api.repo_key
                && crate::worktree::canonical_or_original(&space.repo_root)
                    == crate::worktree::canonical_or_original(&api.source_repo_root)
        }) {
            Some(ws_idx)
        } else {
            self.find_parent_workspace_by_key(&api.repo_key)
        }
    }

    fn start_api_worktree_create(
        &mut self,
        id: String,
        params: WorktreeCreateParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let branch = params
            .branch
            .unwrap_or_else(|| {
                let seed = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
                    .unwrap_or(0);
                crate::worktree::generated_branch_slug(seed)
            })
            .trim()
            .to_string();
        if branch.is_empty() {
            Self::send_api_response(
                respond_to,
                encode_error(id, "invalid_request", "branch is required"),
            );
            return;
        }
        let base = params.base.unwrap_or_else(|| "HEAD".into());
        let source = match self.resolve_worktree_source(params.workspace_id, params.cwd) {
            Ok(source) => source,
            Err(err) => {
                Self::send_api_response(respond_to, encode_error(id, err.code, err.message));
                return;
            }
        };
        let checkout_path = match params.path {
            Some(path) => match absolute_user_path(&path) {
                Ok(path) => path,
                Err(err) => {
                    Self::send_api_response(respond_to, encode_error(id, err.code, err.message));
                    return;
                }
            },
            None => crate::worktree::default_checkout_path(
                &self.state.worktree_directory,
                &source.repo_name,
                &branch,
            ),
        };
        let checkout_key = crate::worktree::canonical_or_original(&checkout_path);
        if self
            .pending_api_worktree_creates
            .contains_key(&checkout_key)
            || self
                .pending_api_worktree_remove_paths
                .contains_key(&checkout_key)
        {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "worktree_operation_in_progress",
                    "worktree operation is already in progress for this checkout",
                ),
            );
            return;
        }
        let operation_id = self.next_api_worktree_operation_id();
        self.pending_api_worktree_creates
            .insert(checkout_key.clone(), operation_id);

        let parent_dir = checkout_path.parent().map(Path::to_path_buf);
        let source_workspace_id = source
            .workspace_idx
            .and_then(|idx| self.state.workspaces.get(idx).map(|ws| ws.id.clone()));
        let source_existing_membership = source_workspace_id.as_ref().and_then(|workspace_id| {
            self.state
                .workspaces
                .iter()
                .find(|ws| &ws.id == workspace_id)
                .and_then(|ws| ws.worktree_space().cloned())
        });
        let api_request = ApiWorktreeAddRequest {
            id,
            operation_id,
            checkout_key,
            source_workspace_id,
            source_existing_membership,
            source_checkout_path: source.source_checkout_path,
            source_repo_root: source.source_repo_root,
            repo_key: source.repo_key,
            repo_name: source.repo_name,
            label: params.label,
            focus: params.focus,
            respond_to,
        };
        let path = checkout_path;
        let source_checkout_path = api_request.source_checkout_path.clone();
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = if let Some(parent_dir) = parent_dir {
                std::fs::create_dir_all(&parent_dir).map_err(|err| err.to_string())
            } else {
                Ok(())
            }
            .and_then(|()| {
                crate::worktree::run_worktree_add_command(
                    &source_checkout_path,
                    &path,
                    &branch,
                    &base,
                )
            });
            let _ = event_tx.blocking_send(AppEvent::WorktreeAddFinished(Box::new(
                crate::events::WorktreeAddResult {
                    path,
                    api_request: Some(api_request),
                    result,
                },
            )));
        });
    }

    fn start_api_worktree_remove(
        &mut self,
        id: String,
        params: WorktreeRemoveParams,
        respond_to: std::sync::mpsc::Sender<String>,
    ) {
        let Some(ws_idx) = self.parse_workspace_id(&params.workspace_id) else {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "workspace_not_found",
                    format!("workspace {} not found", params.workspace_id),
                ),
            );
            return;
        };
        let Some(space) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.worktree_space().cloned())
        else {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "not_linked_worktree",
                    "workspace is not a Herdr-managed worktree checkout",
                ),
            );
            return;
        };
        if !space.is_linked_worktree {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "not_linked_worktree",
                    "workspace is not a linked worktree checkout",
                ),
            );
            return;
        }

        #[cfg(windows)]
        {
            if !params.force
                && crate::worktree::checkout_has_dirty_files(&space.checkout_path).unwrap_or(false)
            {
                Self::send_api_response(
                    respond_to,
                    encode_error(
                        id,
                        "dirty_worktree_requires_force",
                        crate::worktree::worktree_dirty_remove_message(&space.checkout_path),
                    ),
                );
                return;
            }
        }

        let workspace_internal_id = self.state.workspaces[ws_idx].id.clone();
        let checkout_key = crate::worktree::canonical_or_original(&space.checkout_path);
        if self
            .pending_api_worktree_removes
            .contains_key(&workspace_internal_id)
            || self
                .pending_api_worktree_remove_paths
                .contains_key(&checkout_key)
            || self
                .pending_api_worktree_creates
                .contains_key(&checkout_key)
        {
            Self::send_api_response(
                respond_to,
                encode_error(
                    id,
                    "worktree_operation_in_progress",
                    "worktree operation is already in progress for this checkout",
                ),
            );
            return;
        }

        if Self::should_shutdown_workspace_terminal_runtimes_for_worktree_remove(params.force) {
            self.shutdown_workspace_terminal_runtimes_for_worktree_remove(ws_idx);
        }

        let operation_id = self.next_api_worktree_operation_id();
        self.pending_api_worktree_removes
            .insert(workspace_internal_id.clone(), operation_id);
        self.pending_api_worktree_remove_paths
            .insert(checkout_key.clone(), operation_id);
        let workspace_snapshot = self.workspace_info(ws_idx);
        let worktree = self.worktree_info_for_membership(&space, None);
        let command = crate::worktree::build_worktree_remove_command(
            &space.repo_root,
            &space.checkout_path,
            params.force,
        );
        let api_request = ApiWorktreeRemoveRequest {
            id,
            operation_id,
            checkout_key,
            respond_to,
        };
        let repo_root = space.repo_root;
        let path = space.checkout_path;
        let force = params.force;
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = crate::worktree::run_worktree_remove_command_with_recovery(
                &command, &repo_root, &path, force,
            );
            let _ = event_tx.blocking_send(AppEvent::WorktreeRemoveFinished(Box::new(
                crate::events::WorktreeRemoveResult {
                    workspace_id: workspace_internal_id,
                    path,
                    workspace: Some(Box::new(workspace_snapshot)),
                    worktree: Some(Box::new(worktree)),
                    forced: force,
                    api_request: Some(api_request),
                    result,
                },
            )));
        });
    }

    pub(crate) fn handle_api_worktree_add_finished(
        &mut self,
        mut result: crate::events::WorktreeAddResult,
    ) {
        let Some(api) = result.api_request.take() else {
            return;
        };
        let checkout_key = api.checkout_key.clone();
        let operation_matches = self
            .pending_api_worktree_creates
            .get(&checkout_key)
            .is_some_and(|operation_id| *operation_id == api.operation_id);
        if !operation_matches {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "stale_worktree_operation",
                    "worktree create completed after the operation was superseded",
                ),
            );
            return;
        }
        self.pending_api_worktree_creates.remove(&checkout_key);

        if let Err(err) = result.result {
            if let Some(create) = &mut self.state.worktree_create {
                if create.checkout_path == result.path {
                    create.creating = false;
                    create.error = Some(err.clone());
                }
            }
            Self::send_api_response(
                api.respond_to,
                encode_error(api.id, "worktree_create_failed", err),
            );
            return;
        }

        let source_workspace_idx = self.api_create_source_workspace_idx(&api);
        let mut source = WorktreeSource {
            workspace_idx: source_workspace_idx,
            source_checkout_path: api.source_checkout_path,
            source_repo_root: api.source_repo_root,
            repo_key: api.repo_key,
            repo_name: api.repo_name,
        };
        if let Err(err) = self.ensure_source_parent_membership(&mut source, true) {
            Self::send_api_response(api.respond_to, encode_error(api.id, err.code, err.message));
            return;
        }

        let (ws_idx, created_workspace) =
            if let Some(ws_idx) = self.open_workspace_idx_for_checkout(&result.path) {
                if api.focus {
                    self.state.switch_workspace(ws_idx);
                }
                (ws_idx, false)
            } else {
                match self.create_workspace_with_options(result.path.clone(), api.focus) {
                    Ok(ws_idx) => (ws_idx, true),
                    Err(err) => {
                        Self::send_api_response(
                            api.respond_to,
                            encode_error(
                                api.id,
                                "worktree_open_failed",
                                format!("created worktree but failed to open workspace: {err}"),
                            ),
                        );
                        return;
                    }
                }
            };

        self.mark_worktree_membership(
            &source,
            ws_idx,
            result.path.clone(),
            true,
            !created_workspace,
        );
        if let Some(label) = api.label {
            if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                ws.set_custom_name(label);
            }
        }
        if self
            .state
            .worktree_create
            .as_ref()
            .is_some_and(|create| create.checkout_path == result.path)
        {
            self.state.worktree_create = None;
            self.state.name_input.clear();
            self.state.name_input_replace_on_type = false;
            self.state.mode = crate::app::Mode::Terminal;
        }
        self.state.mark_session_dirty();
        if created_workspace {
            self.emit_workspace_open_events(ws_idx);
        }
        let Some(worktree) = self.worktree_info_for_workspace(ws_idx) else {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "worktree_open_failed",
                    "created worktree but failed to record workspace membership",
                ),
            );
            return;
        };
        self.emit_worktree_created_event(ws_idx, worktree.clone());
        let tab_idx = self.state.workspaces[ws_idx].active_tab;
        let response = encode_success(
            api.id,
            ResponseResult::WorktreeCreated {
                workspace: self.workspace_info(ws_idx),
                tab: self
                    .tab_info(ws_idx, tab_idx)
                    .expect("created worktree workspace should have an active tab"),
                root_pane: self
                    .root_pane_info(ws_idx, tab_idx)
                    .expect("created worktree workspace should have an active root pane"),
                worktree,
            },
        );
        Self::send_api_response(api.respond_to, response);
    }

    pub(crate) fn handle_api_worktree_remove_finished(
        &mut self,
        mut result: crate::events::WorktreeRemoveResult,
    ) {
        let Some(api) = result.api_request.take() else {
            return;
        };
        let operation_matches = self
            .pending_api_worktree_removes
            .get(&result.workspace_id)
            .is_some_and(|operation_id| *operation_id == api.operation_id)
            && self
                .pending_api_worktree_remove_paths
                .get(&api.checkout_key)
                .is_some_and(|operation_id| *operation_id == api.operation_id);
        if !operation_matches {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "stale_worktree_operation",
                    "worktree remove completed after the operation was superseded",
                ),
            );
            return;
        }
        self.pending_api_worktree_removes
            .remove(&result.workspace_id);
        self.pending_api_worktree_remove_paths
            .remove(&api.checkout_key);

        if let Err(message) = result.result {
            let code =
                if !result.forced && crate::worktree::is_dirty_worktree_remove_error(&message) {
                    "dirty_worktree_requires_force"
                } else {
                    "worktree_remove_failed"
                };
            if let Some(remove) = &mut self.state.worktree_remove {
                if remove.workspace_id == result.workspace_id && remove.path == result.path {
                    remove.removing = false;
                    if code == "dirty_worktree_requires_force" && !remove.force_confirmation {
                        remove.force_confirmation = true;
                        remove.error = None;
                    } else {
                        remove.error = Some(message.clone());
                    }
                }
            }
            Self::send_api_response(api.respond_to, encode_error(api.id, code, message));
            return;
        }

        let mut workspace_id = result.workspace_id.clone();
        let mut workspace_snapshot = result.workspace.as_deref().cloned();
        let mut worktree = result.worktree.as_deref().cloned();
        if let Some(ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|ws| ws.id == result.workspace_id)
        {
            let current_matches =
                self.state.workspaces[ws_idx]
                    .worktree_space()
                    .is_some_and(|space| {
                        space.is_linked_worktree && space.checkout_path == result.path
                    });
            if current_matches {
                workspace_id = self.public_workspace_id(ws_idx);
                workspace_snapshot.get_or_insert_with(|| self.workspace_info(ws_idx));
                if worktree.is_none() {
                    worktree = self.state.workspaces[ws_idx]
                        .worktree_space()
                        .cloned()
                        .map(|space| self.worktree_info_for_membership(&space, None));
                }
                self.state.selected = ws_idx;
                self.state.close_selected_workspace();
                self.shutdown_detached_terminal_runtimes();
                self.emit_event(EventEnvelope {
                    event: EventKind::WorkspaceClosed,
                    data: EventData::WorkspaceClosed {
                        workspace_id: workspace_id.clone(),
                        workspace: workspace_snapshot.clone(),
                    },
                });
            } else if let Some(snapshot) = workspace_snapshot.as_ref() {
                workspace_id = snapshot.workspace_id.clone();
            }
        } else if let Some(snapshot) = workspace_snapshot.as_ref() {
            workspace_id = snapshot.workspace_id.clone();
        }

        let Some(worktree) = worktree else {
            Self::send_api_response(
                api.respond_to,
                encode_error(
                    api.id,
                    "worktree_remove_failed",
                    "removed worktree but lost worktree snapshot",
                ),
            );
            return;
        };
        self.emit_worktree_removed_event(
            workspace_id.clone(),
            workspace_snapshot,
            worktree,
            result.forced,
        );
        if self.state.worktree_remove.as_ref().is_some_and(|remove| {
            remove.workspace_id == result.workspace_id && remove.path == result.path
        }) {
            self.state.worktree_remove = None;
            self.state.mode = if self.state.active.is_some() {
                crate::app::Mode::Terminal
            } else {
                crate::app::Mode::Navigate
            };
        }
        let response = encode_success(
            api.id,
            ResponseResult::WorktreeRemoved {
                workspace_id,
                path: result.path.display().to_string(),
                forced: result.forced,
            },
        );
        Self::send_api_response(api.respond_to, response);
    }
}
