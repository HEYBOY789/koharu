mod bridge;
#[cfg(test)]
mod tests;

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt as _, AsyncReadExt as _, AsyncWriteExt as _, BufReader},
    process::{Child, Command},
    sync::mpsc,
};

use crate::{AgentModel, Backend, CliConfig, Control, Event, Host, RunId};

const NODE_NAMES: &[&str] = if cfg!(windows) {
    &["node.exe", "node"]
} else {
    &["node"]
};

#[derive(Default)]
pub(crate) struct Session {
    workspace: Option<TempDir>,
    conversation: Option<String>,
    process: Option<Process>,
    bridge: Option<bridge::Bridge>,
}

fn resolve(configured: Option<&str>, names: &[&str]) -> Result<PathBuf> {
    if let Some(path) = configured.filter(|path| !path.trim().is_empty()) {
        let path = PathBuf::from(path.trim());
        if !path.is_file() {
            bail!("Executable does not exist: {}", path.display());
        }
        return Ok(path.canonicalize()?);
    }
    let mut directories = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    if let Some(home) = dirs::home_dir() {
        directories.push(home.join(".local/bin"));
        #[cfg(windows)]
        directories.extend([
            home.join("AppData/Local/agy/bin"),
            home.join("AppData/Roaming/npm"),
        ]);
    }
    for directory in directories {
        for name in names {
            let path = directory.join(name);
            if path.is_file() {
                return Ok(path);
            }
        }
    }
    bail!(
        "Could not find {}. Install/sign in with the CLI, or choose its executable in Advanced setup.",
        names[0]
    )
}

pub(crate) fn executable(backend: Backend, config: &CliConfig) -> Result<PathBuf> {
    resolve(
        config.binary.as_deref(),
        match backend {
            Backend::Pi if cfg!(windows) => &["pi.exe", "pi", "pi.cmd"],
            Backend::Agy if cfg!(windows) => &["agy.exe", "agy"],
            Backend::Pi => &["pi"],
            Backend::Agy => &["agy"],
            Backend::Codex => unreachable!(),
        },
    )
}

fn command(backend: Backend, config: &CliConfig) -> Result<Command> {
    let path = executable(backend, config)?;
    let extension = path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or_default();
    let mut command = if ["js", "mjs", "cjs"].contains(&extension) {
        let mut command = Command::new(resolve(config.node.as_deref(), NODE_NAMES)?);
        command.arg(path);
        command
    } else if cfg!(windows) && ["cmd", "bat"].contains(&extension) {
        // npm shims point at a JS entry. Resolve that entry instead of quoting a shell command.
        let directory = path.parent().context("CLI has no parent directory")?;
        let entry = ["@earendil-works", "@mariozechner"]
            .into_iter()
            .map(|scope| {
                directory.join(format!("node_modules/{scope}/pi-coding-agent/dist/cli.js"))
            })
            .find(|path| path.is_file())
            .context(
                "Select Pi's dist/cli.js or executable instead of an unsupported shell shim",
            )?;
        let mut command = Command::new(resolve(config.node.as_deref(), NODE_NAMES)?);
        command.arg(entry);
        command
    } else {
        Command::new(path)
    };
    command.kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    #[cfg(unix)]
    command.process_group(0);
    Ok(command)
}

