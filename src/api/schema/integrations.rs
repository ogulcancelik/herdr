use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntegrationInstallParams {
    pub target: IntegrationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntegrationUninstallParams {
    pub target: IntegrationTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationTarget {
    Pi,
    Omp,
    Claude,
    Codex,
    Copilot,
    Devin,
    Droid,
    Kimi,
    Opencode,
    Kilo,
    Hermes,
    Qodercli,
    Cursor,
    Mastracode,
}

impl IntegrationTarget {
    pub(crate) const ALL: [Self; 14] = [
        Self::Pi,
        Self::Omp,
        Self::Claude,
        Self::Codex,
        Self::Copilot,
        Self::Devin,
        Self::Droid,
        Self::Kimi,
        Self::Opencode,
        Self::Kilo,
        Self::Hermes,
        Self::Qodercli,
        Self::Cursor,
        Self::Mastracode,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntegrationInstallResult {
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct IntegrationUninstallResult {
    pub messages: Vec<String>,
}
