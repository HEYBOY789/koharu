//! Launches the fork's external agents without passing prompts through a shell.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Command;

use crate::{Event, McpServer, Tools};

const EDITOR_INSTRUCTIONS: &str = "You edit a manga in Koharu. Get the current page ID and inspect the rendered page with view_page before making a visual correction. For lettering use edit on the existing layer, with explicit IDs from the project. Preserve artwork, translations, italic and other intentional styling. Keep regular dialogue readable at mobile and desktop widths. For outlined lettering keep the original ink and halo colors. Run pipeline stages only when the user asks for processing. Verify the result visually before reporting a change.";

#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize, specta::Type)]
#[serde(rename_all = "snake_case")]
pub enum CliBackend {
    #[default]
    Agy,
    Pi,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, specta::Type)]
#[serde(default)]
pub struct CliConfig {
    pub backend: CliBackend,
    pub binary: Option<String>,
    pub node: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reasoning: Option<String>,
}

fn resolve(configured: Option<&str>, names: &[&str]) -> Result<PathBuf> {
    if let Some(path) = configured.map(str::trim).filter(|path| !path.is_empty()) {
        let path = PathBuf::from(path);
        ensure!(
            path.is_file(),
            "executable does not exist: {}",
            path.display()
        );
        return Ok(path.canonicalize()?);
    }
    let mut directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        directories.push(home.join(".local/bin"));
        #[cfg(windows)]
        directories.extend([
            home.join(".pi/agent/bin"),
            home.join("AppData/Local/agy/bin"),
            home.join("AppData/Roaming/npm"),
        ]);
    }
    for directory in directories {
        for name in names {
            let path = directory.join(name);
            if path.is_file() {
                return Ok(path.canonicalize()?);
            }
        }
    }
    bail!("could not find {}", names[0])
}

impl CliConfig {
    pub fn command(&self) -> Result<Command> {
        let path = resolve(
            self.binary.as_deref(),
            match self.backend {
                CliBackend::Pi if cfg!(windows) => &["pi.exe", "pi.cmd", "pi"],
                CliBackend::Agy if cfg!(windows) => &["agy.exe", "agy"],
                CliBackend::Pi => &["pi"],
                CliBackend::Agy => &["agy"],
            },
        )?;
        let extension = path
            .extension()
            .and_then(|v| v.to_str())
            .unwrap_or_default();
        let node = || {
            resolve(
                self.node.as_deref(),
                if cfg!(windows) {
                    &["node.exe", "node"]
                } else {
                    &["node"]
                },
            )
        };
        let mut command = if ["js", "mjs", "cjs"].contains(&extension) {
            let mut command = Command::new(node()?);
            command.arg(path);
            command
        } else if cfg!(windows) && ["cmd", "bat"].contains(&extension) {
            let directory = path.parent().context("CLI has no parent directory")?;
            let entry = std::iter::once(directory.join("pi-launcher.js"))
                .chain(
                    ["@earendil-works", "@mariozechner"]
                        .into_iter()
                        .map(|scope| {
                            directory
                                .join(format!("node_modules/{scope}/pi-coding-agent/dist/cli.js"))
                        }),
                )
                .find(|path| path.is_file())
                .context("select Pi's executable or dist/cli.js")?;
            let mut command = Command::new(node()?);
            command.arg(entry);
            command
        } else {
            Command::new(path)
        };
        command.kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x0800_0000);
        Ok(command)
    }
}

