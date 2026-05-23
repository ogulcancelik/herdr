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

pub(crate) fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Resolve the `[worktrees].directory` template into a concrete root path for
/// a specific source repo.
///
/// Supported placeholders:
///   `{repo_root}`   — absolute path of the source repo (e.g. `/home/me/foo/bar`)
///   `{repo_parent}` — parent directory of the source repo (e.g. `/home/me/foo`)
///   `{repo_name}`   — basename of the source repo (e.g. `bar`)
///
/// Returns the resolved root and whether the template referenced `{repo_name}`
/// or `{repo_root}` — when it did, [`default_checkout_path`] skips the implicit
/// `<repo_name>/` segment so paths don't double up (e.g. a template of
/// `{repo_parent}/{repo_name}.worktrees` resolves to
/// `<parent>/<repo>.worktrees/<branch-slug>`, not
/// `<parent>/<repo>.worktrees/<repo>/<branch-slug>`).
pub(crate) fn resolve_worktree_root(
    template: &str,
    repo_root: &Path,
    repo_name: &str,
) -> (PathBuf, bool) {
    let has_repo_name = template.contains("{repo_name}");
    let has_repo_root = template.contains("{repo_root}");
    let has_repo_parent = template.contains("{repo_parent}");

    if !has_repo_name && !has_repo_root && !has_repo_parent {
        return (expand_tilde_path(template), false);
    }

    // `Path::parent()` returns paths without a trailing `/` for normal paths,
    // but returns `Some("/")` for paths like "/foo" and `None` for "/" itself.
    // For the "/" case (whether falling back from `None` or returned as the
    // parent of a top-level repo), substitute "" so `{repo_parent}/wt` becomes
    // `/wt` rather than `//wt` in user-visible logs.
    let repo_parent = repo_root.parent().unwrap_or_else(|| Path::new("/"));
    let repo_parent_str = if repo_parent == Path::new("/") {
        String::new()
    } else {
        repo_parent.display().to_string()
    };

    let expanded = template
        .replace("{repo_root}", &repo_root.display().to_string())
        .replace("{repo_parent}", &repo_parent_str)
        .replace("{repo_name}", repo_name);

    (expand_tilde_path(&expanded), has_repo_name || has_repo_root)
}

/// Validate a `[worktrees].directory` template, returning user-facing
/// diagnostics for problems Herdr can detect without knowing a specific repo:
/// an empty/whitespace value, or `{...}` tokens that aren't one of the
/// supported placeholders (`{repo_root}`, `{repo_parent}`, `{repo_name}`).
pub(crate) fn template_diagnostics(template: &str) -> Vec<String> {
    let mut diagnostics = Vec::new();

    if template.trim().is_empty() {
        diagnostics.push(
            "worktrees.directory is empty; worktree checkouts will be created relative to \
             Herdr's working directory. Set a path such as `~/.herdr/worktrees`."
                .to_string(),
        );
        return diagnostics;
    }

    const KNOWN: &[&str] = &["{repo_root}", "{repo_parent}", "{repo_name}"];
    let mut cursor = template;
    while let Some(open) = cursor.find('{') {
        let rest = &cursor[open..];
        let Some(close_offset) = rest.find('}') else {
            diagnostics.push(format!(
                "worktrees.directory contains an unclosed `{{` near `{rest}`; supported \
                 placeholders are `{{repo_root}}`, `{{repo_parent}}`, `{{repo_name}}`."
            ));
            break;
        };
        let token = &rest[..=close_offset];
        if !KNOWN.contains(&token) {
            diagnostics.push(format!(
                "worktrees.directory contains unknown placeholder `{token}`; supported \
                 placeholders are `{{repo_root}}`, `{{repo_parent}}`, `{{repo_name}}`."
            ));
        }
        cursor = &rest[close_offset + 1..];
    }

    diagnostics
}

