use crate::api::schema::{
    Method, Request, WorktreeCreateParams, WorktreeListParams, WorktreeOpenParams,
    WorktreeRemoveParams,
};

pub(super) fn run_worktree_command(args: &[String]) -> std::io::Result<i32> {
    let Some(subcommand) = args.first().map(|arg| arg.as_str()) else {
        print_worktree_help();
        return Ok(2);
    };

    match subcommand {
        "list" => worktree_list(&args[1..]),
        "create" => worktree_create(&args[1..]),
        "open" => worktree_open(&args[1..]),
        "remove" => worktree_remove(&args[1..]),
        "kill" => worktree_kill(&args[1..]),
        "help" | "--help" | "-h" => {
            print_worktree_help();
            Ok(0)
        }
        _ => {
            print_worktree_help();
            Ok(2)
        }
    }
}

fn worktree_list(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    if workspace_id.is_some() && cwd.is_some() {
        eprintln!("usage: herdr worktree list [--workspace ID | --cwd PATH] [--json]");
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:worktree:list".into(),
        method: Method::WorktreeList(WorktreeListParams { workspace_id, cwd }),
    })?)
}

fn worktree_create(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut branch = None;
    let mut base = None;
    let mut path = None;
    let mut label = None;
    let mut focus = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--branch" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --branch");
                    return Ok(2);
                };
                branch = Some(value.clone());
                index += 2;
            }
            "--base" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --base");
                    return Ok(2);
                };
                base = Some(value.clone());
                index += 2;
            }
            "--path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --path");
                    return Ok(2);
                };
                path = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    if workspace_id.is_some() && cwd.is_some() {
        eprintln!(
            "usage: herdr worktree create [--workspace ID | --cwd PATH] [--branch NAME] [--base REF] [--path PATH] [--label TEXT] [--focus] [--no-focus] [--json]"
        );
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:worktree:create".into(),
        method: Method::WorktreeCreate(WorktreeCreateParams {
            workspace_id,
            cwd,
            branch,
            base,
            path,
            label,
            focus,
        }),
    })?)
}

fn worktree_open(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut cwd = None;
    let mut path = None;
    let mut branch = None;
    let mut label = None;
    let mut focus = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--cwd" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --cwd");
                    return Ok(2);
                };
                cwd = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--path" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --path");
                    return Ok(2);
                };
                path = Some(normalize_path_arg(value)?);
                index += 2;
            }
            "--branch" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --branch");
                    return Ok(2);
                };
                branch = Some(value.clone());
                index += 2;
            }
            "--label" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --label");
                    return Ok(2);
                };
                label = Some(value.clone());
                index += 2;
            }
            "--focus" => {
                focus = true;
                index += 1;
            }
            "--no-focus" => {
                focus = false;
                index += 1;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }
    if workspace_id.is_some() && cwd.is_some() {
        eprintln!(
            "usage: herdr worktree open [--workspace ID | --cwd PATH] (--path PATH | --branch NAME) [--label TEXT] [--focus] [--no-focus] [--json]"
        );
        return Ok(2);
    }
    if path.is_some() == branch.is_some() {
        eprintln!(
            "usage: herdr worktree open [--workspace ID | --cwd PATH] (--path PATH | --branch NAME) [--label TEXT] [--focus] [--no-focus] [--json]"
        );
        return Ok(2);
    }

    super::print_response(&super::send_request(&Request {
        id: "cli:worktree:open".into(),
        method: Method::WorktreeOpen(WorktreeOpenParams {
            workspace_id,
            cwd,
            path,
            branch,
            label,
            focus,
        }),
    })?)
}

fn worktree_remove(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut force = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(workspace_id) = workspace_id else {
        eprintln!("usage: herdr worktree remove --workspace ID [--force] [--json]");
        return Ok(2);
    };

    super::print_response(&super::send_request(&Request {
        id: "cli:worktree:remove".into(),
        method: Method::WorktreeRemove(WorktreeRemoveParams {
            workspace_id,
            force,
        }),
    })?)
}

