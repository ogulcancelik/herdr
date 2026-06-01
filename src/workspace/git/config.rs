use std::path::{Path, PathBuf};

use super::discovery::{canonicalize_best_effort_path, GitWorktreeInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BranchConfig {
    pub(super) remote: String,
    pub(super) merge_ref: String,
    fetch_refspecs: Vec<(String, String)>,
    remote_urls: Vec<(String, String)>,
}

pub(super) fn read_branch_config(info: &GitWorktreeInfo, branch: &str) -> Option<BranchConfig> {
    read_branch_config_with_user_paths(info, branch, git_user_config_paths())
}

pub(super) fn read_branch_config_with_user_paths(
    info: &GitWorktreeInfo,
    branch: &str,
    user_config_paths: Vec<PathBuf>,
) -> Option<BranchConfig> {
    let worktree_config_enabled =
        worktree_config_enabled(&info.git_common_dir.join("config"), info);
    let config_paths = user_config_paths
        .into_iter()
        .chain(std::iter::once(info.git_common_dir.join("config")))
        .collect::<Vec<_>>();
    let mut remote_urls = Vec::new();
    for path in &config_paths {
        let mut include_stack = Vec::new();
        collect_remote_urls(path, info, branch, &mut remote_urls, &mut include_stack);
    }
    let mut config = BranchConfig {
        remote: String::new(),
        merge_ref: String::new(),
        fetch_refspecs: Vec::new(),
        remote_urls,
    };
    for path in config_paths {
        let mut include_stack = Vec::new();
        merge_git_config(&mut config, &path, branch, info, true, &mut include_stack);
    }
    if worktree_config_enabled {
        let mut include_stack = Vec::new();
        merge_git_config(
            &mut config,
            &info.git_dir.join("config.worktree"),
            branch,
            info,
            false,
            &mut include_stack,
        );
    }
    (!config.remote.is_empty() && !config.merge_ref.is_empty()).then_some(config)
}

fn git_user_config_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(xdg_config_home) = std::env::var_os("XDG_CONFIG_HOME") {
        paths.push(PathBuf::from(xdg_config_home).join("git/config"));
    } else if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".config/git/config"));
    }
    if let Some(home) = std::env::var_os("HOME") {
        paths.push(PathBuf::from(home).join(".gitconfig"));
    }
    paths
}

fn worktree_config_enabled(path: &Path, info: &GitWorktreeInfo) -> bool {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    let mut section = ConfigSection::Other;
    let mut enabled = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if let Some(section_name) = extract_config_section(line) {
            let is_extensions = section_name.eq_ignore_ascii_case("extensions");
            section = if is_extensions {
                ConfigSection::Extensions
            } else {
                parse_config_section(
                    section_name,
                    "",
                    info,
                    path,
                    &BranchConfig {
                        remote: String::new(),
                        merge_ref: String::new(),
                        fetch_refspecs: Vec::new(),
                        remote_urls: Vec::new(),
                    },
                )
            };
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = normalize_config_value(value);
            match &section {
                ConfigSection::Extensions if key.eq_ignore_ascii_case("worktreeConfig") => {
                    enabled = matches!(
                        value.to_ascii_lowercase().as_str(),
                        "true" | "1" | "yes" | "on"
                    );
                }
                _ => {}
            }
            continue;
        }
        if matches!(section, ConfigSection::Extensions)
            && line.eq_ignore_ascii_case("worktreeConfig")
        {
            enabled = true;
        }
    }
    enabled
}

fn collect_remote_urls(
    path: &Path,
    info: &GitWorktreeInfo,
    branch: &str,
    remote_urls: &mut Vec<(String, String)>,
    include_stack: &mut Vec<PathBuf>,
) {
    let path = canonicalize_best_effort_path(path);
    if include_stack.contains(&path) {
        return;
    }
    include_stack.push(path.clone());
    let Ok(contents) = std::fs::read_to_string(&path) else {
        include_stack.pop();
        return;
    };
    let mut section = ConfigSection::Other;
    let dummy_config = BranchConfig {
        remote: String::new(),
        merge_ref: String::new(),
        fetch_refspecs: Vec::new(),
        remote_urls: remote_urls.clone(),
    };
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if let Some(section_name) = extract_config_section(line) {
            section = parse_config_section(section_name, branch, info, &path, &dummy_config);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = normalize_config_value(value);
        match &section {
            ConfigSection::Remote(remote) if key.eq_ignore_ascii_case("url") => {
                remote_urls.push((remote.clone(), value));
            }
            ConfigSection::Include if key.eq_ignore_ascii_case("path") => {
                collect_remote_urls(
                    &resolve_include_path(&path, &value),
                    info,
                    branch,
                    remote_urls,
                    include_stack,
                );
            }
            ConfigSection::IncludeIf(IncludeIfMode::Enabled)
                if key.eq_ignore_ascii_case("path") =>
            {
                collect_remote_urls(
                    &resolve_include_path(&path, &value),
                    info,
                    branch,
                    remote_urls,
                    include_stack,
                );
            }
            _ => {}
        }
    }
    include_stack.pop();
}

