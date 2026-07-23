#[cfg(unix)]
use std::{
    os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
    sync::Arc,
    time::{Duration, Instant},
};

#[cfg(unix)]
pub(crate) fn duplicate_fd(fd: RawFd) -> std::io::Result<RawFd> {
    let duplicated = unsafe { libc::dup(fd) };
    if duplicated < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(duplicated)
}

#[cfg(unix)]
pub(crate) fn set_cloexec(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn set_nonblocking(fd: RawFd) -> std::io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn duplicate_cloexec_fd(fd: RawFd) -> std::io::Result<RawFd> {
    let duplicated = duplicate_fd(fd)?;
    if let Err(err) = set_cloexec(duplicated) {
        let _ = unsafe { libc::close(duplicated) };
        return Err(err);
    }
    Ok(duplicated)
}

#[cfg(unix)]
#[derive(Clone)]
pub(crate) struct WakeWriter {
    fd: Arc<OwnedFd>,
}

#[cfg(unix)]
impl WakeWriter {
    pub(crate) fn wake(&self) -> std::io::Result<()> {
        loop {
            let byte = [1u8];
            let written =
                unsafe { libc::write(self.fd.as_raw_fd(), byte.as_ptr().cast(), byte.len()) };
            if written >= 0 {
                return Ok(());
            }

            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::WouldBlock {
                return Ok(());
            }
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

#[cfg(unix)]
pub(crate) struct WakePipe {
    pub(crate) read_fd: OwnedFd,
    pub(crate) writer: WakeWriter,
}

#[cfg(unix)]
pub(crate) fn create_wake_pipe() -> std::io::Result<WakePipe> {
    let mut fds = [-1; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    for fd in [read_fd.as_raw_fd(), write_fd.as_raw_fd()] {
        set_cloexec(fd).and_then(|_| set_nonblocking(fd))?;
    }

    Ok(WakePipe {
        read_fd,
        writer: WakeWriter {
            fd: Arc::new(write_fd),
        },
    })
}

#[cfg(unix)]
pub(crate) fn drain_wake_fd(fd: RawFd) -> std::io::Result<()> {
    let mut buf = [0u8; 64];
    loop {
        let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if read == 0 {
            return Ok(());
        }
        if read > 0 {
            continue;
        }

        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(());
        }
        if err.kind() != std::io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

#[cfg(unix)]
#[derive(Default)]
pub(crate) struct PtyWakeReadiness {
    pub(crate) pty_read_ready: bool,
    pub(crate) pty_write_ready: bool,
    pub(crate) wake_ready: bool,
}

#[cfg(unix)]
pub(crate) fn poll_pty_and_wake(
    pty_fd: RawFd,
    wake_fd: RawFd,
    poll_pty_read: bool,
    poll_pty_write: bool,
    timeout_ms: i32,
) -> std::io::Result<PtyWakeReadiness> {
    let poll_pty = poll_pty_read || poll_pty_write;
    let mut pty_events = 0;
    if poll_pty_read {
        pty_events |= libc::POLLIN;
    }
    if poll_pty_write {
        pty_events |= libc::POLLOUT;
    }

    let mut poll_fds = [
        libc::pollfd {
            fd: if poll_pty { pty_fd } else { -1 },
            events: pty_events,
            revents: 0,
        },
        libc::pollfd {
            fd: wake_fd,
            events: libc::POLLIN,
            revents: 0,
        },
    ];

    let deadline =
        (timeout_ms >= 0).then(|| Instant::now() + Duration::from_millis(timeout_ms as u64));
    let mut remaining_timeout_ms = timeout_ms;
    loop {
        for poll_fd in &mut poll_fds {
            poll_fd.revents = 0;
        }
        let result = unsafe {
            libc::poll(
                poll_fds.as_mut_ptr(),
                poll_fds.len() as _,
                remaining_timeout_ms,
            )
        };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                let Some(deadline) = deadline else {
                    continue;
                };
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Ok(PtyWakeReadiness::default());
                }
                remaining_timeout_ms = remaining.as_millis().clamp(1, i32::MAX as u128) as i32;
                continue;
            }
            return Err(err);
        }

        let pty_revents = if poll_pty { poll_fds[0].revents } else { 0 };
        let wake_revents = poll_fds[1].revents;
        if (pty_revents | wake_revents) & libc::POLLNVAL != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "poll encountered invalid PTY actor fd",
            ));
        }
        if pty_revents & libc::POLLERR != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "poll encountered PTY fd error",
            ));
        }

        return Ok(PtyWakeReadiness {
            pty_read_ready: pty_revents & (libc::POLLIN | libc::POLLHUP) != 0,
            pty_write_ready: pty_revents & (libc::POLLOUT | libc::POLLHUP) != 0,
            wake_ready: wake_revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0,
        });
    }
}

#[cfg(unix)]
pub(crate) fn resize_pty_fd(
    fd: RawFd,
    rows: u16,
    cols: u16,
    cell_width_px: u32,
    cell_height_px: u32,
) -> std::io::Result<()> {
    let size = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: (cols as u32)
            .saturating_mul(cell_width_px)
            .min(u16::MAX as u32) as u16,
        ws_ypixel: (rows as u32)
            .saturating_mul(cell_height_px)
            .min(u16::MAX as u32) as u16,
    };
    if unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &size) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}
