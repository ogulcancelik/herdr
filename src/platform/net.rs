//! Platform-agnostic Unix-domain socket types.
//!
//! On Unix this is a thin re-export of the standard `std::os::unix::net`
//! types. On Windows we use `uds_windows`, which wraps the native AF_UNIX
//! support shipped with Windows 10 1803+ and exposes the same blocking
//! `UnixStream`/`UnixListener` API surface that the rest of herdr was
//! written against.

#[cfg(unix)]
pub use std::os::unix::net::{UnixListener, UnixStream};

#[cfg(windows)]
pub use uds_windows::{UnixListener, UnixStream};
