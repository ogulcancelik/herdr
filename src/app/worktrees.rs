use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    state::{WorktreeCreateState, WorktreeOpenEntry, WorktreeOpenState, WorktreeRemoveState},
    App, Mode,
};
#[cfg(test)]
use crate::events::AppEvent;
use crate::events::{WorktreeAddResult, WorktreeRemoveResult};

impl App {
    fn worktree_source_metadata(
        &self,
        ws_idx: usize,
    ) -> Result<
        (
            Option<crate::workspace::WorktreeSpaceMembership>,
            crate::workspace::GitSpaceMetadata,
            std::path::PathBuf,
            String,
        ),
        String,
    > {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return Err("Workspace not found.".into());
        };
        let existing_membership = ws.worktree_space().cloned();
        if existing_membership
            .as_ref()
            .is_some_and(|membership| membership.is_linked_worktree)
        {
            return Err(
                "New and open worktree actions start from the repo parent workspace.".into(),
            );
        }

        let git_space = ws.git_space().cloned().or_else(|| {
            ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                .as_deref()
                .and_then(crate::workspace::git_space_metadata)
        });
        if git_space
            .as_ref()
            .is_some_and(|metadata| metadata.is_linked_worktree)
        {
            return Err(
                "New and open worktree actions start from the repo parent workspace.".into(),
            );
        }