fn merge_git_config(
    config: &mut BranchConfig,
    path: &Path,
    branch: &str,
    info: &GitWorktreeInfo,
    collect_hasconfig_urls: bool,
    include_stack: &mut Vec<PathBuf>,
) {
    let path = canonicalize_best_effort_path(path);
    if include_stack.contains(&path) {
        return;
    }
    include_stack.push(path.clone());
    let Ok(contents) = std::fs::read_to_string(&path) else {
        include_stack.pop();
        return;
    };
    let mut section = ConfigSection::Other;

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if let Some(section_name) = extract_config_section(line) {
            section = parse_config_section(section_name, branch, info, &path, config);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = normalize_config_value(value);
        match &section {
            ConfigSection::Branch if key.eq_ignore_ascii_case("remote") => config.remote = value,
            ConfigSection::Branch if key.eq_ignore_ascii_case("merge") => config.merge_ref = value,
            ConfigSection::Remote(remote) if key.eq_ignore_ascii_case("fetch") => {
                config.fetch_refspecs.push((remote.clone(), value));
            }
            ConfigSection::Remote(remote)
                if collect_hasconfig_urls && key.eq_ignore_ascii_case("url") =>
            {
                config.remote_urls.push((remote.clone(), value));
            }
            ConfigSection::Include if key.eq_ignore_ascii_case("path") => {
                let include_path = resolve_include_path(&path, &value);
                merge_git_config(
                    config,
                    &include_path,
                    branch,
                    info,
                    collect_hasconfig_urls,
                    include_stack,
                );
            }
            ConfigSection::IncludeIf(IncludeIfMode::Enabled)
                if key.eq_ignore_ascii_case("path") =>
            {
                let include_path = resolve_include_path(&path, &value);
                merge_git_config(
                    config,
                    &include_path,
                    branch,
                    info,
                    collect_hasconfig_urls,
                    include_stack,
                );
            }
            ConfigSection::IncludeIf(IncludeIfMode::HasConfig)
                if key.eq_ignore_ascii_case("path") =>
            {
                let include_path = resolve_include_path(&path, &value);
                if !included_config_defines_remote_url(
                    &include_path,
                    branch,
                    info,
                    config,
                    include_stack,
                ) {
                    merge_git_config(
                        config,
                        &include_path,
                        branch,
                        info,
                        collect_hasconfig_urls,
                        include_stack,
                    );
                }
            }
            _ => {}
        }
    }
    include_stack.pop();
}

enum ConfigSection {
    Branch,
    Extensions,
    Include,
    IncludeIf(IncludeIfMode),
    Remote(String),
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IncludeIfMode {
    Disabled,
    Enabled,
    HasConfig,
}

fn extract_config_section(line: &str) -> Option<&str> {
    if !line.starts_with('[') {
        return None;
    }
    let mut in_quotes = false;
    let mut escaped = false;
    for (index, ch) in line.char_indices().skip(1) {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            ']' if !in_quotes => {
                let rest = line[index + 1..].trim();
                if rest.is_empty() || rest.starts_with('#') || rest.starts_with(';') {
                    return Some(&line[1..index]);
                }
                return None;
            }
            _ => {}
        }
    }
    None
}

fn parse_config_section(
    section: &str,
    branch: &str,
    info: &GitWorktreeInfo,
    config_path: &Path,
    config: &BranchConfig,
) -> ConfigSection {
    if let Some(name) = quoted_config_subsection(section, "branch") {
        return if name == branch {
            ConfigSection::Branch
        } else {
            ConfigSection::Other
        };
    }
    if let Some(name) = quoted_config_subsection(section, "remote") {
        return ConfigSection::Remote(name.to_string());
    }
    if section.eq_ignore_ascii_case("include") {
        return ConfigSection::Include;
    }
    if let Some(condition) = quoted_config_subsection(section, "includeIf") {
        return ConfigSection::IncludeIf(include_if_mode(
            condition,
            info,
            config_path,
            branch,
            config,
        ));
    }
    ConfigSection::Other
}

