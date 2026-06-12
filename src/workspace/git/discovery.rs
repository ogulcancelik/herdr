use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitSpaceMetadata {
    pub key: String,
    pub checkout_key: String,
    pub label: String,
    pub repo_root: PathBuf,
    pub is_linked_worktree: bool,
    /// Machine-independent project identity: the normalized origin URL
    /// (e.g. "github.com/owner/repo"), or a "dir:<name>" fallback when the
    /// repo has no origin remote. Folds checkouts of the same project across
    /// federated peer servers.
    pub project_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitWorktreeInfo {
    pub repo_root: PathBuf,
    pub git_dir: PathBuf,
    pub git_common_dir: PathBuf,
    pub is_bare: bool,
    pub is_linked_worktree: bool,
}

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

pub fn git_worktree_info(cwd: &Path) -> Option<GitWorktreeInfo> {
    let repo_root = git_repo_root(cwd)?;
    let git_dir = canonicalize_best_effort_path(&git_dir_for_repo_root(&repo_root)?);
    let git_common_dir = canonicalize_best_effort_path(&git_common_dir_for_git_dir(&git_dir));
    let is_linked_worktree = git_dir != git_common_dir;

    Some(GitWorktreeInfo {
        repo_root,
        git_dir,
        git_common_dir,
        is_bare: false,
        is_linked_worktree,
    })
}

pub fn git_space_metadata(cwd: &Path) -> Option<GitSpaceMetadata> {
    git_repo_root(cwd)?;

    let info = git_worktree_info(cwd)?;
    if info.is_bare {
        return None;
    }
    let key = canonicalize_best_effort_path(&info.git_common_dir)
        .display()
        .to_string();
    let checkout_key = canonicalize_best_effort_path(&info.repo_root)
        .display()
        .to_string();
    let label_path = if info
        .git_common_dir
        .file_name()
        .and_then(|name| name.to_str())
        == Some(".git")
    {
        info.git_common_dir.parent().unwrap_or(&info.repo_root)
    } else {
        &info.repo_root
    };
    let label = label_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("repo")
        .to_string();
    let project_key = project_key_for_common_dir(&info.git_common_dir, &label);
    Some(GitSpaceMetadata {
        key,
        checkout_key,
        label,
        repo_root: info.repo_root,
        is_linked_worktree: info.is_linked_worktree,
        project_key,
    })
}

/// Machine-independent project key: normalized origin URL from the repo's
/// git config, falling back to "dir:<label>" for origin-less repos.
pub fn project_key_for_common_dir(git_common_dir: &Path, label: &str) -> String {
    read_git_config_subsection_value(&git_common_dir.join("config"), "remote", "origin", "url")
        .map(|url| normalize_git_remote_url(&url))
        .unwrap_or_else(|| format!("dir:{label}"))
}

/// Normalize a git remote URL to a host/path identity that is stable across
/// transports: scheme, credentials, port, a trailing ".git" and trailing
/// slashes are stripped; the host is lowercased.
/// `git@github.com:owner/Repo.git` == `https://GitHub.com/owner/Repo` ->
/// `github.com/owner/Repo`.
pub fn normalize_git_remote_url(url: &str) -> String {
    let url = url.trim();
    // URL form (ssh://, https://, git://, file://, ...): authority/path.
    if let Some((_, rest)) = url.split_once("://") {
        return match rest.split_once('/') {
            Some((host, path)) => normalize_git_host_path(host, path),
            None => normalize_git_host_path(rest, ""),
        };
    }
    // scp-like form: [user@]host:path (host has no slashes; a single leading
    // char before ':' is a Windows drive letter, i.e. a local path).
    if let Some((host, path)) = url.split_once(':') {
        if host.len() > 1 && !host.contains('/') {
            return normalize_git_host_path(host, path);
        }
    }
    // Local path remote: machine-specific, but keep a stable trimmed form.
    url.trim_end_matches('/').trim_end_matches(".git").into()
}

fn normalize_git_host_path(host: &str, path: &str) -> String {
    // Drop credentials and port from the authority.
    let host = host.rsplit_once('@').map_or(host, |(_, host)| host);
    let host = host.split_once(':').map_or(host, |(host, _)| host);
    let host = host.to_ascii_lowercase();
    let path = path
        .trim_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    if path.is_empty() {
        host
    } else {
        format!("{host}/{path}")
    }
}

pub(super) fn canonicalize_best_effort_path(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn git_common_dir_for_git_dir(git_dir: &Path) -> PathBuf {
    let commondir = git_dir.join("commondir");
    let Ok(contents) = std::fs::read_to_string(commondir) else {
        return git_dir.to_path_buf();
    };
    let path = Path::new(contents.trim());
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        git_dir.join(path)
    }
}

pub fn git_branch(cwd: &Path) -> Option<String> {
    let repo_root = git_repo_root(cwd)?;
    let git_dir = git_dir_for_repo_root(&repo_root)?;
    let git_common_dir = git_common_dir_for_git_dir(&git_dir);
    if git_ref_storage_is_reftable(&git_common_dir) {
        return git_symbolic_head_short(&repo_root);
    }

    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    parse_git_head_branch(&head)
}

pub(super) fn git_dir_for_repo_root(repo_root: &Path) -> Option<PathBuf> {
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

pub(super) fn git_symbolic_head_full(repo_root: &Path) -> Option<String> {
    git_trimmed_stdout(repo_root, &["symbolic-ref", "--quiet", "HEAD"])
}

fn git_symbolic_head_short(repo_root: &Path) -> Option<String> {
    git_trimmed_stdout(repo_root, &["symbolic-ref", "--quiet", "--short", "HEAD"])
}

pub(super) fn git_rev_parse_verify(repo_root: &Path, revision: &str) -> Option<String> {
    git_trimmed_stdout(repo_root, &["rev-parse", "--verify", revision])
}

pub(super) fn git_ref_storage_is_reftable(git_common_dir: &Path) -> bool {
    read_git_config_value(&git_common_dir.join("config"), "extensions", "refstorage")
        .is_some_and(|value| value.eq_ignore_ascii_case("reftable"))
}

fn parse_git_head_branch(head: &str) -> Option<String> {
    let branch = head.trim().strip_prefix("ref: refs/heads/")?;
    (!branch.is_empty()).then(|| branch.to_string())
}

fn read_git_config_value(path: &Path, section: &str, key: &str) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let mut in_section = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section_name) = simple_git_config_section(line) {
            in_section = section_name.eq_ignore_ascii_case(section);
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(key) {
            return Some(strip_git_config_comment(value).trim().to_string());
        }
    }
    None
}

