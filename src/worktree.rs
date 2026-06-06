use std::path::{Path, PathBuf};

const DEFAULT_WORKTREE_PREFIX: &str = "worktree";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WorktreeCommand {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExistingWorktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub is_bare: bool,
    pub is_detached: bool,
    pub is_prunable: bool,
}

pub(crate) fn generated_branch_slug(seed: u64) -> String {
    let adjectives = [
        "brave", "calm", "clear", "green", "lucky", "quiet", "rapid", "silver",
    ];
    let nouns = [
        "river", "cloud", "field", "forest", "harbor", "meadow", "stone", "valley",
    ];
    let adjective = adjectives[(seed as usize) % adjectives.len()];
    let noun = nouns[((seed / adjectives.len() as u64) as usize) % nouns.len()];
    let suffix = seed & 0xffff;
    format!("{DEFAULT_WORKTREE_PREFIX}/{adjective}-{noun}-{suffix:04x}")
}

pub(crate) fn branch_to_path_slug(branch: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in branch.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        DEFAULT_WORKTREE_PREFIX.to_string()
    } else {
        trimmed
    }
}

pub(crate) fn expand_tilde_path(path: &str) -> PathBuf {
    if path == "~" {
        return std::env::var("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(path));
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return std::env::var("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|_| PathBuf::from(path));
    }

    PathBuf::from(path)
}

pub(crate) fn expand_tilde_absolute_path(path: &str) -> PathBuf {
    let path = expand_tilde_path(path);
    if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(&path))
            .unwrap_or(path)
    }
}

pub(crate) fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

pub(crate) fn default_checkout_path(root: &Path, repo_name: &str, branch: &str) -> PathBuf {
    root.join(repo_name).join(branch_to_path_slug(branch))
}

pub(crate) fn build_worktree_remove_command(
    repo_root: &Path,
    path: &Path,
    force: bool,
) -> WorktreeCommand {
    let mut args = vec![
        "-C".to_string(),
        repo_root.display().to_string(),
        "worktree".to_string(),
        "remove".to_string(),
    ];
    if force {
        args.push("--force".to_string());
    }
    args.push(path.display().to_string());

    WorktreeCommand {
        program: "git".to_string(),
        args,
    }
}

pub(crate) fn is_dirty_worktree_remove_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("contains modified or untracked files")
        && lower.contains("use --force to delete it")
}

pub(crate) fn build_worktree_add_new_branch_command(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    base: &str,
) -> WorktreeCommand {
    WorktreeCommand {
        program: "git".to_string(),
        args: vec![
            "-C".to_string(),
            repo_root.display().to_string(),
            "worktree".to_string(),
            "add".to_string(),
            "-b".to_string(),
            branch.to_string(),
            path.display().to_string(),
            base.to_string(),
        ],
    }
}

pub(crate) fn run_worktree_command(command: &WorktreeCommand) -> Result<(), String> {
    let output = std::process::Command::new(&command.program)
        .args(&command.args)
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let message = if stderr.is_empty() { stdout } else { stderr };
    Err(if message.is_empty() {
        format!("{} failed with status {}", command.program, output.status)
    } else {
        message
    })
}

/// Evidence-gated decision for deleting a worktree's local branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeMergeGate {
    /// The branch's work is recorded elsewhere; deleting it is safe.
    Merged { evidence: String },
    /// No merge evidence found; only the checkout should be removed.
    NotMerged,
}

