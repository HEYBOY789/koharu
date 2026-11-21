//! A run keeps the session it started with while the canvas switches.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use anyhow::Context as _;
use koharu_agent::{Agent, AgentConfig, run_cli};
use koharu_llm::{Context, Size, SummaryPrompts};
use serde::Serialize;
use specta::Type;
use tauri::{State, ipc::Channel};
use tokio_util::task::AbortOnDropHandle;
use tracing::Instrument as _;

use super::Result;
use crate::app::App;

/// The agent's runs, numbered in the order they started.
static RUNS: AtomicU64 = AtomicU64::new(0);

/// What an agent run tells the panel: the agent's events as they come, then
/// how it ended. A cancelled run tells nothing more.
#[derive(Clone, Debug, Serialize, Type)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Event { event: koharu_agent::Event },
    Completed { answer: String },
    Failed { error: String },
}

/// Runs `prompt` on the open project, in its conversation, in place of the
/// run going, and answers the run's id at once; `on_event` hears the run's
/// events and how it ended. The model connects inside the run, so closing
/// the project stops a connection as it stops a run.
#[tauri::command]
#[specta::specta]
pub async fn run_agent(
    app: State<'_, Arc<App>>,
    prompt: String,
    on_event: Channel<AgentEvent>,
) -> Result<u64> {
    let config: AgentConfig = koharu_config::get()?;
    let cli = config.cli();
    let model = config.model.clone();
    if cli.is_none() && model.is_none() {
        return Err(anyhow::anyhow!("no agent model is set in the preferences").into());
    }
    let span = tracing::info_span!("agent run", sentry.op = "agent.run", ?model);
    let id = RUNS.fetch_add(1, Ordering::Relaxed) + 1;
    let mut project = app.project.lock();
    let project = project.as_mut().context("no project is open")?;
    let project_id = project.session.lock().id();
    let mut runs = app.agent_runs.lock();
    if runs.contains_key(&project_id) {
        return Err(anyhow::anyhow!("an agent is already running on this project").into());
    }
    let tools = app.tools(project);
    let session = Arc::clone(&project.session);
    let page = project.page;
    let external = if cli.is_some() {
        if project.external_conversation.is_none() {
            project.external_conversation =
                Some(Arc::new(tempfile::tempdir().map_err(anyhow::Error::from)?));
        }
        project.external_conversation.clone()
    } else {
        None
    };
    let conversation = project
        .conversation
        .as_ref()
        .and_then(|(made, conversation)| {
            (Some(made) == model.as_ref()).then(|| Arc::clone(conversation))
        });
    let running = Arc::clone(&app);
    let task = tokio::spawn(
        async move {
            let events = on_event.clone();
            let result = async {
                if let Some(cli) = cli {
                    let workspace = external.context("the external agent has no workspace")?;
                    let prompt = page.map_or_else(
                        || prompt.clone(),
                        |id| format!("The editor is showing page ID {id}.\n\n{prompt}"),
                    );
                    return run_cli(tools, &cli, workspace.path(), &prompt, move |event| {
                        let _ = events.send(AgentEvent::Event { event });
                    })
                    .await;
                }
                let model = model.context("no agent model is set in the preferences")?;
                let conversation = match conversation {
                    Some(conversation) => conversation,
                    None => {
                        let connected = koharu_llm::connect(&model, Size::AGENT).await?;
                        let conversation = Arc::new(tokio::sync::Mutex::new(Agent::new(
                            tools,
                            connected,
                            Context::new(SummaryPrompts::DEFAULT),
                        )));
                        if let Some(project) = running.project.lock().as_mut()
                            && Arc::ptr_eq(&project.session, &session)
                        {
                            project.conversation = Some((model, Arc::clone(&conversation)));
                        }
                        conversation
                    }
                };
                let mut agent = conversation.lock().await;
                agent.set_config(config);
                anyhow::Ok(
                    agent
                        .run(&prompt, move |event| {
                            let _ = events.send(AgentEvent::Event { event });
                        })
                        .await?,
                )
            }
            .await;
            let _ = on_event.send(match result {
                Ok(answer) => AgentEvent::Completed { answer },
                Err(error) => AgentEvent::Failed {
                    error: format!("{error:#}"),
                },
            });
            let mut runs = running.agent_runs.lock();
            if runs.get(&project_id).is_some_and(|(going, _)| *going == id) {
                runs.remove(&project_id);
            }
        }
        .instrument(span),
    );
    runs.insert(project_id, (id, AbortOnDropHandle::new(task)));
    Ok(id)
}

#[tauri::command]
#[specta::specta]
pub fn stop_agent(app: State<'_, Arc<App>>, run: u64) {
    app.agent_runs.lock().retain(|_, (going, _)| *going != run);
}

/// Starts the open project's conversation over, as a chat app's new chat
/// does: the run going stops, and the next run begins a new conversation.
#[tauri::command]
#[specta::specta]
pub fn reset_agent(app: State<'_, Arc<App>>) {
    if let Some(project) = app.project.lock().as_mut() {
        app.agent_runs.lock().remove(&project.session.lock().id());
        project.conversation = None;
        project.external_conversation = None;
    }
}
