use std::path::{Path, PathBuf};

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, ResponseResult, WorktreeInfo, WorktreeListParams,
    WorktreeOpenParams, WorktreeSourceInfo,
};
use crate::app::App;

use super::responses::{encode_error, encode_success};

mod deferred;

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
        let worktree = self.worktree_info_for_entry(&source, entry);
        self.emit_worktree_opened_event(ws_idx, worktree.clone(), already_open.is_some());
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
                worktree,
                already_open: already_open.is_some(),
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

    pub(crate) fn set_worktree_membership(
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

    pub(crate) fn worktree_info_for_membership(
        &self,
        membership: &crate::workspace::WorktreeSpaceMembership,
        open_workspace_id: Option<String>,
    ) -> WorktreeInfo {
        let branch = crate::workspace::git_branch(&membership.checkout_path);
        let is_detached = branch.is_none();
        WorktreeInfo {
            path: membership.checkout_path.display().to_string(),
            branch,
            is_bare: false,
            is_detached,
            is_prunable: false,
            is_linked_worktree: membership.is_linked_worktree,
            open_workspace_id,
            label: membership.label.clone(),
        }
    }

    pub(crate) fn open_workspace_idx_for_checkout(&self, checkout_path: &Path) -> Option<usize> {
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

    pub(crate) fn worktree_info_for_workspace(&self, ws_idx: usize) -> Option<WorktreeInfo> {
        let membership = self.state.workspaces.get(ws_idx)?.worktree_space()?;
        Some(self.worktree_info_for_membership(membership, Some(self.public_workspace_id(ws_idx))))
    }

    pub(crate) fn emit_worktree_created_event(&mut self, ws_idx: usize, worktree: WorktreeInfo) {
        self.emit_event(EventEnvelope {
            event: EventKind::WorktreeCreated,
            data: EventData::WorktreeCreated {
                workspace: self.workspace_info(ws_idx),
                worktree,
            },
        });
    }

    #[cfg(test)]
    pub(crate) fn emit_worktree_opened_for_workspace(&mut self, ws_idx: usize, already_open: bool) {
        let Some(worktree) = self.worktree_info_for_workspace(ws_idx) else {
            return;
        };
        self.emit_worktree_opened_event(ws_idx, worktree, already_open);
    }

    fn emit_worktree_opened_event(
        &mut self,
        ws_idx: usize,
        worktree: WorktreeInfo,
        already_open: bool,
    ) {
        self.emit_event(EventEnvelope {
            event: EventKind::WorktreeOpened,
            data: EventData::WorktreeOpened {
                workspace: self.workspace_info(ws_idx),
                worktree,
                already_open,
            },
        });
    }

    pub(crate) fn emit_worktree_removed_event(
        &mut self,
        workspace_id: String,
        workspace: Option<crate::api::schema::WorkspaceInfo>,
        worktree: WorktreeInfo,
        forced: bool,
    ) {
        self.emit_event(EventEnvelope {
            event: EventKind::WorktreeRemoved,
            data: EventData::WorktreeRemoved {
                workspace_id,
                workspace,
                worktree,
                forced,
            },
        });
    }

    fn emit_workspace_updated(&mut self, ws_idx: usize) {
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
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::api::schema::{
        ErrorResponse, Request, SuccessResponse, WorktreeCreateParams, WorktreeRemoveParams,
    };
    use crate::events::{
        ApiWorktreeAddRequest, ApiWorktreeRemoveRequest, AppEvent, WorktreeAddResult,
        WorktreeRemoveResult,
    };
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

    #[cfg(windows)]
    fn test_shell() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn test_shell() -> &'static str {
        "/usr/bin/true"
    }

    fn app_with_parent(repo: &Path) -> App {
        let mut app = test_app();
        app.state.default_shell = test_shell().into();
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.to_path_buf();
        app.state.workspaces = vec![parent];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app
    }

    fn wait_for_app_event(app: &mut App) -> AppEvent {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if let Ok(event) = app.event_rx.try_recv() {
                return event;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "timed out waiting for app event"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn install_event_plugin(app: &mut App, name: &str, event: &str) -> PathBuf {
        let plugin_root = unique_temp_path(name);
        std::fs::create_dir_all(&plugin_root).unwrap();
        let manifest_path = plugin_root.join("herdr-plugin.toml");
        std::fs::write(&manifest_path, format!("id = 'example.{name}'\n")).unwrap();
        app.state.installed_plugins.insert(
            format!("example.{name}"),
            crate::api::schema::InstalledPluginInfo {
                plugin_id: format!("example.{name}"),
                name: name.into(),
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
                    on: event.into(),
                    platforms: None,
                    command: vec!["sh".into(), "-c".into(), "true".into()],
                }],
                panes: Vec::new(),
                link_handlers: Vec::new(),
                source: crate::api::schema::PluginSourceInfo::default(),
                warnings: Vec::new(),
            },
        );
        plugin_root
    }

    fn response_channel() -> (
        std::sync::mpsc::Sender<String>,
        std::sync::mpsc::Receiver<String>,
    ) {
        std::sync::mpsc::channel()
    }

    fn run_deferred_api_request(app: &mut App, request: Request) -> String {
        let (respond_to, response_rx) = response_channel();
        assert!(app.handle_deferred_worktree_api_request(request, respond_to));
        if let Ok(response) = response_rx.recv_timeout(std::time::Duration::from_millis(50)) {
            return response;
        }

        let event = wait_for_app_event(app);
        app.handle_internal_event(event);
        response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("deferred API request should respond after completion event")
    }

    #[tokio::test]
    async fn api_worktree_create_opens_workspace_and_marks_membership() {
        let repo = create_committed_repo("api-worktree-create-repo");
        let worktree_root = unique_temp_path("api-worktree-create-root");
        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.clone();
        app.state.workspaces = vec![parent];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.worktree_directory = worktree_root.clone();
        let workspace_id = app.state.workspaces[0].id.clone();

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                    workspace_id: Some(workspace_id),
                    branch: Some("worktree/api-create".into()),
                    ..WorktreeCreateParams::default()
                }),
            },
        );

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
        let events = event_hub.events_after(0);
        assert!(events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorktreeCreated {
                    workspace: event_workspace,
                    worktree: event_worktree,
                } if event_workspace.workspace_id == workspace.workspace_id
                    && event_worktree.branch.as_deref() == Some("worktree/api-create")
                    && event_worktree.is_linked_worktree
            )
        }));
        let kinds = events
            .iter()
            .map(|(_, event)| event.event)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds
                .iter()
                .filter(|event| **event == EventKind::WorktreeCreated)
                .count(),
            1
        );
        assert_eq!(
            &kinds[kinds.len() - 4..],
            &[
                EventKind::WorkspaceCreated,
                EventKind::TabCreated,
                EventKind::PaneCreated,
                EventKind::WorktreeCreated,
            ]
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let remove =
            crate::worktree::build_worktree_remove_command(&repo, Path::new(&worktree.path), false);
        crate::worktree::run_worktree_command(&remove).unwrap();
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn deferred_api_worktree_create_preserves_event_and_plugin_context() {
        let repo = create_committed_repo("api-worktree-create-deferred-repo");
        let worktree_root = unique_temp_path("api-worktree-create-deferred-root");
        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.clone();
        app.state.workspaces = vec![parent];
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.worktree_directory = worktree_root.clone();
        let plugin_root = install_event_plugin(&mut app, "deferred-create", "worktree.created");
        let (respond_to, response_rx) = response_channel();

        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                    workspace_id: Some(app.state.workspaces[0].id.clone()),
                    branch: Some("worktree/api-create-deferred".into()),
                    ..WorktreeCreateParams::default()
                }),
            },
            respond_to,
        ));
        assert!(response_rx.try_recv().is_err());

        let event = wait_for_app_event(&mut app);
        app.handle_internal_event(event);
        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("deferred worktree create should respond after completion event");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeCreated {
            workspace,
            worktree,
            ..
        } = success.result
        else {
            panic!("expected worktree_created response");
        };
        let event_kinds = event_hub
            .events_after(0)
            .into_iter()
            .map(|(_, event)| event.event)
            .collect::<Vec<_>>();
        assert_eq!(
            &event_kinds[event_kinds.len() - 4..],
            &[
                EventKind::WorkspaceCreated,
                EventKind::TabCreated,
                EventKind::PaneCreated,
                EventKind::WorktreeCreated,
            ]
        );
        assert_eq!(
            workspace
                .worktree
                .as_ref()
                .map(|worktree| worktree.checkout_path.as_str()),
            Some(worktree.path.as_str())
        );
        assert!(app.state.plugin_command_logs.iter().any(|log| {
            log.event.as_deref() == Some("worktree.created")
                && log.status == crate::api::schema::PluginCommandStatus::Running
        }));

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(plugin_root);
    }

    #[tokio::test]
    async fn deferred_api_worktree_create_checks_out_existing_branch() {
        let repo = create_committed_repo("api-worktree-create-existing-branch-repo");
        let worktree_root = unique_temp_path("api-worktree-create-existing-branch-root");
        let branch = "foo";
        run_git(&repo, &["branch", branch]);
        let mut app = test_app();
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.clone();
        let parent_id = parent.id.clone();
        app.state.workspaces = vec![parent];
        app.state.ensure_test_terminals();
        app.state.worktree_directory = worktree_root.clone();
        let request = || Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                workspace_id: Some(parent_id.clone()),
                branch: Some(branch.into()),
                ..WorktreeCreateParams::default()
            }),
        };

        let (response_tx, response_rx) = response_channel();
        assert!(app.handle_deferred_worktree_api_request(request(), response_tx));
        let event = wait_for_app_event(&mut app);
        app.handle_internal_event(event);
        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("create should respond");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::WorktreeCreated { worktree, .. } = success.result else {
            panic!("expected worktree_created response");
        };
        assert_eq!(worktree.branch.as_deref(), Some(branch));
        let checkout = Path::new(&worktree.path);
        assert!(checkout.join("README.md").exists());
        let branch_name = std::process::Command::new("git")
            .arg("-C")
            .arg(checkout)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert!(branch_name.status.success());
        assert_eq!(
            String::from_utf8(branch_name.stdout).unwrap().trim(),
            branch
        );
        assert!(app.pending_api_worktree_creates.is_empty());

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn deferred_api_worktree_create_failure_clears_pending_checkout() {
        let repo = create_committed_repo("api-worktree-create-failure-repo");
        let worktree_root = unique_temp_path("api-worktree-create-failure-root");
        let branch_name = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(["branch", "--show-current"])
            .output()
            .unwrap();
        assert!(branch_name.status.success());
        let branch = String::from_utf8(branch_name.stdout)
            .unwrap()
            .trim()
            .to_string();
        let mut app = test_app();
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.clone();
        let parent_id = parent.id.clone();
        app.state.workspaces = vec![parent];
        app.state.ensure_test_terminals();
        app.state.worktree_directory = worktree_root.clone();
        let request = || Request {
            id: "req".into(),
            method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                workspace_id: Some(parent_id.clone()),
                branch: Some(branch.clone()),
                ..WorktreeCreateParams::default()
            }),
        };

        let (first_tx, first_rx) = response_channel();
        assert!(app.handle_deferred_worktree_api_request(request(), first_tx));
        let event = wait_for_app_event(&mut app);
        app.handle_internal_event(event);
        let response = first_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("failed create should respond");
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "worktree_create_failed");
        assert!(app.pending_api_worktree_creates.is_empty());

        let (second_tx, second_rx) = response_channel();
        assert!(app.handle_deferred_worktree_api_request(request(), second_tx));
        assert!(second_rx.try_recv().is_err());
        let event = wait_for_app_event(&mut app);
        app.handle_internal_event(event);
        let response = second_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("retry should reach git instead of pending guard");
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "worktree_create_failed");
        assert_ne!(error.error.code, "worktree_operation_in_progress");

        let _ = std::fs::remove_dir_all(worktree_root);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn deferred_api_worktree_create_completes_after_source_workspace_changes() {
        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        let repo = create_committed_repo("api-worktree-create-changed-source-repo");
        let checkout = unique_temp_path("api-worktree-create-changed-source-checkout");
        std::fs::create_dir_all(&checkout).unwrap();
        let checkout_key = crate::worktree::canonical_or_original(&checkout);
        let mut source = Workspace::test_new("source");
        source.identity_cwd = repo.clone();
        let source_id = source.id.clone();
        app.state.workspaces = vec![source];
        app.pending_api_worktree_creates
            .insert(checkout_key.clone(), 9);
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "other-key".into(),
            label: "other".into(),
            repo_root: "/repo/other".into(),
            checkout_path: "/repo/other".into(),
            is_linked_worktree: false,
        });
        let (respond_to, response_rx) = response_channel();

        app.handle_api_worktree_add_finished(WorktreeAddResult {
            path: checkout.clone(),
            api_request: Some(ApiWorktreeAddRequest {
                id: "req".into(),
                operation_id: 9,
                checkout_key,
                source_workspace_id: Some(source_id),
                source_existing_membership: None,
                source_checkout_path: repo.clone(),
                source_repo_root: repo.clone(),
                repo_key: "repo-key".into(),
                repo_name: "herdr".into(),
                label: None,
                focus: false,
                respond_to,
            }),
            result: Ok(()),
        });

        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("changed-source create completion should respond");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::WorktreeCreated { .. }
        ));
        assert!(event_hub
            .events_after(0)
            .into_iter()
            .any(|(_, event)| event.event == EventKind::WorktreeCreated));
        assert_eq!(app.state.workspaces.len(), 3);
        assert_eq!(
            app.state.workspaces[0]
                .worktree_space()
                .map(|membership| membership.label.as_str()),
            Some("other")
        );

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
        let _ = std::fs::remove_dir_all(checkout);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[tokio::test]
    async fn api_worktree_create_from_cwd_emits_parent_with_membership() {
        let repo = create_committed_repo("api-worktree-create-cwd-repo");
        let worktree_root = unique_temp_path("api-worktree-create-cwd-root");
        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        app.state.worktree_directory = worktree_root.clone();
        app.state.default_shell = test_shell().into();

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                    cwd: Some(repo.display().to_string()),
                    branch: Some("worktree/api-create-cwd".into()),
                    ..WorktreeCreateParams::default()
                }),
            },
        );
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

        for (_, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
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

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                    cwd: Some(repo.display().to_string()),
                    branch: Some("   ".into()),
                    ..WorktreeCreateParams::default()
                }),
            },
        );

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
        let workspace_id = app.state.workspaces[0].id.clone();

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                    workspace_id: Some(workspace_id),
                    branch: Some("worktree/relative".into()),
                    path: Some("relative-checkout".into()),
                    ..WorktreeCreateParams::default()
                }),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_request");
        assert_eq!(app.state.workspaces.len(), 1);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn raw_api_worktree_create_rejects_relative_cwd() {
        let mut app = test_app();

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                    cwd: Some("relative-repo".into()),
                    branch: Some("worktree/relative-cwd".into()),
                    ..WorktreeCreateParams::default()
                }),
            },
        );

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

        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        let mut parent = Workspace::test_new("main");
        parent.identity_cwd = repo.clone();
        app.state.workspaces = vec![parent];
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
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorktreeOpened {
                    workspace: event_workspace,
                    worktree: event_worktree,
                    already_open,
                } if event_workspace.workspace_id == workspace.workspace_id
                    && event_worktree.branch.as_deref() == Some("worktree/api-open")
                    && event_worktree.is_linked_worktree
                    && *already_open
            )
        }));

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
        app.state.default_shell = test_shell().into();

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

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: child_id.clone(),
                    force: false,
                }),
            },
        );
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "dirty_worktree_requires_force");
        assert!(checkout.exists());
        assert_eq!(app.state.workspaces.len(), 2);

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: child_id,
                    force: true,
                }),
            },
        );
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

        let response = run_deferred_api_request(
            &mut app,
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: child_id.clone(),
                    force: false,
                }),
            },
        );
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
                EventData::WorkspaceClosed {
                    workspace_id,
                    workspace: Some(workspace),
                } if workspace_id == &child_id
                    && workspace
                        .worktree
                        .as_ref()
                        .is_some_and(|worktree| worktree.is_linked_worktree)
            )
        }));
        assert!(event_hub.events_after(0).iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::WorktreeRemoved {
                    workspace_id,
                    workspace: Some(workspace),
                    worktree,
                    forced,
                } if workspace_id == &child_id
                    && workspace.workspace_id == child_id
                    && worktree.branch.as_deref() == Some("worktree/api-remove-event")
                    && worktree.is_linked_worktree
                    && worktree.open_workspace_id.is_none()
                    && !forced
            )
        }));

        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn deferred_api_worktree_remove_preserves_event_and_plugin_context() {
        let repo = create_committed_repo("api-worktree-remove-deferred-repo");
        let checkout = unique_temp_path("api-worktree-remove-deferred-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-remove-deferred",
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
            label: "api-worktree-remove-deferred-repo".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();
        app.state.active = Some(0);
        app.state.selected = 0;
        let plugin_root = install_event_plugin(&mut app, "deferred-remove", "worktree.removed");
        let (respond_to, response_rx) = response_channel();

        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: child_id.clone(),
                    force: false,
                }),
            },
            respond_to,
        ));
        assert!(response_rx.try_recv().is_err());

        let event = wait_for_app_event(&mut app);
        app.handle_internal_event(event);
        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("deferred worktree remove should respond after completion event");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::WorktreeRemoved { .. }
        ));
        assert_eq!(
            event_hub
                .events_after(0)
                .into_iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            vec![EventKind::WorkspaceClosed, EventKind::WorktreeRemoved]
        );
        assert!(app.state.plugin_command_logs.iter().any(|log| {
            log.event.as_deref() == Some("worktree.removed")
                && log.status == crate::api::schema::PluginCommandStatus::Running
        }));
        assert!(app.state.workspaces.is_empty());

        let _ = std::fs::remove_dir_all(repo);
        let _ = std::fs::remove_dir_all(plugin_root);
    }

    #[test]
    fn deferred_api_worktree_remove_rejects_duplicate_in_flight_request() {
        let repo = create_committed_repo("api-worktree-remove-duplicate-repo");
        let checkout = unique_temp_path("api-worktree-remove-duplicate-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-remove-duplicate",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let mut app = test_app();
        let mut child = Workspace::test_new("child");
        child.identity_cwd = checkout.clone();
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: crate::workspace::git_space_metadata(&repo).unwrap().key,
            label: "api-worktree-remove-duplicate-repo".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();
        let (first_tx, _first_rx) = response_channel();
        let (second_tx, second_rx) = response_channel();

        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "first".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: child_id.clone(),
                    force: false,
                }),
            },
            first_tx,
        ));
        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "second".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: child_id,
                    force: false,
                }),
            },
            second_tx,
        ));
        let response = second_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("duplicate request should respond immediately");
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "worktree_operation_in_progress");

        let event = wait_for_app_event(&mut app);
        app.handle_internal_event(event);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn deferred_api_worktree_remove_rejects_duplicate_checkout_path_in_flight_request() {
        let repo = create_committed_repo("api-worktree-remove-duplicate-path-repo");
        let checkout = unique_temp_path("api-worktree-remove-duplicate-path-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-remove-duplicate-path",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let mut app = test_app();
        let membership = crate::workspace::WorktreeSpaceMembership {
            key: crate::workspace::git_space_metadata(&repo).unwrap().key,
            label: "api-worktree-remove-duplicate-path-repo".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        };
        let mut first = Workspace::test_new("first");
        first.identity_cwd = checkout.clone();
        first.worktree_space = Some(membership.clone());
        let first_id = first.id.clone();
        let mut second = Workspace::test_new("second");
        second.identity_cwd = checkout.clone();
        second.worktree_space = Some(membership);
        let second_id = second.id.clone();
        app.state.workspaces = vec![first, second];
        app.state.ensure_test_terminals();
        let (first_tx, _first_rx) = response_channel();
        let (second_tx, second_rx) = response_channel();

        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "first".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: first_id,
                    force: true,
                }),
            },
            first_tx,
        ));
        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "second".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: second_id,
                    force: true,
                }),
            },
            second_tx,
        ));
        let response = second_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("duplicate path request should respond immediately");
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "worktree_operation_in_progress");

        let event = wait_for_app_event(&mut app);
        app.handle_internal_event(event);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn deferred_api_worktree_create_rejects_checkout_with_remove_in_flight() {
        let repo = create_committed_repo("api-worktree-create-remove-in-flight-repo");
        let checkout = unique_temp_path("api-worktree-create-remove-in-flight-checkout");
        let mut app = test_app();
        app.pending_api_worktree_remove_paths
            .insert(crate::worktree::canonical_or_original(&checkout), 7);
        let (respond_to, response_rx) = response_channel();

        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeCreate(WorktreeCreateParams {
                    workspace_id: None,
                    cwd: Some(repo.display().to_string()),
                    branch: Some("worktree/create-remove-in-flight".into()),
                    base: None,
                    path: Some(checkout.display().to_string()),
                    label: None,
                    focus: false,
                }),
            },
            respond_to,
        ));

        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("create should reject checkout with remove in flight");
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "worktree_operation_in_progress");
        assert!(app.event_rx.try_recv().is_err());
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn deferred_api_worktree_remove_rejects_checkout_with_create_in_flight() {
        let repo = create_committed_repo("api-worktree-remove-create-in-flight-repo");
        let checkout = unique_temp_path("api-worktree-remove-create-in-flight-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "worktree/api-remove-create-in-flight",
                checkout.to_str().unwrap(),
                "HEAD",
            ],
        );

        let mut app = test_app();
        let mut child = Workspace::test_new("child");
        child.identity_cwd = checkout.clone();
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: crate::workspace::git_space_metadata(&repo).unwrap().key,
            label: "api-worktree-remove-create-in-flight-repo".into(),
            repo_root: repo.clone(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        app.state.ensure_test_terminals();
        app.pending_api_worktree_creates
            .insert(crate::worktree::canonical_or_original(&checkout), 7);
        let (respond_to, response_rx) = response_channel();

        assert!(app.handle_deferred_worktree_api_request(
            Request {
                id: "req".into(),
                method: crate::api::schema::Method::WorktreeRemove(WorktreeRemoveParams {
                    workspace_id: child_id,
                    force: false,
                }),
            },
            respond_to,
        ));

        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("remove should reject checkout with create in flight");
        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "worktree_operation_in_progress");
        assert!(app.event_rx.try_recv().is_err());
        let remove = crate::worktree::build_worktree_remove_command(&repo, &checkout, true);
        let _ = crate::worktree::run_worktree_command(&remove);
        let _ = std::fs::remove_dir_all(repo);
    }

    #[test]
    fn deferred_api_worktree_remove_emits_removed_after_workspace_changes() {
        let event_hub = crate::api::EventHub::default();
        let mut app = test_app_with_event_hub(event_hub.clone());
        let checkout = PathBuf::from("/repo/herdr-issue");
        let mut child = Workspace::test_new("child");
        child.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: checkout.clone(),
            is_linked_worktree: true,
        });
        let child_id = child.id.clone();
        app.state.workspaces.push(child);
        let workspace_snapshot = app.workspace_info(0);
        let worktree_snapshot = app
            .worktree_info_for_membership(app.state.workspaces[0].worktree_space().unwrap(), None);
        app.pending_api_worktree_removes.insert(child_id.clone(), 7);
        app.pending_api_worktree_remove_paths
            .insert(crate::worktree::canonical_or_original(&checkout), 7);
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/other".into(),
            is_linked_worktree: true,
        });
        let (respond_to, response_rx) = response_channel();

        app.handle_api_worktree_remove_finished(WorktreeRemoveResult {
            workspace_id: child_id,
            path: checkout.clone(),
            workspace: Some(Box::new(workspace_snapshot)),
            worktree: Some(Box::new(worktree_snapshot)),
            forced: true,
            api_request: Some(ApiWorktreeRemoveRequest {
                id: "req".into(),
                operation_id: 7,
                checkout_key: crate::worktree::canonical_or_original(&checkout),
                respond_to,
            }),
            result: Ok(()),
        });

        let response = response_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("changed-workspace remove completion should respond");
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(
            success.result,
            ResponseResult::WorktreeRemoved { .. }
        ));
        assert_eq!(
            event_hub
                .events_after(0)
                .into_iter()
                .map(|(_, event)| event.event)
                .collect::<Vec<_>>(),
            vec![EventKind::WorktreeRemoved]
        );
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0]
                .worktree_space()
                .map(|membership| membership.checkout_path.as_path()),
            Some(Path::new("/repo/other"))
        );
    }
}
