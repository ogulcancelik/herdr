use crate::api::schema::{
    InstalledPluginInfo, PluginManifestAction, PluginManifestBuild, PluginManifestEventHook,
    PluginManifestLinkHandler, PluginManifestPane, PluginPanePlacement, PluginPlatform,
    PluginSourceInfo, PluginSourceKind,
};

const PLUGIN_ID_MAX_CHARS: usize = 120;
const PLUGIN_ACTION_ID_MAX_CHARS: usize = 120;

#[derive(serde::Deserialize)]
struct RawPluginManifest {
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    min_herdr_version: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    #[serde(default)]
    build: Vec<RawPluginManifestBuild>,
    #[serde(default)]
    actions: Vec<RawPluginManifestAction>,
    #[serde(default)]
    events: Vec<RawPluginManifestEventHook>,
    #[serde(default)]
    panes: Vec<RawPluginManifestPane>,
    #[serde(default)]
    link_handlers: Vec<RawPluginManifestLinkHandler>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestBuild {
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestAction {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    contexts: Vec<crate::api::schema::PluginActionContext>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestEventHook {
    on: String,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    command: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestPane {
    id: String,
    title: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
    #[serde(default)]
    placement: PluginPanePlacement,
    command: Vec<String>,
}

#[derive(serde::Deserialize)]
struct RawPluginManifestLinkHandler {
    id: String,
    title: String,
    pattern: String,
    action: String,
    #[serde(default)]
    platforms: Option<Vec<RawPlatform>>,
}

/// Raw string platform value from the manifest, validated before conversion.
#[derive(serde::Deserialize)]
#[serde(try_from = "String")]
struct RawPlatform(PluginPlatform);

impl TryFrom<String> for RawPlatform {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "linux" => Ok(RawPlatform(PluginPlatform::Linux)),
            "macos" => Ok(RawPlatform(PluginPlatform::Macos)),
            "windows" => Ok(RawPlatform(PluginPlatform::Windows)),
            other => Err(format!(
                "invalid_plugin_platform: unknown platform '{other}'"
            )),
        }
    }
}

pub(crate) fn load_plugin_manifest(
    path: &str,
    enabled: bool,
) -> Result<InstalledPluginInfo, (&'static str, String)> {
    let path = std::path::PathBuf::from(path);
    let manifest_path = if path.is_dir() {
        path.join("herdr-plugin.toml")
    } else {
        path
    };
    let manifest_path = manifest_path
        .canonicalize()
        .map_err(|err| ("plugin_manifest_not_found", err.to_string()))?;
    let plugin_root = manifest_path
        .parent()
        .ok_or_else(|| {
            (
                "invalid_plugin_manifest_path",
                "manifest path has no parent directory".to_string(),
            )
        })?
        .to_path_buf();
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|err| ("plugin_manifest_read_failed", err.to_string()))?;
    let raw: RawPluginManifest = toml::from_str(&content)
        .map_err(|err| ("plugin_manifest_parse_failed", err.to_string()))?;
    let plugin_id = normalize_plugin_id(&raw.id)
        .ok_or_else(|| ("invalid_plugin_id", "invalid plugin id".to_string()))?;
    let name = non_empty_trimmed(&raw.name, "invalid_plugin_name", "plugin name is required")?;
    let version = non_empty_trimmed(
        &raw.version,
        "invalid_plugin_version",
        "plugin version is required",
    )?;
    let min_herdr_version = validate_min_herdr_version(raw.min_herdr_version.as_deref())?;
    let description = raw
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(raw.platforms)?;
    let build = raw
        .build
        .into_iter()
        .map(normalize_manifest_build)
        .collect::<Result<Vec<_>, _>>()?;
    let mut actions = raw
        .actions
        .into_iter()
        .map(normalize_manifest_action)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_action_ids(&actions)?;
    actions.sort_by(|a, b| a.id.cmp(&b.id));
    let mut events = raw
        .events
        .into_iter()
        .map(normalize_manifest_event)
        .collect::<Result<Vec<_>, _>>()?;
    events.sort_by(|a, b| a.on.cmp(&b.on).then_with(|| a.command.cmp(&b.command)));
    let mut panes = raw
        .panes
        .into_iter()
        .map(normalize_manifest_pane)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_pane_ids(&panes)?;
    panes.sort_by(|a, b| a.id.cmp(&b.id));
    let link_handlers = raw
        .link_handlers
        .into_iter()
        .map(normalize_manifest_link_handler)
        .collect::<Result<Vec<_>, _>>()?;
    reject_duplicate_link_handler_ids(&link_handlers)?;
    validate_link_handler_actions(&link_handlers, &actions)?;

    let mut warnings = validate_event_names(&events);
    if platforms.is_none() {
        warnings.push("manifest does not declare platforms; platform support unknown".to_string());
    }

    Ok(InstalledPluginInfo {
        plugin_id,
        name,
        version,
        min_herdr_version,
        description,
        manifest_path: manifest_path.display().to_string(),
        plugin_root: plugin_root.display().to_string(),
        enabled,
        platforms,
        build,
        actions,
        events,
        panes,
        link_handlers,
        source: Default::default(),
        warnings,
    })
}

fn validate_min_herdr_version(value: Option<&str>) -> Result<String, (&'static str, String)> {
    let Some(value) = value else {
        return Err((
            "invalid_plugin_min_herdr_version",
            "plugin min_herdr_version is required".to_string(),
        ));
    };
    let value = non_empty_trimmed(
        value,
        "invalid_plugin_min_herdr_version",
        "plugin min_herdr_version is required",
    )?;
    let required = crate::update::Version::parse(&value).ok_or_else(|| {
        (
            "invalid_plugin_min_herdr_version",
            format!(
                "plugin min_herdr_version must be a semantic version like {}",
                crate::build_info::BASE_VERSION
            ),
        )
    })?;
    let current = crate::update::Version::current();
    if required > current {
        return Err((
            "plugin_requires_newer_herdr",
            format!("plugin requires Herdr {required} or newer; current Herdr is {current}"),
        ));
    }
    Ok(required.to_string())
}

fn normalize_manifest_build(
    build: RawPluginManifestBuild,
) -> Result<PluginManifestBuild, (&'static str, String)> {
    let platforms = normalize_platforms(build.platforms)?;
    let command = normalize_command(build.command)?;
    Ok(PluginManifestBuild { platforms, command })
}

pub(super) fn normalize_plugin_source(
    plugin: &InstalledPluginInfo,
    source: PluginSourceInfo,
) -> Result<PluginSourceInfo, (&'static str, String)> {
    if source.kind == PluginSourceKind::Local {
        return Ok(source);
    }
    let Some(managed_path) = source.managed_path.as_deref() else {
        return Err((
            "invalid_plugin_source",
            "GitHub plugin source requires managed_path".to_string(),
        ));
    };
    let managed_path = std::path::PathBuf::from(managed_path)
        .canonicalize()
        .map_err(|err| ("invalid_plugin_source", err.to_string()))?;
    let plugin_root = std::path::PathBuf::from(&plugin.plugin_root)
        .canonicalize()
        .map_err(|err| ("invalid_plugin_source", err.to_string()))?;
    let expected = crate::session::data_dir()
        .join("plugins")
        .join("github")
        .join(crate::api::schema::plugin_managed_path_component(
            &plugin.plugin_id,
        ))
        .canonicalize()
        .map_err(|err| ("invalid_plugin_source", err.to_string()))?;
    if managed_path != expected {
        return Err((
            "invalid_plugin_source",
            "GitHub plugin managed_path does not match the plugin id".to_string(),
        ));
    }
    if !plugin_root.starts_with(&managed_path) {
        return Err((
            "invalid_plugin_source",
            "plugin manifest is not inside the managed checkout".to_string(),
        ));
    }
    Ok(source)
}

fn reject_duplicate_action_ids(
    actions: &[PluginManifestAction],
) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for action in actions {
        if !seen.insert(action.id.as_str()) {
            return Err((
                "duplicate_plugin_action_id",
                format!("duplicate action id '{}'", action.id),
            ));
        }
    }
    Ok(())
}

