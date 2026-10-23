//! External agent CLI that writes one chapter summary, wiki delta, and
//! translation setting. The app owns the workspace and commits its output.

mod backend;
mod skill;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use specta::Type;
use tokio::process::Command;

pub(crate) use backend::run_agent;

use super::Error;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SummarizerBackend {
    #[default]
    Agy,
    Pi,
}

impl SummarizerBackend {
    const fn binary(self) -> &'static str {
        match self {
            Self::Agy => "agy",
            Self::Pi => "pi",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(default)]
pub struct SummarizerConfig {
    pub backend: SummarizerBackend,
    pub binary: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub skill: Option<String>,
    pub summarize_default: bool,
}

impl Default for SummarizerConfig {
    fn default() -> Self {
        Self {
            backend: SummarizerBackend::default(),
            binary: None,
            model: None,
            effort: None,
            skill: None,
            summarize_default: true,
        }
    }
}

impl SummarizerConfig {
    pub(crate) fn load() -> Result<koharu_config::Config<Self>> {
        koharu_config::load("summarizer")
    }

    pub(crate) fn resolved_binary(&self) -> Result<PathBuf> {
        if let Some(binary) = self
            .binary
            .as_deref()
            .map(str::trim)
            .filter(|path| !path.is_empty())
        {
            let path = PathBuf::from(binary);
            return if path.is_file() {
                Ok(path)
            } else {
                bail!("summarizer binary does not exist: {}", path.display())
            };
        }
        resolve_on_path(self.backend).with_context(|| {
            format!(
                "could not find `{}`; install it or set the binary path in Settings",
                self.backend.binary()
            )
        })
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SummarizerProbe {
    pub path: Option<String>,
    pub version: Option<String>,
    pub models: Vec<String>,
    pub error: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_summarizer_config() -> std::result::Result<SummarizerConfig, Error> {
    Ok(SummarizerConfig::load()?.read()?.clone())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn save_summarizer_config(
    config: SummarizerConfig,
) -> std::result::Result<SummarizerConfig, Error> {
    if config
        .effort
        .as_deref()
        .is_some_and(|effort| !matches!(effort, "" | "low" | "medium" | "high" | "xhigh" | "max"))
    {
        return Err(anyhow::anyhow!(
            "unsupported reasoning effort `{}`",
            config.effort.as_deref().unwrap_or_default()
        )
        .into());
    }
    let handle = SummarizerConfig::load()?;
    let model = clean(config.model);
    // `agy` encodes effort in some model ids (`gemini-3.8-flash-high`) and rejects a
    // conflicting `--effort`, so the model id wins and the separate choice is dropped.
    let effort = if config.backend == SummarizerBackend::Agy
        && model
            .as_deref()
            .is_some_and(|model| effort_suffix(model).is_some())
    {
        None
    } else {
        clean(config.effort)
    };
    {
        let mut value = handle.write()?;
        *value = SummarizerConfig {
            binary: clean(config.binary),
            model,
            effort,
            skill: clean(config.skill),
            ..config
        };
        value.save()?;
    }
    Ok(handle.read()?.clone())
}

/// Returns the effort an `agy` model id already carries, if any.
pub(crate) fn effort_suffix(model: &str) -> Option<&'static str> {
    ["low", "medium", "high", "xhigh", "max"]
        .into_iter()
        .find(|effort| {
            model
                .strip_suffix(effort)
                .is_some_and(|prefix| prefix.ends_with('-'))
        })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn probe_summarizer(
    config: SummarizerConfig,
) -> std::result::Result<SummarizerProbe, Error> {
    let path = config.resolved_binary()?;
    let mut probe = SummarizerProbe {
        path: Some(path.display().to_string()),
        version: None,
        models: Vec::new(),
        error: None,
    };
    let mut version_command = invocation(&path);
    version_command.arg("--version").kill_on_drop(true);
    let version = version_command.output();
    match tokio::time::timeout(Duration::from_secs(15), version).await {
        Ok(Ok(output)) => {
            probe.version = first_line(&output.stdout).or_else(|| first_line(&output.stderr));
            if !output.status.success() {
                probe.error = Some(format!("`--version` exited with {}", output.status));
            }
        }
        Ok(Err(error)) => probe.error = Some(format!("could not start agent: {error}")),
        Err(_) => probe.error = Some("agent did not answer within 15 seconds".to_owned()),
    }

    let mut list_command = invocation(&path);
    match config.backend {
        SummarizerBackend::Agy => {
            list_command.arg("models");
        }
        SummarizerBackend::Pi => {
            list_command.arg("--list-models");
        }
    }
    let list = list_command.output();
    if let Ok(Ok(output)) = tokio::time::timeout(Duration::from_secs(30), list).await {
        let text = String::from_utf8_lossy(&output.stdout);
        for line in text.lines() {
            // `agy models` prints `id<TAB>name`; keep the identifier `--model` accepts.
            let Some(id) = line.split('\t').next() else {
                continue;
            };
            let id = id.trim().trim_start_matches(['-', '*']).trim();
            if id.is_empty() || probe.models.iter().any(|known| known == id) {
                continue;
            }
            probe.models.push(id.to_owned());
            if probe.models.len() >= 500 {
                break;
            }
        }
        if probe.error.is_none() && !output.status.success() {
            probe.error = first_line(&output.stderr)
                .or_else(|| Some(format!("model listing exited with {}", output.status)));
        }
    }
    Ok(probe)
}

/// Resolve the vendored skill, or the configured override directory.
fn skill_source(config: &SummarizerConfig) -> Result<skill::SkillSource> {
    if let Some(path) = config
        .skill
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty())
    {
        let path = PathBuf::from(path);
        let path = if path.is_dir() {
            path.join("SKILL.md")
        } else {
            path
        };
        return if path.is_file() {
            Ok(skill::SkillSource::External(path))
        } else {
            bail!("summarizer skill does not exist: {}", path.display())
        };
    }
    Ok(skill::SkillSource::Vendored)
}

/// Fails before a job starts when the configured skill is missing.
pub(crate) fn check_skill(config: &SummarizerConfig) -> Result<()> {
    skill_source(config).map(|_| ())
}

pub(crate) fn install_skill(
    workspace: &std::path::Path,
    config: &SummarizerConfig,
) -> Result<PathBuf> {
    let source = skill_source(config)?;
    skill::install(workspace, &source)
}

fn resolve_on_path(backend: SummarizerBackend) -> Option<PathBuf> {
    let names: &[&str] = match backend {
        SummarizerBackend::Agy => &["agy.exe", "agy"],
        SummarizerBackend::Pi => &["pi.exe", "pi.cmd", "pi.bat", "pi"],
    };
    let mut directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    if cfg!(windows) {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            directories.push(PathBuf::from(local).join("agy").join("bin"));
        }
        if let Some(profile) = std::env::var_os("USERPROFILE") {
            let profile = PathBuf::from(profile);
            directories.push(profile.join(".local").join("bin"));
            directories.push(profile.join("AppData").join("Roaming").join("npm"));
        }
    }
    for directory in directories {
        for name in names {
            let candidate = directory.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn invocation(path: &std::path::Path) -> Command {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if cfg!(windows) && matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat") {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(path);
        command
    } else {
        Command::new(path)
    }
}

fn first_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

fn clean(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::effort_suffix;

    /// `agy` fails the run when `--effort` contradicts the model id.
    #[test]
    fn effort_embedded_in_an_agy_model_id_is_detected() {
        assert_eq!(effort_suffix("gemini-3.8-flash-high"), Some("high"));
        assert_eq!(effort_suffix("gemini-3.8-flash-medium"), Some("medium"));
        assert_eq!(effort_suffix("gemini-3.8-flash"), None);
        assert_eq!(effort_suffix("gpt-oss-120b-medium"), Some("medium"));
        assert_eq!(effort_suffix("high"), None);
    }
}