        let space = existing_membership
            .as_ref()
            .map_or(git_space, |membership| {
                Some(crate::workspace::GitSpaceMetadata {
                    key: membership.key.clone(),
                    checkout_key: membership.checkout_path.display().to_string(),
                    label: membership.label.clone(),
                    repo_root: membership.repo_root.clone(),
                    is_linked_worktree: membership.is_linked_worktree,
                })
            })
            .ok_or_else(|| {
                "Herdr worktree actions require a workspace inside a Git work tree.".to_string()
            })?;
        let source_checkout_path = existing_membership
            .as_ref()
            .map(|membership| membership.checkout_path.clone())
            .unwrap_or_else(|| space.repo_root.clone());
        let source_workspace_id = self.state.workspaces[ws_idx].id.clone();
        Ok((
            existing_membership,
            space,
            source_checkout_path,
            source_workspace_id,
        ))
    }

    pub(crate) fn open_new_linked_worktree_dialog(&mut self, ws_idx: usize) {
        let (existing_membership, space, source_checkout_path, source_workspace_id) =
            match self.worktree_source_metadata(ws_idx) {
                Ok(metadata) => metadata,
                Err(err) => {
                    self.state.config_diagnostic = Some(err);
                    return;
                }
            };

        let repo_name = space.label.clone();
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_micros().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0);
        let branch = crate::worktree::generated_branch_slug(seed);
        let checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &repo_name,
            &branch,
        );

        tracing::info!(
            ws_idx,
            repo_root = %space.repo_root.display(),
            branch,
            checkout_path = %checkout_path.display(),
            "opening worktree dialog"
        );
        self.state.selected = ws_idx;
        self.state.name_input = branch.clone();
        self.state.name_input_replace_on_type = true;
        self.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id,
            source_checkout_path,
            source_existing_membership: existing_membership,
            source_repo_root: space.repo_root,
            repo_key: space.key,
            repo_name,
            branch,
            checkout_path,
            error: None,
            creating: false,
        });
        self.state.mode = Mode::NewLinkedWorktree;
    }

    pub(crate) fn open_remove_linked_worktree_confirmation(&mut self, ws_idx: usize) {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return;
        };
        if !ws
            .worktree_space()
            .is_some_and(|space| space.is_linked_worktree)
        {
            self.state.config_diagnostic =
                Some("This workspace is not a Herdr-managed worktree checkout.".into());
            return;
        }
        let Some(space) = ws.worktree_space().cloned() else {
            return;
        };
        self.state.selected = ws_idx;
        self.state.worktree_remove = Some(WorktreeRemoveState {
            workspace_id: ws.id.clone(),
            repo_root: space.repo_root,
            path: space.checkout_path,
            error: None,
            removing: false,
            force_confirmation: false,
        });
        self.state.mode = Mode::ConfirmRemoveWorktree;
    }

    pub(crate) fn open_existing_worktree_dialog(&mut self, ws_idx: usize) {
        let (existing_membership, space, source_checkout_path, source_workspace_id) =
            match self.worktree_source_metadata(ws_idx) {
                Ok(metadata) => metadata,
                Err(err) => {
                    self.state.config_diagnostic = Some(err);
                    return;
                }
            };

        let list = match crate::worktree::list_existing_worktrees(&space.repo_root) {
            Ok(list) => list,
            Err(err) => {
                self.state.config_diagnostic = Some(err);
                return;
            }
        };
        let entries = list
            .into_iter()
            .filter(|entry| !entry.is_bare && !entry.is_prunable)
            .map(|entry| {
                let entry_checkout_path = crate::worktree::canonical_or_original(&entry.path);
                let entry_checkout_key = entry_checkout_path.display().to_string();
                let repo_checkout_path = crate::worktree::canonical_or_original(&space.repo_root);
                let already_open_ws_idx = self.state.workspaces.iter().position(|ws| {
                    if let Some(membership) = ws.worktree_space() {
                        return crate::worktree::canonical_or_original(&membership.checkout_path)
                            == entry_checkout_path;
                    }

                    let git_space = ws.git_space().cloned().or_else(|| {
                        ws.resolved_identity_cwd_from(
                            &self.state.terminals,
                            &self.terminal_runtimes,
                        )
                        .as_deref()
                        .and_then(crate::workspace::git_space_metadata)
                    });
                    if git_space
                        .as_ref()
                        .is_some_and(|metadata| metadata.checkout_key == entry_checkout_key)
                    {
                        return true;
                    }

                    ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                        .as_deref()
                        .is_some_and(|cwd| {
                            crate::worktree::canonical_or_original(cwd) == entry_checkout_path
                        })
                });
                WorktreeOpenEntry {
                    is_linked_worktree: entry_checkout_path != repo_checkout_path,
                    path: entry.path,
                    branch: entry.branch,
                    already_open_ws_idx,
                }
            })
            .collect::<Vec<_>>();

        if entries.is_empty() {
            self.state.config_diagnostic = Some("No Git worktrees found for this repo.".into());
            return;
        }

        self.state.selected = ws_idx;
        self.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id,
            source_existing_membership: existing_membership,
            source_checkout_path,
            source_repo_root: space.repo_root,
            repo_key: space.key,
            repo_name: space.label,
            entries,
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        });
        self.state.mode = Mode::OpenExistingWorktree;
    }

    pub(crate) fn handle_worktree_create_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self
                    .state
                    .worktree_create
                    .as_ref()
                    .is_some_and(|create| create.creating)
                {
                    return;
                }
                self.close_worktree_create_dialog();
            }
            KeyCode::Enter => self.submit_worktree_create_via_api(),
            KeyCode::Backspace => {
                if self.state.name_input_replace_on_type {
                    self.state.name_input.clear();
                    self.state.name_input_replace_on_type = false;
                } else {
                    self.state.name_input.pop();
                }
                self.sync_worktree_branch_from_input();
            }
            KeyCode::Char(c) => {
                self.insert_worktree_create_text(&c.to_string());
            }
            _ => {}
        }
    }

    pub(crate) fn insert_worktree_create_text(&mut self, text: &str) {
        if self.state.name_input_replace_on_type {
            self.state.name_input.clear();
            self.state.name_input_replace_on_type = false;
        }
        self.state.name_input.push_str(text);
        self.sync_worktree_branch_from_input();
    }

    pub(crate) fn handle_worktree_open_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.state.worktree_open = None;
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
            }
            KeyCode::Up => {
                if let Some(open) = &mut self.state.worktree_open {
                    open.select_previous_filtered();
                }
            }
            KeyCode::Down => {
                if let Some(open) = &mut self.state.worktree_open {
                    open.select_next_filtered();
                }
            }
            KeyCode::Char('/') => {
                if let Some(open) = &mut self.state.worktree_open {
                    if open.search_focused {
                        open.query.push('/');
                        open.normalize_selection();
                    } else {
                        open.search_focused = true;
                    }
                }
            }
            KeyCode::Char(ch)
                if self
                    .state
                    .worktree_open
                    .as_ref()
                    .is_some_and(|open| open.search_focused)
                    && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                    && !ch.is_control() =>
            {
                self.insert_worktree_open_search_text(&ch.to_string());
            }
            KeyCode::Backspace
                if self
                    .state
                    .worktree_open
                    .as_ref()
                    .is_some_and(|open| open.search_focused) =>
            {
                if let Some(open) = &mut self.state.worktree_open {
                    open.query.pop();
                    open.normalize_selection();
                }
            }
            KeyCode::Enter => self.submit_worktree_open_via_api(),
            _ => {}
        }
    }

    pub(crate) fn insert_worktree_open_search_text(&mut self, text: &str) {
        let Some(open) = &mut self.state.worktree_open else {
            return;
        };
        if !open.search_focused {
            return;
        }
        open.query.push_str(text);
        open.normalize_selection();
    }

    #[cfg(test)]
    pub(crate) fn open_selected_existing_worktree(&mut self) {
        let Some(open) = self.state.worktree_open.as_ref() else {
            return;
        };
        let Some(entry_idx) = open.selected_entry_index() else {
            return;
        };
        let Some(entry) = open.entries.get(entry_idx).cloned() else {
            return;
        };
        let source_workspace_id = open.source_workspace_id.clone();
        let source_existing_membership = open.source_existing_membership.clone();
        let source_checkout_path = open.source_checkout_path.clone();
        let source_repo_root = open.source_repo_root.clone();
        let repo_key = open.repo_key.clone();
        let repo_name = open.repo_name.clone();
        self.state.worktree_open = None;

        if let Some(ws_idx) = self.open_workspace_idx_for_checkout(&entry.path) {
            self.mark_opened_existing_worktree_membership(
                &source_workspace_id,
                source_existing_membership,
                source_checkout_path,
                source_repo_root,
                repo_key,
                repo_name,
                ws_idx,
                entry.path,
                entry.is_linked_worktree,
            );
            self.state.switch_workspace(ws_idx);
            self.state.mode = Mode::Terminal;
            self.emit_worktree_opened_for_workspace(ws_idx, true);
            return;
        }

        if let Some(source_ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|ws| ws.id == source_workspace_id)
        {
            let source_membership = source_existing_membership.clone().unwrap_or(
                crate::workspace::WorktreeSpaceMembership {
                    key: repo_key.clone(),
                    label: repo_name.clone(),
                    repo_root: source_repo_root.clone(),
                    checkout_path: source_checkout_path.clone(),
                    is_linked_worktree: false,
                },
            );
            self.set_worktree_membership(source_ws_idx, source_membership, true);
        }

        match self.create_workspace_with_options(entry.path.clone(), true) {
            Ok(new_ws_idx) => {
                self.set_worktree_membership(
                    new_ws_idx,
                    crate::workspace::WorktreeSpaceMembership {
                        key: repo_key,
                        label: repo_name,
                        repo_root: source_repo_root,
                        checkout_path: entry.path,
                        is_linked_worktree: entry.is_linked_worktree,
                    },
                    false,
                );
                self.emit_workspace_open_events(new_ws_idx);
                self.emit_worktree_opened_for_workspace(new_ws_idx, false);
            }
            Err(err) => {
                self.state.worktree_open = Some(WorktreeOpenState {
                    source_workspace_id,
                    source_existing_membership,
                    source_checkout_path,
                    source_repo_root,
                    repo_key,
                    repo_name,
                    entries: vec![entry],
                    selected: 0,
                    query: String::new(),
                    search_focused: false,
                    error: Some(format!("failed to open worktree: {err}")),
                });
                self.state.mode = Mode::OpenExistingWorktree;
            }
        }
    }

    // The caller has already extracted the open-worktree dialog state; keeping the
    // membership fields explicit here avoids borrowing AppState across workspace creation.
    #[allow(clippy::too_many_arguments)]
    #[cfg(test)]
    fn mark_opened_existing_worktree_membership(
        &mut self,
        source_workspace_id: &str,
        source_existing_membership: Option<crate::workspace::WorktreeSpaceMembership>,
        source_checkout_path: std::path::PathBuf,
        source_repo_root: std::path::PathBuf,
        repo_key: String,
        repo_name: String,
        target_ws_idx: usize,
        target_path: std::path::PathBuf,
        target_is_linked_worktree: bool,
    ) {
        if let Some(source_ws_idx) = self
            .state
            .workspaces
            .iter()
            .position(|ws| ws.id == source_workspace_id)
        {
            let source_membership =
                source_existing_membership.unwrap_or(crate::workspace::WorktreeSpaceMembership {
                    key: repo_key.clone(),
                    label: repo_name.clone(),
                    repo_root: source_repo_root.clone(),
                    checkout_path: source_checkout_path,
                    is_linked_worktree: false,
                });
            self.set_worktree_membership(source_ws_idx, source_membership, true);
        }
        self.set_worktree_membership(
            target_ws_idx,
            crate::workspace::WorktreeSpaceMembership {
                key: repo_key,
                label: repo_name,
                repo_root: source_repo_root,
                checkout_path: target_path,
                is_linked_worktree: target_is_linked_worktree,
            },
            true,
        );
    }

    fn close_worktree_create_dialog(&mut self) {
        self.state.worktree_create = None;
        self.state.name_input.clear();
        self.state.name_input_replace_on_type = false;
        self.state.mode = if self.state.active.is_some() {
            Mode::Terminal
        } else {
            Mode::Navigate
        };
    }

    fn sync_worktree_branch_from_input(&mut self) {
        let Some(create) = &mut self.state.worktree_create else {
            return;
        };
        create.branch = self.state.name_input.clone();
        create.checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &create.repo_name,
            &create.branch,
        );
        create.error = None;
    }

    #[cfg(test)]
    pub(crate) fn start_worktree_add(&mut self) {
        self.sync_worktree_branch_from_input();
        let Some(create) = &mut self.state.worktree_create else {
            return;
        };
        let branch = create.branch.trim().to_string();
        if branch.is_empty() {
            create.error = Some("branch is required".into());
            return;
        }
        if create.creating {
            return;
        }

        create.branch = branch.clone();
        self.state.name_input = branch.clone();
        create.checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &create.repo_name,
            &branch,
        );
        create.creating = true;
        create.error = None;

        let parent_dir = create
            .checkout_path
            .parent()
            .map(std::path::Path::to_path_buf);
        tracing::info!(
            repo_root = %create.source_repo_root.display(),
            branch = %create.branch,
            checkout_path = %create.checkout_path.display(),
            "starting git worktree add"
        );
        let path = create.checkout_path.clone();
        let source_checkout_path = create.source_checkout_path.clone();
        let branch = create.branch.clone();
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
                    "HEAD",
                )
            });
            let _ = event_tx.blocking_send(AppEvent::WorktreeAddFinished(Box::new(
                WorktreeAddResult {
                    path,
                    api_request: None,
                    result,
                },
            )));
        });
    }

    pub(crate) fn submit_worktree_create_via_api(&mut self) {
        self.sync_worktree_branch_from_input();
        let Some(create) = &mut self.state.worktree_create else {
            return;
        };
        let branch = create.branch.trim().to_string();
        if branch.is_empty() {
            create.error = Some("branch is required".into());
            return;
        }
        if create.creating {
            return;
        }

        create.branch = branch.clone();
        self.state.name_input = branch.clone();
        create.checkout_path = crate::worktree::default_checkout_path(
            &self.state.worktree_directory,
            &create.repo_name,
            &branch,
        );
        create.creating = true;
        create.error = None;
        let workspace_id = create.source_workspace_id.clone();
        let checkout_path = create.checkout_path.display().to_string();

        let immediate_response = self.runtime_worktree_create_deferred(
            "tui.worktree.create",
            crate::api::schema::WorktreeCreateParams {
                workspace_id: Some(workspace_id),
                cwd: None,
                branch: Some(branch),
                path: Some(checkout_path),
                base: Some("HEAD".into()),
                focus: true,
                label: None,
            },
        );
        if let Some(message) = immediate_api_error_message(immediate_response.as_deref()) {
            if let Some(create) = &mut self.state.worktree_create {
                create.creating = false;
                create.error = Some(message);
            }
        }
    }

    pub(crate) fn handle_worktree_remove_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if self
                    .state
                    .worktree_remove
                    .as_ref()
                    .is_some_and(|remove| remove.removing)
                {
                    return;
                }
                self.state.worktree_remove = None;
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
            }
            KeyCode::Enter => self.submit_worktree_remove_via_api(),
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn start_worktree_remove(&mut self) {
        let Some((workspace_id, repo_root, path, force)) =
            self.state.worktree_remove.as_mut().and_then(|remove| {
                if remove.removing {
                    return None;
                }
                #[cfg(windows)]
                if !remove.force_confirmation
                    && crate::worktree::checkout_has_dirty_files(&remove.path).unwrap_or(false)
                {
                    remove.force_confirmation = true;
                    remove.error = None;
                    return None;
                }
                remove.removing = true;
                remove.error = None;
                Some((
                    remove.workspace_id.clone(),
                    remove.repo_root.clone(),
                    remove.path.clone(),
                    remove.force_confirmation,
                ))
            })
        else {
            return;
        };

        if Self::should_shutdown_workspace_terminal_runtimes_for_worktree_remove(force) {
            if let Some(ws_idx) = self
                .state
                .workspaces
                .iter()
                .position(|ws| ws.id == workspace_id)
            {
                self.shutdown_workspace_terminal_runtimes_for_worktree_remove(ws_idx);
            }
        }

        let (workspace_snapshot, worktree_snapshot) = self
            .state
            .workspaces
            .iter()
            .position(|ws| ws.id == workspace_id)
            .map(|ws_idx| {
                let workspace = Box::new(self.workspace_info(ws_idx));
                let worktree = self.state.workspaces[ws_idx]
                    .worktree_space()
                    .cloned()
                    .map(|space| Box::new(self.worktree_info_for_membership(&space, None)));
                (Some(workspace), worktree)
            })
            .unwrap_or((None, None));

        let command = crate::worktree::build_worktree_remove_command(&repo_root, &path, force);
        tracing::info!(workspace_id = %workspace_id, path = %path.display(), force, "starting git worktree remove");
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = crate::worktree::run_worktree_remove_command_with_recovery(
                &command, &repo_root, &path, force,
            );
            let _ = event_tx.blocking_send(AppEvent::WorktreeRemoveFinished(Box::new(
                WorktreeRemoveResult {
                    workspace_id,
                    path,
                    workspace: workspace_snapshot,
                    worktree: worktree_snapshot,
                    forced: force,
                    api_request: None,
                    result,
                },
            )));
        });
    }

    pub(crate) fn submit_worktree_open_via_api(&mut self) {
        let Some(open) = self.state.worktree_open.as_ref() else {
            return;
        };
        let Some(entry_idx) = open.selected_entry_index() else {
            return;
        };
        let Some(entry) = open.entries.get(entry_idx).cloned() else {
            return;
        };
        let source_workspace_id = open.source_workspace_id.clone();

        let response = self.runtime_worktree_open(
            "tui.worktree.open",
            crate::api::schema::WorktreeOpenParams {
                workspace_id: Some(source_workspace_id),
                cwd: None,
                path: Some(entry.path.display().to_string()),
                branch: None,
                focus: true,
                label: None,
            },
        );
        if serde_json::from_str::<crate::api::schema::SuccessResponse>(&response).is_ok() {
            self.state.worktree_open = None;
            self.state.mode = Mode::Terminal;
        } else if let Ok(error) =
            serde_json::from_str::<crate::api::schema::ErrorResponse>(&response)
        {
            if let Some(open) = &mut self.state.worktree_open {
                open.error = Some(error.error.message);
            }
        }
    }

    pub(crate) fn submit_worktree_remove_via_api(&mut self) {
        let Some(remove) = self.state.worktree_remove.as_mut() else {
            return;
        };
        if remove.removing {
            return;
        }
        #[cfg(windows)]
        if !remove.force_confirmation
            && crate::worktree::checkout_has_dirty_files(&remove.path).unwrap_or(false)
        {
            remove.force_confirmation = true;
            remove.error = None;
            return;
        }

        remove.removing = true;
        remove.error = None;
        let workspace_id = remove.workspace_id.clone();
        let force = remove.force_confirmation;
        let immediate_response = self.runtime_worktree_remove_deferred(
            "tui.worktree.remove",
            crate::api::schema::WorktreeRemoveParams {
                workspace_id,
                force,
            },
        );
        if let Some(message) = immediate_api_error_message(immediate_response.as_deref()) {
            if let Some(remove) = &mut self.state.worktree_remove {
                remove.removing = false;
                remove.error = Some(message);
            }
        }
    }

    pub(crate) fn handle_worktree_add_finished(&mut self, result: WorktreeAddResult) {
        if result.api_request.is_some() {
            self.handle_api_worktree_add_finished(result);
            return;
        }
        let Some(create) = &mut self.state.worktree_create else {
            return;
        };
        if create.checkout_path != result.path {
            return;
        }

        match result.result {
            Ok(()) => {
                tracing::info!(checkout_path = %create.checkout_path.display(), "git worktree add completed");
                let path = create.checkout_path.clone();
                let source_workspace_id = create.source_workspace_id.clone();
                let source_checkout_path = create.source_checkout_path.clone();
                let source_existing_membership = create.source_existing_membership.clone();
                let repo_key = create.repo_key.clone();
                let repo_name = create.repo_name.clone();
                let source_repo_root = create.source_repo_root.clone();
                self.state.worktree_create = None;
                self.state.name_input.clear();
                self.state.name_input_replace_on_type = false;
                let source_membership = source_existing_membership.unwrap_or(
                    crate::workspace::WorktreeSpaceMembership {
                        key: repo_key.clone(),
                        label: repo_name.clone(),
                        repo_root: source_repo_root.clone(),
                        checkout_path: source_checkout_path,
                        is_linked_worktree: false,
                    },
                );
                if let Some(source_ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|ws| ws.id == source_workspace_id)
                {
                    self.set_worktree_membership(source_ws_idx, source_membership, true);
                }
                if let Some(ws_idx) = self.open_workspace_idx_for_checkout(&path) {
                    self.set_worktree_membership(
                        ws_idx,
                        crate::workspace::WorktreeSpaceMembership {
                            key: repo_key,
                            label: repo_name,
                            repo_root: source_repo_root,
                            checkout_path: path,
                            is_linked_worktree: true,
                        },
                        true,
                    );
                    self.state.switch_workspace(ws_idx);
                    self.state.mode = Mode::Terminal;
                    if let Some(worktree) = self.worktree_info_for_workspace(ws_idx) {
                        self.emit_worktree_created_event(ws_idx, worktree);
                    }
                } else {
                    match self.create_workspace_with_options(path.clone(), true) {
                        Ok(ws_idx) => {
                            self.set_worktree_membership(
                                ws_idx,
                                crate::workspace::WorktreeSpaceMembership {
                                    key: repo_key,
                                    label: repo_name,
                                    repo_root: source_repo_root,
                                    checkout_path: path,
                                    is_linked_worktree: true,
                                },
                                false,
                            );
                            self.emit_workspace_open_events(ws_idx);
                            if let Some(worktree) = self.worktree_info_for_workspace(ws_idx) {
                                self.emit_worktree_created_event(ws_idx, worktree);
                            }
                        }
                        Err(err) => {
                            self.state.config_diagnostic = Some(format!(
                                "created worktree but failed to open workspace: {err}"
                            ));
                            self.state.mode = Mode::Navigate;
                        }
                    }
                }
                self.render_dirty.store(true, Ordering::Release);
                self.render_notify.notify_one();
            }
            Err(message) => {
                tracing::warn!(checkout_path = %create.checkout_path.display(), error = %message, "git worktree add failed");
                create.creating = false;
                create.error = Some(message);
                self.render_dirty.store(true, Ordering::Release);
                self.render_notify.notify_one();
            }
        }
    }
    pub(crate) fn handle_worktree_remove_finished(&mut self, result: WorktreeRemoveResult) {
        if result.api_request.is_some() {
            self.handle_api_worktree_remove_finished(result);
            return;
        }
        let Some(remove) = &mut self.state.worktree_remove else {
            return;
        };
        if remove.workspace_id != result.workspace_id || remove.path != result.path {
            return;
        }

        match result.result {
            Ok(()) => {
                tracing::info!(workspace_id = %result.workspace_id, path = %result.path.display(), "git worktree remove completed");
                let forced = result.forced;
                self.state.worktree_remove = None;
                let mut workspace_id = result.workspace_id.clone();
                let mut workspace_snapshot = result.workspace.as_deref().cloned();
                let mut worktree = result.worktree.as_deref().cloned();
                if let Some(ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|ws| ws.id == result.workspace_id)
                {
                    workspace_id = self.public_workspace_id(ws_idx);
                    workspace_snapshot.get_or_insert_with(|| self.workspace_info(ws_idx));
                    if worktree.is_none() {
                        worktree = self.state.workspaces[ws_idx]
                            .worktree_space()
                            .cloned()
                            .map(|space| self.worktree_info_for_membership(&space, None));
                    }
                    let still_same_linked_worktree = self.state.workspaces[ws_idx]
                        .worktree_space()
                        .is_some_and(|space| {
                            space.is_linked_worktree && space.checkout_path == result.path
                        });
                    if still_same_linked_worktree {
                        self.state.selected = ws_idx;
                        self.state.close_selected_workspace();
                        self.shutdown_detached_terminal_runtimes();
                        self.emit_event(crate::api::schema::EventEnvelope {
                            event: crate::api::schema::EventKind::WorkspaceClosed,
                            data: crate::api::schema::EventData::WorkspaceClosed {
                                workspace_id: workspace_id.clone(),
                                workspace: workspace_snapshot.clone(),
                            },
                        });
                    }
                } else if let Some(snapshot) = workspace_snapshot.as_ref() {
                    workspace_id = snapshot.workspace_id.clone();
                }
                if let Some(worktree) = worktree {
                    self.emit_worktree_removed_event(
                        workspace_id,
                        workspace_snapshot,
                        worktree,
                        forced,
                    );
                }
                self.state.mode = if self.state.active.is_some() {
                    Mode::Terminal
                } else {
                    Mode::Navigate
                };
                self.render_dirty.store(true, Ordering::Release);
                self.render_notify.notify_one();
            }
            Err(message) => {
                tracing::warn!(workspace_id = %result.workspace_id, path = %result.path.display(), error = %message, "git worktree remove failed");
                remove.removing = false;
                if !remove.force_confirmation
                    && crate::worktree::is_dirty_worktree_remove_error(&message)
                {
                    remove.force_confirmation = true;
                    remove.error = None;
                } else {
                    remove.error = Some(message);
                }
                self.render_dirty.store(true, Ordering::Release);
                self.render_notify.notify_one();
            }
        }
    }

    pub(crate) fn should_shutdown_workspace_terminal_runtimes_for_worktree_remove(
        force: bool,
    ) -> bool {
        force || cfg!(windows)
    }

    pub(crate) fn shutdown_workspace_terminal_runtimes_for_worktree_remove(
        &mut self,
        ws_idx: usize,
    ) {
        for terminal_id in self.state.terminal_ids_for_workspace(ws_idx) {
            if let Some(runtime) = self.terminal_runtimes.remove(&terminal_id) {
                tracing::debug!(
                    workspace_index = ws_idx,
                    terminal_id = %terminal_id,
                    "shutting down terminal runtime before worktree removal"
                );
                runtime.shutdown();
            }
        }
    }
}