pub async fn run_cli(
    tools: Tools,
    config: &CliConfig,
    workspace: &Path,
    prompt: &str,
    events: impl Fn(Event),
) -> Result<String> {
    let server = McpServer::private().await?;
    server.open(Some(tools));
    let url = server.url();
    let mut command = config.command()?;
    command
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    let log = tempfile::tempfile()?;
    command.stderr(log.try_clone()?);
    match config.backend {
        CliBackend::Pi => {
            let extension = workspace.join("koharu-tools.mjs");
            tokio::fs::write(&extension, include_str!("pi-tools.mjs")).await?;
            command
                .args([
                    "--print",
                    "--mode",
                    "json",
                    "--no-approve",
                    "--no-builtin-tools",
                    "--no-extensions",
                    "--no-skills",
                    "--no-prompt-templates",
                    "--no-context-files",
                ])
                .arg("--extension")
                .arg(extension)
                .arg("--session")
                .arg(workspace.join("session.jsonl"))
                .args(["--system-prompt", EDITOR_INSTRUCTIONS])
                .env("KOHARU_EDITOR_TOOLS", &url);
        }
        CliBackend::Agy => {
            let plugin = workspace.join(".agents/plugins/koharu-editor");
            tokio::fs::create_dir_all(&plugin).await?;
            tokio::fs::write(plugin.join("plugin.json"), r#"{"name":"koharu-editor"}"#).await?;
            tokio::fs::write(
                plugin.join("mcp_config.json"),
                json!({"mcpServers": {"koharu": {"url": url}}}).to_string(),
            )
            .await?;
            let agents = workspace.join(".agents/agents");
            tokio::fs::create_dir_all(&agents).await?;
            let instructions = format!(
                "---\nname: koharu-editor\ndescription: Edit a manga through Koharu tools.\nmainAgent: true\nsubagent: false\ncommandExecutionPolicy: off\ntools: []\nplugins: [../plugins/koharu-editor]\n---\n\n{EDITOR_INSTRUCTIONS}\n"
            );
            tokio::fs::write(agents.join("koharu-editor.md"), instructions).await?;
            command.args([
                "--output-format",
                "stream-json",
                "--mode",
                "accept-edits",
                "--sandbox",
                "--agent",
                "koharu-editor",
            ]);
        }
    }
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    if let Some(reasoning) = config.reasoning.as_ref().or(config.effort.as_ref()) {
        command
            .arg(match config.backend {
                CliBackend::Pi => "--thinking",
                CliBackend::Agy => "--effort",
            })
            .arg(reasoning);
    }
    if matches!(config.backend, CliBackend::Agy) {
        command.arg("-p");
    } else {
        command.arg("--");
    }
    let mut child = command
        .arg(prompt)
        .spawn()
        .context("start the editor agent")?;
    let mut output = BufReader::new(
        child
            .stdout
            .take()
            .context("editor agent stdout is missing")?,
    )
    .lines();
    let mut answer = String::new();
    let mut failure = None;
    while let Some(line) = output.next_line().await? {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if matches!(config.backend, CliBackend::Agy) {
            if let Some(delta) = value["step_update"]["text_delta"].as_str() {
                events(Event::Text {
                    delta: delta.to_owned(),
                });
            }
            if value["event"] == "result" {
                let result = &value["result"];
                if result["status"] != "SUCCESS" {
                    bail!(
                        "{}",
                        result["error"]
                            .as_str()
                            .unwrap_or("AGY returned an unsuccessful result")
                    );
                }
                answer = result["response"].as_str().unwrap_or_default().to_owned();
            }
            continue;
        }
        match value["type"].as_str().unwrap_or_default() {
            "extension_error" => bail!(
                "Pi extension failed: {}",
                value["error"].as_str().unwrap_or("unknown error")
            ),
            "message_update" => {
                let message = &value["assistantMessageEvent"];
                if let Some(delta) = message["delta"].as_str() {
                    match message["type"].as_str() {
                        Some("text_delta") => events(Event::Text {
                            delta: delta.to_owned(),
                        }),
                        Some("thinking_delta") => events(Event::Reasoning {
                            delta: delta.to_owned(),
                        }),
                        _ => {}
                    }
                }
            }
            "tool_execution_start" => events(Event::ToolStarted {
                tool: value["toolName"].as_str().unwrap_or_default().to_owned(),
                arguments: value["args"].clone(),
            }),
            "tool_execution_end" => events(Event::ToolFinished {
                tool: value["toolName"].as_str().unwrap_or_default().to_owned(),
                error: (value["isError"] == true).then(|| {
                    value["result"]["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|item| item["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                }),
            }),
            "message_end" if value["message"]["role"] == "assistant" => {
                let message = &value["message"];
                failure = matches!(message["stopReason"].as_str(), Some("error" | "aborted")).then(
                    || {
                        message["errorMessage"]
                            .as_str()
                            .unwrap_or("Pi request failed")
                            .to_owned()
                    },
                );
                answer = message["content"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|item| item["type"] == "text")
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
            }
            _ => {}
        }
    }
    let status = child.wait().await?;
    if let Some(failure) = failure {
        bail!("{failure}");
    }
    if !status.success() {
        use std::io::{Read as _, Seek as _};
        let mut log = log;
        log.seek(std::io::SeekFrom::Start(
            log.metadata()?.len().saturating_sub(2048),
        ))?;
        let mut tail = Vec::new();
        log.take(2048).read_to_end(&mut tail)?;
        bail!(
            "agent exited with {status}: {}",
            String::from_utf8_lossy(&tail).trim()
        );
    }
    ensure!(
        !answer.trim().is_empty(),
        "the external agent returned no answer"
    );
    Ok(answer)
}
