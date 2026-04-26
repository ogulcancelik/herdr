//! Session persistence — save/restore workspaces, layouts, and working directories.
//!
//! Stored at `~/.config/herdr/session.json`.

mod io;
mod restore;
mod snapshot;

#[allow(unused_imports)]
pub use self::io::{
    clear, clear_session, delete_session, list_session_names, load, load_session, save,
    save_session, session_path_for, sessions_dir, validate_session_name, SessionId,
    SessionLoadError, SessionName,
};
pub use self::restore::restore;
pub(crate) use self::snapshot::SNAPSHOT_VERSION;
pub use self::snapshot::{
    capture, DirectionSnapshot, LayoutSnapshot, SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
};
