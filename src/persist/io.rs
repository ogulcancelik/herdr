use std::path::{Path, PathBuf};

use tracing::{error, info, warn};

use super::snapshot::{parse_snapshot, snapshot_file_version, SessionSnapshot, SNAPSHOT_VERSION};

fn session_path() -> PathBuf {
    crate::config::config_dir().join("session.json")
}

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

pub fn save(snapshot: &SessionSnapshot) {
    let path = session_path();
    if let Err(err) = save_to_path(&path, snapshot) {
        error!(err = %err, path = %path.display(), "failed to save session");
        return;
    }
    info!(workspaces = snapshot.workspaces.len(), "session saved");
}

pub fn clear() {
    let path = session_path();
    if let Err(err) = clear_path(&path) {
        error!(err = %err, path = %path.display(), "failed to clear session");
        return;
    }
    info!(path = %path.display(), "session cleared");
}

pub fn load() -> Option<SessionSnapshot> {
    let path = session_path();
    if !path.exists() {
        return None;
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(err) => {
            warn!(err = %err, "failed to read session file");
            return None;
        }
    };
    match parse_snapshot(&content) {
        Ok(snapshot) => Some(snapshot),
        Err(err) => {
            if let Some(version) = snapshot_file_version(&content) {
                if version > SNAPSHOT_VERSION {
                    warn!(
                        file_version = version,
                        supported = SNAPSHOT_VERSION,
                        "session file is from a newer herdr version, ignoring"
                    );
                    return None;
                }
            }
            warn!(err = %err, "failed to parse session file, ignoring");
            None
        }
    }
}
