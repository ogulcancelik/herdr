use std::path::{Path, PathBuf};

use tracing::warn;

use super::snapshot::{parse_snapshot, snapshot_file_version, SessionSnapshot, SNAPSHOT_VERSION};

fn session_path() -> PathBuf {
    crate::config::config_dir().join("session.json")
}

fn sessions_dir_path() -> PathBuf {
    crate::config::config_dir().join("sessions")
}

// ---------------------------------------------------------------------------
// SessionName newtype
// ---------------------------------------------------------------------------

/// A validated session name.
/// Alphanumeric, hyphens, underscores; max 32 chars; rejects empty and reserved names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionName(pub String);

impl SessionName {
    #[allow(dead_code)]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for SessionName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// Validates a proposed session name.
/// Returns `Ok(SessionName)` if valid, `Err(message)` otherwise.
pub fn validate_session_name(name: &str) -> Result<SessionName, String> {
    if name.is_empty() {
        return Err("session name cannot be empty".into());
    }
    if name.len() > 32 {
        return Err("session name cannot exceed 32 characters".into());
    }
    if name.eq_ignore_ascii_case("default") {
        return Err("'default' is a reserved session name".into());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(
            "session name may only contain letters, digits, hyphens, and underscores".into(),
        );
    }
    Ok(SessionName(name.to_string()))
}

// ---------------------------------------------------------------------------
// SessionId
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionId {
    Default,
    Named(SessionName),
}

impl SessionId {
    pub fn is_default(&self) -> bool {
        matches!(self, SessionId::Default)
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionId::Default => write!(f, "default"),
            SessionId::Named(name) => write!(f, "{name}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Path helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub fn sessions_dir() -> PathBuf {
    sessions_dir_path()
}

pub fn session_path_for(id: &SessionId) -> PathBuf {
    match id {
        SessionId::Default => session_path(),
        SessionId::Named(name) => sessions_dir_path().join(format!("{}.json", name.0)),
    }
}

// ---------------------------------------------------------------------------
// Persistence API
// ---------------------------------------------------------------------------

pub(super) fn save_to_path(path: &Path, snapshot: &SessionSnapshot) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(snapshot)?;
    let tmp_path = path.with_extension("json.tmp");
    std::fs::write(&tmp_path, &json)?;
    if let Err(err) = std::fs::rename(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(err);
    }
    Ok(())
}

pub(super) fn clear_path(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// List all session names (excluding the default).
/// Returns only names, no active flag. Filters `.json`, skips dotfiles/tempfiles.
pub fn list_session_names() -> std::io::Result<Vec<SessionName>> {
    let dir = sessions_dir_path();
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || !name_str.ends_with(".json") {
            continue;
        }
        let stem = &name_str[..name_str.len() - 5];
        if let Ok(session_name) = validate_session_name(stem) {
            names.push(session_name);
        }
    }
    names.sort();
    Ok(names)
}

/// Load a snapshot for the given session id.
/// Missing default returns `Ok(None)`. Missing named returns `Err(SessionLoadError)`.
pub fn load_session(id: &SessionId) -> Result<Option<SessionSnapshot>, SessionLoadError> {
    let path = session_path_for(id);
    if !path.exists() {
        return if id.is_default() {
            Ok(None)
        } else {
            Err(SessionLoadError::NotFound)
        };
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            return Err(SessionLoadError::Io(err));
        }
    };
    match parse_snapshot(&content) {
        Ok(snapshot) => Ok(Some(snapshot)),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    return Err(SessionLoadError::NewerVersion {
                        file_version: version,
                        supported: SNAPSHOT_VERSION,
                    });
                }
            }
            Err(SessionLoadError::Parse(err.to_string()))
        }
    }
}

#[derive(Debug)]
pub enum SessionLoadError {
    NotFound,
    Io(std::io::Error),
    Parse(String),
    NewerVersion { file_version: u32, supported: u32 },
}

impl std::fmt::Display for SessionLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionLoadError::NotFound => write!(f, "session file not found"),
            SessionLoadError::Io(err) => write!(f, "failed to read session file: {err}"),
            SessionLoadError::Parse(msg) => write!(f, "failed to parse session file: {msg}"),
            SessionLoadError::NewerVersion {
                file_version,
                supported,
            } => {
                write!(
                    f,
                    "session file version {file_version} is newer than supported {supported}"
                )
            }
        }
    }
}

impl std::error::Error for SessionLoadError {}

/// Save a snapshot for the given session id.
pub fn save_session(id: &SessionId, snapshot: &SessionSnapshot) -> std::io::Result<()> {
    let path = session_path_for(id);
    if let Err(err) = save_to_path(&path, snapshot) {
        crate::logging::session_save_failed(&path, &err.to_string());
        return Err(err);
    }
    crate::logging::session_saved(&path, snapshot.workspaces.len());
    Ok(())
}

/// Clear (delete) the session file for the given session id.
pub fn clear_session(id: &SessionId) -> std::io::Result<()> {
    let path = session_path_for(id);
    if let Err(err) = clear_path(&path) {
        crate::logging::session_clear_failed(&path, &err.to_string());
        return Err(err);
    }
    crate::logging::session_cleared(&path);
    Ok(())
}

/// Delete a named session file.
pub fn delete_session(name: &SessionName) -> std::io::Result<()> {
    let path = sessions_dir_path().join(format!("{}.json", name.0));
    clear_path(&path)
}

// ---------------------------------------------------------------------------
// Backward-compatible default session wrappers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub fn save(snapshot: &SessionSnapshot) -> std::io::Result<()> {
    save_session(&SessionId::Default, snapshot)
}

#[allow(dead_code)]
pub fn clear() -> std::io::Result<()> {
    clear_session(&SessionId::Default)
}

pub fn load() -> Option<SessionSnapshot> {
    match load_session(&SessionId::Default) {
        Ok(snap) => snap,
        Err(err) => {
            warn!(err = %err, "failed to load default session");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::AgentPanelScope;

    fn temp_session_path(name: &str) -> PathBuf {
        let unique = format!(
            "herdr-session-tests-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        std::env::temp_dir().join(unique).join("session.json")
    }

    fn empty_snapshot() -> SessionSnapshot {
        SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces: vec![],
            active: None,
            selected: 0,
            agent_panel_scope: AgentPanelScope::CurrentWorkspace,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
        }
    }

    #[test]
    fn clear_path_removes_existing_session_file() {
        let path = temp_session_path("clear-existing");
        save_to_path(&path, &empty_snapshot()).unwrap();

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }

    #[test]
    fn clear_path_ignores_missing_session_file() {
        let path = temp_session_path("clear-missing");

        clear_path(&path).unwrap();

        assert!(!path.exists());
    }
}
