use std::{
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock, RwLock},
};

use regex::Regex;
use serde::Deserialize;

use super::{
    agent_label, manifest_update::ManifestVersion, parse_agent_label, Agent, AgentDetection,
    AgentState,
};

pub const DEFAULT_KNOWN_AGENT_IDLE_FALLBACK: &str = "default_known_agent_idle_fallback";

/// Input to the detection engine, carrying the screen snapshot plus any
/// OSC-derived strings captured from the terminal title / progress sequences.
/// Pass empty strings for `osc_title` and `osc_progress` when the data is not
/// available — behavior is identical to the pre-OSC engine in that case.
#[derive(Debug, Clone, Copy)]
pub struct DetectionInput<'a> {
    pub screen: &'a str,
    pub osc_title: &'a str,
    pub osc_progress: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectionExplain {
    pub agent: Option<String>,
    pub state: AgentState,
    pub source: Option<ManifestSource>,
    pub matched_rule: Option<MatchedRule>,
    pub screen_detection_skipped: bool,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    pub skip_state_update: bool,
    pub skipped_update_reason: Option<String>,
    pub fallback_reason: Option<String>,
    pub evaluated_rules: Vec<EvaluatedRule>,
    pub warning: Option<String>,
    pub manifest_version: Option<String>,
    pub cached_remote_version: Option<String>,
    pub local_override_shadowing_remote: bool,
    pub remote_update_status: Option<String>,
    pub remote_update_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManifestSource {
    Bundled,
    Remote { path: PathBuf, version: String },
    Override(PathBuf),
}

impl ManifestSource {
    pub fn label(&self) -> String {
        match self {
            Self::Bundled => "bundled".to_string(),
            Self::Remote { path, .. } => format!("remote:{}", path.display()),
            Self::Override(path) => path.display().to_string(),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::Remote { .. } => "remote",
            Self::Override(_) => "local override",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentManifestSummary {
    pub(crate) agent: Agent,
    pub(crate) active_source: ManifestSource,
    pub(crate) active_version: Option<String>,
    pub(crate) cached_remote_version: Option<String>,
    pub(crate) local_override_shadowing_remote: bool,
    pub(crate) warning: Option<String>,
}

pub(crate) fn manifest_summaries() -> Vec<AgentManifestSummary> {
    let lock = manifest_cache();
    let guard = match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    manifest_summaries_from_cache(&guard)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatchedRule {
    pub id: String,
    pub priority: i32,
    pub region: String,
    pub state: AgentState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvaluatedRule {
    pub id: String,
    pub priority: i32,
    pub region: String,
    pub evidence: RuleEvidence,
    pub state: AgentState,
    pub matched: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEvidence {
    pub contains: Vec<String>,
    pub regex: Vec<String>,
    pub line_regex: Vec<String>,
    pub all_count: usize,
    pub any_count: usize,
    pub not_count: usize,
    pub region_bytes: usize,
    pub region_preview: String,
}

#[derive(Debug, Clone)]
struct LoadedManifest {
    manifest: AgentManifest,
    compiled_rules: Vec<CompiledRule>,
    source: ManifestSource,
    warning: Option<String>,
    cached_remote_version: Option<String>,
    local_override_shadowing_remote: bool,
}

#[derive(Debug, Clone)]
struct ManifestCache {
    manifests: Vec<(Agent, Option<LoadedManifest>)>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub(crate) struct AgentManifest {
    id: String,
    version: Option<ManifestVersion>,
    min_engine_version: Option<u32>,
    #[serde(rename = "updated_at")]
    _updated_at: Option<String>,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    rules: Vec<ManifestRule>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestRule {
    id: String,
    state: Option<ManifestState>,
    #[serde(default)]
    priority: i32,
    #[serde(default = "default_region")]
    region: String,
    #[serde(default)]
    visible_idle: bool,
    #[serde(default)]
    visible_blocker: bool,
    #[serde(default)]
    visible_working: bool,
    #[serde(default)]
    skip_state_update: bool,
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
struct ManifestGate {
    #[serde(default)]
    all: Vec<ManifestGate>,
    #[serde(default)]
    any: Vec<ManifestGate>,
    #[serde(default, rename = "not")]
    not_gate: Vec<ManifestGate>,
    #[serde(default)]
    contains: Vec<String>,
    #[serde(default)]
    regex: Vec<String>,
    #[serde(default)]
    line_regex: Vec<String>,
}

#[derive(Debug, Clone)]
struct CompiledRule {
    gate: CompiledGate,
}

#[derive(Debug, Clone)]
struct CompiledGate {
    all: Vec<CompiledGate>,
    any: Vec<CompiledGate>,
    not_gate: Vec<CompiledGate>,
    contains: Vec<String>,
    regex: Vec<Regex>,
    line_regex: Vec<Regex>,
}

#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ManifestState {
    Idle,
    Working,
    Blocked,
    Unknown,
}

impl From<ManifestState> for AgentState {
    fn from(value: ManifestState) -> Self {
        match value {
            ManifestState::Idle => AgentState::Idle,
            ManifestState::Working => AgentState::Working,
            ManifestState::Blocked => AgentState::Blocked,
            ManifestState::Unknown => AgentState::Unknown,
        }
    }
}

fn default_region() -> String {
    "whole_recent".to_string()
}

const BUNDLED_MANIFESTS: &[(&str, &str)] = &[
    ("amp", include_str!("manifests/amp.toml")),
    ("agy", include_str!("manifests/antigravity.toml")),
    ("claude", include_str!("manifests/claude.toml")),
    ("cline", include_str!("manifests/cline.toml")),
    ("codex", include_str!("manifests/codex.toml")),
    ("cursor", include_str!("manifests/cursor.toml")),
    ("devin", include_str!("manifests/devin.toml")),
    ("droid", include_str!("manifests/droid.toml")),
    ("gemini", include_str!("manifests/gemini.toml")),
    ("grok", include_str!("manifests/grok.toml")),
    ("hermes", include_str!("manifests/hermes.toml")),
    ("kilo", include_str!("manifests/kilo.toml")),
    ("kimi", include_str!("manifests/kimi.toml")),
    ("kiro", include_str!("manifests/kiro.toml")),
    ("opencode", include_str!("manifests/opencode.toml")),
    ("pi", include_str!("manifests/pi.toml")),
    ("qodercli", include_str!("manifests/qodercli.toml")),
    ("copilot", include_str!("manifests/github-copilot.toml")),
];

static MANIFEST_CACHE: OnceLock<RwLock<ManifestCache>> = OnceLock::new();
static MANIFEST_RELOAD_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const MAX_RULES_PER_MANIFEST: usize = 128;
const MAX_GATE_DEPTH: usize = 8;
const MAX_TOTAL_GATES: usize = 512;
const MAX_MATCHERS_PER_GATE: usize = 32;
const MAX_TOTAL_MATCHERS: usize = 1024;
const MAX_MATCHER_CHARS: usize = 512;

pub(crate) fn reload_manifests() -> Vec<AgentManifestSummary> {
    let _reload_guard = MANIFEST_RELOAD_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cache = build_manifest_cache();
    let summaries = manifest_summaries_from_cache(&cache);
    let lock = MANIFEST_CACHE.get_or_init(|| RwLock::new(cache.clone()));
    match lock.write() {
        Ok(mut guard) => *guard = cache,
        Err(poisoned) => *poisoned.into_inner() = cache,
    }
    summaries
}

fn manifest_cache() -> &'static RwLock<ManifestCache> {
    MANIFEST_CACHE.get_or_init(|| RwLock::new(build_manifest_cache()))
}

fn build_manifest_cache() -> ManifestCache {
    ManifestCache {
        manifests: Agent::ALL
            .into_iter()
            .map(|agent| (agent, load_manifest_uncached(agent)))
            .collect(),
    }
}

fn manifest_summaries_from_cache(cache: &ManifestCache) -> Vec<AgentManifestSummary> {
    cache
        .manifests
        .iter()
        .filter_map(|(agent, loaded)| {
            loaded
                .clone()
                .map(|loaded| manifest_summary_from_loaded(*agent, loaded))
        })
        .collect()
}

fn manifest_summary_from_loaded(agent: Agent, loaded: LoadedManifest) -> AgentManifestSummary {
    AgentManifestSummary {
        agent,
        active_version: loaded.manifest.version.as_ref().map(ToString::to_string),
        active_source: loaded.source,
        cached_remote_version: loaded.cached_remote_version,
        local_override_shadowing_remote: loaded.local_override_shadowing_remote,
        warning: loaded.warning,
    }
}

#[allow(dead_code)] // shim for existing callers; detect_with_osc is the real path
pub fn detect(agent: Agent, screen_content: &str) -> AgentDetection {
    detect_with_osc(
        agent,
        DetectionInput {
            screen: screen_content,
            osc_title: "",
            osc_progress: "",
        },
    )
}

pub fn detect_with_osc(agent: Agent, input: DetectionInput<'_>) -> AgentDetection {
    let Some(loaded) = load_manifest(agent) else {
        return fallback_explain(Some(agent), None, false).into_detection();
    };
    evaluate_loaded_manifest(agent, input, loaded, false).into_detection()
}

pub fn explain(agent: Agent, screen_content: &str) -> DetectionExplain {
    explain_with_input(
        agent,
        DetectionInput {
            screen: screen_content,
            osc_title: "",
            osc_progress: "",
        },
    )
}

pub fn explain_with_input(agent: Agent, input: DetectionInput<'_>) -> DetectionExplain {
    let Some(loaded) = load_manifest(agent) else {
        return fallback_explain(Some(agent), None, true);
    };
    evaluate_loaded_manifest(agent, input, loaded, true)
}

pub fn explain_for_label(agent_label: &str, screen_content: &str) -> DetectionExplain {
    let Some(agent) = parse_agent_label(agent_label) else {
        return DetectionExplain {
            agent: Some(agent_label.to_string()),
            state: AgentState::Unknown,
            source: None,
            matched_rule: None,
            screen_detection_skipped: false,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
            skip_state_update: false,
            skipped_update_reason: None,
            fallback_reason: Some("unknown_agent".to_string()),
            evaluated_rules: Vec::new(),
            warning: None,
            manifest_version: None,
            cached_remote_version: None,
            local_override_shadowing_remote: false,
            remote_update_status: None,
            remote_update_error: None,
        };
    };
    explain(agent, screen_content)
}

pub fn should_skip_state_update(agent: Agent, screen_content: &str) -> bool {
    let Some(loaded) = load_manifest(agent) else {
        return false;
    };
    evaluate_loaded_manifest(
        agent,
        DetectionInput {
            screen: screen_content,
            osc_title: "",
            osc_progress: "",
        },
        loaded,
        false,
    )
    .skip_state_update
}

impl DetectionExplain {
    fn into_detection(self) -> AgentDetection {
        AgentDetection {
            state: self.state,
            skip_state_update: self.skip_state_update,
            visible_idle: self.visible_idle,
            visible_blocker: self.visible_blocker,
            visible_working: self.visible_working,
        }
    }
}

fn evaluate_loaded_manifest(
    agent: Agent,
    input: DetectionInput<'_>,
    loaded: LoadedManifest,
    include_update_status: bool,
) -> DetectionExplain {
    let mut matched: Option<(&ManifestRule, String)> = None;
    let mut evaluated_rules = Vec::new();

    for (rule, compiled_rule) in loaded.manifest.rules.iter().zip(&loaded.compiled_rules) {
        let region_text = region(input, &rule.region);
        let matched_rule = compiled_rule_matches(compiled_rule, region_text);
        evaluated_rules.push(EvaluatedRule {
            id: rule.id.clone(),
            priority: rule.priority,
            region: rule.region.clone(),
            evidence: rule_evidence(rule, region_text),
            state: rule
                .state
                .map(AgentState::from)
                .unwrap_or(AgentState::Unknown),
            matched: matched_rule,
        });

        if !matched_rule {
            continue;
        }

        match matched {
            Some((previous, _)) if previous.priority >= rule.priority => {}
            _ => matched = Some((rule, rule.region.clone())),
        }
    }

    let Some((rule, region_name)) = matched else {
        return fallback_explain(
            Some(agent),
            Some((loaded, evaluated_rules)),
            include_update_status,
        );
    };

    let state = rule
        .state
        .map(AgentState::from)
        .unwrap_or(AgentState::Unknown);
    let skipped_update_reason = rule
        .skip_state_update
        .then(|| format!("matched_rule:{}", rule.id));

    let remote_update_status = include_update_status
        .then(|| remote_update_status(agent))
        .flatten();

    DetectionExplain {
        agent: Some(agent_label(agent).to_string()),
        state,
        source: Some(loaded.source),
        matched_rule: Some(MatchedRule {
            id: rule.id.clone(),
            priority: rule.priority,
            region: region_name,
            state,
        }),
        screen_detection_skipped: false,
        visible_idle: rule.visible_idle && state == AgentState::Idle,
        visible_blocker: rule.visible_blocker && state == AgentState::Blocked,
        visible_working: rule.visible_working && state == AgentState::Working,
        skip_state_update: rule.skip_state_update,
        skipped_update_reason,
        fallback_reason: None,
        evaluated_rules,
        warning: loaded.warning,
        manifest_version: loaded.manifest.version.as_ref().map(ToString::to_string),
        cached_remote_version: loaded.cached_remote_version,
        local_override_shadowing_remote: loaded.local_override_shadowing_remote,
        remote_update_status: remote_update_status
            .as_ref()
            .map(|status| status.last_result.clone()),
        remote_update_error: remote_update_status.and_then(|status| status.last_error),
    }
}

fn fallback_explain(
    agent: Option<Agent>,
    context: Option<(LoadedManifest, Vec<EvaluatedRule>)>,
    include_update_status: bool,
) -> DetectionExplain {
    let (
        source,
        evaluated_rules,
        warning,
        manifest_version,
        cached_remote_version,
        local_override_shadowing_remote,
    ) = context
        .map(|(loaded, evaluated)| {
            (
                Some(loaded.source),
                evaluated,
                loaded.warning,
                loaded.manifest.version.as_ref().map(ToString::to_string),
                loaded.cached_remote_version,
                loaded.local_override_shadowing_remote,
            )
        })
        .unwrap_or((None, Vec::new(), None, None, None, false));
    let known_agent = agent.is_some();
    let remote_update_status = include_update_status
        .then(|| agent.and_then(remote_update_status))
        .flatten();

    DetectionExplain {
        agent: agent.map(|agent| agent_label(agent).to_string()),
        state: if known_agent {
            AgentState::Idle
        } else {
            AgentState::Unknown
        },
        source,
        matched_rule: None,
        screen_detection_skipped: false,
        visible_idle: false,
        visible_blocker: false,
        visible_working: false,
        skip_state_update: false,
        skipped_update_reason: None,
        fallback_reason: known_agent.then(|| DEFAULT_KNOWN_AGENT_IDLE_FALLBACK.to_string()),
        evaluated_rules,
        warning,
        manifest_version,
        cached_remote_version,
        local_override_shadowing_remote,
        remote_update_status: remote_update_status
            .as_ref()
            .map(|status| status.last_result.clone()),
        remote_update_error: remote_update_status.and_then(|status| status.last_error),
    }
}

fn load_manifest(agent: Agent) -> Option<LoadedManifest> {
    let lock = manifest_cache();
    let guard = match lock.read() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    guard
        .manifests
        .iter()
        .find(|(cached_agent, _)| *cached_agent == agent)
        .and_then(|(_, loaded)| loaded.clone())
}

fn load_manifest_uncached(agent: Agent) -> Option<LoadedManifest> {
    let bundled = bundled_manifest(agent)?;
    let mut remote = read_remote_manifest(agent, &bundled);
    let cached_remote_version = remote.as_ref().and_then(|loaded| match &loaded.source {
        _ if loaded.cached_remote_version.is_some() => loaded.cached_remote_version.clone(),
        ManifestSource::Remote { version, .. } => Some(version.clone()),
        _ => None,
    });
    let Some(path) = override_path(agent) else {
        if let Some(loaded) = remote.as_mut() {
            loaded.cached_remote_version = cached_remote_version.clone();
        }
        return Some(remote.unwrap_or_else(|| {
            bundled_loaded_manifest(agent, bundled, None, cached_remote_version, false)
        }));
    };
    let local_override_shadowing_remote = path.exists() && cached_remote_version.is_some();
    if let Some(loaded) = remote.as_mut() {
        loaded.cached_remote_version = cached_remote_version.clone();
        loaded.local_override_shadowing_remote = local_override_shadowing_remote;
    }

    if !path.exists() {
        return Some(remote.unwrap_or_else(|| {
            bundled_loaded_manifest(
                agent,
                bundled,
                None,
                cached_remote_version,
                local_override_shadowing_remote,
            )
        }));
    }

    match read_override_manifest(&path) {
        Ok(manifest) if manifest_matches_agent(&manifest, agent) => {
            match loaded_manifest(
                manifest,
                ManifestSource::Override(path.clone()),
                None,
                cached_remote_version.clone(),
                local_override_shadowing_remote,
            ) {
                Ok(loaded) => Some(loaded),
                Err(err) => {
                    let mut loaded = remote.unwrap_or_else(|| {
                        bundled_loaded_manifest(
                            agent,
                            bundled,
                            None,
                            cached_remote_version,
                            local_override_shadowing_remote,
                        )
                    });
                    loaded.warning = Some(format!(
                        "ignored override {} because it could not be compiled: {err}",
                        path.display()
                    ));
                    Some(loaded)
                }
            }
        }
        Ok(manifest) => {
            let mut loaded = remote.unwrap_or_else(|| {
                bundled_loaded_manifest(
                    agent,
                    bundled,
                    None,
                    cached_remote_version,
                    local_override_shadowing_remote,
                )
            });
            loaded.warning = Some(format!(
                "ignored override {} because manifest id {} does not match {}",
                path.display(),
                manifest.id,
                agent_label(agent)
            ));
            Some(loaded)
        }
        Err(err) => {
            let mut loaded = remote.unwrap_or_else(|| {
                bundled_loaded_manifest(
                    agent,
                    bundled,
                    None,
                    cached_remote_version,
                    local_override_shadowing_remote,
                )
            });
            loaded.warning = Some(format!(
                "ignored override {} because it could not be loaded: {err}",
                path.display()
            ));
            Some(loaded)
        }
    }
}

fn loaded_manifest(
    manifest: AgentManifest,
    source: ManifestSource,
    warning: Option<String>,
    cached_remote_version: Option<String>,
    local_override_shadowing_remote: bool,
) -> Result<LoadedManifest, String> {
    let compiled_rules = compile_manifest(&manifest)?;
    Ok(LoadedManifest {
        manifest,
        compiled_rules,
        source,
        warning,
        cached_remote_version,
        local_override_shadowing_remote,
    })
}

fn bundled_loaded_manifest(
    agent: Agent,
    manifest: AgentManifest,
    warning: Option<String>,
    cached_remote_version: Option<String>,
    local_override_shadowing_remote: bool,
) -> LoadedManifest {
    loaded_manifest(
        manifest,
        ManifestSource::Bundled,
        warning,
        cached_remote_version,
        local_override_shadowing_remote,
    )
    .unwrap_or_else(|err| {
        panic!(
            "bundled {} manifest could not be compiled: {err}",
            agent_label(agent)
        )
    })
}

fn bundled_manifest(agent: Agent) -> Option<AgentManifest> {
    let id = agent_label(agent);
    BUNDLED_MANIFESTS
        .iter()
        .find(|(manifest_id, _)| *manifest_id == id)
        .map(|(_, content)| {
            parse_manifest(content)
                .unwrap_or_else(|err| panic!("bundled {id} manifest is invalid: {err}"))
        })
}

fn read_override_manifest(path: &Path) -> Result<AgentManifest, String> {
    let content = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
    parse_manifest(&content)
}

fn read_remote_manifest(agent: Agent, bundled: &AgentManifest) -> Option<LoadedManifest> {
    let path = super::manifest_update::remote_manifest_path(agent);
    if !path.exists() {
        return None;
    }
    match std::fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|content| {
            parse_remote_manifest_for_agent(agent, &content).map(|parsed| parsed.manifest)
        }) {
        Ok(manifest) => {
            let version = manifest
                .version
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unknown".to_string());
            if let (Some(remote_version), Some(bundled_version)) =
                (manifest.version.as_ref(), bundled.version.as_ref())
            {
                if remote_version < bundled_version {
                    return Some(bundled_loaded_manifest(
                        agent,
                        bundled.clone(),
                        Some(format!(
                            "ignored remote manifest {} because cached version {remote_version} is older than bundled {bundled_version}",
                            path.display()
                        )),
                        Some(remote_version.to_string()),
                        false,
                    ));
                }
            }
            match loaded_manifest(
                manifest,
                ManifestSource::Remote {
                    path: path.clone(),
                    version,
                },
                None,
                None,
                false,
            ) {
                Ok(loaded) => Some(loaded),
                Err(err) => Some(bundled_loaded_manifest(
                    agent,
                    bundled.clone(),
                    Some(format!(
                        "ignored remote manifest {} because it could not be compiled: {err}",
                        path.display()
                    )),
                    None,
                    false,
                )),
            }
        }
        Err(err) => Some(bundled_loaded_manifest(
            agent,
            bundled.clone(),
            Some(format!(
                "ignored remote manifest {} because it could not be loaded: {err}",
                path.display()
            )),
            None,
            false,
        )),
    }
}

pub fn agent_state_label(state: AgentState) -> &'static str {
    match state {
        AgentState::Idle => "idle",
        AgentState::Working => "working",
        AgentState::Blocked => "blocked",
        AgentState::Unknown => "unknown",
    }
}

pub fn explain_to_json_value(explain: &DetectionExplain) -> serde_json::Value {
    let matched_rule = explain.matched_rule.as_ref().map(|rule| {
        serde_json::json!({
            "id": rule.id,
            "priority": rule.priority,
            "region": rule.region,
            "state": agent_state_label(rule.state),
        })
    });
    let evaluated_rules: Vec<_> = explain
        .evaluated_rules
        .iter()
        .map(|rule| {
            serde_json::json!({
                "id": rule.id,
                "priority": rule.priority,
                "region": rule.region,
                "state": agent_state_label(rule.state),
                "matched": rule.matched,
                "evidence": {
                    "contains": &rule.evidence.contains,
                    "regex": &rule.evidence.regex,
                    "line_regex": &rule.evidence.line_regex,
                    "all_count": rule.evidence.all_count,
                    "any_count": rule.evidence.any_count,
                    "not_count": rule.evidence.not_count,
                    "region_bytes": rule.evidence.region_bytes,
                    "region_preview": &rule.evidence.region_preview,
                },
            })
        })
        .collect();

    serde_json::json!({
        "agent": explain.agent,
        "state": agent_state_label(explain.state),
        "manifest_source": explain.source.as_ref().map(|source| source.label()),
        "manifest_version": &explain.manifest_version,
        "cached_remote_version": &explain.cached_remote_version,
        "local_override_shadowing_remote": explain.local_override_shadowing_remote,
        "remote_update_status": &explain.remote_update_status,
        "remote_update_error": &explain.remote_update_error,
        "matched_rule": matched_rule,
        "visible_idle": explain.visible_idle,
        "visible_blocker": explain.visible_blocker,
        "visible_working": explain.visible_working,
        "screen_detection_skipped": explain.screen_detection_skipped,
        "skip_state_update": explain.skip_state_update,
        "skipped_update_reason": explain.skipped_update_reason,
        "fallback_reason": explain.fallback_reason,
        "warning": explain.warning,
        "evaluated_rules": evaluated_rules,
    })
}

pub(crate) struct ParsedRemoteManifest {
    pub(crate) manifest: AgentManifest,
    pub(crate) version: ManifestVersion,
}

pub(crate) fn parse_manifest(content: &str) -> Result<AgentManifest, String> {
    let manifest = toml::from_str::<AgentManifest>(content).map_err(|err| err.to_string())?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub(crate) fn parse_remote_manifest_for_agent(
    agent: Agent,
    content: &str,
) -> Result<ParsedRemoteManifest, String> {
    let manifest = parse_manifest(content)?;
    if !manifest_matches_agent(&manifest, agent) {
        return Err(format!(
            "manifest id {} does not match {}",
            manifest.id,
            agent_label(agent)
        ));
    }
    let version = manifest
        .version
        .clone()
        .ok_or("remote manifest must include version")?;
    let min_engine_version = manifest
        .min_engine_version
        .ok_or("remote manifest must include min_engine_version")?;
    if min_engine_version > super::manifest_update::MANIFEST_ENGINE_VERSION {
        return Err(format!(
            "manifest requires engine {min_engine_version}, current engine is {}",
            super::manifest_update::MANIFEST_ENGINE_VERSION
        ));
    }
    Ok(ParsedRemoteManifest { manifest, version })
}

fn validate_manifest(manifest: &AgentManifest) -> Result<(), String> {
    if manifest.rules.is_empty() {
        return Err("manifest must contain at least one rule".to_string());
    }
    if manifest.rules.len() > MAX_RULES_PER_MANIFEST {
        return Err(format!(
            "manifest contains {} rules, max is {MAX_RULES_PER_MANIFEST}",
            manifest.rules.len()
        ));
    }

    let mut complexity = ManifestComplexity::default();
    for rule in &manifest.rules {
        if rule.id.trim().is_empty() {
            return Err("manifest rule id must not be empty".to_string());
        }
        if rule.skip_state_update {
            if rule.state != Some(ManifestState::Unknown) {
                return Err(format!(
                    "rule {} uses skip_state_update without state = \"unknown\"",
                    rule.id
                ));
            }
            if rule.visible_idle || rule.visible_blocker || rule.visible_working {
                return Err(format!(
                    "rule {} uses skip_state_update with visible state evidence",
                    rule.id
                ));
            }
        }
        validate_region_name(&rule.region)
            .map_err(|err| format!("rule {} uses invalid region: {err}", rule.id))?;
        validate_rule_gate(rule, &mut complexity)
            .map_err(|err| format!("rule {} has invalid matcher gates: {err}", rule.id))?;
    }

    Ok(())
}

#[derive(Default)]
struct ManifestComplexity {
    total_gates: usize,
    total_matchers: usize,
}

fn validate_rule_gate(
    rule: &ManifestRule,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    validate_gate(&manifest_gate_from_rule(rule), "rule", 0, complexity)
}

fn validate_gate(
    gate: &ManifestGate,
    context: &str,
    depth: usize,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("{context} exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, context, complexity)?;
    if !gate_has_positive_matcher(gate) {
        return Err(format!("{context} must contain a positive matcher"));
    }
    validate_regex_patterns(&gate.regex, context, "regex")?;
    validate_regex_patterns(&gate.line_regex, context, "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        if !gate_has_any_matcher(nested) {
            return Err(format!("{context} contains an empty not gate"));
        }
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_not_gate(
    gate: &ManifestGate,
    depth: usize,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    if depth > MAX_GATE_DEPTH {
        return Err(format!("not gate exceeds max gate depth {MAX_GATE_DEPTH}"));
    }
    complexity.total_gates += 1;
    if complexity.total_gates > MAX_TOTAL_GATES {
        return Err(format!("manifest exceeds max gate count {MAX_TOTAL_GATES}"));
    }
    validate_matcher_limits(gate, "not gate", complexity)?;
    if !gate_has_any_matcher(gate) {
        return Err("not gate must contain a matcher".to_string());
    }
    validate_regex_patterns(&gate.regex, "not gate", "regex")?;
    validate_regex_patterns(&gate.line_regex, "not gate", "line_regex")?;
    for nested in &gate.all {
        validate_gate(nested, "not all gate", depth + 1, complexity)?;
    }
    for nested in &gate.any {
        validate_gate(nested, "not any gate", depth + 1, complexity)?;
    }
    for nested in &gate.not_gate {
        validate_not_gate(nested, depth + 1, complexity)?;
    }
    Ok(())
}

fn validate_matcher_limits(
    gate: &ManifestGate,
    context: &str,
    complexity: &mut ManifestComplexity,
) -> Result<(), String> {
    let matcher_count = gate.contains.len() + gate.regex.len() + gate.line_regex.len();
    if matcher_count > MAX_MATCHERS_PER_GATE {
        return Err(format!(
            "{context} has {matcher_count} direct matchers, max is {MAX_MATCHERS_PER_GATE}"
        ));
    }
    complexity.total_matchers += matcher_count;
    if complexity.total_matchers > MAX_TOTAL_MATCHERS {
        return Err(format!(
            "manifest exceeds max matcher count {MAX_TOTAL_MATCHERS}"
        ));
    }
    for value in gate
        .contains
        .iter()
        .chain(gate.regex.iter())
        .chain(gate.line_regex.iter())
    {
        if value.chars().count() > MAX_MATCHER_CHARS {
            return Err(format!(
                "{context} matcher exceeds max length {MAX_MATCHER_CHARS}"
            ));
        }
    }
    Ok(())
}

fn validate_regex_patterns(patterns: &[String], context: &str, field: &str) -> Result<(), String> {
    for pattern in patterns {
        Regex::new(pattern).map_err(|err| {
            format!("{context} contains invalid {field} pattern {pattern:?}: {err}")
        })?;
    }
    Ok(())
}

fn gate_has_positive_matcher(gate: &ManifestGate) -> bool {
    !gate.contains.is_empty()
        || !gate.regex.is_empty()
        || !gate.line_regex.is_empty()
        || !gate.all.is_empty()
        || !gate.any.is_empty()
}

fn gate_has_any_matcher(gate: &ManifestGate) -> bool {
    gate_has_positive_matcher(gate) || !gate.not_gate.is_empty()
}

fn validate_region_name(spec: &str) -> Result<(), String> {
    let trimmed = spec.trim();
    match trimmed {
        "whole_recent"
        | "after_last_prompt_marker"
        | "before_current_prompt_marker"
        | "whole_recent_without_current_prompt_marker"
        | "current_prompt_block_marker"
        | "after_current_prompt_block_marker"
        | "prompt_box_body"
        | "above_prompt_box"
        | "last_non_empty_above_prompt_box"
        | "after_last_horizontal_rule"
        | "osc_title"
        | "osc_progress" => Ok(()),
        _ if region_count(trimmed, "bottom_lines").is_some()
            || region_count(trimmed, "bottom_non_empty_lines").is_some() =>
        {
            Ok(())
        }
        _ => Err(trimmed.to_string()),
    }
}

fn override_path(agent: Agent) -> Option<PathBuf> {
    Some(
        crate::config::config_dir()
            .join("agent-detection")
            .join(format!("{}.toml", agent_label(agent))),
    )
}

fn remote_update_status(agent: Agent) -> Option<super::manifest_update::AgentRemoteStatus> {
    super::manifest_update::load_status().agent_status(agent)
}

fn manifest_matches_agent(manifest: &AgentManifest, agent: Agent) -> bool {
    let id = agent_label(agent);
    manifest.id == id
        || manifest.aliases.iter().any(|alias| alias == id)
        || parse_agent_label(&manifest.id) == Some(agent)
        || manifest
            .aliases
            .iter()
            .any(|alias| parse_agent_label(alias) == Some(agent))
}

fn manifest_gate_from_rule(rule: &ManifestRule) -> ManifestGate {
    ManifestGate {
        all: rule.all.clone(),
        any: rule.any.clone(),
        not_gate: rule.not_gate.clone(),
        contains: rule.contains.clone(),
        regex: rule.regex.clone(),
        line_regex: rule.line_regex.clone(),
    }
}

fn compile_manifest(manifest: &AgentManifest) -> Result<Vec<CompiledRule>, String> {
    manifest
        .rules
        .iter()
        .map(|rule| {
            compile_gate(&manifest_gate_from_rule(rule))
                .map(|gate| CompiledRule { gate })
                .map_err(|err| format!("rule {} could not be compiled: {err}", rule.id))
        })
        .collect()
}

fn compile_gate(gate: &ManifestGate) -> Result<CompiledGate, String> {
    Ok(CompiledGate {
        all: gate
            .all
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        any: gate
            .any
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        not_gate: gate
            .not_gate
            .iter()
            .map(compile_gate)
            .collect::<Result<_, _>>()?,
        contains: gate
            .contains
            .iter()
            .map(|needle| needle.to_lowercase())
            .collect(),
        regex: gate
            .regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
        line_regex: gate
            .line_regex
            .iter()
            .map(|pattern| Regex::new(pattern).map_err(|err| err.to_string()))
            .collect::<Result<_, _>>()?,
    })
}

fn compiled_rule_matches(rule: &CompiledRule, text: &str) -> bool {
    let lower_text = text.to_lowercase();
    compiled_gate_matches(&rule.gate, text, &lower_text)
}

fn rule_evidence(rule: &ManifestRule, region_text: &str) -> RuleEvidence {
    RuleEvidence {
        contains: rule.contains.clone(),
        regex: rule.regex.clone(),
        line_regex: rule.line_regex.clone(),
        all_count: rule.all.len(),
        any_count: rule.any.len(),
        not_count: rule.not_gate.len(),
        region_bytes: region_text.len(),
        region_preview: bounded_preview(region_text),
    }
}

fn bounded_preview(text: &str) -> String {
    const MAX_CHARS: usize = 240;
    let mut preview: String = text.chars().take(MAX_CHARS).collect();
    if text.chars().count() > MAX_CHARS {
        preview.push_str("...");
    }
    preview
}

fn compiled_gate_matches(gate: &CompiledGate, text: &str, lower_text: &str) -> bool {
    if !gate
        .contains
        .iter()
        .all(|needle| lower_text.contains(needle))
    {
        return false;
    }

    if !gate.regex.iter().all(|regex| regex.is_match(text)) {
        return false;
    }

    if !gate
        .line_regex
        .iter()
        .all(|regex| text.lines().any(|line| regex.is_match(line)))
    {
        return false;
    }

    if !gate
        .all
        .iter()
        .all(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    if !gate.any.is_empty()
        && !gate
            .any
            .iter()
            .any(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    if gate
        .not_gate
        .iter()
        .any(|nested| compiled_gate_matches(nested, text, lower_text))
    {
        return false;
    }

    true
}

fn region<'a>(input: DetectionInput<'a>, spec: &str) -> &'a str {
    let trimmed = spec.trim();
    // OSC regions source from their dedicated fields, not the screen.
    match trimmed {
        "osc_title" => return input.osc_title,
        "osc_progress" => return input.osc_progress,
        _ => {}
    }
    // All other regions operate on the screen content as before.
    let content = input.screen;
    match trimmed {
        "whole_recent" => content,
        "after_last_prompt_marker" => after_last_prompt_marker(content),
        "before_current_prompt_marker" => before_current_prompt_marker(content),
        "whole_recent_without_current_prompt_marker" => {
            whole_recent_without_current_prompt_marker(content)
        }
        "current_prompt_block_marker" => current_prompt_block_marker(content).unwrap_or(""),
        "after_current_prompt_block_marker" => {
            after_current_prompt_block_marker(content).unwrap_or("")
        }
        "prompt_box_body" => prompt_box_body(content).unwrap_or(""),
        "above_prompt_box" => above_prompt_box(content),
        "last_non_empty_above_prompt_box" => last_non_empty_line(above_prompt_box(content)),
        "after_last_horizontal_rule" => after_last_horizontal_rule(content),
        _ => {
            if let Some(count) = region_count(trimmed, "bottom_lines") {
                return bottom_lines(content, count);
            }
            if let Some(count) = region_count(trimmed, "bottom_non_empty_lines") {
                return bottom_non_empty_lines(content, count);
            }
            ""
        }
    }
}

fn region_count(spec: &str, name: &str) -> Option<usize> {
    spec.strip_prefix(name)
        .and_then(|rest| rest.strip_prefix('('))
        .and_then(|rest| rest.strip_suffix(')'))
        .and_then(|count| count.parse::<usize>().ok())
}

fn bottom_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(count);
    slice_from_line_index(content, &lines, start)
}

fn bottom_non_empty_lines(content: &str, count: usize) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(start_index) = lines
        .iter()
        .enumerate()
        .rev()
        .filter(|(_, line)| !line.trim().is_empty())
        .take(count)
        .last()
        .map(|(index, _)| index)
    else {
        return "";
    };
    slice_from_line_index(content, &lines, start_index)
}

fn after_last_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = lines.iter().rposition(|line| codex_prompt_line(line)) else {
        return content;
    };
    slice_from_line_index(content, &lines, index + 1)
}

fn before_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(index) = current_codex_prompt_index(&lines) else {
        return content;
    };
    let byte_offset = lines[..index]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>();
    &content[..byte_offset.min(content.len())]
}

fn whole_recent_without_current_prompt_marker(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    if current_codex_prompt_index(&lines).is_some() {
        ""
    } else {
        content
    }
}

fn current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    lines[..prompt_index]
        .iter()
        .rev()
        .find(|line| codex_block_marker_line(line))
        .copied()
}

fn after_current_prompt_block_marker(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let prompt_index = current_codex_prompt_index(&lines)?;
    let block_index = lines[..prompt_index]
        .iter()
        .rposition(|line| codex_block_marker_line(line))?;
    Some(slice_from_line_index(content, &lines, block_index))
}

fn current_codex_prompt_index(lines: &[&str]) -> Option<usize> {
    let prompt_index = lines.iter().rposition(|line| codex_prompt_line(line))?;
    if lines[prompt_index + 1..]
        .iter()
        .any(|line| codex_block_marker_line(line))
    {
        return None;
    }
    Some(prompt_index)
}

fn codex_prompt_line(line: &str) -> bool {
    line == "›" || line.starts_with("› ")
}

fn codex_block_marker_line(line: &str) -> bool {
    line.starts_with('•') || line.starts_with('■') || line.starts_with('✗') || line.starts_with('✓')
}

fn prompt_box_body(content: &str) -> Option<&str> {
    let lines: Vec<&str> = content.lines().collect();
    let top = prompt_box_top_border_index(&lines)?;
    let start = line_start_offset(content, &lines, top + 1);
    let end_index = lines[top + 1..]
        .iter()
        .position(|line| is_horizontal_rule(line))
        .map(|relative| top + 1 + relative)
        .unwrap_or(lines.len());
    let end = line_start_offset(content, &lines, end_index);
    Some(&content[start.min(content.len())..end.min(content.len())])
}

fn above_prompt_box(content: &str) -> &str {
    let lines: Vec<&str> = content.lines().collect();
    let Some(top) = prompt_box_top_border_index(&lines) else {
        return content;
    };
    let end = line_start_offset(content, &lines, top);
    &content[..end.min(content.len())]
}

fn after_last_horizontal_rule(content: &str) -> &str {
    let mut last_rule_end = 0usize;
    let mut offset = 0usize;
    for line in content.lines() {
        let next_offset = offset + line.len() + 1;
        if is_horizontal_rule(line) {
            last_rule_end = next_offset.min(content.len());
        }
        offset = next_offset;
    }
    &content[last_rule_end..]
}

fn last_non_empty_line(content: &str) -> &str {
    content
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("")
}

fn prompt_box_top_border_index(lines: &[&str]) -> Option<usize> {
    let mut border_count = 0;
    for index in (0..lines.len()).rev() {
        if is_horizontal_rule(lines[index]) {
            border_count += 1;
            if border_count == 2 {
                return Some(index);
            }
        }
    }
    None
}

fn is_horizontal_rule(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return false;
    }

    let rule_chars = trimmed.chars().take_while(|&ch| ch == '─').count();
    if rule_chars == 0 {
        return false;
    }

    let rule_bytes = trimmed
        .char_indices()
        .nth(rule_chars)
        .map(|(index, _)| index)
        .unwrap_or(trimmed.len());
    let suffix = trimmed[rule_bytes..].trim_start();

    suffix.is_empty() || rule_chars >= 3
}

fn slice_from_line_index<'a>(content: &'a str, lines: &[&str], index: usize) -> &'a str {
    let byte_offset = line_start_offset(content, lines, index);
    &content[byte_offset.min(content.len())..]
}

fn line_start_offset(content: &str, lines: &[&str], index: usize) -> usize {
    lines[..index.min(lines.len())]
        .iter()
        .map(|line| line.len() + 1)
        .sum::<usize>()
        .min(content.len())
}

#[cfg(test)]
mod tests;
