use std::path::{Path, PathBuf};

pub fn derive_label_from_cwd(cwd: &Path) -> String {
    if let Some(repo_root) = git_repo_root(cwd) {
        if let Some(name) = repo_root.file_name().and_then(|n| n.to_str()) {
            return name.to_string();
        }
    }

    if let Ok(home) = std::env::var("HOME") {
        let home = Path::new(&home);
        if cwd == home {
            return "~".to_string();
        }
    }

    cwd.file_name()
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| cwd.display().to_string())
}

pub fn git_branch(cwd: &Path) -> Option<String> {
    let repo_root = git_repo_root(cwd)?;
    let git_dir = git_dir_for_repo_root(&repo_root)?;
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    parse_git_head_branch(&head)
}

fn git_dir_for_repo_root(repo_root: &Path) -> Option<PathBuf> {
    let git_path = repo_root.join(".git");
    if git_path.is_dir() {
        return Some(git_path);
    }

    let gitdir = std::fs::read_to_string(&git_path).ok()?;
    let relative = gitdir.trim().strip_prefix("gitdir:")?.trim();
    let resolved = Path::new(relative);
    Some(if resolved.is_absolute() {
        resolved.to_path_buf()
    } else {
        repo_root.join(resolved)
    })
}

fn parse_git_head_branch(head: &str) -> Option<String> {
    let branch = head.trim().strip_prefix("ref: refs/heads/")?;
    (!branch.is_empty()).then(|| branch.to_string())
}

fn git_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };

    loop {
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

pub(super) fn git_ahead_behind(cwd: &Path) -> Option<(usize, usize)> {
    git_repo_root(cwd)?;

    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-list", "--left-right", "--count", "HEAD...@{upstream}"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_git_ahead_behind_output(&stdout)
}

fn parse_git_ahead_behind_output(stdout: &str) -> Option<(usize, usize)> {
    let mut parts = stdout.split_whitespace();
    let ahead = parts.next()?.parse().ok()?;
    let behind = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn temp_test_dir(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-workspace-tests-{}-{}-{}",
            name,
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn git_branch_reads_head_from_standard_repo() {
        let root = temp_test_dir("standard-repo");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();

        assert_eq!(git_branch(&root).as_deref(), Some("main"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_branch_reads_head_from_worktree_gitdir_file() {
        let root = temp_test_dir("worktree");
        let worktree_git_dir = root.join(".bare/worktrees/feature");
        std::fs::create_dir_all(&worktree_git_dir).unwrap();
        std::fs::write(root.join(".git"), "gitdir: .bare/worktrees/feature\n").unwrap();
        std::fs::write(worktree_git_dir.join("HEAD"), "ref: refs/heads/feature\n").unwrap();

        assert_eq!(git_branch(&root).as_deref(), Some("feature"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_branch_returns_none_for_detached_head() {
        let root = temp_test_dir("detached-head");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "3e1b9a8d\n").unwrap();

        assert_eq!(git_branch(&root), None);

        std::fs::remove_dir_all(root).unwrap();
    }
}
