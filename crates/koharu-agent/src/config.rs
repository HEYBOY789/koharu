use anyhow::Result;
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentModel {
    pub id: String,
    pub name: String,
    pub reasoning: Vec<Reasoning>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(default)]
pub struct Config {
    pub backend: Backend,
    pub codex: ModelConfig,
    pub pi: CliConfig,
    pub agy: CliConfig,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    #[default]
    Codex,
    Pi,
    Agy,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(default)]
pub struct ModelConfig {
    pub model: Option<String>,
    pub reasoning: Reasoning,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(default)]
pub struct CliConfig {
    pub binary: Option<String>,
    pub node: Option<String>,
    pub model: Option<String>,
    pub reasoning: Option<Reasoning>,
}

impl Config {
    pub fn load() -> Result<koharu_config::Config<Self>> {
        koharu_config::load("agent")
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum Reasoning {
    Low,
    #[default]
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}

impl Reasoning {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
            Self::Ultra => "ultra",
        }
    }
}
