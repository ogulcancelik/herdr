#[cfg(unix)]
use std::os::fd::RawFd;

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
pub(crate) fn poll_read_ready(fd: RawFd, timeout_ms: i32) -> std::io::Result<bool> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(result > 0 && (poll_fd.revents & (libc::POLLIN | libc::POLLHUP)) != 0);
    }
}

#[cfg(unix)]
pub(crate) fn poll_write_ready(fd: RawFd, timeout_ms: i32) -> std::io::Result<bool> {
    let mut poll_fd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    loop {
        let result = unsafe { libc::poll(&mut poll_fd, 1, timeout_ms) };
        if result < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(result > 0 && (poll_fd.revents & (libc::POLLOUT | libc::POLLHUP)) != 0);
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