/// Read a key from a quoted-subsection git config section, e.g.
/// `[remote "origin"] url = ...` via ("remote", "origin", "url").
fn read_git_config_subsection_value(
    path: &Path,
    section: &str,
    subsection: &str,
    key: &str,
) -> Option<String> {
    let contents = std::fs::read_to_string(path).ok()?;
    let header = format!("{} \"{}\"", section.to_ascii_lowercase(), subsection);
    let mut in_section = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.split_once(']')) {
            // Section names are case-insensitive; subsection names are not.
            let name = name.0.trim();
            in_section = match name.split_once(char::is_whitespace) {
                Some((base, rest)) => {
                    format!("{} {}", base.to_ascii_lowercase(), rest.trim()) == header
                }
                None => false,
            };
            continue;
        }
        if !in_section {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(key) {
            return Some(strip_git_config_comment(value).trim().to_string());
        }
    }
    None
}

fn simple_git_config_section(line: &str) -> Option<&str> {
    let section = line.strip_prefix('[')?.split_once(']')?.0.trim();
    (!section.contains('"')).then_some(section)
}

fn strip_git_config_comment(value: &str) -> &str {
    let value = value.trim();
    for marker in ['#', ';'] {
        if let Some((prefix, _)) = value.split_once(marker) {
            if prefix.chars().next_back().is_some_and(char::is_whitespace) {
                return prefix;
            }
        }
    }
    value
}

fn git_trimmed_stdout(repo_root: &Path, args: &[&str]) -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8(output.stdout).ok()?;
    let stdout = stdout.trim();
    (!stdout.is_empty()).then(|| stdout.to_string())
}

