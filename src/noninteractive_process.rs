use std::{ffi::OsStr, process::Command};

/// Builds a subprocess whose stdio is controlled by the caller and which never opens a Windows console.
pub(crate) fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    crate::platform::configure_background_command(&mut command);
    command
}

pub(crate) fn curl_command() -> Command {
    command("curl")
}