fn validate_event_names(events: &[crate::api::schema::PluginManifestEventHook]) -> Vec<String> {
    let known = crate::api::schema::plugin_hook_event_names();
    events
        .iter()
        .filter(|hook| !known.contains(&hook.on.as_str()))
        .map(|hook| format!("unknown event '{}'", hook.on))
        .collect()
}

fn reject_duplicate_pane_ids(panes: &[PluginManifestPane]) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for pane in panes {
        if !seen.insert(pane.id.as_str()) {
            return Err((
                "duplicate_plugin_pane_id",
                format!("duplicate pane id '{}'", pane.id),
            ));
        }
    }
    Ok(())
}

fn reject_duplicate_link_handler_ids(
    handlers: &[PluginManifestLinkHandler],
) -> Result<(), (&'static str, String)> {
    let mut seen = std::collections::HashSet::new();
    for handler in handlers {
        if !seen.insert(handler.id.as_str()) {
            return Err((
                "duplicate_plugin_link_handler_id",
                format!("duplicate link handler id '{}'", handler.id),
            ));
        }
    }
    Ok(())
}

fn validate_link_handler_actions(
    handlers: &[PluginManifestLinkHandler],
    actions: &[PluginManifestAction],
) -> Result<(), (&'static str, String)> {
    for handler in handlers {
        if !actions.iter().any(|action| action.id == handler.action) {
            return Err((
                "invalid_plugin_link_handler_action",
                format!(
                    "link handler '{}' references unknown action '{}'",
                    handler.id, handler.action
                ),
            ));
        }
    }
    Ok(())
}