/// Kill a linked worktree workspace through the same merge gate as the TUI's
/// "Kill worktree & branch": evidence required before the local branch dies.
/// The gate functions are the single source of truth shared with the TUI.
fn worktree_kill(args: &[String]) -> std::io::Result<i32> {
    let mut workspace_id = None;
    let mut dry_run = false;
    let mut force = false;
    let mut keep_branch = false;

    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--workspace" => {
                let Some(value) = args.get(index + 1) else {
                    eprintln!("missing value for --workspace");
                    return Ok(2);
                };
                workspace_id = Some(super::normalize_workspace_id(value));
                index += 2;
            }
            "--dry-run" => {
                dry_run = true;
                index += 1;
            }
            "--force" => {
                force = true;
                index += 1;
            }
            "--keep-branch" => {
                keep_branch = true;
                index += 1;
            }
            "--json" => index += 1,
            other => {
                eprintln!("unknown option: {other}");
                return Ok(2);
            }
        }
    }

    let Some(workspace_id) = workspace_id else {
        eprintln!(
            "usage: herdr worktree kill --workspace ID [--dry-run] [--force] [--keep-branch] [--json]"
        );
        return Ok(2);
    };

    // Resolve the workspace's worktree membership through the server.
    let response = super::send_request(&Request {
        id: "cli:worktree:kill:lookup".into(),
        method: Method::WorkspaceGet(crate::api::schema::WorkspaceTarget {
            workspace_id: workspace_id.clone(),
        }),
    })?;
    let value = response;
    let worktree = value
        .pointer("/result/workspace/worktree")
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    if !worktree
        .get("is_linked_worktree")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        eprintln!("workspace {workspace_id} is not a linked worktree checkout");
        return Ok(2);
    }
    let repo_root = std::path::PathBuf::from(
        worktree
            .get("repo_root")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
    );
    let checkout = std::path::PathBuf::from(
        worktree
            .get("checkout_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default(),
    );

    let branch = crate::worktree::checkout_branch_name(&checkout);
    let gate = match branch.as_deref() {
        Some(branch) => crate::worktree::branch_merge_gate(&repo_root, &checkout, branch),
        None => crate::worktree::WorktreeMergeGate::NotMerged,
    };
    let (merged, evidence) = match &gate {
        crate::worktree::WorktreeMergeGate::Merged { evidence } => (true, evidence.clone()),
        crate::worktree::WorktreeMergeGate::NotMerged => (false, String::new()),
    };
    let delete_branch = merged && !keep_branch;

    if dry_run {
        println!(
            "{}",
            serde_json::json!({
                "workspace_id": workspace_id,
                "checkout_path": checkout,
                "branch": branch,
                "merged": merged,
                "evidence": if merged { Some(&evidence) } else { None },
                "would_delete_branch": delete_branch,
            })
        );
        return Ok(if merged { 0 } else { 3 });
    }

    let remove_response = super::send_request(&Request {
        id: "cli:worktree:kill".into(),
        method: Method::WorktreeRemove(WorktreeRemoveParams {
            workspace_id: workspace_id.clone(),
            force,
        }),
    })?;
    let remove_value = remove_response;
    if remove_value.get("error").is_some() {
        println!("{remove_value}");
        let dirty = remove_value
            .pointer("/error/code")
            .and_then(|v| v.as_str())
            .is_some_and(|code| code == "dirty_worktree_requires_force");
        return Ok(if dirty { 4 } else { 1 });
    }

    let mut branch_deleted = false;
    let mut branch_delete_error = None;
    if delete_branch {
        if let Some(branch) = branch.as_deref() {
            match crate::worktree::delete_local_branch(&repo_root, branch) {
                Ok(()) => branch_deleted = true,
                Err(err) => branch_delete_error = Some(err),
            }
        }
    }

    println!(
        "{}",
        serde_json::json!({
            "workspace_id": workspace_id,
            "removed": true,
            "branch": branch,
            "merged": merged,
            "evidence": if merged { Some(&evidence) } else { None },
            "branch_deleted": branch_deleted,
            "branch_delete_error": branch_delete_error,
        })
    );
    Ok(0)
}

fn print_worktree_help() {
    eprintln!("herdr worktree commands:");
    eprintln!("  herdr worktree list [--workspace ID | --cwd PATH] [--json]");
    eprintln!(
        "  herdr worktree create [--workspace ID | --cwd PATH] [--branch NAME] [--base REF] [--path PATH] [--label TEXT] [--focus] [--no-focus] [--json]"
    );
    eprintln!(
        "  herdr worktree open [--workspace ID | --cwd PATH] (--path PATH | --branch NAME) [--label TEXT] [--focus] [--no-focus] [--json]"
    );
    eprintln!("  herdr worktree remove --workspace ID [--force] [--json]");
    eprintln!(
        "  herdr worktree kill --workspace ID [--dry-run] [--force] [--keep-branch] [--json]"
    );
}

fn normalize_path_arg(value: &str) -> std::io::Result<String> {
    let path = crate::worktree::expand_tilde_path(value);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(absolute.display().to_string())
}