fn run_command_capture(
    program: &str,
    args: &[&str],
    cwd: Option<&std::path::Path>,
) -> Result<String, String> {
    let mut command = std::process::Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command.output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("{program} failed with status {}", output.status)
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Branch checked out in `checkout`, if any (detached HEAD yields None).
pub(crate) fn checkout_branch_name(checkout: &std::path::Path) -> Option<String> {
    let path = checkout.to_string_lossy().to_string();
    run_command_capture("git", &["-C", &path, "branch", "--show-current"], None)
        .ok()
        .filter(|branch| !branch.is_empty())
}

/// The repo's default branch: origin/HEAD when set, else main/master if present.
pub(crate) fn detect_default_branch(repo_root: &std::path::Path) -> Option<String> {
    let root = repo_root.to_string_lossy().to_string();
    if let Ok(full) = run_command_capture(
        "git",
        &[
            "-C",
            &root,
            "symbolic-ref",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
        None,
    ) {
        if let Some(branch) = full.strip_prefix("origin/") {
            return Some(branch.to_string());
        }
    }
    for candidate in ["main", "master"] {
        let probe = format!("refs/heads/{candidate}");
        if run_command_capture(
            "git",
            &["-C", &root, "show-ref", "--verify", "--quiet", &probe],
            None,
        )
        .is_ok()
        {
            return Some(candidate.to_string());
        }
    }
    None
}

/// Parse "owner/repo" out of a github remote URL (ssh or https).
fn github_repo_from_remote_url(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let repo = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut parts = repo.splitn(3, '/');
    let owner = parts.next()?;
    let name = parts.next()?;
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}

/// gh matches PRs by branch NAME; commits added locally after the merge
/// would not be covered by that evidence. Only accept it when the PR's head
/// equals the local tip — otherwise fall through to the tip-exact checks.
fn gh_pr_merged_evidence(
    args: &[&str],
    cwd: &std::path::Path,
    local_tip: Option<&str>,
) -> Option<String> {
    let json = run_command_capture("gh", args, Some(cwd)).ok()?;
    let value = serde_json::from_str::<serde_json::Value>(&json).ok()?;
    if value.get("state").and_then(|v| v.as_str()) != Some("MERGED") {
        return None;
    }
    let head_oid = value.get("headRefOid").and_then(|v| v.as_str())?;
    if local_tip != Some(head_oid) {
        return None;
    }
    Some(match value.get("number").and_then(|v| v.as_u64()) {
        Some(number) => format!("PR #{number} merged"),
        None => "PR merged".to_string(),
    })
}

/// PR-merged gate for deleting `branch`. Evidence sources, in order:
/// 1. `gh pr view` with gh's own repo resolution, then pinned to the origin
///    remote's repo (multi-remote checkouts resolve to upstream otherwise).
/// 2. `git branch --merged <default-branch>`.
/// 3. Remote containment: the branch tip is reachable from another pushed
///    remote ref (e.g. merged into a feature branch) — the work is recorded,
///    so deleting the local branch loses nothing.
///    Anything inconclusive is NotMerged — deletion needs positive evidence.
pub(crate) fn branch_merge_gate(
    repo_root: &std::path::Path,
    checkout: &std::path::Path,
    branch: &str,
) -> WorktreeMergeGate {
    let root = repo_root.to_string_lossy().to_string();
    let local_tip = run_command_capture("git", &["-C", &root, "rev-parse", branch], None).ok();
    if let Some(evidence) = gh_pr_merged_evidence(
        &["pr", "view", branch, "--json", "state,number,headRefOid"],
        checkout,
        local_tip.as_deref(),
    ) {
        return WorktreeMergeGate::Merged { evidence };
    }
    if let Some(repo) =
        run_command_capture("git", &["-C", &root, "remote", "get-url", "origin"], None)
            .ok()
            .as_deref()
            .and_then(github_repo_from_remote_url)
    {
        if let Some(evidence) = gh_pr_merged_evidence(
            &[
                "pr",
                "view",
                branch,
                "--repo",
                &repo,
                "--json",
                "state,number,headRefOid",
            ],
            checkout,
            local_tip.as_deref(),
        ) {
            return WorktreeMergeGate::Merged { evidence };
        }
    }

    if let Some(default_branch) = detect_default_branch(repo_root) {
        if let Ok(merged) = run_command_capture(
            "git",
            &[
                "-C",
                &root,
                "branch",
                "--merged",
                &default_branch,
                "--format",
                "%(refname:short)",
            ],
            None,
        ) {
            if merged.lines().any(|line| line.trim() == branch) {
                return WorktreeMergeGate::Merged {
                    evidence: format!("merged into {default_branch}"),
                };
            }
        }
    }

    // Remote containment: any remote ref other than the branch's own
    // tracking ref that contains the tip.
    if let Ok(containing) = run_command_capture(
        "git",
        &[
            "-C",
            &root,
            "branch",
            "-r",
            "--contains",
            branch,
            "--format",
            "%(refname:short)",
        ],
        None,
    ) {
        let own_suffix = format!("/{branch}");
        if let Some(remote_ref) = containing
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .find(|line| !line.ends_with(&own_suffix) && !line.contains("HEAD"))
        {
            return WorktreeMergeGate::Merged {
                evidence: format!("contained in {remote_ref}"),
            };
        }
    }

    WorktreeMergeGate::NotMerged
}

/// `git branch -D <branch>` in `repo_root`. Only called once the merge gate
/// produced positive evidence; -D because -d judges merges against the
/// current HEAD, not the default branch.
pub(crate) fn delete_local_branch(repo_root: &std::path::Path, branch: &str) -> Result<(), String> {
    let root = repo_root.to_string_lossy().to_string();
    run_command_capture("git", &["-C", &root, "branch", "-D", branch], None).map(|_| ())
}

pub(crate) fn parse_worktree_list_porcelain(output: &str) -> Vec<ExistingWorktree> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch = None;
    let mut is_bare = false;
    let mut is_detached = false;
    let mut is_prunable = false;

    let finish = |entries: &mut Vec<ExistingWorktree>,
                  path: &mut Option<PathBuf>,
                  branch: &mut Option<String>,
                  is_bare: &mut bool,
                  is_detached: &mut bool,
                  is_prunable: &mut bool| {
        if let Some(path) = path.take() {
            entries.push(ExistingWorktree {
                path,
                branch: branch.take(),
                is_bare: *is_bare,
                is_detached: *is_detached,
                is_prunable: *is_prunable,
            });
        }
        *is_bare = false;
        *is_detached = false;
        *is_prunable = false;
    };

    for line in output.lines() {
        if line.trim().is_empty() {
            finish(
                &mut entries,
                &mut path,
                &mut branch,
                &mut is_bare,
                &mut is_detached,
                &mut is_prunable,
            );
            continue;
        }
        if let Some(value) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(value));
        } else if let Some(value) = line.strip_prefix("branch ") {
            branch = Some(
                value
                    .strip_prefix("refs/heads/")
                    .unwrap_or(value)
                    .to_string(),
            );
        } else if line == "detached" {
            is_detached = true;
        } else if line == "bare" {
            is_bare = true;
        } else if line.starts_with("prunable") {
            is_prunable = true;
        }
    }

    finish(
        &mut entries,
        &mut path,
        &mut branch,
        &mut is_bare,
        &mut is_detached,
        &mut is_prunable,
    );
    entries
}