fn include_if_mode(
    condition: &str,
    info: &GitWorktreeInfo,
    config_path: &Path,
    branch: &str,
    config: &BranchConfig,
) -> IncludeIfMode {
    let (case_insensitive, pattern) = if let Some(pattern) = condition.strip_prefix("gitdir/i:") {
        (true, pattern)
    } else if let Some(pattern) = condition.strip_prefix("gitdir:") {
        (false, pattern)
    } else if let Some(pattern) = condition.strip_prefix("onbranch:") {
        let pattern = normalize_branch_include_pattern(pattern);
        return if wildcard_match(&pattern, branch, false) {
            IncludeIfMode::Enabled
        } else {
            IncludeIfMode::Disabled
        };
    } else if let Some(pattern) = condition.strip_prefix("hasconfig:remote.*.url:") {
        return if config
            .remote_urls
            .iter()
            .any(|(_, url)| wildcard_match(pattern, url, false))
        {
            IncludeIfMode::HasConfig
        } else {
            IncludeIfMode::Disabled
        };
    } else {
        return IncludeIfMode::Disabled;
    };
    let pattern = normalize_gitdir_include_pattern(pattern, config_path);
    let candidates = [
        info.git_dir.display().to_string(),
        info.git_common_dir.display().to_string(),
        info.repo_root.join(".git").display().to_string(),
    ];
    if candidates
        .iter()
        .any(|candidate| wildcard_match(&pattern, candidate, case_insensitive))
    {
        IncludeIfMode::Enabled
    } else {
        IncludeIfMode::Disabled
    }
}

fn included_config_defines_remote_url(
    path: &Path,
    branch: &str,
    info: &GitWorktreeInfo,
    config: &BranchConfig,
    include_stack: &mut Vec<PathBuf>,
) -> bool {
    let path = canonicalize_best_effort_path(path);
    if include_stack.contains(&path) {
        return false;
    }
    include_stack.push(path.clone());
    let Ok(contents) = std::fs::read_to_string(&path) else {
        include_stack.pop();
        return false;
    };
    let mut section = ConfigSection::Other;
    let mut defines_remote_url = false;
    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if let Some(section_name) = extract_config_section(line) {
            section = parse_config_section(section_name, branch, info, &path, config);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if matches!(section, ConfigSection::Remote(_)) && key.eq_ignore_ascii_case("url") {
            defines_remote_url = true;
            break;
        }
        let value = normalize_config_value(value);
        match &section {
            ConfigSection::Include if key.eq_ignore_ascii_case("path") => {
                let include_path = resolve_include_path(&path, &value);
                if !included_config_defines_remote_url(
                    &include_path,
                    branch,
                    info,
                    config,
                    include_stack,
                ) {
                    continue;
                }
                defines_remote_url = true;
                break;
            }
            ConfigSection::IncludeIf(IncludeIfMode::Enabled | IncludeIfMode::HasConfig)
                if key.eq_ignore_ascii_case("path") =>
            {
                let include_path = resolve_include_path(&path, &value);
                if !included_config_defines_remote_url(
                    &include_path,
                    branch,
                    info,
                    config,
                    include_stack,
                ) {
                    continue;
                }
                defines_remote_url = true;
                break;
            }
            _ => {}
        }
    }
    include_stack.pop();
    defines_remote_url
}

fn normalize_branch_include_pattern(pattern: &str) -> String {
    if pattern.ends_with('/') {
        format!("{pattern}**")
    } else {
        pattern.to_string()
    }
}

fn normalize_gitdir_include_pattern(pattern: &str, config_path: &Path) -> String {
    let mut pattern = if let Some(rest) = pattern.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()
            .join(rest)
            .display()
            .to_string()
    } else if let Some(rest) = pattern.strip_prefix("./") {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(rest)
            .display()
            .to_string()
    } else if Path::new(pattern).is_absolute() {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    };
    if pattern.ends_with('/') {
        pattern.push_str("**");
    }
    pattern
}

