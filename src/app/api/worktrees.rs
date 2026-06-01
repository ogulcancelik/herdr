use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, WorktreeCreateParams, WorktreeInfo,
    WorktreeListParams, WorktreeOpenParams, WorktreeRemoveParams, WorktreeSourceInfo,
};
use crate::app::App;

use super::responses::{encode_error, encode_success};

struct ApiFailure {
    code: &'static str,
    message: String,
}

impl ApiFailure {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

fn absolute_user_path(path: &str) -> Result<PathBuf, ApiFailure> {
    let path = crate::worktree::expand_tilde_path(path);
    if path.is_absolute() {
        Ok(path)
    } else {
        Err(ApiFailure::new(
            "invalid_request",
            "worktree path must be absolute",
        ))
    }
}

struct WorktreeSource {
    workspace_idx: Option<usize>,
    source_checkout_path: PathBuf,
    source_repo_root: PathBuf,
    repo_key: String,
    repo_name: String,
}

impl App {
    pub(super) fn handle_worktree_list(
        &mut self,
        id: String,
        params: WorktreeListParams,
    ) -> String {
        let source = match self.resolve_worktree_list_source(params.workspace_id, params.cwd) {
            Ok(source) => source,
            Err(err) => return encode_error(id, err.code, err.message),
        };
        let entries = match crate::worktree::list_existing_worktrees(&source.source_repo_root) {
            Ok(entries) => entries,
            Err(err) => return encode_error(id, "worktree_list_failed", err),
        };
        let worktrees = entries
            .into_iter()
            .map(|entry| self.worktree_info_for_entry(&source, entry))
            .collect();

        encode_success(
            id,
            ResponseResult::WorktreeList {
                source: self.worktree_source_info(&source),
                worktrees,
            },
        )
    }

    pub(super) fn handle_worktree_create(
        &mut self,
        id: String,
        params: WorktreeCreateParams,
    ) -> String {
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
            return encode_error(id, "invalid_request", "branch is required");
        }
        let base = params.base.unwrap_or_else(|| "HEAD".into());
        let mut source = match self.resolve_worktree_source(params.workspace_id, params.cwd) {
            Ok(source) => source,
            Err(err) => return encode_error(id, err.code, err.message),
        };
        let checkout_path = match params.path {
            Some(path) => match absolute_user_path(&path) {
                Ok(path) => path,
                Err(err) => return encode_error(id, err.code, err.message),
            },
            None => crate::worktree::default_checkout_path(
                &self.state.worktree_directory,
                &source.repo_name,
                &branch,
            ),
        };

        if let Some(parent_dir) = checkout_path.parent() {
            if let Err(err) = std::fs::create_dir_all(parent_dir) {
                return encode_error(id, "worktree_create_failed", err.to_string());
            }
        }

        let command = crate::worktree::build_worktree_add_new_branch_command(
            &source.source_checkout_path,
            &checkout_path,
            &branch,
            &base,
        );
        if let Err(err) = crate::worktree::run_worktree_command(&command) {
            return encode_error(id, "worktree_create_failed", err);
        }
        if let Err(err) = self.ensure_source_parent_membership(&mut source, true) {
            return encode_error(id, err.code, err.message);
        }

        let ws_idx = match self.create_workspace_with_options(checkout_path.clone(), params.focus) {
            Ok(ws_idx) => ws_idx,
            Err(err) => {
                return encode_error(
                    id,
                    "worktree_open_failed",
                    format!("created worktree but failed to open workspace: {err}"),
                );
            }
        };
        self.mark_worktree_membership(&source, ws_idx, checkout_path, true, false);
        if let Some(label) = params.label {
            if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                ws.set_custom_name(label);
            }
        }
        self.state.mark_session_dirty();
        self.emit_workspace_open_events(ws_idx);

