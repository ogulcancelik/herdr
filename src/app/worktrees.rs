use std::sync::atomic::Ordering;
use std::time::{SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    state::{WorktreeCreateState, WorktreeOpenEntry, WorktreeOpenState, WorktreeRemoveState},
    App, Mode,
};
use crate::events::{AppEvent, WorktreeAddResult, WorktreeRemoveResult};

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
            KeyCode::Enter => self.start_worktree_add(),
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
                if self.state.name_input_replace_on_type {
                    self.state.name_input.clear();
                    self.state.name_input_replace_on_type = false;
                }
                self.state.name_input.push(c);
                self.sync_worktree_branch_from_input();
            }
            _ => {}
        }
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
                    && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT) =>
            {
                if let Some(open) = &mut self.state.worktree_open {
                    if !ch.is_control() {
                        open.query.push(ch);
                        open.normalize_selection();
                    }
                }
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
            KeyCode::Enter => self.open_selected_existing_worktree(),
            _ => {}
        }
    }

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

        if let Some(ws_idx) = entry.already_open_ws_idx {
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
            return;
        }

        match self.create_workspace_with_options(entry.path.clone(), true) {
            Ok(new_ws_idx) => {
                self.mark_opened_existing_worktree_membership(
                    &source_workspace_id,
                    source_existing_membership,
                    source_checkout_path,
                    source_repo_root,
                    repo_key,
                    repo_name,
                    new_ws_idx,
                    entry.path,
                    entry.is_linked_worktree,
                );
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
            if let Some(source_membership) = source_existing_membership {
                self.state.workspaces[source_ws_idx].worktree_space = Some(source_membership);
            } else {
                self.state.workspaces[source_ws_idx].worktree_space =
                    Some(crate::workspace::WorktreeSpaceMembership {
                        key: repo_key.clone(),
                        label: repo_name.clone(),
                        repo_root: source_repo_root.clone(),
                        checkout_path: source_checkout_path,
                        is_linked_worktree: false,
                    });
            }
        }
        if let Some(target) = self.state.workspaces.get_mut(target_ws_idx) {
            target.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                key: repo_key,
                label: repo_name,
                repo_root: source_repo_root,
                checkout_path: target_path,
                is_linked_worktree: target_is_linked_worktree,
            });
        }
        self.state.mark_session_dirty();
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

        let command = crate::worktree::build_worktree_add_new_branch_command(
            &create.source_checkout_path,
            &create.checkout_path,
            &create.branch,
            "HEAD",
        );
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
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = if let Some(parent_dir) = parent_dir {
                std::fs::create_dir_all(&parent_dir)
                    .map_err(|err| err.to_string())
                    .and_then(|()| crate::worktree::run_worktree_command(&command))
            } else {
                crate::worktree::run_worktree_command(&command)
            };
            let _ = event_tx.blocking_send(AppEvent::WorktreeAddFinished(WorktreeAddResult {
                path,
                result,
            }));
        });
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
            KeyCode::Enter => self.start_worktree_remove(),
            _ => {}
        }
    }

    pub(crate) fn start_worktree_remove(&mut self) {
        let Some(remove) = &mut self.state.worktree_remove else {
            return;
        };
        if remove.removing {
            return;
        }
        remove.removing = true;
        remove.error = None;
        let force = remove.force_confirmation;

        let command =
            crate::worktree::build_worktree_remove_command(&remove.repo_root, &remove.path, force);
        tracing::info!(workspace_id = %remove.workspace_id, path = %remove.path.display(), force, "starting git worktree remove");
        let path = remove.path.clone();
        let workspace_id = remove.workspace_id.clone();
        let event_tx = self.event_tx.clone();
        std::thread::spawn(move || {
            let result = crate::worktree::run_worktree_command(&command);
            let _ =
                event_tx.blocking_send(AppEvent::WorktreeRemoveFinished(WorktreeRemoveResult {
                    workspace_id,
                    path,
                    result,
                }));
        });
    }

    pub(crate) fn handle_worktree_add_finished(&mut self, result: WorktreeAddResult) {
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
                match self.create_workspace_with_options(path.clone(), true) {
                    Ok(ws_idx) => {
                        let source_membership = source_existing_membership.unwrap_or(
                            crate::workspace::WorktreeSpaceMembership {
                                key: repo_key.clone(),
                                label: repo_name.clone(),
                                repo_root: source_repo_root.clone(),
                                checkout_path: source_checkout_path,
                                is_linked_worktree: false,
                            },
                        );
                        if let Some(ws) = self
                            .state
                            .workspaces
                            .iter_mut()
                            .find(|ws| ws.id == source_workspace_id)
                        {
                            ws.worktree_space = Some(source_membership);
                        }
                        if let Some(ws) = self.state.workspaces.get_mut(ws_idx) {
                            ws.worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
                                key: repo_key,
                                label: repo_name,
                                repo_root: source_repo_root,
                                checkout_path: path,
                                is_linked_worktree: true,
                            });
                        }
                        self.state.mark_session_dirty();
                    }
                    Err(err) => {
                        self.state.config_diagnostic = Some(format!(
                            "created worktree but failed to open workspace: {err}"
                        ));
                        self.state.mode = Mode::Navigate;
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
        let Some(remove) = &mut self.state.worktree_remove else {
            return;
        };
        if remove.workspace_id != result.workspace_id || remove.path != result.path {
            return;
        }

        match result.result {
            Ok(()) => {
                tracing::info!(workspace_id = %result.workspace_id, path = %result.path.display(), "git worktree remove completed");
                self.state.worktree_remove = None;
                if let Some(ws_idx) = self
                    .state
                    .workspaces
                    .iter()
                    .position(|ws| ws.id == result.workspace_id)
                {
                    let still_same_linked_worktree = self.state.workspaces[ws_idx]
                        .worktree_space()
                        .is_some_and(|space| {
                            space.is_linked_worktree && space.checkout_path == result.path
                        });
                    if still_same_linked_worktree {
                        self.state.selected = ws_idx;
                        self.state.close_selected_workspace();
                    }
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
        App::new(
            &crate::config::Config::default(),
            true,
            None,
            tokio::sync::mpsc::unbounded_channel().1,
            crate::api::EventHub::default(),
        )
    }

    #[test]
    fn open_selected_existing_worktree_focuses_already_open_workspace() {
        let mut app = app_for_worktree_tests();
        app.state.workspaces = vec![
            crate::workspace::Workspace::test_new("main"),
            crate::workspace::Workspace::test_new("issue"),
        ];
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

        let mut app = app_for_worktree_tests();
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
        let safe_event = wait_for_worktree_event(&mut app);
        match safe_event {
            AppEvent::WorktreeRemoveFinished(result) => {
                assert_eq!(result.workspace_id, workspace_id);
                assert_eq!(result.path, checkout);
                assert!(result.result.is_err());
                app.handle_worktree_remove_finished(result);
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let remove = app.state.worktree_remove.as_ref().unwrap();
        assert!(!remove.removing);
        assert!(remove.force_confirmation);
        assert!(checkout.exists());

        app.start_worktree_remove();
        let force_event = wait_for_worktree_event(&mut app);
        match force_event {
            AppEvent::WorktreeRemoveFinished(result) => {
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

        let _ = std::fs::remove_dir_all(repo);
    }
}
