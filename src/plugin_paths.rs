use std::path::{Path, PathBuf};

const PLUGIN_CONFIG_PATH_COMPONENT_MAX_CHARS: usize = 120;

pub(crate) fn plugin_config_dir(plugin_id: &str) -> PathBuf {
    crate::config::config_dir()
        .join("plugins")
        .join("config")
        .join(plugin_config_path_component(plugin_id))
}

pub(crate) fn plugin_state_dir(plugin_id: &str) -> PathBuf {
    crate::config::state_dir()
        .join("plugins")
        .join(plugin_config_path_component(plugin_id))
}

pub(crate) fn ensure_plugin_user_dirs(plugin_id: &str) -> std::io::Result<()> {
    ensure_plugin_config_dir(plugin_id)?;
    std::fs::create_dir_all(plugin_state_dir(plugin_id))?;
    Ok(())
}

fn ensure_plugin_config_dir(plugin_id: &str) -> std::io::Result<()> {
    let config_dir = plugin_config_dir(plugin_id);
    if config_dir.exists() {
        return std::fs::create_dir_all(config_dir);
    }
    if let Some(legacy_dir) = legacy_plugin_config_dirs(plugin_id)
        .into_iter()
        .find(|path| path.is_dir())
    {
        copy_dir_all(&legacy_dir, &config_dir)?;
        return Ok(());
    }
    std::fs::create_dir_all(config_dir)
}

fn legacy_plugin_config_dirs(plugin_id: &str) -> Vec<PathBuf> {
    let plugins_dir = crate::config::config_dir().join("plugins");
    let old_unhashed =
        (!matches!(plugin_id, "config" | "github")).then(|| plugins_dir.join(plugin_id));
    let current_hashed =
        plugins_dir.join(crate::api::schema::plugin_managed_path_component(plugin_id));
    let mut candidates = Vec::new();
    if let Some(old_unhashed) = old_unhashed {
        if old_unhashed != current_hashed {
            candidates.push(old_unhashed);
        }
    }
    candidates.push(current_hashed);
    candidates
}

fn plugin_config_path_component(value: &str) -> String {
    let mut component = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        {
            component.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(component, "%{byte:02X}");
        }
    }
    if component.ends_with('.') {
        component.pop();
        component.push_str("%2E");
    }
    if component.is_empty() {
        return "%plugin".to_string();
    }
    if crate::api::schema::has_windows_reserved_stem_for_path_component(&component) {
        component = format!("%{component}");
    }
    if component.chars().count() > PLUGIN_CONFIG_PATH_COMPONENT_MAX_CHARS {
        let hash = crate::api::schema::short_plugin_id_hash_for_path_component(value);
        let prefix_len = PLUGIN_CONFIG_PATH_COMPONENT_MAX_CHARS - hash.len() - 1;
        let prefix = component.chars().take(prefix_len).collect::<String>();
        return format!("{prefix}-{hash}");
    }
    component
}

fn copy_dir_all(source: &Path, destination: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(destination)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        let destination_path = destination.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &destination_path)?;
        } else {
            std::fs::copy(entry.path(), destination_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_config_path_component_is_readable_and_collision_free() {
        assert_eq!(
            plugin_config_path_component("examples.agent-telegram-notify"),
            "examples.agent-telegram-notify"
        );
        assert_eq!(plugin_config_path_component("example:a"), "example%3Aa");
        assert_eq!(plugin_config_path_component("Example"), "%45xample");
        assert_ne!(
            plugin_config_path_component("example:a"),
            plugin_config_path_component("example-a")
        );
        assert_ne!(
            plugin_config_path_component("Example"),
            plugin_config_path_component("example")
        );
        assert_ne!(
            plugin_config_path_component(&"A".repeat(120)),
            plugin_config_path_component(&"B".repeat(120))
        );
        assert!(plugin_config_path_component(&"A".repeat(120)).len() <= 120);
        assert_eq!(plugin_config_path_component("con"), "%con");
        assert_eq!(plugin_config_path_component("example."), "example%2E");
    }
}