fn normalize_manifest_action(
    action: RawPluginManifestAction,
) -> Result<PluginManifestAction, (&'static str, String)> {
    let id = normalize_action_id(&action.id)
        .ok_or_else(|| ("invalid_plugin_action_id", "invalid action id".to_string()))?;
    let title = non_empty_trimmed(
        &action.title,
        "invalid_plugin_action_title",
        "action title is required",
    )?;
    let description = action
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(action.platforms)?;
    let command = normalize_command(action.command)?;
    Ok(PluginManifestAction {
        id,
        title,
        description,
        contexts: action.contexts,
        platforms,
        command,
    })
}

fn normalize_manifest_pane(
    pane: RawPluginManifestPane,
) -> Result<PluginManifestPane, (&'static str, String)> {
    let id = normalize_action_id(&pane.id)
        .ok_or_else(|| ("invalid_plugin_pane_id", "invalid pane id".to_string()))?;
    let title = non_empty_trimmed(
        &pane.title,
        "invalid_plugin_pane_title",
        "pane title is required",
    )?;
    let description = pane
        .description
        .map(|description| description.trim().to_string())
        .filter(|description| !description.is_empty());
    let platforms = normalize_platforms(pane.platforms)?;
    let command = normalize_command(pane.command)?;
    Ok(PluginManifestPane {
        id,
        title,
        description,
        platforms,
        placement: pane.placement,
        command,
    })
}

fn normalize_manifest_event(
    event: RawPluginManifestEventHook,
) -> Result<PluginManifestEventHook, (&'static str, String)> {
    let on = non_empty_trimmed(&event.on, "invalid_plugin_event", "event name is required")?;
    let platforms = normalize_platforms(event.platforms)?;
    let command = normalize_command(event.command)?;
    Ok(PluginManifestEventHook {
        on,
        platforms,
        command,
    })
}

