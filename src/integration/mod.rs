use std::fs;
use std::io;
use std::path::PathBuf;

use portable_pty::CommandBuilder;

use crate::layout::PaneId;

pub(crate) const HERDR_PANE_ID_ENV_VAR: &str = "HERDR_PANE_ID";
const PI_EXTENSION_INSTALL_NAME: &str = "herdr-agent-state.ts";
const PI_EXTENSION_ASSET: &str = include_str!("assets/pi/herdr-agent-state.ts");

pub(crate) fn apply_pane_env(cmd: &mut CommandBuilder, pane_id: PaneId) {
    cmd.env(crate::api::SOCKET_PATH_ENV_VAR, crate::api::socket_path());
    cmd.env(HERDR_PANE_ID_ENV_VAR, format!("p_{}", pane_id.raw()));
}

pub(crate) fn install_pi() -> io::Result<PathBuf> {
    let dir = pi_extension_dir()?;
    if !dir.is_dir() {
        return Err(io::Error::other(format!(
            "pi extension directory not found at {}. install pi and create the extensions directory first",
            dir.display()
        )));
    }

    let path = dir.join(PI_EXTENSION_INSTALL_NAME);
    fs::write(&path, PI_EXTENSION_ASSET)?;
    Ok(path)
}

fn pi_extension_dir() -> io::Result<PathBuf> {
    let home = std::env::var("HOME")
        .map(PathBuf::from)
        .map_err(|_| io::Error::other("HOME is not set; cannot locate ~/.pi/agent/extensions"))?;
    Ok(home.join(".pi/agent/extensions"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_base() -> PathBuf {
        std::env::temp_dir().join(format!(
            "herdr-integration-install-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn install_pi_writes_embedded_asset_to_pi_extensions_dir() {
        let base = unique_base();
        let home = base.join("home");
        let ext_dir = home.join(".pi/agent/extensions");
        fs::create_dir_all(&ext_dir).unwrap();
        std::env::set_var("HOME", &home);

        let path = install_pi().unwrap();
        let content = fs::read_to_string(&path).unwrap();

        assert_eq!(path, ext_dir.join(PI_EXTENSION_INSTALL_NAME));
        assert_eq!(content, PI_EXTENSION_ASSET);

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn install_pi_errors_when_extension_dir_missing() {
        let base = unique_base();
        let home = base.join("home");
        fs::create_dir_all(&home).unwrap();
        std::env::set_var("HOME", &home);

        let err = install_pi().unwrap_err().to_string();

        assert!(err.contains("pi extension directory not found"));

        std::env::remove_var("HOME");
        let _ = fs::remove_dir_all(base);
    }
}