fn wildcard_match(pattern: &str, value: &str, case_insensitive: bool) -> bool {
    let pattern = if case_insensitive {
        pattern.to_ascii_lowercase()
    } else {
        pattern.to_string()
    };
    let value = if case_insensitive {
        value.to_ascii_lowercase()
    } else {
        value.to_string()
    };
    wildcard_match_bytes(pattern.as_bytes(), value.as_bytes())
}

fn wildcard_match_bytes(pattern: &[u8], value: &[u8]) -> bool {
    match pattern.split_first() {
        None => value.is_empty(),
        Some((&b'*', rest)) => {
            wildcard_match_bytes(rest, value)
                || (!value.is_empty() && wildcard_match_bytes(pattern, &value[1..]))
        }
        Some((&expected, rest)) => value.split_first().is_some_and(|(&actual, value_rest)| {
            actual == expected && wildcard_match_bytes(rest, value_rest)
        }),
    }
}

fn quoted_config_subsection<'a>(section: &'a str, name: &str) -> Option<&'a str> {
    let prefix_len = name.len() + 2;
    if section.len() <= prefix_len {
        return None;
    }
    let prefix = &section[..prefix_len];
    if !prefix.eq_ignore_ascii_case(&format!("{name} \"")) {
        return None;
    }
    section[prefix_len..].strip_suffix('"')
}

fn resolve_include_path(config_path: &Path, include_path: &str) -> PathBuf {
    let include_path = include_path.strip_prefix("~/").map_or_else(
        || PathBuf::from(include_path),
        |rest| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default()
                .join(rest)
        },
    );
    if include_path.is_absolute() {
        include_path
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(include_path)
    }
}

fn normalize_config_value(value: &str) -> String {
    let value = value.trim();
    let mut in_quotes = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match ch {
            '\\' if in_quotes => escaped = true,
            '"' => in_quotes = !in_quotes,
            '#' | ';'
                if !in_quotes
                    && value[..index]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_whitespace) =>
            {
                return unquote_config_value(value[..index].trim());
            }
            _ => {}
        }
    }
    unquote_config_value(value)
}

fn unquote_config_value(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .to_string()
}

pub(super) fn upstream_full_ref(config: &BranchConfig) -> Option<String> {
    if config.remote == "." {
        return Some(config.merge_ref.clone());
    }
    let default_refspec = format!("+refs/heads/*:refs/remotes/{}/*", config.remote);
    let remote_refspecs = config
        .fetch_refspecs
        .iter()
        .filter(|(remote, _)| remote == &config.remote)
        .map(|(_, refspec)| refspec);
    let refspecs = remote_refspecs.collect::<Vec<_>>();
    if refspecs.is_empty() {
        return map_fetch_refspec(&default_refspec, &config.merge_ref).into_ref();
    }
    refspecs
        .into_iter()
        .find_map(|refspec| map_fetch_refspec(refspec, &config.merge_ref).into_ref())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum FetchRefspecMatch {
    Ref(String),
    NoMatch,
}

impl FetchRefspecMatch {
    fn into_ref(self) -> Option<String> {
        match self {
            FetchRefspecMatch::Ref(value) => Some(value),
            FetchRefspecMatch::NoMatch => None,
        }
    }
}

fn map_fetch_refspec(refspec: &str, merge_ref: &str) -> FetchRefspecMatch {
    let refspec = refspec.strip_prefix('+').unwrap_or(refspec);
    if refspec.starts_with('^') {
        return FetchRefspecMatch::NoMatch;
    }
    let Some((source, destination)) = refspec.split_once(':') else {
        return FetchRefspecMatch::NoMatch;
    };
    match (source.split_once('*'), destination.split_once('*')) {
        (None, None) => {
            if source == merge_ref {
                FetchRefspecMatch::Ref(destination.to_string())
            } else {
                FetchRefspecMatch::NoMatch
            }
        }
        (Some((source_prefix, source_suffix)), Some((destination_prefix, destination_suffix))) => {
            let Some(matched) = merge_ref
                .strip_prefix(source_prefix)
                .and_then(|matched| matched.strip_suffix(source_suffix))
            else {
                return FetchRefspecMatch::NoMatch;
            };
            FetchRefspecMatch::Ref(format!("{destination_prefix}{matched}{destination_suffix}"))
        }
        _ => FetchRefspecMatch::NoMatch,
    }
}
