use std::path::{Path, PathBuf};

use crate::workspace::WorkspaceGitStatusSnapshot;

use super::{
    config::{read_branch_config, upstream_full_ref},
    discovery::{
        canonicalize_best_effort_path, git_branch, git_space_metadata, git_worktree_info,
        read_ref_oid, GitWorktreeInfo,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusCacheEntry {
    pub fingerprint: GitStatusFingerprint,
    pub snapshot: WorkspaceGitStatusSnapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusFingerprint {
    pub git_dir: PathBuf,
    pub git_common_dir: PathBuf,
    pub head: GitHeadIdentity,
    pub upstream: Option<GitUpstreamIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitHeadIdentity {
    Branch {
        full_ref: String,
        short_name: String,
        oid: Option<String>,
    },
    Detached {
        oid: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitUpstreamIdentity {
    pub remote: String,
    pub merge_ref: String,
    pub full_ref: String,
    pub oid: Option<String>,
}

pub fn git_status_cache_key(cwd: &Path) -> Option<PathBuf> {
    git_worktree_info(cwd).map(|info| canonicalize_best_effort_path(&info.repo_root))
}

pub fn git_status_snapshot_for_cwd(
    cwd: &Path,
    cached: Option<&GitStatusCacheEntry>,
) -> (WorkspaceGitStatusSnapshot, Option<GitStatusCacheEntry>) {
    let branch = git_branch(cwd);
    let space = git_space_metadata(cwd);
    let Some(fingerprint) = git_status_fingerprint(cwd) else {
        return (
            WorkspaceGitStatusSnapshot {
                branch,
                ahead_behind: None,
                space,
            },
            None,
        );
    };

    if let Some(cached) = cached.filter(|entry| entry.fingerprint == fingerprint) {
        let snapshot = WorkspaceGitStatusSnapshot {
            branch,
            ahead_behind: cached.snapshot.ahead_behind,
            space,
        };
        return (
            snapshot.clone(),
            Some(GitStatusCacheEntry {
                fingerprint,
                snapshot,
            }),
        );
    }

    let ahead_behind = fingerprint
        .head_oid()
        .zip(fingerprint.upstream_oid())
        .and_then(|(head_oid, upstream_oid)| git_ahead_behind_between(cwd, head_oid, upstream_oid));
    let snapshot = WorkspaceGitStatusSnapshot {
        branch,
        ahead_behind,
        space,
    };
    (
        snapshot.clone(),
        Some(GitStatusCacheEntry {
            fingerprint,
            snapshot,
        }),
    )
}

pub(super) fn git_status_fingerprint(cwd: &Path) -> Option<GitStatusFingerprint> {
    let info = git_worktree_info(cwd)?;
    let head = read_head_identity(&info)?;
    let upstream = match &head {
        GitHeadIdentity::Branch { short_name, .. } => read_upstream_identity(&info, short_name),
        GitHeadIdentity::Detached { .. } => None,
    };

    Some(GitStatusFingerprint {
        git_dir: canonicalize_best_effort_path(&info.git_dir),
        git_common_dir: canonicalize_best_effort_path(&info.git_common_dir),
        head,
        upstream,
    })
}

impl GitStatusFingerprint {
    fn head_oid(&self) -> Option<&str> {
        match &self.head {
            GitHeadIdentity::Branch { oid, .. } => oid.as_deref(),
            GitHeadIdentity::Detached { oid } => Some(oid.as_str()),
        }
    }

    fn upstream_oid(&self) -> Option<&str> {
        self.upstream
            .as_ref()
            .and_then(|upstream| upstream.oid.as_deref())
    }
}

fn read_head_identity(info: &GitWorktreeInfo) -> Option<GitHeadIdentity> {
    let head = std::fs::read_to_string(info.git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(full_ref) = head.strip_prefix("ref: ") {
        let short_name = full_ref.strip_prefix("refs/heads/")?.to_string();
        let oid = read_ref_oid(&info.git_common_dir, full_ref);
        return Some(GitHeadIdentity::Branch {
            full_ref: full_ref.to_string(),
            short_name,
            oid,
        });
    }

    (!head.is_empty()).then(|| GitHeadIdentity::Detached {
        oid: head.to_string(),
    })
}

fn read_upstream_identity(info: &GitWorktreeInfo, branch: &str) -> Option<GitUpstreamIdentity> {
    let config = read_branch_config(info, branch)?;
    let full_ref = upstream_full_ref(&config)?;
    let oid = read_ref_oid(&info.git_common_dir, &full_ref);
    Some(GitUpstreamIdentity {
        remote: config.remote,
        merge_ref: config.merge_ref,
        full_ref,
        oid,
    })
}

#[cfg(test)]
pub(crate) fn git_ahead_behind(cwd: &Path) -> Option<(usize, usize)> {
    super::discovery::git_repo_root(cwd)?;

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

fn git_ahead_behind_between(
    cwd: &Path,
    head_oid: &str,
    upstream_oid: &str,
) -> Option<(usize, usize)> {
    let range = format!("{head_oid}...{upstream_oid}");
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-list", "--left-right", "--count", &range])
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
    use super::*;
    use crate::workspace::git::test_support::{run_git, temp_test_dir, write_fake_tracked_repo};

    #[test]
    fn git_status_cache_key_ignores_invalid_git_marker() {
        let base = temp_test_dir("invalid-git-root");
        let cwd = base.join("workspace");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        assert_eq!(git_status_cache_key(&cwd), None);

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn git_status_reuses_cached_ahead_behind_when_fingerprint_matches() {
        let root = temp_test_dir("cache-hit");
        write_fake_tracked_repo(&root);
        let fingerprint = git_status_fingerprint(&root).unwrap();
        let cached = GitStatusCacheEntry {
            fingerprint,
            snapshot: WorkspaceGitStatusSnapshot {
                branch: Some("main".into()),
                ahead_behind: Some((2, 1)),
                space: git_space_metadata(&root),
            },
        };

        let (snapshot, update) = git_status_snapshot_for_cwd(&root, Some(&cached));

        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.ahead_behind, Some((2, 1)));
        assert_eq!(update.unwrap().snapshot.ahead_behind, Some((2, 1)));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_does_not_reuse_cache_when_branch_changes_at_same_oid() {
        let root = temp_test_dir("branch-switch");
        write_fake_tracked_repo(&root);
        let fingerprint = git_status_fingerprint(&root).unwrap();
        let cached = GitStatusCacheEntry {
            fingerprint,
            snapshot: WorkspaceGitStatusSnapshot {
                branch: Some("main".into()),
                ahead_behind: Some((4, 0)),
                space: git_space_metadata(&root),
            },
        };
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/feature\n").unwrap();
        std::fs::write(
            root.join(".git/refs/heads/feature"),
            "1111111111111111111111111111111111111111\n",
        )
        .unwrap();
        std::fs::write(
            root.join(".git/config"),
            "[branch \"feature\"]\n\tremote = origin\n\tmerge = refs/heads/main\n",
        )
        .unwrap();

        let (snapshot, _) = git_status_snapshot_for_cwd(&root, Some(&cached));

        assert_eq!(snapshot.branch.as_deref(), Some("feature"));
        assert_eq!(snapshot.ahead_behind, None);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_clears_ahead_behind_when_upstream_is_unset() {
        let root = temp_test_dir("upstream-unset");
        write_fake_tracked_repo(&root);
        let fingerprint = git_status_fingerprint(&root).unwrap();
        let cached = GitStatusCacheEntry {
            fingerprint,
            snapshot: WorkspaceGitStatusSnapshot {
                branch: Some("main".into()),
                ahead_behind: Some((0, 3)),
                space: git_space_metadata(&root),
            },
        };
        std::fs::write(root.join(".git/config"), "").unwrap();

        let (snapshot, _) = git_status_snapshot_for_cwd(&root, Some(&cached));

        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert_eq!(snapshot.ahead_behind, None);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_fingerprint_reads_packed_refs() {
        let root = temp_test_dir("packed-refs");
        write_fake_tracked_repo(&root);
        std::fs::remove_file(root.join(".git/refs/remotes/origin/main")).unwrap();
        std::fs::write(
            root.join(".git/packed-refs"),
            "# pack-refs with: peeled fully-peeled sorted\n2222222222222222222222222222222222222222 refs/remotes/origin/main\n",
        )
        .unwrap();

        let fingerprint = git_status_fingerprint(&root).unwrap();

        assert_eq!(
            fingerprint.upstream.unwrap().oid.as_deref(),
            Some("2222222222222222222222222222222222222222")
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_status_cache_key_is_per_linked_worktree_checkout() {
        let base = temp_test_dir("linked-worktree-keys");
        let common_dir = base.join("repo/.git");
        let worktree_one = base.join("one");
        let worktree_two = base.join("two");
        let git_dir_one = common_dir.join("worktrees/one");
        let git_dir_two = common_dir.join("worktrees/two");
        std::fs::create_dir_all(&git_dir_one).unwrap();
        std::fs::create_dir_all(&git_dir_two).unwrap();
        std::fs::create_dir_all(&worktree_one).unwrap();
        std::fs::create_dir_all(&worktree_two).unwrap();
        std::fs::write(
            worktree_one.join(".git"),
            format!("gitdir: {}\n", git_dir_one.display()),
        )
        .unwrap();
        std::fs::write(
            worktree_two.join(".git"),
            format!("gitdir: {}\n", git_dir_two.display()),
        )
        .unwrap();
        std::fs::write(git_dir_one.join("HEAD"), "ref: refs/heads/one\n").unwrap();
        std::fs::write(git_dir_two.join("HEAD"), "ref: refs/heads/two\n").unwrap();

        assert_ne!(
            git_status_cache_key(&worktree_one),
            git_status_cache_key(&worktree_two)
        );

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn git_status_recomputes_ahead_behind_when_head_moves() {
        let base = temp_test_dir("head-moves");
        let remote = base.join("remote.git");
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let remote_arg = remote.to_string_lossy().to_string();
        run_git(&base, &["init", "--bare", &remote_arg]);
        run_git(&repo, &["init"]);
        run_git(&repo, &["config", "user.email", "herdr@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Herdr Test"]);
        run_git(&repo, &["commit", "--allow-empty", "-m", "initial"]);
        run_git(&repo, &["branch", "-M", "main"]);
        run_git(&repo, &["remote", "add", "origin", &remote_arg]);
        run_git(&repo, &["push", "-u", "origin", "main"]);

        let (initial, cache_entry) = git_status_snapshot_for_cwd(&repo, None);
        assert_eq!(initial.ahead_behind, Some((0, 0)));
        run_git(&repo, &["commit", "--allow-empty", "-m", "ahead"]);

        let (updated, _) = git_status_snapshot_for_cwd(&repo, cache_entry.as_ref());

        assert_eq!(updated.branch.as_deref(), Some("main"));
        assert_eq!(updated.ahead_behind, Some((1, 0)));

        std::fs::remove_dir_all(base).unwrap();
    }
}