pub(super) fn git_repo_root(start: &Path) -> Option<PathBuf> {
    let mut current = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent()?.to_path_buf()
    };

    loop {
        if git_dir_for_repo_root(&current)
            .map(|git_dir| git_dir.join("HEAD").is_file())
            .unwrap_or(false)
        {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

pub(super) fn read_ref_oid(common_dir: &Path, full_ref: &str) -> Option<String> {
    let loose_ref = common_dir.join(full_ref);
    if let Ok(contents) = std::fs::read_to_string(loose_ref) {
        let oid = contents.trim();
        if !oid.is_empty() {
            return Some(oid.to_string());
        }
    }

    let packed_refs = std::fs::read_to_string(common_dir.join("packed-refs")).ok()?;
    for line in packed_refs.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('^') {
            continue;
        }
        let mut parts = line.split_whitespace();
        let oid = parts.next()?;
        let name = parts.next()?;
        if name == full_ref {
            return Some(oid.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::workspace::git::test_support::run_git;

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

    #[test]
    fn git_branch_reads_symbolic_head_from_reftable_repo() {
        let root = temp_test_dir("reftable-branch");
        let root_arg = root.to_string_lossy().to_string();
        let output = std::process::Command::new("git")
            .args(["init", "--ref-format=reftable", "-b", "main", &root_arg])
            .output()
            .unwrap();
        if !output.status.success() {
            std::fs::remove_dir_all(root).unwrap();
            return;
        }

        assert_eq!(git_branch(&root).as_deref(), Some("main"));

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_repo_root_ignores_invalid_git_marker() {
        let base = temp_test_dir("invalid-git-root");
        let cwd = base.join("workspace");
        std::fs::create_dir_all(base.join(".git")).unwrap();
        std::fs::create_dir_all(&cwd).unwrap();

        assert_eq!(git_repo_root(&cwd), None);

        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn derive_label_prefers_repo_root_name() {
        let root = temp_test_dir("label-repo");
        let nested = root.join("nested");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(
            derive_label_from_cwd(&nested),
            root.file_name().and_then(|name| name.to_str()).unwrap()
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn derive_label_uses_path_name_outside_git() {
        let root = temp_test_dir("label-plain");
        let label = root.file_name().and_then(|name| name.to_str()).unwrap();

        assert_eq!(derive_label_from_cwd(Path::new(&root)), label);

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn normalize_git_remote_url_folds_transport_variants() {
        for url in [
            "git@github.com:gerchowl/herdr.git",
            "https://github.com/gerchowl/herdr.git",
            "https://GitHub.com/gerchowl/herdr",
            "ssh://git@github.com:22/gerchowl/herdr.git",
            "git://github.com/gerchowl/herdr.git/",
            "https://user:token@github.com/gerchowl/herdr.git",
        ] {
            assert_eq!(
                normalize_git_remote_url(url),
                "github.com/gerchowl/herdr",
                "url: {url}"
            );
        }
    }

    #[test]
    fn normalize_git_remote_url_keeps_path_case_and_local_paths() {
        assert_eq!(
            normalize_git_remote_url("git@gitlab.psi.ch:Group/MyRepo.git"),
            "gitlab.psi.ch/Group/MyRepo"
        );
        assert_eq!(
            normalize_git_remote_url("/srv/git/herdr.git/"),
            "/srv/git/herdr"
        );
        assert_eq!(
            normalize_git_remote_url("C:\\repos\\herdr"),
            "C:\\repos\\herdr"
        );
    }

    #[test]
    fn project_key_reads_origin_from_git_config() {
        let root = temp_test_dir("project-key");
        let git_dir = root.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(
            git_dir.join("config"),
            "[core]\n\trepositoryformatversion = 0\n[remote \"origin\"]\n\turl = git@github.com:gerchowl/herdr.git\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        )
        .unwrap();

        assert_eq!(
            project_key_for_common_dir(&git_dir, "herdr"),
            "github.com/gerchowl/herdr"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn project_key_falls_back_to_dir_label_without_origin() {
        let root = temp_test_dir("project-key-no-origin");
        let git_dir = root.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        std::fs::write(git_dir.join("config"), "[core]\n\tbare = false\n").unwrap();

        assert_eq!(
            project_key_for_common_dir(&git_dir, "scratch"),
            "dir:scratch"
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn read_subsection_value_ignores_other_remotes() {
        let root = temp_test_dir("subsection-remotes");
        let config = root.join("config");
        std::fs::write(
            &config,
            "[remote \"upstream\"]\n\turl = git@github.com:ogulcancelik/herdr.git\n[Remote \"origin\"]\n\turl = git@github.com:gerchowl/herdr.git # fork\n",
        )
        .unwrap();

        assert_eq!(
            read_git_config_subsection_value(&config, "remote", "origin", "url").as_deref(),
            Some("git@github.com:gerchowl/herdr.git")
        );
        assert_eq!(
            read_git_config_subsection_value(&config, "remote", "missing", "url"),
            None
        );

        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn git_rev_parse_verify_reads_reftable_refs() {
        let root = temp_test_dir("reftable-ref-oid");
        let root_arg = root.to_string_lossy().to_string();
        let output = std::process::Command::new("git")
            .args(["init", "--ref-format=reftable", "-b", "main", &root_arg])
            .output()
            .unwrap();
        if !output.status.success() {
            std::fs::remove_dir_all(root).unwrap();
            return;
        }

        run_git(&root, &["config", "user.email", "herdr@example.invalid"]);
        run_git(&root, &["config", "user.name", "Herdr Test"]);
        run_git(&root, &["commit", "--allow-empty", "-m", "initial"]);

        let head_oid = git_rev_parse_verify(&root, "HEAD").unwrap();

        assert_eq!(
            git_rev_parse_verify(&root, "refs/heads/main").as_deref(),
            Some(head_oid.as_str())
        );

        std::fs::remove_dir_all(root).unwrap();
    }
}
