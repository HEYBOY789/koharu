use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use tokio::io::{AsyncBufReadExt as _, BufReader};
use tokio::process::Command;

use koharu_pipeline::StopToken;
use tauri::{AppHandle, Manager as _};
use tauri_runtime_cef::CefRuntime;

use super::{SummarizerBackend, SummarizerConfig};
use crate::commands::ChannelExt as _;
use crate::commands::processing::{JobChannel, JobId, Processing};

/// Runs one agent turn in the chapter workspace. Returns `true` when the run was
/// cancelled before the agent finished.
pub(crate) async fn run_agent(
    handle: AppHandle<CefRuntime>,
    id: JobId,
    config: &SummarizerConfig,
    workspace: &Path,
    skill: &Path,
    prompt: &str,
    stop: StopToken,
) -> Result<bool> {
    let binary = config.resolved_binary()?;
    let mut command = command_for(&binary);
    command
        .current_dir(workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    match config.backend {
        SummarizerBackend::Agy => {
            command
                .arg("-p")
                .arg(prompt)
                .arg("--dangerously-skip-permissions")
                .arg("--output-format")
                .arg("text");
            if let Some(model) = config.model.as_deref() {
                command.arg("--model").arg(model);
            }
            if let Some(effort) = config.effort.as_deref() {
                command.arg("--effort").arg(effort);
            }
        }
        SummarizerBackend::Pi => {
            command
                .arg("--print")
                .arg("--mode")
                .arg("text")
                .arg("--no-session")
                .arg("--approve")
                .arg("--tools")
                .arg("read,write,edit,ls,find,grep,bash")
                .arg("--skill")
                .arg(skill)
                .arg(prompt);
            if let Some(model) = config.model.as_deref() {
                command.arg("--model").arg(model);
            }
            if let Some(effort) = config.effort.as_deref() {
                command.arg("--thinking").arg(effort);
            }
        }
    }
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    tracing::debug!(?binary, backend = ?config.backend, "starting chapter summarizer");

    let mut child = command
        .spawn()
        .context("failed to start the summarizer agent")?;
    let pid = child.id();
    let stdout = child.stdout.take().context("agent stdout is missing")?;
    let stderr = child.stderr.take().context("agent stderr is missing")?;
    let mut stdout = BufReader::new(stdout).lines();
    let mut stderr = BufReader::new(stderr).lines();
    let mut output = String::new();
    let mut errors = String::new();
    let mut out_open = true;
    let mut err_open = true;
    while out_open || err_open {
        tokio::select! {
            line = stdout.next_line(), if out_open => {
                match line? {
                    Some(line) => {
                        report(&handle, id, &line);
                        output.push_str(&line);
                        output.push('\n');
                    }
                    None => out_open = false,
                }
            }
            line = stderr.next_line(), if err_open => {
                match line? {
                    Some(line) => {
                        report(&handle, id, &line);
                        errors.push_str(&line);
                        errors.push('\n');
                    }
                    None => err_open = false,
                }
            }
            () = tokio::time::sleep(Duration::from_millis(400)) => {
                if stop.stopped() {
                    stop_process(&mut child, pid).await;
                    return Ok(true);
                }
            }
        }
    }
    let status = child.wait().await?;
    if stop.stopped() {
        return Ok(true);
    }
    if !status.success() {
        let tail = errors
            .lines()
            .chain(output.lines())
            .filter(|line| !line.trim().is_empty())
            .rev()
            .take(6)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        if tail.is_empty() {
            bail!("summarizer exited with {status}");
        }
        bail!("summarizer exited with {status}: {tail}");
    }
    Ok(false)
}

fn command_for(path: &Path) -> Command {
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

fn report(handle: &AppHandle<CefRuntime>, id: JobId, line: &str) {
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    let detail = if line.len() > 240 {
        let mut end = 239;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &line[..end])
    } else {
        line.to_owned()
    };
    let job = {
        let processing = handle.state::<Processing>();
        let mut jobs = processing.jobs.lock();
        jobs.get_mut(&id).map(|job| {
            job.detail = Some(detail);
            job.clone()
        })
    };
    if let Some(job) = job {
        handle.state::<JobChannel>().channel.publish(job);
    }
}

pub(crate) async fn stop_process(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    #[cfg(unix)]
    if let Some(pid) = pid {
        let _ = Command::new("kill")
            .args(["-TERM", &format!("-{pid}")])
            .status()
            .await;
    }
    let _ = child.kill().await;
}
