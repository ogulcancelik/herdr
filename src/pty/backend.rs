#[cfg(unix)]
mod unix;

#[cfg(unix)]
pub(crate) use unix::*;

#[cfg(windows)]
use std::{
    ffi::{OsStr, OsString},
    sync::Mutex,
};

#[cfg(windows)]
use portable_pty::{native_pty_system, Child, CommandBuilder, MasterPty, PtyPair, PtySize};

#[cfg(windows)]
static CONPTY_LOAD_ENV_LOCK: Mutex<()> = Mutex::new(());

#[cfg(windows)]
pub(crate) struct SpawnedPty {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
}

#[cfg(windows)]
pub(crate) fn spawn_with_portable_pty(
    rows: u16,
    cols: u16,
    cmd: CommandBuilder,
) -> std::io::Result<SpawnedPty> {
    let pair = openpty_preferring_system_conpty(PtySize {
        rows,
        cols,
        pixel_width: 0,
        pixel_height: 0,
    })?;
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| std::io::Error::other(err.to_string()))?;

    Ok(SpawnedPty {
        master: pair.master,
        child,
    })
}

#[cfg(windows)]
fn openpty_preferring_system_conpty(size: PtySize) -> std::io::Result<PtyPair> {
    let _guard = CONPTY_LOAD_ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _path_guard = ConptyPathGuard::install();
    native_pty_system()
        .openpty(size)
        .map_err(|err| std::io::Error::other(err.to_string()))
}

#[cfg(windows)]
struct ConptyPathGuard {
    original: Option<OsString>,
    changed: bool,
}

#[cfg(windows)]
impl ConptyPathGuard {
    fn install() -> Self {
        let original = std::env::var_os("PATH");
        if let Some(path) = original.as_deref() {
            if let Some(sanitized) = path_without_sideloaded_conpty(path) {
                if sanitized.as_os_str() != path {
                    std::env::set_var("PATH", &sanitized);
                    return Self {
                        original,
                        changed: true,
                    };
                }
            }
        }

        Self {
            original,
            changed: false,
        }
    }
}

#[cfg(windows)]
impl Drop for ConptyPathGuard {
    fn drop(&mut self) {
        if !self.changed {
            return;
        }

        match &self.original {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
    }
}

#[cfg(windows)]
fn path_without_sideloaded_conpty(path: &OsStr) -> Option<OsString> {
    let entries = std::env::split_paths(path)
        .filter(|entry| !entry.join("conpty.dll").is_file())
        .collect::<Vec<_>>();
    std::env::join_paths(entries).ok()
}

#[cfg(all(test, windows))]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn conpty_path_filter_removes_directories_with_conpty_dll() {
        let base =
            std::env::temp_dir().join(format!("herdr-conpty-path-filter-{}", std::process::id()));
        let sideload = base.join("wezterm");
        let keep = base.join("keep");
        fs::create_dir_all(&sideload).unwrap();
        fs::create_dir_all(&keep).unwrap();
        fs::write(sideload.join("conpty.dll"), b"fixture").unwrap();

        let path = std::env::join_paths([sideload.clone(), keep.clone()]).unwrap();

        let filtered = path_without_sideloaded_conpty(&path).unwrap();
        let entries = std::env::split_paths(&filtered).collect::<Vec<_>>();

        assert_eq!(entries, vec![keep]);

        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn conpty_path_filter_preserves_directories_without_conpty_dll() {
        let base =
            std::env::temp_dir().join(format!("herdr-conpty-path-preserve-{}", std::process::id()));
        let first = base.join("first");
        let second = base.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();

        let path = std::env::join_paths([first.clone(), second.clone()]).unwrap();

        let filtered = path_without_sideloaded_conpty(&path).unwrap();
        let entries = std::env::split_paths(&filtered).collect::<Vec<_>>();

        assert_eq!(entries, vec![first, second]);

        let _ = fs::remove_dir_all(base);
    }
}