fn immediate_api_error_message(response: Option<&str>) -> Option<String> {
    response
        .and_then(|response| {
            serde_json::from_str::<crate::api::schema::ErrorResponse>(response).ok()
        })
        .map(|response| response.error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
    }

    fn run_git(repo: &std::path::Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "git command failed: git -C {} {}",
            repo.display(),
            args.join(" ")
        );
    }

    fn create_committed_repo(name: &str) -> std::path::PathBuf {
        let repo = unique_temp_path(name);
        std::fs::create_dir_all(&repo).unwrap();
        run_git(&repo, &["init", "--quiet"]);
        run_git(&repo, &["config", "user.email", "herdr@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Herdr Test"]);
        std::fs::write(repo.join("README.md"), "test\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "--quiet", "-m", "initial"]);
        repo
    }

    fn wait_for_worktree_event(app: &mut App) -> AppEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Ok(event) = app.event_rx.try_recv() {
                return event;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("timed out waiting for worktree event");
    }

    fn app_for_worktree_tests() -> App {
        app_for_worktree_tests_with_event_hub(crate::api::EventHub::default())
    }

    fn app_for_worktree_tests_with_event_hub(event_hub: crate::api::EventHub) -> App {
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            event_hub,
        )
    }

    fn event_kinds(event_hub: &crate::api::EventHub) -> Vec<crate::api::schema::EventKind> {
        event_hub
            .events_after(0)
            .into_iter()
            .map(|(_, event)| event.event)
            .collect()
    }

    fn shutdown_test_runtimes(app: &mut App) {
        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn ui_create_workspace_emits_initial_workspace_tab_and_pane_events() {
        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());

        app.create_workspace();

        assert_eq!(
            event_kinds(&event_hub),
            vec![
                crate::api::schema::EventKind::WorkspaceCreated,
                crate::api::schema::EventKind::TabCreated,
                crate::api::schema::EventKind::PaneCreated,
                crate::api::schema::EventKind::LayoutUpdated,
            ]
        );
        shutdown_test_runtimes(&mut app);
    }

    #[tokio::test]
    async fn ui_create_tab_emits_tab_and_pane_events() {
        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.create_workspace_with_options(std::env::temp_dir(), true)
            .unwrap();

        app.create_tab();

        assert_eq!(
            event_kinds(&event_hub),
            vec![
                crate::api::schema::EventKind::TabCreated,
                crate::api::schema::EventKind::PaneCreated,
                crate::api::schema::EventKind::LayoutUpdated,
            ]
        );
        shutdown_test_runtimes(&mut app);
    }

    #[test]
    fn worktree_create_replaces_prefilled_branch_on_paste_and_syncs_state() {
        let mut app = app_for_worktree_tests();
        app.state.name_input = "generated-branch".into();
        app.state.name_input_replace_on_type = true;
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "generated-branch".into(),
            checkout_path: "/repo/herdr-generated-branch".into(),
            error: None,
            creating: false,
        });

        app.insert_worktree_create_text("feature/linear-302");

        assert_eq!(app.state.name_input, "feature/linear-302");
        assert!(!app.state.name_input_replace_on_type);
        assert_eq!(
            app.state
                .worktree_create
                .as_ref()
                .map(|create| create.branch.as_str()),
            Some("feature/linear-302")
        );
    }

    #[test]
    fn worktree_open_search_accepts_pasted_text_when_focused() {
        let mut app = app_for_worktree_tests();
        app.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id: "source".into(),
            source_existing_membership: None,
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![
                WorktreeOpenEntry {
                    path: "/repo/herdr-main".into(),
                    branch: Some("main".into()),
                    is_linked_worktree: false,
                    already_open_ws_idx: None,
                },
                WorktreeOpenEntry {
                    path: "/repo/feature-linear-302".into(),
                    branch: Some("feature/linear-302".into()),
                    is_linked_worktree: true,
                    already_open_ws_idx: None,
                },
            ],
            selected: 0,
            query: String::new(),
            search_focused: true,
            error: None,
        });

        app.insert_worktree_open_search_text("linear-302");

        let open = app.state.worktree_open.as_ref().unwrap();
        assert_eq!(open.query, "linear-302");
        assert_eq!(open.selected_entry_index(), Some(1));
    }

    #[test]
    fn worktree_open_search_ignores_paste_when_search_is_not_focused() {
        let mut app = app_for_worktree_tests();
        app.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id: "source".into(),
            source_existing_membership: None,
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: Vec::new(),
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        });

        app.insert_worktree_open_search_text("linear-302");

        assert_eq!(
            app.state
                .worktree_open
                .as_ref()
                .map(|open| open.query.as_str()),
            Some("")
        );
    }

    #[test]
    fn open_selected_existing_worktree_focuses_already_open_workspace() {
        let mut app = app_for_worktree_tests();
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("main"),
            crate::workspace::Workspace::test_new("issue"),
        ];
        app.state.workspaces[1].identity_cwd = "/repo/herdr-issue".into();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id: app.state.workspaces[0].id.clone(),
            source_existing_membership: None,
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![WorktreeOpenEntry {
                path: "/repo/herdr-issue".into(),
                branch: Some("worktree/issue".into()),
                is_linked_worktree: true,
                already_open_ws_idx: Some(1),
            }],
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        });

        app.open_selected_existing_worktree();

        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.selected, 1);
        assert!(app.state.worktree_open.is_none());
        assert!(app.state.workspaces[0].worktree_space().is_some());
        let target_membership = app.state.workspaces[1].worktree_space().unwrap();
        assert_eq!(target_membership.key, "repo-key");
        assert_eq!(
            target_membership.checkout_path,
            std::path::PathBuf::from("/repo/herdr-issue")
        );
        assert!(target_membership.is_linked_worktree);
    }

    #[tokio::test]
    async fn ui_worktree_open_new_workspace_emits_api_parity_events() {
        let checkout = unique_temp_path("app-worktree-open-event-checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("source")];
        let source_workspace_id = app.state.workspaces[0].id.clone();
        let source_membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        };
        app.state.workspaces[0].worktree_space = Some(source_membership.clone());
        app.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id,
            source_existing_membership: Some(source_membership),
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![WorktreeOpenEntry {
                path: checkout.clone(),
                branch: Some("worktree/open-event".into()),
                is_linked_worktree: true,
                already_open_ws_idx: None,
            }],
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        });

        app.open_selected_existing_worktree();

        assert_eq!(
            event_kinds(&event_hub),
            vec![
                crate::api::schema::EventKind::WorkspaceCreated,
                crate::api::schema::EventKind::TabCreated,
                crate::api::schema::EventKind::PaneCreated,
                crate::api::schema::EventKind::LayoutUpdated,
                crate::api::schema::EventKind::WorktreeOpened,
            ]
        );
        shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(checkout);
    }

    #[tokio::test]
    async fn open_selected_existing_worktree_recomputes_stale_already_open_state() {
        let checkout = unique_temp_path("app-worktree-stale-open-checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("source"),
            crate::workspace::Workspace::test_new("other"),
        ];
        let source_workspace_id = app.state.workspaces[0].id.clone();
        app.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id,
            source_existing_membership: None,
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![WorktreeOpenEntry {
                path: checkout.clone(),
                branch: Some("worktree/stale-open".into()),
                is_linked_worktree: true,
                already_open_ws_idx: Some(1),
            }],
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        });

        app.open_selected_existing_worktree();

        assert_eq!(app.state.workspaces.len(), 3);
        assert_eq!(
            event_kinds(&event_hub),
            vec![
                crate::api::schema::EventKind::WorkspaceUpdated,
                crate::api::schema::EventKind::WorkspaceCreated,
                crate::api::schema::EventKind::TabCreated,
                crate::api::schema::EventKind::PaneCreated,
                crate::api::schema::EventKind::LayoutUpdated,
                crate::api::schema::EventKind::WorktreeOpened,
            ]
        );
        let opened = event_hub
            .events_after(0)
            .into_iter()
            .find_map(|(_, event)| match event.data {
                crate::api::schema::EventData::WorktreeOpened { already_open, .. } => {
                    Some(already_open)
                }
                _ => None,
            })
            .expect("worktree.opened should be emitted");
        assert!(!opened);
        shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(checkout);
    }

    #[test]
    fn worktree_open_search_filters_entries() {
        let mut app = app_for_worktree_tests();
        app.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id: "source".into(),
            source_existing_membership: None,
            source_checkout_path: "/repo/herdr".into(),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![
                WorktreeOpenEntry {
                    path: "/repo/herdr".into(),
                    branch: Some("main".into()),
                    is_linked_worktree: false,
                    already_open_ws_idx: Some(0),
                },
                WorktreeOpenEntry {
                    path: "/repo/fd-cleanup".into(),
                    branch: Some("fd-cleanup".into()),
                    is_linked_worktree: true,
                    already_open_ws_idx: None,
                },
                WorktreeOpenEntry {
                    path: "/repo/bell-forward-macos-bounce".into(),
                    branch: Some("bell-forward-macos-bounce".into()),
                    is_linked_worktree: true,
                    already_open_ws_idx: None,
                },
            ],
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        });

        app.handle_worktree_open_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('/'),
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_worktree_open_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('f'),
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_worktree_open_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('d'),
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_worktree_open_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('-'),
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_worktree_open_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('c'),
            crossterm::event::KeyModifiers::empty(),
        ));
        app.handle_worktree_open_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('l'),
            crossterm::event::KeyModifiers::empty(),
        ));

        let open = app.state.worktree_open.as_ref().unwrap();
        assert_eq!(open.query, "fd-cl");
        assert_eq!(open.filtered_indices(), vec![1]);
        assert_eq!(open.selected_entry_index(), Some(1));
    }

    #[test]
    fn open_existing_worktree_detects_already_open_checkout_from_subdirectory() {
        let repo = create_committed_repo("app-worktree-open-existing-repo");
        let checkout = unique_temp_path("app-worktree-open-existing-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/open-existing",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        let subdir = checkout.join("nested");
        std::fs::create_dir_all(&subdir).unwrap();

        let mut app = app_for_worktree_tests();
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("main"),
            crate::workspace::Workspace::test_new("nested"),
        ];
        app.state.workspaces[0].identity_cwd = repo;
        app.state.workspaces[1].identity_cwd = subdir;

        app.open_existing_worktree_dialog(0);

        let open = app.state.worktree_open.as_ref().unwrap();
        let checkout = crate::worktree::canonical_or_original(&checkout);
        let entry = open
            .entries
            .iter()
            .find(|entry| crate::worktree::canonical_or_original(&entry.path) == checkout)
            .unwrap_or_else(|| panic!("missing checkout in entries: {:?}", open.entries));
        assert_eq!(entry.already_open_ws_idx, Some(1));
    }

    #[test]
    fn worktree_create_and_open_dialogs_reject_linked_child_source() {
        let mut app = app_for_worktree_tests();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("issue")];
        app.state.mode = Mode::Navigate;
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });

        app.open_new_linked_worktree_dialog(0);

        assert_eq!(app.state.mode, Mode::Navigate);
        assert!(app.state.worktree_create.is_none());
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("New and open worktree actions start from the repo parent workspace.")
        );

        app.state.config_diagnostic = None;
        app.open_existing_worktree_dialog(0);

        assert!(app.state.worktree_open.is_none());
        assert_eq!(
            app.state.config_diagnostic.as_deref(),
            Some("New and open worktree actions start from the repo parent workspace.")
        );
    }

    #[test]
    fn sync_worktree_branch_updates_derived_path() {
        let mut app = app_for_worktree_tests();
        app.state.worktree_directory = std::path::PathBuf::from("/w");
        app.state.name_input = "issue/137".into();
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: std::path::PathBuf::from("/repo/herdr"),
            source_existing_membership: None,
            source_repo_root: std::path::PathBuf::from("/repo/herdr"),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "old".into(),
            checkout_path: std::path::PathBuf::from("/old"),
            error: Some("old error".into()),
            creating: false,
        });

        app.sync_worktree_branch_from_input();

        let create = app.state.worktree_create.unwrap();
        assert_eq!(create.branch, "issue/137");
        assert_eq!(
            create.checkout_path,
            std::path::PathBuf::from("/w/herdr/issue-137")
        );
        assert_eq!(create.error, None);
    }

    #[test]
    fn worktree_create_enter_submits_through_api_path() {
        let mut app = app_for_worktree_tests();
        app.state.worktree_directory = std::path::PathBuf::from("/w");
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("source")];
        let source_workspace_id = app.state.workspaces[0].id.clone();
        let source_membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr".into(),
            is_linked_worktree: false,
        };
        let branch = "issue/195";
        let checkout_path =
            crate::worktree::default_checkout_path(&app.state.worktree_directory, "herdr", branch);
        let checkout_key = crate::worktree::canonical_or_original(&checkout_path);
        app.pending_api_worktree_creates.insert(checkout_key, 1);
        app.state.workspaces[0].worktree_space = Some(source_membership.clone());
        app.state.mode = Mode::NewLinkedWorktree;
        app.state.name_input = branch.into();
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id,
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: Some(source_membership),
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: branch.into(),
            checkout_path,
            error: None,
            creating: false,
        });

        app.handle_worktree_create_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        let create = app.state.worktree_create.as_ref().unwrap();
        assert!(!create.creating);
        assert_eq!(
            create.error.as_deref(),
            Some("worktree operation is already in progress for this checkout")
        );
    }

    #[tokio::test]
    async fn worktree_open_enter_submits_through_api_path() {
        let repo = create_committed_repo("app-worktree-open-enter-repo");
        let checkout = unique_temp_path("app-worktree-open-enter-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/open-enter",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("source")];
        let source_workspace_id = app.state.workspaces[0].id.clone();
        let source_membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: repo.clone(),
            checkout_path: repo.clone(),
            is_linked_worktree: false,
        };
        app.state.workspaces[0].worktree_space = Some(source_membership.clone());
        app.state.mode = Mode::OpenExistingWorktree;
        app.state.worktree_open = Some(WorktreeOpenState {
            source_workspace_id,
            source_existing_membership: Some(source_membership),
            source_checkout_path: repo.clone(),
            source_repo_root: repo.clone(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            entries: vec![WorktreeOpenEntry {
                path: checkout.clone(),
                branch: Some("worktree/open-enter".into()),
                is_linked_worktree: true,
                already_open_ws_idx: None,
            }],
            selected: 0,
            query: String::new(),
            search_focused: false,
            error: None,
        });

        app.handle_worktree_open_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        assert!(app.state.worktree_open.is_none());
        assert_eq!(
            event_kinds(&event_hub),
            vec![
                crate::api::schema::EventKind::WorkspaceCreated,
                crate::api::schema::EventKind::TabCreated,
                crate::api::schema::EventKind::PaneCreated,
                crate::api::schema::EventKind::LayoutUpdated,
                crate::api::schema::EventKind::WorktreeOpened,
            ]
        );
        shutdown_test_runtimes(&mut app);
        run_git(
            &repo,
            &["worktree", "remove", "--force", checkout.to_str().unwrap()],
        );
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn worktree_remove_enter_submits_through_api_path() {
        let mut app = app_for_worktree_tests();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("issue")];
        let workspace_id = app.state.workspaces[0].id.clone();
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        app.open_remove_linked_worktree_confirmation(0);
        app.pending_api_worktree_removes
            .insert(workspace_id.clone(), 1);

        app.handle_worktree_remove_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));

        let remove = app.state.worktree_remove.as_ref().unwrap();
        assert_eq!(remove.workspace_id, workspace_id);
        assert!(!remove.removing);
        assert_eq!(
            remove.error.as_deref(),
            Some("worktree operation is already in progress for this checkout")
        );
    }

    #[tokio::test]
    async fn ui_worktree_create_emits_api_parity_events_after_membership_is_committed() {
        let repo = create_committed_repo("app-worktree-create-event-repo");
        let worktree_root = unique_temp_path("app-worktree-create-event-root");
        let branch = "worktree/ui-create-event";
        let checkout = crate::worktree::default_checkout_path(&worktree_root, "herdr", branch);
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                branch,
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("source")];
        let source_workspace_id = app.state.workspaces[0].id.clone();
        let source_membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: repo.clone(),
            checkout_path: repo.clone(),
            is_linked_worktree: false,
        };
        app.state.workspaces[0].worktree_space = Some(source_membership.clone());
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id,
            source_checkout_path: repo.clone(),
            source_existing_membership: Some(source_membership),
            source_repo_root: repo.clone(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: branch.into(),
            checkout_path: checkout.clone(),
            error: None,
            creating: true,
        });
        let plugin_root = unique_temp_path("app-worktree-create-plugin");
        std::fs::create_dir_all(&plugin_root).unwrap();
        let manifest_path = plugin_root.join("herdr-plugin.toml");
        std::fs::write(&manifest_path, "id = 'example.ui-worktree-create'\n").unwrap();
        app.state.installed_plugins.insert(
            "example.ui-worktree-create".into(),
            crate::api::schema::InstalledPluginInfo {
                plugin_id: "example.ui-worktree-create".into(),
                name: "UI Worktree Create".into(),
                version: "0.1.0".into(),
                min_herdr_version: "0.7.0".into(),
                description: None,
                manifest_path: manifest_path.display().to_string(),
                plugin_root: plugin_root.display().to_string(),
                enabled: true,
                platforms: None,
                build: Vec::new(),
                actions: Vec::new(),
                events: vec![crate::api::schema::PluginManifestEventHook {
                    on: "worktree.created".into(),
                    platforms: None,
                    command: vec!["sh".into(), "-c".into(), "true".into()],
                }],
                panes: Vec::new(),
                link_handlers: Vec::new(),
                source: crate::api::schema::PluginSourceInfo::default(),
                warnings: Vec::new(),
            },
        );

        app.handle_worktree_add_finished(WorktreeAddResult {
            path: checkout.clone(),
            api_request: None,
            result: Ok(()),
        });

        assert_eq!(
            event_kinds(&event_hub),
            vec![
                crate::api::schema::EventKind::WorkspaceCreated,
                crate::api::schema::EventKind::TabCreated,
                crate::api::schema::EventKind::PaneCreated,
                crate::api::schema::EventKind::LayoutUpdated,
                crate::api::schema::EventKind::WorktreeCreated,
            ]
        );
        let events = event_hub.events_after(0);
        let workspace_created = events
            .iter()
            .find(|(_, event)| event.event == crate::api::schema::EventKind::WorkspaceCreated)
            .map(|(_, event)| event)
            .expect("workspace.created should be emitted");
        let crate::api::schema::EventData::WorkspaceCreated { workspace } = &workspace_created.data
        else {
            panic!("unexpected event data");
        };
        let checkout_path = checkout.display().to_string();
        assert_eq!(
            workspace
                .worktree
                .as_ref()
                .map(|worktree| worktree.checkout_path.as_str()),
            Some(checkout_path.as_str())
        );
        let worktree_created = events
            .iter()
            .find(|(_, event)| event.event == crate::api::schema::EventKind::WorktreeCreated)
            .map(|(_, event)| event)
            .expect("worktree.created should be emitted");
        let crate::api::schema::EventData::WorktreeCreated {
            workspace,
            worktree,
        } = &worktree_created.data
        else {
            panic!("unexpected event data");
        };
        assert_eq!(
            workspace
                .worktree
                .as_ref()
                .map(|worktree| worktree.checkout_path.as_str()),
            Some(checkout_path.as_str())
        );
        assert_eq!(
            worktree.open_workspace_id.as_deref(),
            Some(workspace.workspace_id.as_str())
        );
        assert!(app.state.plugin_command_logs.iter().any(|log| {
            log.event.as_deref() == Some("worktree.created")
                && log.status == crate::api::schema::PluginCommandStatus::Running
        }));

        shutdown_test_runtimes(&mut app);
        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(plugin_root);
    }

    #[test]
    fn worktree_create_finished_reuses_checkout_opened_before_result() {
        let checkout = unique_temp_path("app-worktree-create-race-checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("source"),
            crate::workspace::Workspace::test_new("opened-by-race"),
        ];
        let source_workspace_id = app.state.workspaces[0].id.clone();
        app.state.workspaces[1].identity_cwd = checkout.clone();
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id,
            source_checkout_path: "/repo/herdr".into(),
            source_existing_membership: None,
            source_repo_root: "/repo/herdr".into(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: "worktree/create-race".into(),
            checkout_path: checkout.clone(),
            error: None,
            creating: true,
        });

        app.handle_worktree_add_finished(WorktreeAddResult {
            path: checkout.clone(),
            api_request: None,
            result: Ok(()),
        });

        assert_eq!(app.state.workspaces.len(), 2);
        let kinds = event_kinds(&event_hub);
        assert!(!kinds.contains(&crate::api::schema::EventKind::WorkspaceCreated));
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| **kind == crate::api::schema::EventKind::WorktreeCreated)
                .count(),
            1
        );
        assert_eq!(
            app.state.workspaces[1]
                .worktree_space()
                .map(|membership| membership.checkout_path.as_path()),
            Some(checkout.as_path())
        );
        shutdown_test_runtimes(&mut app);
        let _ = std::fs::remove_dir_all(checkout);
    }

    #[test]
    fn start_worktree_add_runs_git_on_worker_and_emits_result() {
        let repo = create_committed_repo("app-worktree-add-repo");
        let worktree_root = unique_temp_path("app-worktree-add-root");
        let branch = "worktree/app-worker";
        let checkout = crate::worktree::default_checkout_path(&worktree_root, "herdr", branch);
        let mut app = app_for_worktree_tests();
        app.state.worktree_directory = worktree_root.clone();
        app.state.name_input = branch.into();
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: repo.clone(),
            source_existing_membership: None,
            source_repo_root: repo.clone(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: branch.into(),
            checkout_path: checkout.clone(),
            error: None,
            creating: false,
        });

        app.start_worktree_add();

        assert!(app
            .state
            .worktree_create
            .as_ref()
            .is_some_and(|create| create.creating));
        let event = wait_for_worktree_event(&mut app);
        match event {
            AppEvent::WorktreeAddFinished(result) => {
                let result = *result;
                assert_eq!(result.path, checkout);
                assert_eq!(result.result, Ok(()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(checkout.join("README.md").exists());

        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn start_worktree_add_existing_branch_checks_out_branch() {
        let repo = create_committed_repo("app-worktree-add-existing-branch-repo");
        let worktree_root = unique_temp_path("app-worktree-add-existing-branch-root");
        let branch = "foo";
        let checkout = crate::worktree::default_checkout_path(&worktree_root, "herdr", branch);
        run_git(&repo, &["branch", branch]);
        let mut app = app_for_worktree_tests();
        app.state.worktree_directory = worktree_root.clone();
        app.state.name_input = branch.into();
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: repo.clone(),
            source_existing_membership: None,
            source_repo_root: repo.clone(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: branch.into(),
            checkout_path: checkout.clone(),
            error: None,
            creating: false,
        });

        app.start_worktree_add();

        assert!(app
            .state
            .worktree_create
            .as_ref()
            .is_some_and(|create| create.creating));
        let event = wait_for_worktree_event(&mut app);
        match event {
            AppEvent::WorktreeAddFinished(result) => {
                let result = *result;
                assert_eq!(result.path, checkout);
                assert_eq!(result.result, Ok(()));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        assert!(checkout.join("README.md").exists());
        let branch_name = std::process::Command::new("git")
            .arg("-C")
            .arg(&checkout)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert!(branch_name.status.success());
        assert_eq!(
            String::from_utf8(branch_name.stdout).unwrap().trim(),
            branch
        );

        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn open_new_worktree_dialog_supports_standalone_bare_repo_source() {
        let repo = create_committed_repo("app-worktree-dialog-bare-origin");
        let bare = unique_temp_path("app-worktree-dialog-bare-repo");
        run_git(
            &repo,
            &["clone", "--quiet", "--bare", ".", bare.to_str().unwrap()],
        );
        let worktree_root = unique_temp_path("app-worktree-dialog-bare-root");

        let mut app = app_for_worktree_tests();
        app.state.worktree_directory = worktree_root.clone();
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("source")];
        app.state.workspaces[0].identity_cwd = bare.clone();

        app.open_new_linked_worktree_dialog(0);

        assert_eq!(app.state.mode, Mode::NewLinkedWorktree);
        assert!(app.state.config_diagnostic.is_none());
        let create = app.state.worktree_create.as_ref().unwrap();
        assert_eq!(create.source_checkout_path, bare);
        assert_eq!(create.source_repo_root, create.source_checkout_path);
        let source_checkout_path = create.source_checkout_path.clone();

        let branch = "worktree/from-bare-source";
        let repo_name = create.repo_name.clone();
        let checkout = crate::worktree::default_checkout_path(&worktree_root, &repo_name, branch);
        app.state.name_input = branch.into();

        app.start_worktree_add();

        let event = wait_for_worktree_event(&mut app);
        match event {
            AppEvent::WorktreeAddFinished(result) => {
                let result = *result;
                assert_eq!(result.path, checkout);
                assert_eq!(result.result, Ok(()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(checkout.join("README.md").exists());

        let remove_new =
            crate::worktree::build_worktree_remove_command(&source_checkout_path, &checkout, false);
        crate::worktree::run_worktree_command(&remove_new).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(source_checkout_path);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn start_worktree_add_uses_source_checkout_head_as_base() {
        let repo = create_committed_repo("app-worktree-add-source-repo");
        let source_checkout = unique_temp_path("app-worktree-add-source-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/source-base",
                source_checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(source_checkout.join("SOURCE.md"), "source branch\n").unwrap();
        run_git(&source_checkout, &["add", "SOURCE.md"]);
        run_git(&source_checkout, &["commit", "--quiet", "-m", "source"]);

        let worktree_root = unique_temp_path("app-worktree-add-from-source-root");
        let branch = "worktree/from-source";
        let checkout = crate::worktree::default_checkout_path(&worktree_root, "herdr", branch);
        let mut app = app_for_worktree_tests();
        app.state.worktree_directory = worktree_root.clone();
        app.state.name_input = branch.into();
        app.state.worktree_create = Some(WorktreeCreateState {
            source_workspace_id: "source".into(),
            source_checkout_path: source_checkout.clone(),
            source_existing_membership: None,
            source_repo_root: repo.clone(),
            repo_key: "repo-key".into(),
            repo_name: "herdr".into(),
            branch: branch.into(),
            checkout_path: checkout.clone(),
            error: None,
            creating: false,
        });

        app.start_worktree_add();

        let event = wait_for_worktree_event(&mut app);
        match event {
            AppEvent::WorktreeAddFinished(result) => {
                let result = *result;
                assert_eq!(result.path, checkout);
                assert_eq!(result.result, Ok(()));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        assert!(checkout.join("SOURCE.md").exists());

        let remove_new = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove_new).unwrap();
        let remove_source =
            crate::worktree::build_worktree_remove_command(&repo, &source_checkout, false);
        crate::worktree::run_worktree_command(&remove_source).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn dirty_worktree_remove_failure_requests_force_confirmation() {
        let path = std::path::PathBuf::from("/w/herdr/dirty");
        let mut app = app_for_worktree_tests();
        app.state.worktree_remove = Some(WorktreeRemoveState {
            workspace_id: "ws".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            path: path.clone(),
            error: None,
            removing: true,
            force_confirmation: false,
        });

        app.handle_worktree_remove_finished(WorktreeRemoveResult {
            workspace_id: "ws".into(),
            path,
            workspace: None,
            worktree: None,
            forced: false,
            api_request: None,
            result: Err(
                "fatal: '/w/herdr/dirty' contains modified or untracked files, use --force to delete it"
                    .into(),
            ),
        });

        let remove = app.state.worktree_remove.unwrap();
        assert!(!remove.removing);
        assert!(remove.force_confirmation);
        assert_eq!(remove.error, None);
    }

    #[test]
    fn non_dirty_worktree_remove_failure_keeps_error_message() {
        let path = std::path::PathBuf::from("/w/herdr/missing");
        let mut app = app_for_worktree_tests();
        app.state.worktree_remove = Some(WorktreeRemoveState {
            workspace_id: "ws".into(),
            repo_root: std::path::PathBuf::from("/repo/herdr"),
            path: path.clone(),
            error: None,
            removing: true,
            force_confirmation: false,
        });

        app.handle_worktree_remove_finished(WorktreeRemoveResult {
            workspace_id: "ws".into(),
            path,
            workspace: None,
            worktree: None,
            forced: false,
            api_request: None,
            result: Err("fatal: '/w/herdr/missing' is not a working tree".into()),
        });

        let remove = app.state.worktree_remove.unwrap();
        assert!(!remove.removing);
        assert!(!remove.force_confirmation);
        assert_eq!(
            remove.error,
            Some("fatal: '/w/herdr/missing' is not a working tree".into())
        );
    }

    #[test]
    fn worktree_remove_finished_emits_removed_event_from_snapshot_after_workspace_closed() {
        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("issue")];
        let internal_workspace_id = app.state.workspaces[0].id.clone();
        let checkout = std::path::PathBuf::from("/repo/herdr-issue");
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let workspace_snapshot = app.workspace_info(0);
        let worktree_snapshot = crate::api::schema::WorktreeInfo {
            path: checkout.display().to_string(),
            branch: Some("worktree/issue".into()),
            is_bare: false,
            is_detached: false,
            is_prunable: false,
            is_linked_worktree: true,
            open_workspace_id: None,
            label: "herdr".into(),
        };
        app.state.worktree_remove = Some(WorktreeRemoveState {
            workspace_id: internal_workspace_id.clone(),
            repo_root: "/repo/herdr".into(),
            path: checkout.clone(),
            error: None,
            removing: true,
            force_confirmation: true,
        });
        app.state.workspaces.clear();

        app.handle_worktree_remove_finished(WorktreeRemoveResult {
            workspace_id: internal_workspace_id,
            path: checkout,
            workspace: Some(Box::new(workspace_snapshot.clone())),
            worktree: Some(Box::new(worktree_snapshot)),
            forced: true,
            api_request: None,
            result: Ok(()),
        });

        assert_eq!(
            event_kinds(&event_hub),
            vec![crate::api::schema::EventKind::WorktreeRemoved]
        );
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                crate::api::schema::EventData::WorktreeRemoved {
                    workspace_id,
                    workspace: Some(workspace),
                    worktree,
                    forced,
                } if workspace_id == &workspace_snapshot.workspace_id
                    && workspace.workspace_id == workspace_snapshot.workspace_id
                    && worktree.branch.as_deref() == Some("worktree/issue")
                    && *forced
            )
        }));
    }

    #[test]
    fn dirty_worktree_remove_retries_with_force_and_closes_workspace() {
        let repo = create_committed_repo("app-worktree-dirty-remove-repo");
        let checkout = unique_temp_path("app-worktree-dirty-remove-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/dirty-remove",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(checkout.join("README.md"), "dirty\n").unwrap();

        let event_hub = crate::api::EventHub::default();
        let mut app = app_for_worktree_tests_with_event_hub(event_hub.clone());
        app.state.workspaces = vec![crate::workspace::Workspace::test_new("issue")];
        let workspace_id = app.state.workspaces[0].id.clone();
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        app.state.active = Some(0);
        app.state.selected = 0;
        app.open_remove_linked_worktree_confirmation(0);

        app.start_worktree_remove();

        #[cfg(not(windows))]
        {
            let safe_event = wait_for_worktree_event(&mut app);
            match safe_event {
                AppEvent::WorktreeRemoveFinished(result) => {
                    let result = *result;
                    assert_eq!(result.workspace_id, workspace_id);
                    assert_eq!(result.path, checkout);
                    assert!(result.result.is_err());
                    app.handle_worktree_remove_finished(result);
                }
                other => panic!("unexpected event: {other:?}"),
            }
        }

        let remove = app.state.worktree_remove.as_ref().unwrap();
        assert!(!remove.removing);
        assert!(remove.force_confirmation);
        assert!(checkout.exists());

        app.start_worktree_remove();
        let force_event = wait_for_worktree_event(&mut app);
        match force_event {
            AppEvent::WorktreeRemoveFinished(result) => {
                let result = *result;
                assert_eq!(result.workspace_id, workspace_id);
                assert_eq!(result.path, checkout);
                assert_eq!(result.result, Ok(()));
                app.handle_worktree_remove_finished(result);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        assert!(!checkout.exists());
        assert!(app.state.worktree_remove.is_none());
        assert!(app.state.workspaces.is_empty());
        assert_eq!(
            event_kinds(&event_hub),
            vec![
                crate::api::schema::EventKind::WorkspaceClosed,
                crate::api::schema::EventKind::WorktreeRemoved,
            ]
        );
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                crate::api::schema::EventData::WorktreeRemoved { worktree, .. }
                    if worktree.branch.as_deref() == Some("worktree/dirty-remove")
                        && !worktree.is_detached
            )
        }));

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn worktree_remove_runtime_shutdown_policy_preserves_windows_safe_remove() {
        assert_eq!(
            App::should_shutdown_workspace_terminal_runtimes_for_worktree_remove(false),
            cfg!(windows)
        );
        assert!(App::should_shutdown_workspace_terminal_runtimes_for_worktree_remove(true));
    }
}