pub(crate) fn default_checkout_path(
    template: &str,
    repo_root: &Path,
    repo_name: &str,
    branch: &str,
) -> PathBuf {
    let (root, repo_already_in_root) = resolve_worktree_root(template, repo_root, repo_name);
    let base = if repo_already_in_root {
        root
    } else {
        root.join(repo_name)
    };
    base.join(branch_to_path_slug(branch))
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
                "/home/me/.herdr/worktrees",
                Path::new("/home/me/code/herdr"),
                "herdr",
                "worktree/brave-river",
            ),
            PathBuf::from("/home/me/.herdr/worktrees/herdr/worktree-brave-river")
        );
    }

    #[test]
    fn default_checkout_path_expands_tilde_in_legacy_template() {
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", "/home/me");
        assert_eq!(
            default_checkout_path(
                "~/.herdr/worktrees",
                Path::new("/home/me/code/herdr"),
                "herdr",
                "main",
            ),
            PathBuf::from("/home/me/.herdr/worktrees/herdr/main")
        );
        if let Some(home) = old_home {
            std::env::set_var("HOME", home);
        } else {
            std::env::remove_var("HOME");
        }
    }

    #[test]
    fn default_checkout_path_with_repo_parent_and_name_creates_sibling_layout() {
        // Sibling layout: {repo_parent}/{repo_name}.worktrees lives next to the source repo.
        // Because the template references {repo_name}, the implicit <repo>/ segment is
        // dropped to avoid `.../bar.worktrees/bar/<branch>`.
        assert_eq!(
            default_checkout_path(
                "{repo_parent}/{repo_name}.worktrees",
                Path::new("/home/me/foo/bar"),
                "bar",
                "issue/137",
            ),
            PathBuf::from("/home/me/foo/bar.worktrees/issue-137")
        );
    }

    #[test]
    fn default_checkout_path_with_repo_root_nests_inside_repo() {
        // {repo_root} also drops the implicit <repo>/ segment.
        assert_eq!(
            default_checkout_path(
                "{repo_root}/.worktrees",
                Path::new("/home/me/foo/bar"),
                "bar",
                "feature/x",
            ),
            PathBuf::from("/home/me/foo/bar/.worktrees/feature-x")
        );
    }

    #[test]
    fn default_checkout_path_with_only_repo_parent_keeps_repo_segment() {
        // {repo_parent} alone does NOT include the repo name, so the implicit
        // <repo>/ segment is preserved (matches legacy <root>/<repo>/<branch> shape).
        assert_eq!(
            default_checkout_path(
                "{repo_parent}/shared-worktrees",
                Path::new("/home/me/foo/bar"),
                "bar",
                "feature/x",
            ),
            PathBuf::from("/home/me/foo/shared-worktrees/bar/feature-x")
        );
    }

    #[test]
    fn resolve_worktree_root_signals_when_repo_name_is_in_template() {
        let (root, repo_already_in_root) = resolve_worktree_root(
            "{repo_parent}/{repo_name}.worktrees",
            Path::new("/home/me/foo/bar"),
            "bar",
        );
        assert_eq!(root, PathBuf::from("/home/me/foo/bar.worktrees"));
        assert!(repo_already_in_root);

        let (root, repo_already_in_root) =
            resolve_worktree_root("/w", Path::new("/home/me/foo/bar"), "bar");
        assert_eq!(root, PathBuf::from("/w"));
        assert!(!repo_already_in_root);
    }

    #[test]
    fn resolve_worktree_root_handles_repo_at_filesystem_root() {
        // `parent()` returns None for /, so {repo_parent} substitutes to "".
        // Asserting on display().to_string() (not PathBuf equality, which would
        // silently normalize //wt == /wt) ensures user-visible logs are clean.
        let (root, _) = resolve_worktree_root("{repo_parent}/wt", Path::new("/"), "");
        assert_eq!(root.display().to_string(), "/wt");
    }

    #[test]
    fn resolve_worktree_root_substitutes_empty_for_root_parent_to_avoid_double_slash() {
        // /foo has parent /, which would naively yield "//wt". {repo_parent}
        // substitutes to "" in that case so the result displays as "/wt".
        let (root, _) = resolve_worktree_root("{repo_parent}/wt", Path::new("/foo"), "foo");
        assert_eq!(root.display().to_string(), "/wt");
    }

    #[test]
    fn template_diagnostics_flags_empty_and_whitespace_templates() {
        assert!(!template_diagnostics("").is_empty());
        assert!(!template_diagnostics("   ").is_empty());
    }

    #[test]
    fn template_diagnostics_flags_unknown_placeholders() {
        let diagnostics = template_diagnostics("~/wt/{repo_nam}");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("{repo_nam}"));
    }

    #[test]
    fn template_diagnostics_accepts_all_known_placeholders() {
        assert!(template_diagnostics("{repo_parent}/{repo_name}.worktrees/{repo_root}").is_empty());
        assert!(template_diagnostics("~/.herdr/worktrees").is_empty());
    }

    #[test]
    fn template_diagnostics_flags_unclosed_open_brace() {
        let diagnostics = template_diagnostics("~/wt/{repo_name");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].contains("unclosed"));
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
}