pub(crate) async fn inspect(
    backend: Backend,
    config: &CliConfig,
) -> Result<(String, Vec<AgentModel>)> {
    let executable = executable(backend, config)?.display().to_string();
    let mut help = command(backend, config)?;
    help.stdin(Stdio::null()).arg("--help");
    let help = tokio::time::timeout(Duration::from_secs(10), help.output())
        .await
        .context("CLI capability check timed out")??;
    let help = String::from_utf8_lossy(&help.stdout);
    let required: &[&str] = if backend == Backend::Agy {
        &["--input-format", "--output-format", "--agent", "--settings"]
    } else {
        &[
            "--mode",
            "--extension",
            "--no-builtin-tools",
            "--no-context-files",
        ]
    };
    if !required.iter().all(|flag| help.contains(flag)) {
        bail!(
            "This CLI version does not support the editor protocol. Update the CLI and check again."
        );
    }
    if backend == Backend::Pi {
        let version = tokio::time::timeout(
            Duration::from_secs(10),
            command(backend, config)?.arg("--version").output(),
        )
        .await
        .context("Pi version check timed out")??;
        let version = String::from_utf8_lossy(&version.stdout);
        let parts: Vec<_> = version
            .trim()
            .trim_start_matches('v')
            .split('.')
            .take(2)
            .filter_map(|v| v.parse::<u32>().ok())
            .collect();
        if parts.len() != 2 || parts[0] != 1 || parts[1] < 4 {
            bail!(
                "The editor requires Pi 1.4 or newer in the 1.x series. Install the current @earendil-works/pi-coding-agent package."
            );
        }
    }
    let mut command = command(backend, config)?;
    command
        .stdin(Stdio::null())
        .arg(if backend == Backend::Agy {
            "models"
        } else {
            "--list-models"
        });
    if backend == Backend::Pi {
        command.arg("--offline");
    }
    let output = tokio::time::timeout(Duration::from_secs(15), command.output())
        .await
        .context("CLI model discovery timed out")??;
    if !output.status.success() {
        bail!(
            "CLI model discovery failed: {}",
            redact(&String::from_utf8_lossy(&output.stderr))
        );
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let models: Vec<_> = text
        .lines()
        .filter_map(|line| {
            if backend == Backend::Agy {
                let (id, name) = line.split_once('\t')?;
                Some(AgentModel {
                    id: id.into(),
                    name: name.into(),
                    reasoning: vec![],
                })
            } else {
                let mut fields = line.split_whitespace();
                let provider = fields.next()?;
                let model = fields.next()?;
                if provider == "provider"
                    || provider.starts_with('-')
                    || matches!(provider, "No" | "Use" | "See:" | "Error:")
                {
                    return None;
                }
                let id = format!("{provider}/{model}");
                Some(AgentModel {
                    name: id.clone(),
                    id,
                    reasoning: vec![],
                })
            }
        })
        .collect();
    if models.is_empty() {
        bail!("No models are available. Open and sign in with this CLI first, then check again.");
    }
    Ok((executable, models))
}

pub(crate) async fn run<F>(
    session: &mut Session,
    backend: Backend,
    config: &CliConfig,
    host: Arc<dyn Host>,
    run: RunId,
    prompt: &str,
    control: &Control,
    publish: &mut F,
) -> Result<String>
where
    F: FnMut(Event) + Send,
{
    control.ensure_running()?;
    if session.workspace.is_none() {
        session.workspace = Some(
            tempfile::Builder::new()
                .prefix("koharu-editor-")
                .tempdir()?,
        );
    }
    let workspace = session.workspace.as_ref().unwrap().path().to_path_buf();
    let (events, mut receiver) = mpsc::channel(32);
    if let Some(bridge) = &session.bridge {
        bridge
            .activate(host.clone(), control.clone(), events, run)
            .await;
    } else {
        session.bridge =
            Some(bridge::Bridge::start(host.clone(), control.clone(), events, run).await?);
    }
    if session.process.is_none() {
        match start_child(
            &workspace,
            backend,
            config,
            &session.bridge.as_ref().unwrap().url,
        )
        .await
        {
            Ok(process) => session.process = Some(process),
            Err(error) => {
                control.cancel();
                session.close().await;
                return Err(error);
            }
        }
    }
    let outcome = session
        .process
        .as_mut()
        .unwrap()
        .ask(backend, host, run, prompt, control, publish, &mut receiver)
        .await;
    if outcome.is_err() {
        control.cancel();
    }
    {
        let deactivating = session.bridge.as_ref().unwrap().deactivate();
        tokio::pin!(deactivating);
        loop {
            tokio::select! {
                () = &mut deactivating => break,
                Some(event) = receiver.recv() => publish(event),
            }
        }
    }
    while let Ok(event) = receiver.try_recv() {
        publish(event);
    }
    match outcome {
        Ok((message, conversation)) => {
            if conversation.is_some() {
                session.conversation = conversation;
            }
            Ok(message)
        }
        Err(error) => {
            let diagnostic = session.close().await;
            Err(error).with_context(|| {
                if diagnostic.is_empty() {
                    "Agent CLI request failed".into()
                } else {
                    format!("Agent CLI request failed: {diagnostic}")
                }
            })
        }
    }
}

impl Session {
    pub(crate) async fn close(&mut self) -> String {
        let mut diagnostic = String::new();
        if let Some(mut process) = self.process.take() {
            drop(process.stdin.take());
            if tokio::time::timeout(Duration::from_secs(2), process.child.wait())
                .await
                .is_err()
            {
                stop_child(&mut process.child).await;
            }
            diagnostic = tokio::time::timeout(Duration::from_secs(2), process.diagnostic)
                .await
                .ok()
                .and_then(Result::ok)
                .unwrap_or_default();
        }
        if let Some(bridge) = self.bridge.take() {
            bridge.close().await;
        }
        self.workspace.take();
        self.conversation.take();
        diagnostic
    }
}

struct Process {
    child: Child,
    stdin: Option<tokio::process::ChildStdin>,
    output: BufReader<tokio::process::ChildStdout>,
    diagnostic: tokio::task::JoinHandle<String>,
}

async fn start_child(
    workspace: &Path,
    backend: Backend,
    config: &CliConfig,
    url: &str,
) -> Result<Process> {
    let mut command = command(backend, config)?;
    command
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match backend {
        Backend::Agy => {
            let plugin = workspace.join(".agents/plugins/koharu-editor");
            tokio::fs::create_dir_all(&plugin).await?;
            tokio::fs::write(plugin.join("plugin.json"), r#"{"name":"koharu-editor"}"#).await?;
            tokio::fs::write(
                plugin.join("mcp_config.json"),
                json!({"mcpServers":{"koharu":{"url":url}}}).to_string(),
            )
            .await?;
            let agents = workspace.join(".agents/agents");
            tokio::fs::create_dir_all(&agents).await?;
            // MCP tools are injected separately; AGY's component registry does not
            // accept the proxy function name `call_mcp_tool` as a built-in component.
            tokio::fs::write(agents.join("koharu-editor.md"), format!("---\nname: koharu-editor\ndescription: Edit the open Koharu manga project through its native tools.\nmainAgent: true\nsubagent: false\ncommandExecutionPolicy: off\ntools: []\n---\n\n{}\n", super::agent::INSTRUCTIONS)).await?;
            command.args([
                "--input-format",
                "stream-json",
                "--output-format",
                "stream-json",
                "--mode",
                "accept-edits",
                "--sandbox",
                "--agent",
                "koharu-editor",
            ]);
        }
        Backend::Pi => {
            let extension = workspace.join("koharu-tools.mjs");
            tokio::fs::write(&extension, include_str!("pi-extension.mjs")).await?;
            command
                .args([
                    "--mode",
                    "rpc",
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
                .arg("--system-prompt")
                .arg(super::agent::INSTRUCTIONS)
                .env("KOHARU_EDITOR_TOOLS", url);
        }
        Backend::Codex => unreachable!(),
    }
    if let Some(model) = config.model.as_deref().filter(|v| !v.is_empty()) {
        command.args(["--model", model]);
    }
    if let Some(reasoning) = config.reasoning {
        let supported = match backend {
            Backend::Pi => matches!(
                reasoning,
                crate::Reasoning::Low
                    | crate::Reasoning::Medium
                    | crate::Reasoning::High
                    | crate::Reasoning::Xhigh
            ),
            Backend::Agy => reasoning != crate::Reasoning::Ultra,
            Backend::Codex => false,
        };
        if !supported {
            bail!("The selected CLI does not support this reasoning level");
        }
        command.args([
            if backend == Backend::Pi {
                "--thinking"
            } else {
                "--effort"
            },
            reasoning.as_str(),
        ]);
    }
    let mut child = command.spawn().context("Could not start the Agent CLI")?;
    let stdin = Some(child.stdin.take().context("CLI stdin missing")?);
    let output = BufReader::new(child.stdout.take().context("CLI stdout missing")?);
    let mut stderr = child.stderr.take().context("CLI stderr missing")?;
    let diagnostic = tokio::spawn(async move {
        let mut tail = Vec::new();
        let mut chunk = [0; 4096];
        while let Ok(count) = stderr.read(&mut chunk).await {
            if count == 0 {
                break;
            }
            tail.extend_from_slice(&chunk[..count]);
            if tail.len() > 65536 {
                tail.drain(..tail.len() - 65536);
            }
        }
        redact(&String::from_utf8_lossy(&tail))
    });
    Ok(Process {
        child,
        stdin,
        output,
        diagnostic,
    })
}

impl Process {
    async fn ask<F>(
        &mut self,
        backend: Backend,
        host: Arc<dyn Host>,
        run: RunId,
        prompt: &str,
        control: &Control,
        publish: &mut F,
        events: &mut mpsc::Receiver<Event>,
    ) -> Result<(String, Option<String>)>
    where
        F: FnMut(Event) + Send,
    {
        let context = host.context().await?;
        let text = format!(
            "{}\n\nUse only the Koharu project tools for all project edits. Do not use file edits, shell commands or other tools to modify project storage.\n<koharu_project_context>\n{}\n</koharu_project_context>\n{}",
            super::agent::INSTRUCTIONS,
            context,
            prompt
        );
        let input = if backend == Backend::Agy {
            json!({"event":"user","message":{"content":text}})
        } else {
            json!({"id":"koharu-prompt","type":"prompt","message":text})
        };
        let stdin = &mut self.stdin;
        let output = &mut self.output;
        let outcome = async {
        let input_pipe = stdin.as_mut().context("CLI input pipe closed")?;
        tokio::select! {
            () = control.cancelled() => { control.ensure_running()?; }
            written = async { input_pipe.write_all(format!("{input}\n").as_bytes()).await?; input_pipe.flush().await } => { written?; }
        }
        let mut parser = Stream::default();
        let mut line = Vec::new();
        loop {
            let mut limited = (&mut *output).take((16 * 1024 * 1024 + 1 - line.len()) as u64);
            tokio::select! {
                biased;
                () = control.cancelled() => { control.ensure_running()?; }
                Some(event) = events.recv() => { publish(event); }
                count = limited.read_until(b'\n', &mut line) => {
                    let count = count?;
                    if count == 0 { bail!("Agent CLI exited before a completed result"); }
                    if line.len() > 16 * 1024 * 1024 { bail!("Agent CLI record exceeded 16 MiB"); }
                    let value: Value = serde_json::from_slice(&line).context("Agent CLI returned invalid JSON")?;
                    line.clear();
                    if let Some(message) = parser.accept(backend, value, run, publish)? { return Ok::<_, anyhow::Error>((message, parser.conversation)); }
                }
                () = tokio::time::sleep(Duration::from_secs(20 * 60)) => bail!("Agent CLI stopped responding for 20 minutes"),
            }
        }
    }.await;
        outcome
    }
}

async fn stop_child(child: &mut Child) {
    #[cfg(windows)]
    if let Some(id) = child.id() {
        let mut kill = Command::new("taskkill");
        kill.args(["/PID", &id.to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x0800_0000);
        let _ = kill.status().await;
    }
    #[cfg(unix)]
    if let Some(id) = child.id() {
        // All CLI children start in their own process group. Stop descendants too.
        let _ = Command::new("kill")
            .args(["-KILL", "--", &format!("-{id}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[derive(Default)]
struct Stream {
    conversation: Option<String>,
    text: String,
    error: Option<String>,
}

impl Stream {
    fn accept(
        &mut self,
        backend: Backend,
        value: Value,
        run: RunId,
        publish: &mut impl FnMut(Event),
    ) -> Result<Option<String>> {
        if backend == Backend::Agy {
            if let Some(id) = value.get("conversation_id").and_then(Value::as_str) {
                self.conversation = Some(id.into());
            }
            if value["event"] == "step_update" {
                if let Some(delta) = value["step_update"]["text_delta"].as_str() {
                    publish(Event::TextDelta {
                        run,
                        delta: delta.into(),
                    });
                }
            }
            if value["event"] == "result" {
                let result = &value["result"];
                if let Some(id) = result["conversation_id"].as_str() {
                    self.conversation = Some(id.into());
                }
                if result["status"] != "SUCCESS" {
                    bail!(
                        "{}",
                        redact(
                            result["error"]
                                .as_str()
                                .unwrap_or("AGY returned an unsuccessful result")
                        )
                    );
                }
                return Ok(Some(result["response"].as_str().unwrap_or_default().into()));
            }
        } else {
            match value["type"].as_str().unwrap_or_default() {
                "extension_error" => bail!(
                    "Pi tool extension failed: {}",
                    redact(value["error"].as_str().unwrap_or("unknown error"))
                ),
                "response" if value["success"] == false => {
                    bail!(
                        "Pi rejected request: {}",
                        redact(value["error"].as_str().unwrap_or("unknown error"))
                    )
                }
                "message_update" => {
                    let event = &value["assistantMessageEvent"];
                    match event["type"].as_str() {
                        Some("text_delta") => {
                            if let Some(delta) = event["delta"].as_str() {
                                publish(Event::TextDelta {
                                    run,
                                    delta: delta.into(),
                                });
                            }
                        }
                        Some("thinking_delta") => publish(Event::ReasoningDelta {
                            run,
                            delta: String::new(),
                        }),
                        _ => {}
                    }
                }
                "message_end" if value["message"]["role"] == "assistant" => {
                    let message = &value["message"];
                    if message["stopReason"] == "error" || message["stopReason"] == "aborted" {
                        self.error = Some(redact(
                            message["errorMessage"]
                                .as_str()
                                .unwrap_or("Pi request failed"),
                        ));
                    } else {
                        self.error = None;
                    }
                    self.text = message["content"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|block| block["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("");
                }
                "agent_settled" => {
                    if let Some(error) = self.error.take() {
                        bail!("{error}");
                    }
                    return Ok(Some(self.text.clone()));
                }
                _ => {}
            }
        }
        Ok(None)
    }
}

fn redact(text: &str) -> String {
    text.lines()
        .filter(|line| {
            ![
                "bearer ",
                "access_token",
                "refresh_token",
                "api_key",
                "api-key",
                "access-token",
                "refresh-token",
                "password",
                "authorization",
            ]
            .iter()
            .any(|key| line.to_ascii_lowercase().contains(key))
        })
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(4000)
        .collect()
}
