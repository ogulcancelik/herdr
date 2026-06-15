use crate::api::schema::InstalledPluginInfo;

pub(super) fn plugin_config_dir(plugin_id: &str) -> std::path::PathBuf {
    crate::plugin_paths::plugin_config_dir(plugin_id)
}

pub(super) fn plugin_state_dir(plugin_id: &str) -> std::path::PathBuf {
    crate::plugin_paths::plugin_state_dir(plugin_id)
}

pub(super) fn ensure_plugin_user_dirs(plugin: &InstalledPluginInfo) -> std::io::Result<()> {
    crate::plugin_paths::ensure_plugin_user_dirs(&plugin.plugin_id)
}

pub(super) fn plugin_path_env(plugin: &InstalledPluginInfo) -> Vec<(String, String)> {
    let config_dir = plugin_config_dir(&plugin.plugin_id);
    let state_dir = plugin_state_dir(&plugin.plugin_id);

    vec![
        ("HERDR_PLUGIN_ROOT".to_string(), plugin.plugin_root.clone()),
        (
            "HERDR_PLUGIN_CONFIG_DIR".to_string(),
            config_dir.display().to_string(),
        ),
        (
            "HERDR_PLUGIN_STATE_DIR".to_string(),
            state_dir.display().to_string(),
        ),
    ]
}