pub(crate) fn list_existing_worktrees(repo_root: &Path) -> Result<Vec<ExistingWorktree>, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|err| err.to_string())?;

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        return Ok(parse_worktree_list_porcelain(&stdout));
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(if stderr.is_empty() {
        format!("git worktree list failed with status {}", output.status)
    } else {
        stderr
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
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

    #[test]
    fn generated_branch_slug_is_worktree_namespaced_and_stable() {
        assert_eq!(generated_branch_slug(0), "worktree/brave-river-0000");
        assert_eq!(generated_branch_slug(9), "worktree/calm-cloud-0009");
    }

    #[test]
    fn parses_git_worktree_list_porcelain() {
        let output = "\
worktree /repo/main
HEAD abc
branch refs/heads/main

worktree /repo/issue
HEAD def
branch refs/heads/worktree/issue

worktree /repo/detached
HEAD fed
detached
prunable stale

";

        assert_eq!(
            parse_worktree_list_porcelain(output),
            vec![
                ExistingWorktree {
                    path: PathBuf::from("/repo/main"),
                    branch: Some("main".into()),
                    is_bare: false,
                    is_detached: false,
                    is_prunable: false,
                },
                ExistingWorktree {
                    path: PathBuf::from("/repo/issue"),
                    branch: Some("worktree/issue".into()),
                    is_bare: false,
                    is_detached: false,
                    is_prunable: false,
                },
                ExistingWorktree {
                    path: PathBuf::from("/repo/detached"),
                    branch: None,
                    is_bare: false,
                    is_detached: true,
                    is_prunable: true,
                },
            ]
        );
    }

    #[test]
    fn branch_to_path_slug_makes_branch_safe_folder_name() {
        assert_eq!(
            branch_to_path_slug("worktree/brave-river"),
            "worktree-brave-river"
        );
        assert_eq!(
            branch_to_path_slug("issue/137 Worktree Spaces"),
            "issue-137-worktree-spaces"
        );
        assert_eq!(branch_to_path_slug("///"), "worktree");
    }

    #[test]
    fn expand_tilde_path_uses_home_when_available() {
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/me");
        assert_eq!(
            expand_tilde_path("~/.herdr/worktrees"),
            PathBuf::from("/home/me/.herdr/worktrees")
        );
        assert_eq!(
            expand_tilde_path("/tmp/worktrees"),
            PathBuf::from("/tmp/worktrees")
        );
        if let Some(home) = old_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn default_checkout_path_appends_repo_and_branch_slug() {
        assert_eq!(
            default_checkout_path(
                Path::new("/home/me/.herdr/worktrees"),
                "herdr",
                "worktree/brave-river",
            ),
            PathBuf::from("/home/me/.herdr/worktrees/herdr/worktree-brave-river")
        );
    }

    #[test]
    fn worktree_remove_command_preserves_branch_by_not_deleting_it() {
        let command = build_worktree_remove_command(
            Path::new("/repo/herdr"),
            Path::new("/w/herdr/issue-137"),
            false,
        );
        assert_eq!(command.program, "git");
        assert_eq!(
            command.args,
            vec![
                "-C",
                "/repo/herdr",
                "worktree",
                "remove",
                "/w/herdr/issue-137"
            ]
        );
    }

    #[test]
    fn forced_worktree_remove_command_uses_git_force_flag() {
        let command = build_worktree_remove_command(
            Path::new("/repo/herdr"),
            Path::new("/w/herdr/issue-137"),
            true,
        );
        assert_eq!(
            command.args,
            vec![
                "-C",
                "/repo/herdr",
                "worktree",
                "remove",
                "--force",
                "/w/herdr/issue-137"
            ]
        );
    }

    #[test]
    fn dirty_remove_error_detection_matches_git_force_hint() {
        assert!(is_dirty_worktree_remove_error(
            "fatal: '/w/herdr' contains modified or untracked files, use --force to delete it"
        ));
        assert!(!is_dirty_worktree_remove_error(
            "fatal: '/w/herdr' is a missing but already registered worktree"
        ));
        assert!(!is_dirty_worktree_remove_error(
            "fatal: '/w/herdr' contains a locked worktree, use --force only if you know why"
        ));
    }

    #[test]
    fn worktree_add_command_creates_new_branch_from_base() {
        let command = build_worktree_add_new_branch_command(
            Path::new("/repo/herdr"),
            Path::new("/w/herdr/worktree-brave-river"),
            "worktree/brave-river",
            "HEAD",
        );
        assert_eq!(command.program, "git");
        assert_eq!(
            command.args,
            vec![
                "-C",
                "/repo/herdr",
                "worktree",
                "add",
                "-b",
                "worktree/brave-river",
                "/w/herdr/worktree-brave-river",
                "HEAD"
            ]
        );
    }

    #[test]
    fn run_worktree_add_and_remove_create_and_delete_checkout() {
        let repo = create_committed_repo("worktree-run-repo");
        let checkout = unique_temp_path("worktree-run-checkout");
        let branch = "worktree/test-create-remove";

        let add = build_worktree_add_new_branch_command(&repo, &checkout, branch, "HEAD");
        run_worktree_command(&add).unwrap();

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

        let remove = build_worktree_remove_command(&repo, &checkout, false);
        run_worktree_command(&remove).unwrap();
        assert!(!checkout.exists());

        let _ = std::fs::remove_dir_all(repo);
    }
    #[test]
    fn checkout_branch_name_and_default_branch_detection() {
        let repo = create_committed_repo("merge-gate-names");
        let checkout = unique_temp_path("merge-gate-names-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/gate",
                checkout.to_str().unwrap(),
            ],
        );

        assert_eq!(
            checkout_branch_name(&checkout).as_deref(),
            Some("feature/gate")
        );
        // create_committed_repo commits on the default init branch; detection
        // falls back to main/master existence when origin/HEAD is unset.
        let default = detect_default_branch(&repo);
        assert!(
            default.as_deref() == Some("master") || default.as_deref() == Some("main"),
            "unexpected default branch: {default:?}"
        );

        let _ = std::fs::remove_dir_all(&checkout);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn branch_merge_gate_requires_positive_evidence() {
        let repo = create_committed_repo("merge-gate-evidence");
        let checkout = unique_temp_path("merge-gate-evidence-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/unmerged",
                checkout.to_str().unwrap(),
            ],
        );
        std::fs::write(checkout.join("new.txt"), "x\n").unwrap();
        run_git(&checkout, &["add", "new.txt"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "feature work"]);

        // Unmerged branch: no evidence (gh pr view fails in a remote-less repo).
        assert_eq!(
            branch_merge_gate(&repo, &checkout, "feature/unmerged"),
            WorktreeMergeGate::NotMerged
        );

        // Merge it into the default branch: the git fallback now has evidence.
        let default = detect_default_branch(&repo).expect("default branch");
        run_git(&repo, &["merge", "--quiet", "feature/unmerged"]);
        let gate = branch_merge_gate(&repo, &checkout, "feature/unmerged");
        assert_eq!(
            gate,
            WorktreeMergeGate::Merged {
                evidence: format!("merged into {default}")
            }
        );

        let _ = std::fs::remove_dir_all(&checkout);
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn delete_local_branch_removes_merged_branch() {
        let repo = create_committed_repo("merge-gate-delete");
        run_git(&repo, &["branch", "feature/done"]);
        delete_local_branch(&repo, "feature/done").expect("branch delete should succeed");
        let out = std::process::Command::new("git")
            .args([
                "-C",
                repo.to_str().unwrap(),
                "branch",
                "--list",
                "feature/done",
            ])
            .output()
            .unwrap();
        assert!(String::from_utf8_lossy(&out.stdout).trim().is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }
    #[test]
    fn github_repo_parses_ssh_and_https_remote_urls() {
        assert_eq!(
            github_repo_from_remote_url("git@github.com:gerchowl/herdr.git").as_deref(),
            Some("gerchowl/herdr")
        );
        assert_eq!(
            github_repo_from_remote_url("https://github.com/ogulcancelik/herdr").as_deref(),
            Some("ogulcancelik/herdr")
        );
        assert_eq!(github_repo_from_remote_url("https://example.com/x/y"), None);
        assert_eq!(github_repo_from_remote_url("git@github.com:broken"), None);
    }

    #[test]
    fn branch_merge_gate_accepts_remote_containment_in_feature_branch() {
        // origin bare repo; feature branch merged into a NON-default branch
        // that is pushed — the containment fallback must accept it.
        let origin = unique_temp_path("merge-gate-containment-origin");
        std::fs::create_dir_all(&origin).unwrap();
        run_git(&origin, &["init", "--quiet", "--bare"]);

        let repo = create_committed_repo("merge-gate-containment-repo");
        run_git(
            &repo,
            &["remote", "add", "origin", origin.to_str().unwrap()],
        );
        let default = detect_default_branch(&repo).expect("default branch");
        run_git(&repo, &["push", "--quiet", "origin", &default]);

        let checkout = unique_temp_path("merge-gate-containment-checkout");
        run_git(
            &repo,
            &[
                "worktree",
                "add",
                "--quiet",
                "-b",
                "feature/float",
                checkout.to_str().unwrap(),
            ],
        );
        std::fs::write(checkout.join("w.txt"), "w\n").unwrap();
        run_git(&checkout, &["add", "w.txt"]);
        run_git(&checkout, &["commit", "--quiet", "-m", "float work"]);
        run_git(&checkout, &["push", "--quiet", "origin", "feature/float"]);

        // Not merged anywhere else yet: own tracking ref must NOT count.
        assert_eq!(
            branch_merge_gate(&repo, &checkout, "feature/float"),
            WorktreeMergeGate::NotMerged
        );

        // Merge into a pushed integration branch (not the default).
        run_git(&repo, &["branch", "integration", &default]);
        run_git(&repo, &["checkout", "--quiet", "integration"]);
        run_git(&repo, &["merge", "--quiet", "feature/float"]);
        run_git(&repo, &["push", "--quiet", "origin", "integration"]);
        run_git(&repo, &["checkout", "--quiet", &default]);
        run_git(&repo, &["fetch", "--quiet", "origin"]);

        assert_eq!(
            branch_merge_gate(&repo, &checkout, "feature/float"),
            WorktreeMergeGate::Merged {
                evidence: "contained in origin/integration".to_string()
            }
        );

        let _ = std::fs::remove_dir_all(&checkout);
        let _ = std::fs::remove_dir_all(&repo);
        let _ = std::fs::remove_dir_all(&origin);
    }
}