        let worktree = self
            .worktree_info_for_checkout(&source, ws_idx)
            .expect("created worktree workspace should have worktree info");
        encode_success(
            id,
            ResponseResult::WorktreeCreated {
                workspace: self.workspace_info(ws_idx),
                tab: self
                    .tab_info(ws_idx, 0)
                    .expect("new worktree workspace should have an initial tab"),
                root_pane: self
                    .root_pane_info(ws_idx, 0)
                    .expect("new worktree workspace should have an initial root pane"),
                worktree,
            },
        )
    }

    pub(super) fn handle_worktree_open(
        &mut self,
        id: String,
        params: WorktreeOpenParams,
    ) -> String {
        if params.path.is_some() == params.branch.is_some() {
            return encode_error(
                id,
                "invalid_request",
                "exactly one of path or branch is required",
            );
        }
        let mut source = match self.resolve_worktree_source(params.workspace_id, params.cwd) {
            Ok(source) => source,
            Err(err) => return encode_error(id, err.code, err.message),
        };
        let entry = match self.find_worktree_entry(&source, params.path, params.branch) {
            Ok(entry) => entry,
            Err(err) => return encode_error(id, err.code, err.message),
        };
        if entry.is_bare || entry.is_prunable {
            return encode_error(id, "worktree_not_found", "worktree cannot be opened");
        }
        let canonical_path = crate::worktree::canonical_or_original(&entry.path);
        let canonical_source = crate::worktree::canonical_or_original(&source.source_checkout_path);
        let target_is_source = canonical_path == canonical_source;
        let already_open = self.open_workspace_idx_for_checkout(&canonical_path);
        let defer_source_created_event = target_is_source && already_open.is_none();
        let created_source_workspace =
            match self.ensure_source_parent_membership(&mut source, !defer_source_created_event) {
                Ok(created) => created,
                Err(err) => return encode_error(id, err.code, err.message),
            };
        let (ws_idx, created_workspace) = if let Some(ws_idx) = already_open {
            if params.focus {
                self.state.switch_workspace(ws_idx);
            }
            (ws_idx, false)
        } else if target_is_source {
            let ws_idx = source
                .workspace_idx
                .expect("source workspace should exist after membership ensure");
            if params.focus {
                self.state.switch_workspace(ws_idx);
            }
            (ws_idx, created_source_workspace)
        } else {
            match self.create_workspace_with_options(entry.path.clone(), params.focus) {
                Ok(ws_idx) => (ws_idx, true),
                Err(err) => return encode_error(id, "worktree_open_failed", err.to_string()),
            }
        };
        self.mark_worktree_membership(
            &source,
            ws_idx,
            entry.path.clone(),
            canonical_path != crate::worktree::canonical_or_original(&source.source_repo_root),
            !created_workspace,
        );
        if let Some(label) = params.label {
            let workspace_id = self.public_workspace_id(ws_idx);
            if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                ws.set_custom_name(label.clone());
                crate::logging::workspace_renamed(&ws.id);
            }
            if !created_workspace {
                self.emit_event(EventEnvelope {
                    event: EventKind::WorkspaceRenamed,
                    data: EventData::WorkspaceRenamed {
                        workspace_id,
                        label,
                    },
                });
            }
        }
        self.state.mark_session_dirty();
        if created_workspace {
            self.emit_workspace_open_events(ws_idx);
        }

        let tab_idx = self.state.workspaces[ws_idx].active_tab;
        encode_success(
            id,
            ResponseResult::WorktreeOpened {
                workspace: self.workspace_info(ws_idx),
                tab: self
                    .tab_info(ws_idx, tab_idx)
                    .expect("opened worktree workspace should have an active tab"),
                root_pane: self
                    .root_pane_info(ws_idx, tab_idx)
                    .expect("opened worktree workspace should have an active root pane"),
                worktree: self.worktree_info_for_entry(&source, entry),
                already_open: already_open.is_some(),
            },
        )
    }

    pub(super) fn handle_worktree_remove(
        &mut self,
        id: String,
        params: WorktreeRemoveParams,
    ) -> String {
        let Some(ws_idx) = self.parse_workspace_id(&params.workspace_id) else {
            return encode_error(
                id,
                "workspace_not_found",
                format!("workspace {} not found", params.workspace_id),
            );
        };
        let Some(space) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.worktree_space().cloned())
        else {
            return encode_error(
                id,
                "not_linked_worktree",
                "workspace is not a Herdr-managed worktree checkout",
            );
        };
        if !space.is_linked_worktree {
            return encode_error(
                id,
                "not_linked_worktree",
                "workspace is not a linked worktree checkout",
            );
        }

        let command = crate::worktree::build_worktree_remove_command(
            &space.repo_root,
            &space.checkout_path,
            params.force,
        );
        if let Err(err) = crate::worktree::run_worktree_command(&command) {
            let code = if !params.force && crate::worktree::is_dirty_worktree_remove_error(&err) {
                "dirty_worktree_requires_force"
            } else {
                "worktree_remove_failed"
            };
            return encode_error(id, code, err);
        }

        let workspace_id = self.public_workspace_id(ws_idx);
        let path = space.checkout_path.display().to_string();
        let still_same_linked_worktree = self.state.workspaces[ws_idx]
            .worktree_space()
            .is_some_and(|current| {
                current.is_linked_worktree && current.checkout_path == space.checkout_path
            });
        if still_same_linked_worktree {
            self.state.selected = ws_idx;
            self.state.close_selected_workspace();
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id: workspace_id.clone(),
                },
            });
        }

        encode_success(
            id,
            ResponseResult::WorktreeRemoved {
                workspace_id,
                path,
                forced: params.force,
            },
        )
    }

    fn resolve_worktree_source(
        &mut self,
        workspace_id: Option<String>,
        cwd: Option<String>,
    ) -> Result<WorktreeSource, ApiFailure> {
        if workspace_id.is_some() && cwd.is_some() {
            return Err(ApiFailure::new(
                "invalid_request",
                "only one of workspace_id or cwd may be supplied",
            ));
        }

        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return Err(ApiFailure::new(
                    "workspace_not_found",
                    format!("workspace {workspace_id} not found"),
                ));
            };
            return self.worktree_source_from_workspace(ws_idx);
        }

        if let Some(cwd) = cwd {
            let path = absolute_user_path(&cwd)?;
            let space = crate::workspace::git_space_metadata(&path).ok_or_else(|| {
                ApiFailure::new(
                    "not_git_worktree",
                    "Herdr worktree actions require a path inside a Git work tree",
                )
            })?;
            if space.is_linked_worktree {
                return Err(ApiFailure::new(
                    "linked_worktree_source",
                    "New and open worktree actions start from the repo parent workspace.",
                ));
            }
            let source = WorktreeSource {
                workspace_idx: self.find_parent_workspace_for_space(&space),
                source_checkout_path: space.repo_root.clone(),
                source_repo_root: space.repo_root,
                repo_key: space.key,
                repo_name: space.label,
            };
            return Ok(source);
        }

        let Some(ws_idx) = self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        }) else {
            return Err(ApiFailure::new(
                "invalid_request",
                "workspace_id or cwd is required when no workspace is active",
            ));
        };
        self.worktree_source_from_workspace(ws_idx)
    }

    fn resolve_worktree_list_source(
        &mut self,
        workspace_id: Option<String>,
        cwd: Option<String>,
    ) -> Result<WorktreeSource, ApiFailure> {
        if workspace_id.is_some() && cwd.is_some() {
            return Err(ApiFailure::new(
                "invalid_request",
                "only one of workspace_id or cwd may be supplied",
            ));
        }

        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                return Err(ApiFailure::new(
                    "workspace_not_found",
                    format!("workspace {workspace_id} not found"),
                ));
            };
            return self.worktree_list_source_from_workspace(ws_idx);
        }

        if let Some(cwd) = cwd {
            let path = absolute_user_path(&cwd)?;
            let space = crate::workspace::git_space_metadata(&path).ok_or_else(|| {
                ApiFailure::new(
                    "not_git_worktree",
                    "Herdr worktree actions require a path inside a Git work tree",
                )
            })?;
            let workspace_idx = self.list_source_workspace_idx_for_space(&space);
            return Ok(worktree_source_from_space(space, workspace_idx, true));
        }

        let Some(ws_idx) = self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        }) else {
            return Err(ApiFailure::new(
                "invalid_request",
                "workspace_id or cwd is required when no workspace is active",
            ));
        };
        self.worktree_list_source_from_workspace(ws_idx)
    }

    fn worktree_source_from_workspace(&self, ws_idx: usize) -> Result<WorktreeSource, ApiFailure> {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return Err(ApiFailure::new(
                "workspace_not_found",
                "workspace not found",
            ));
        };
        if let Some(membership) = ws.worktree_space() {
            if membership.is_linked_worktree {
                return Err(ApiFailure::new(
                    "linked_worktree_source",
                    "New and open worktree actions start from the repo parent workspace.",
                ));
            }
            return Ok(WorktreeSource {
                workspace_idx: Some(ws_idx),
                source_checkout_path: membership.checkout_path.clone(),
                source_repo_root: membership.repo_root.clone(),
                repo_key: membership.key.clone(),
                repo_name: membership.label.clone(),
            });
        }

        let git_space = ws.git_space().cloned().or_else(|| {
            ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                .as_deref()
                .and_then(crate::workspace::git_space_metadata)
        });
        let Some(space) = git_space else {
            return Err(ApiFailure::new(
                "not_git_worktree",
                "Herdr worktree actions require a workspace inside a Git work tree",
            ));
        };
        if space.is_linked_worktree {
            return Err(ApiFailure::new(
                "linked_worktree_source",
                "New and open worktree actions start from the repo parent workspace.",
            ));
        }
        Ok(WorktreeSource {
            workspace_idx: Some(ws_idx),
            source_checkout_path: space.repo_root.clone(),
            source_repo_root: space.repo_root,
            repo_key: space.key,
            repo_name: space.label,
        })
    }

    fn worktree_list_source_from_workspace(
        &self,
        ws_idx: usize,
    ) -> Result<WorktreeSource, ApiFailure> {
        let Some(ws) = self.state.workspaces.get(ws_idx) else {
            return Err(ApiFailure::new(
                "workspace_not_found",
                "workspace not found",
            ));
        };
        if let Some(membership) = ws.worktree_space() {
            let source_checkout_path = if membership.is_linked_worktree {
                membership.repo_root.clone()
            } else {
                membership.checkout_path.clone()
            };
            let workspace_idx = if membership.is_linked_worktree {
                self.open_workspace_idx_for_checkout(&membership.repo_root)
            } else {
                Some(ws_idx)
            };
            return Ok(WorktreeSource {
                workspace_idx,
                source_checkout_path,
                source_repo_root: membership.repo_root.clone(),
                repo_key: membership.key.clone(),
                repo_name: membership.label.clone(),
            });
        }

        let git_space = ws.git_space().cloned().or_else(|| {
            ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                .as_deref()
                .and_then(crate::workspace::git_space_metadata)
        });
        let Some(space) = git_space else {
            return Err(ApiFailure::new(
                "not_git_worktree",
                "Herdr worktree actions require a workspace inside a Git work tree",
            ));
        };
        let workspace_idx = if space.is_linked_worktree {
            self.list_source_workspace_idx_for_space(&space)
        } else {
            Some(ws_idx)
        };
        Ok(worktree_source_from_space(space, workspace_idx, true))
    }

    fn ensure_source_parent_membership(
        &mut self,
        source: &mut WorktreeSource,
        emit_created_event: bool,
    ) -> Result<bool, ApiFailure> {
        if source.workspace_idx.is_none() {
            source.workspace_idx = self.find_parent_workspace_by_key(&source.repo_key);
        }
        let mut created_parent = false;
        if source.workspace_idx.is_none() {
            let ws_idx = self
                .create_workspace_with_options(source.source_checkout_path.clone(), false)
                .map_err(|err| ApiFailure::new("worktree_open_failed", err.to_string()))?;
            source.workspace_idx = Some(ws_idx);
            created_parent = true;
        }
        if let Some(ws_idx) = source.workspace_idx {
            let membership =
                worktree_membership(source, source.source_checkout_path.clone(), false);
            self.set_worktree_membership(ws_idx, membership, !created_parent);
            if created_parent && emit_created_event {
                self.emit_workspace_open_events(ws_idx);
            }
        }
        Ok(created_parent)
    }

    fn find_parent_workspace_for_space(
        &self,
        space: &crate::workspace::GitSpaceMetadata,
    ) -> Option<usize> {
        self.find_parent_workspace_by_key(&space.key)
            .or_else(|| self.open_workspace_idx_for_checkout(&space.repo_root))
    }

    fn list_source_workspace_idx_for_space(
        &self,
        space: &crate::workspace::GitSpaceMetadata,
    ) -> Option<usize> {
        if space.is_linked_worktree {
            let parent_checkout = parent_checkout_path_for_space(space);
            self.open_workspace_idx_for_checkout(&parent_checkout)
        } else {
            self.find_parent_workspace_for_space(space)
        }
    }

    fn find_parent_workspace_by_key(&self, repo_key: &str) -> Option<usize> {
        self.state.workspaces.iter().position(|ws| {
            ws.worktree_space()
                .is_some_and(|space| space.key == repo_key && !space.is_linked_worktree)
                || ws
                    .git_space()
                    .is_some_and(|space| space.key == repo_key && !space.is_linked_worktree)
        })
    }

    fn mark_worktree_membership(
        &mut self,
        source: &WorktreeSource,
        target_ws_idx: usize,
        target_path: PathBuf,
        target_is_linked_worktree: bool,
        emit_update: bool,
    ) {
        let membership = worktree_membership(source, target_path, target_is_linked_worktree);
        self.set_worktree_membership(target_ws_idx, membership, emit_update);
    }

    fn set_worktree_membership(
        &mut self,
        ws_idx: usize,
        membership: crate::workspace::WorktreeSpaceMembership,
        emit_update: bool,
    ) {
        let changed = if let Some(workspace) = self.state.workspaces.get_mut(ws_idx) {
            if workspace.worktree_space.as_ref() == Some(&membership) {
                false
            } else {
                workspace.worktree_space = Some(membership);
                true
            }
        } else {
            false
        };
        if changed {
            self.state.mark_session_dirty();
            if emit_update {
                self.emit_workspace_updated(ws_idx);
            }
        }
    }

    fn find_worktree_entry(
        &self,
        source: &WorktreeSource,
        path: Option<String>,
        branch: Option<String>,
    ) -> Result<crate::worktree::ExistingWorktree, ApiFailure> {
        let entries = crate::worktree::list_existing_worktrees(&source.source_repo_root)
            .map_err(|err| ApiFailure::new("worktree_list_failed", err))?;
        if let Some(path) = path {
            let expected = absolute_user_path(&path)?;
            let expected = crate::worktree::canonical_or_original(&expected);
            entries
                .into_iter()
                .find(|entry| crate::worktree::canonical_or_original(&entry.path) == expected)
                .ok_or_else(|| ApiFailure::new("worktree_not_found", "worktree path not found"))
        } else if let Some(branch) = branch {
            let matches = entries
                .into_iter()
                .filter(|entry| {
                    !entry.is_bare
                        && !entry.is_prunable
                        && !entry.is_detached
                        && entry.branch.as_deref() == Some(branch.as_str())
                })
                .collect::<Vec<_>>();
            match matches.len() {
                0 => Err(ApiFailure::new(
                    "worktree_not_found",
                    "worktree branch not found",
                )),
                1 => Ok(matches.into_iter().next().expect("one match should exist")),
                _ => Err(ApiFailure::new(
                    "ambiguous_worktree_branch",
                    "multiple worktrees matched branch",
                )),
            }
        } else {
            Err(ApiFailure::new(
                "invalid_request",
                "exactly one of path or branch is required",
            ))
        }
    }

    fn worktree_source_info(&self, source: &WorktreeSource) -> WorktreeSourceInfo {
        WorktreeSourceInfo {
            repo_key: source.repo_key.clone(),
            repo_name: source.repo_name.clone(),
            repo_root: source.source_repo_root.display().to_string(),
            source_checkout_path: source.source_checkout_path.display().to_string(),
            source_workspace_id: source
                .workspace_idx
                .map(|idx| self.public_workspace_id(idx)),
        }
    }

    fn worktree_info_for_entry(
        &self,
        source: &WorktreeSource,
        entry: crate::worktree::ExistingWorktree,
    ) -> WorktreeInfo {
        let canonical_path = crate::worktree::canonical_or_original(&entry.path);
        let repo_root = crate::worktree::canonical_or_original(&source.source_repo_root);
        WorktreeInfo {
            path: entry.path.display().to_string(),
            branch: entry.branch,
            is_bare: entry.is_bare,
            is_detached: entry.is_detached,
            is_prunable: entry.is_prunable,
            is_linked_worktree: canonical_path != repo_root,
            open_workspace_id: self
                .open_workspace_idx_for_checkout(&canonical_path)
                .map(|idx| self.public_workspace_id(idx)),
            label: source.repo_name.clone(),
        }
    }

    fn worktree_info_for_checkout(
        &self,
        source: &WorktreeSource,
        ws_idx: usize,
    ) -> Option<WorktreeInfo> {
        let membership = self.state.workspaces.get(ws_idx)?.worktree_space()?;
        let branch = crate::workspace::git_branch(&membership.checkout_path);
        let is_detached = branch.is_none();
        Some(WorktreeInfo {
            path: membership.checkout_path.display().to_string(),
            branch,
            is_bare: false,
            is_detached,
            is_prunable: false,
            is_linked_worktree: membership.is_linked_worktree,
            open_workspace_id: Some(self.public_workspace_id(ws_idx)),
            label: source.repo_name.clone(),
        })
    }

    fn open_workspace_idx_for_checkout(&self, checkout_path: &Path) -> Option<usize> {
        let canonical_checkout = crate::worktree::canonical_or_original(checkout_path);
        let checkout_key = canonical_checkout.display().to_string();
        self.state.workspaces.iter().position(|ws| {
            if ws.worktree_space().is_some_and(|space| {
                crate::worktree::canonical_or_original(&space.checkout_path) == canonical_checkout
            }) {
                return true;
            }

            let git_space = ws.git_space().cloned().or_else(|| {
                ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                    .as_deref()
                    .and_then(crate::workspace::git_space_metadata)
            });
            if git_space
                .as_ref()
                .is_some_and(|metadata| metadata.checkout_key == checkout_key)
            {
                return true;
            }

            ws.resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
                .as_deref()
                .is_some_and(|cwd| {
                    crate::worktree::canonical_or_original(cwd) == canonical_checkout
                })
        })
    }

    fn emit_workspace_open_events(&self, ws_idx: usize) {
        let workspace_info = self.workspace_info(ws_idx);
        let Some(tab) = self.tab_info(ws_idx, 0) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, 0) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceCreated,
            data: EventData::WorkspaceCreated {
                workspace: workspace_info,
            },
        });
        self.emit_event(EventEnvelope {
            event: EventKind::TabCreated,
            data: EventData::TabCreated { tab },
        });
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: root_pane },
        });
    }

    fn emit_workspace_updated(&self, ws_idx: usize) {
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceUpdated,
            data: EventData::WorkspaceUpdated {
                workspace: self.workspace_info(ws_idx),
            },
        });
    }
}

