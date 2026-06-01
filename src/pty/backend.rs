use std::os::fd::{FromRawFd, OwnedFd};

use portable_pty::{native_pty_system, Child, CommandBuilder, PtySize};

use crate::pty::fd;

pub(crate) struct SpawnedPty {
    pub master_fd: OwnedFd,
    pub child: Box<dyn Child + Send + Sync>,
}

pub(crate) fn spawn_with_portable_pty(
    rows: u16,
    cols: u16,
    cmd: CommandBuilder,
) -> std::io::Result<SpawnedPty> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    let master_fd = pair
        .master
        .as_raw_fd()
        .ok_or_else(|| std::io::Error::other("pty master fd is unavailable"))?;
    let actor_fd = fd::duplicate_cloexec_fd(master_fd)?;
    let actor_fd = unsafe { OwnedFd::from_raw_fd(actor_fd) };
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    drop(pair);

    Ok(SpawnedPty {
        master_fd: actor_fd,
        child,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    fn pty_fd_test_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn parent_pty_fd_targets() -> Vec<String> {
        let Ok(entries) = std::fs::read_dir("/proc/self/fd") else {
            return Vec::new();
        };
        let mut targets: Vec<String> = entries
            .filter_map(Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .map(|target| target.to_string_lossy().into_owned())
            .filter(|target| target.starts_with("/dev/pts/") || target == "/dev/ptmx")
            .collect();
        targets.sort();
        targets
    }

    fn parent_pty_fd_count() -> usize {
        parent_pty_fd_targets().len()
    }

    #[test]
    fn portable_pty_setup_leaves_one_parent_pty_fd() {
        let _guard = pty_fd_test_lock().lock().expect("pty fd test lock");
        let before = parent_pty_fd_count();
        let mut cmd = CommandBuilder::new("/bin/cat");
        cmd.env(crate::HERDR_ENV_VAR, crate::HERDR_ENV_VALUE);

        let mut spawned =
            spawn_with_portable_pty(24, 80, cmd).expect("portable pty setup succeeds");
        let after_spawn = parent_pty_fd_count();

        assert_eq!(
            after_spawn,
            before + 1,
            "portable-pty setup should leave only the Herdr-owned master fd in the parent: {:?}",
            parent_pty_fd_targets()
        );

        let _ = spawned.child.kill();
        let _ = spawned.child.wait();
        drop(spawned.master_fd);
    }
}