fn normalize_manifest_link_handler(
    handler: RawPluginManifestLinkHandler,
) -> Result<PluginManifestLinkHandler, (&'static str, String)> {
    let id = normalize_action_id(&handler.id).ok_or_else(|| {
        (
            "invalid_plugin_link_handler_id",
            "invalid link handler id".to_string(),
        )
    })?;
    let title = non_empty_trimmed(
        &handler.title,
        "invalid_plugin_link_handler_title",
        "link handler title is required",
    )?;
    let pattern = non_empty_trimmed(
        &handler.pattern,
        "invalid_plugin_link_handler_pattern",
        "link handler pattern is required",
    )?;
    regex::Regex::new(&pattern)
        .map_err(|err| ("invalid_plugin_link_handler_pattern", err.to_string()))?;
    let action = normalize_action_id(&handler.action).ok_or_else(|| {
        (
            "invalid_plugin_link_handler_action",
            "invalid link handler action".to_string(),
        )
    })?;
    let platforms = normalize_platforms(handler.platforms)?;
    Ok(PluginManifestLinkHandler {
        id,
        title,
        pattern,
        action,
        platforms,
    })
}

fn normalize_platforms(
    raw: Option<Vec<RawPlatform>>,
) -> Result<Option<Vec<PluginPlatform>>, (&'static str, String)> {
    match raw {
        None => Ok(None),
        Some(list) if list.is_empty() => Err((
            "invalid_plugin_platform",
            "platforms must not be an empty array; omit the field to leave platforms undeclared"
                .to_string(),
        )),
        Some(list) => Ok(Some(list.into_iter().map(|p| p.0).collect())),
    }
}

/// Returns the platform the current binary was compiled for.
fn current_platform() -> PluginPlatform {
    if cfg!(target_os = "linux") {
        PluginPlatform::Linux
    } else if cfg!(target_os = "macos") {
        PluginPlatform::Macos
    } else {
        PluginPlatform::Windows
    }
}

/// Resolve the effective platforms for an action or event: use the item's own
/// platforms if declared, otherwise inherit from the plugin-level platforms.
/// Returns a reference to whichever `Option<Vec<PluginPlatform>>` applies.
pub(super) fn effective_platforms<'a>(
    item_platforms: &'a Option<Vec<PluginPlatform>>,
    plugin_platforms: &'a Option<Vec<PluginPlatform>>,
) -> &'a Option<Vec<PluginPlatform>> {
    if item_platforms.is_some() {
        item_platforms
    } else {
        plugin_platforms
    }
}

pub(super) fn ensure_platform_supported(
    platforms: &Option<Vec<PluginPlatform>>,
    subject: &str,
) -> Result<(), (&'static str, String)> {
    if let Some(platforms) = platforms {
        let host = current_platform();
        if !platforms.contains(&host) {
            return Err((
                "platform_unsupported",
                format!(
                    "{subject} does not support the current platform ({})",
                    platform_name(host)
                ),
            ));
        }
    }
    Ok(())
}

fn platform_name(p: PluginPlatform) -> &'static str {
    match p {
        PluginPlatform::Linux => "linux",
        PluginPlatform::Macos => "macos",
        PluginPlatform::Windows => "windows",
    }
}

fn normalize_command(command: Vec<String>) -> Result<Vec<String>, (&'static str, String)> {
    let command = command
        .into_iter()
        .map(|arg| arg.trim().to_string())
        .collect::<Vec<_>>();
    if command.is_empty() || command.iter().any(|arg| arg.is_empty()) {
        return Err((
            "invalid_plugin_command",
            "command must contain non-empty argv strings".to_string(),
        ));
    }
    Ok(command)
}

fn non_empty_trimmed(
    value: &str,
    code: &'static str,
    message: &'static str,
) -> Result<String, (&'static str, String)> {
    let value = value.trim().to_string();
    if value.is_empty() {
        Err((code, message.to_string()))
    } else {
        Ok(value)
    }
}

pub(super) fn normalize_plugin_id(value: &str) -> Option<String> {
    normalize_identifier(value, PLUGIN_ID_MAX_CHARS)
}

pub(super) fn normalize_action_id(value: &str) -> Option<String> {
    normalize_local_identifier(value, PLUGIN_ACTION_ID_MAX_CHARS)
}

fn normalize_identifier(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= max_chars
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-')))
    .then(|| value.to_string())
}

fn normalize_local_identifier(value: &str, max_chars: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()
        && value.chars().count() <= max_chars
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-')))
    .then(|| value.to_string())
}