fn worktree_source_from_space(
    space: crate::workspace::GitSpaceMetadata,
    workspace_idx: Option<usize>,
    allow_linked: bool,
) -> WorktreeSource {
    let source_checkout_path = if allow_linked {
        parent_checkout_path_for_space(&space)
    } else {
        space.repo_root.clone()
    };
    WorktreeSource {
        workspace_idx,
        source_checkout_path: source_checkout_path.clone(),
        source_repo_root: source_checkout_path,
        repo_key: space.key,
        repo_name: space.label,
    }
}

fn parent_checkout_path_for_space(space: &crate::workspace::GitSpaceMetadata) -> PathBuf {
    if !space.is_linked_worktree {
        return space.repo_root.clone();
    }

    crate::worktree::list_existing_worktrees(&space.repo_root)
        .ok()
        .and_then(|entries| {
            entries.into_iter().find_map(|entry| {
                let entry_space = crate::workspace::git_space_metadata(&entry.path)?;
                if entry_space.key == space.key && !entry_space.is_linked_worktree {
                    Some(entry_space.repo_root)
                } else {
                    None
                }
            })
        })
        .unwrap_or_else(|| space.repo_root.clone())
}

fn worktree_membership(
    source: &WorktreeSource,
    checkout_path: PathBuf,
    is_linked_worktree: bool,
) -> crate::workspace::WorktreeSpaceMembership {
    crate::workspace::WorktreeSpaceMembership {
        key: source.repo_key.clone(),
        label: source.repo_name.clone(),
        repo_root: source.source_repo_root.clone(),
        checkout_path,
        is_linked_worktree,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{ErrorResponse, Request, SuccessResponse};
    use crate::{config::Config, workspace::Workspace};

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("herdr-{name}-{}-{nanos}", std::process::id()))
    }

    fn run_git(repo: &Path, args: &[&str]) {
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

    fn create_committed_repo(name: &str) -> PathBuf {
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

    fn test_app() -> App {
        test_app_with_event_hub(crate::api::EventHub::default())
    }

    fn test_app_with_event_hub(event_hub: crate::api::EventHub) -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        App::new(&Config::default(), true, None, api_rx, event_hub)
    }

    fn app_with_parent(repo: &Path) -> App {
        let mut app = test_app();
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.to_path_buf();
        app.state.workspaces = vec![parent];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app
    }

    #[tokio::test]
    async fn api_worktree_create_opens_workspace_and_marks_membership() {
        let repo = create_committed_repo("api-worktree-create-repo");
        let worktree_root = unique_temp_path("api-worktree-create-root");
        let mut app = app_with_parent(&repo);
        app.state.worktree_directory = worktree_root.clone();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                workspace_id: Some(app.state.workspaces[0].id.clone()),
                branch: Some("worktree/api-create".into()),
                ..WorktreeCreateParams::default()
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeCreated {
            workspace,
            tab,
            root_pane,
            worktree,
        } = success.result
        else {
            panic!("expected worktree_created response");
        };
        assert_eq!(tab.workspace_id, workspace.workspace_id);
        assert_eq!(root_pane.workspace_id, workspace.workspace_id);
        assert_eq!(worktree.branch.as_deref(), Some("worktree/api-create"));
        assert!(Path::new(&worktree.path).join("README.md").exists());
        assert_eq!(app.state.workspaces.len(), 2);
        assert!(
            !app.state.workspaces[0]
                .worktree_space()
                .unwrap()
                .is_linked_worktree
        );
        assert!(
            app.state.workspaces[1]
                .worktree_space()
                .unwrap()
                .is_linked_worktree
        );
        assert!(workspace.worktree.unwrap().is_linked_worktree);

        let remove =
            crate::worktree::build_worktree_remove_command(&repo, Path::new(&worktree.path), false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn api_worktree_create_from_cwd_emits_parent_with_membership() {
        let repo = create_committed_repo("api-worktree-create-cwd-repo");
        let worktree_root = unique_temp_path("api-worktree-create-cwd-root");
        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        app.state.worktree_directory = worktree_root.clone();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                cwd: Some(repo.display().to_string()),
                branch: Some("worktree/api-create-cwd".into()),
                ..WorktreeCreateParams::default()
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeCreated { worktree, .. } = success.result else {
            panic!("expected worktree_created response");
        };

        let events = event_hub.events_after(0);
        let parent_created = events
            .iter()
            .filter_map(|(_, event)| match &event.data {
                EventData::WorkspaceCreated { workspace } => Some(workspace),
                _ => None,
            })
            .find(|workspace| {
                workspace
                    .worktree
                    .as_ref()
                    .is_some_and(|worktree| !worktree.is_linked_worktree)
            });
        assert!(
            parent_created.is_some(),
            "auto-created parent workspace event should include parent worktree membership"
        );

        let remove =
            crate::worktree::build_worktree_remove_command(&repo, Path::new(&worktree.path), false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn invalid_worktree_create_from_cwd_does_not_create_parent_workspace() {
        let repo = create_committed_repo("api-worktree-create-invalid-cwd-repo");
        let mut app = test_app();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                cwd: Some(repo.display().to_string()),
                branch: Some("   ".into()),
                ..WorktreeCreateParams::default()
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_request");
        assert!(app.state.workspaces.is_empty());
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn invalid_worktree_open_from_cwd_does_not_create_parent_workspace() {
        let repo = create_committed_repo("api-worktree-open-invalid-cwd-repo");
        let mut app = test_app();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeOpen(WorktreeOpenParams {
                cwd: Some(repo.display().to_string()),
                path: Some("/tmp/one".into()),
                branch: Some("worktree/one".into()),
                ..WorktreeOpenParams::default()
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_request");
        assert!(app.state.workspaces.is_empty());
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn raw_api_worktree_create_rejects_relative_path_override() {
        let repo = create_committed_repo("api-worktree-relative-path-repo");
        let mut app = app_with_parent(&repo);

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                workspace_id: Some(app.state.workspaces[0].id.clone()),
                branch: Some("worktree/relative".into()),
                path: Some("relative-checkout".into()),
                ..WorktreeCreateParams::default()
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_request");
        assert_eq!(app.state.workspaces.len(), 1);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn raw_api_worktree_create_rejects_relative_cwd() {
        let mut app = test_app();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                cwd: Some("relative-repo".into()),
                branch: Some("worktree/relative-cwd".into()),
                ..WorktreeCreateParams::default()
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_request");
        assert!(app.state.workspaces.is_empty());
    }

    #[test]
    fn api_worktree_open_reuses_already_open_checkout_from_subdirectory() {
        let repo = create_committed_repo("api-worktree-open-repo");
        let checkout = unique_temp_path("api-worktree-open-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-open",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        let subdir = checkout.join("nested");
        std::fs::create_dir_all(&subdir).unwrap();

        let mut app = app_with_parent(&repo);
        let mut child = Workspace::test_new("child");
        child.identity_cwd = subdir;
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeOpen(WorktreeOpenParams {
                workspace_id: Some(app.state.workspaces[0].id.clone()),
                branch: Some("worktree/api-open".into()),
                focus: true,
                ..WorktreeOpenParams::default()
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeOpened {
            workspace,
            already_open,
            ..
        } = success.result
        else {
            panic!("expected worktree_opened response");
        };
        assert!(already_open);
        assert_eq!(app.state.workspaces.len(), 2);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(workspace.workspace_id, app.state.workspaces[1].id);
        assert!(
            app.state.workspaces[1]
                .worktree_space()
                .unwrap()
                .is_linked_worktree
        );

        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn api_worktree_open_label_on_already_open_checkout_emits_rename_event() {
        let repo = create_committed_repo("api-worktree-open-label-repo");
        let checkout = unique_temp_path("api-worktree-open-label-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-open-label",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.clone();
        app.state.workspaces = vec![parent];
        let mut child = Workspace::test_new("child");
        child.identity_cwd = checkout.clone();
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeOpen(WorktreeOpenParams {
                workspace_id: Some(app.state.workspaces[0].id.clone()),
                branch: Some("worktree/api-open-label".into()),
                label: Some("review".into()),
                ..WorktreeOpenParams::default()
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeOpened {
            workspace,
            already_open,
            ..
        } = success.result
        else {
            panic!("expected worktree_opened response");
        };
        assert!(already_open);
        assert_eq!(workspace.workspace_id, child_id);
        assert_eq!(workspace.label, "review");
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceUpdated { workspace }
                    if workspace.workspace_id == child_id
                        && workspace
                            .worktree
                            .as_ref()
                            .is_some_and(|worktree| worktree.is_linked_worktree)
            )
        }));
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceRenamed {
                    workspace_id,
                    label
                } if workspace_id == &child_id && label == "review"
            )
        }));

        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn api_worktree_open_source_checkout_created_by_request_is_not_already_open() {
        let repo = create_committed_repo("api-worktree-open-source-repo");
        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        app.state.default_shell = "/usr/bin/true".into();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeOpen(WorktreeOpenParams {
                cwd: Some(repo.display().to_string()),
                path: Some(repo.display().to_string()),
                label: Some("source checkout".into()),
                ..WorktreeOpenParams::default()
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap_or_else(|err| {
            panic!("expected success response, got {response}: {err}");
        });
        let ResponseResult::WorktreeOpened {
            workspace,
            already_open,
            ..
        } = success.result
        else {
            panic!("expected worktree_opened response");
        };
        assert!(!already_open);
        assert_eq!(workspace.label, "source checkout");
        assert_eq!(app.state.workspaces.len(), 1);
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceCreated { workspace }
                    if workspace.label == "source checkout"
                        && workspace
                            .worktree
                            .as_ref()
                            .is_some_and(|worktree| !worktree.is_linked_worktree)
            )
        }));
        assert!(!event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| { matches!(&event.data, EventData::WorkspaceRenamed { .. }) }));

        app.state.selected = 0;
        app.state.close_selected_workspace();
        app.shutdown_detached_terminal_runtimes();
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn api_worktree_list_reports_open_workspace_ids() {
        let repo = create_committed_repo("api-worktree-list-repo");
        let checkout = unique_temp_path("api-worktree-list-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-list",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        let mut app = app_with_parent(&repo);
        let mut child = Workspace::test_new("child");
        child.identity_cwd = checkout.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeList(WorktreeListParams {
                workspace_id: Some(app.state.workspaces[0].id.clone()),
                cwd: None,
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeList { worktrees, .. } = success.result else {
            panic!("expected worktree_list response");
        };
        let entry = worktrees
            .iter()
            .find(|entry| entry.branch.as_deref() == Some("worktree/api-list"))
            .unwrap();
        assert_eq!(
            entry.open_workspace_id.as_deref(),
            Some(app.state.workspaces[1].id.as_str())
        );
        assert!(entry.is_linked_worktree);

        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn api_worktree_list_accepts_linked_checkout_sources() {
        let repo = create_committed_repo("api-worktree-list-linked-repo");
        let checkout = unique_temp_path("api-worktree-list-linked-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-list-linked",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        let mut app = app_with_parent(&repo);
        let parent_id = app.state.workspaces[0].id.clone();
        let mut child = Workspace::test_new("child");
        child.identity_cwd = checkout.clone();
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();

        for method in [
            crate::api::schema::Method::WorktreeList(WorktreeListParams {
                workspace_id: Some(child_id),
                cwd: None,
            }),
            crate::api::schema::Method::WorktreeList(WorktreeListParams {
                workspace_id: None,
                cwd: Some(checkout.display().to_string()),
            }),
        ] {
            let response = app.handle_api_request(Request {
                id: "req".into(),
                method,
            });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::WorktreeList { source, worktrees } = success.result else {
                panic!("expected worktree_list response");
            };
            assert_eq!(
                crate::worktree::canonical_or_original(std::path::Path::new(&source.repo_root)),
                crate::worktree::canonical_or_original(&repo)
            );
            assert_eq!(
                source.source_workspace_id.as_deref(),
                Some(parent_id.as_str())
            );
            assert!(worktrees.iter().any(|entry| {
                entry.branch.as_deref() == Some("worktree/api-list-linked")
                    && entry.is_linked_worktree
            }));
        }

        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn api_worktree_list_preserves_prunable_entries() {
        let repo = create_committed_repo("api-worktree-list-prunable-repo");
        let checkout = unique_temp_path("api-worktree-list-prunable-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-list-prunable",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::remove_dir_all(&checkout).unwrap();
        let mut app = app_with_parent(&repo);

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeList(WorktreeListParams {
                workspace_id: Some(app.state.workspaces[0].id.clone()),
                cwd: None,
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeList { worktrees, .. } = success.result else {
            panic!("expected worktree_list response");
        };
        let entry = worktrees
            .iter()
            .find(|entry| entry.branch.as_deref() == Some("worktree/api-list-prunable"))
            .unwrap();
        assert!(entry.is_prunable);
        assert!(entry.is_linked_worktree);

        run_git(&repo, &["worktree", "prune"]);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn api_worktree_remove_requires_force_for_dirty_checkout() {
        let repo = create_committed_repo("api-worktree-remove-repo");
        let checkout = unique_temp_path("api-worktree-remove-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-remove",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );
        std::fs::write(checkout.join("README.md"), "dirty\n").unwrap();

        let mut app = app_with_parent(&repo);
        let mut child = Workspace::test_new("child");
        child.identity_cwd = checkout.clone();
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: crate::workspace::git_space_metadata(&repo).unwrap().key,
            label: "api-worktree-remove-repo".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                workspace_id: child_id.clone(),
                force: false,
            }),
        });
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "dirty_worktree_requires_force");
        assert!(checkout.exists());
        assert_eq!(app.state.workspaces.len(), 2);

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                workspace_id: child_id,
                force: true,
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeRemoved { forced, path, .. } = success.result else {
            panic!("expected worktree_removed response");
        };
        assert!(forced);
        assert_eq!(path, checkout.display().to_string());
        assert!(!checkout.exists());
        assert_eq!(app.state.workspaces.len(), 1);

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn api_worktree_remove_emits_close_event_and_drains_runtime_shutdowns() {
        let repo = create_committed_repo("api-worktree-remove-event-repo");
        let checkout = unique_temp_path("api-worktree-remove-event-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-remove-event",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        let mut child = Workspace::test_new("child");
        child.identity_cwd = checkout.clone();
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: crate::workspace::git_space_metadata(&repo).unwrap().key,
            label: "api-worktree-remove-event-repo".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;

        let response = app.handle_api_request(Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                workspace_id: child_id.clone(),
                force: false,
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::WorktreeRemoved { .. }
        ));
        assert!(app.state.workspaces.is_empty());
        assert!(app.state.terminal_runtime_shutdowns.is_empty());
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorkspaceClosed { workspace_id } if workspace_id == &child_id
            )
        }));

        let _ = std::fs::remove_dir_all(repo);
    }
}
