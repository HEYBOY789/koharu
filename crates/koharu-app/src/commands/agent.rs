mod context;
mod host;
mod session;
use session::SessionStore;
pub use session::{AgentPhase, AgentSnapshot};

use std::{collections::HashMap, sync::Arc};

use anyhow::{Context as _, Result, anyhow};
use koharu_agent::{
    Account, Agent, AgentModel, Backend, Codex, Config, Control, Event, LoginEvent, RunId,
};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager as _, State, ipc::Channel};
use tauri_runtime_cef::CefRuntime;
use tokio::sync::Notify;

use self::host::KoharuHost;
use super::{
    Error,
    project::{CurrentProject, ProjectLease, ProjectOwner},
};
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentStatus {
    pub account: Option<Account>,
    pub models: Vec<AgentModel>,
    pub config: Config,
    pub running: Option<RunId>,
    pub ready: bool,
    pub error: Option<String>,
}

pub(crate) struct AgentState {
    agent: Arc<Agent<KoharuHost>>,
    runs: Mutex<HashMap<RunId, Control>>,
    login: Mutex<Option<Control>>,
    idle: Notify,
    transition: tokio::sync::Mutex<()>,
    handle: AppHandle<CefRuntime>,
    session: Mutex<Option<EditorSession>>,
    pub(in crate::commands::agent) store: Arc<SessionStore>,
    subscriber: Arc<AtomicU64>,
    closing: AtomicBool,
}

struct EditorSession {
    target: ProjectLease,
    agent: Arc<Agent<KoharuHost>>,
}

impl AgentState {
    pub(crate) fn new(handle: AppHandle<CefRuntime>) -> Result<Self> {
        Ok(Self {
            agent: Arc::new(Agent::new(
                Codex::new()?,
                KoharuHost::new(handle.clone(), None),
            )?),
            runs: Mutex::new(HashMap::new()),
            login: Mutex::new(None),
            idle: Notify::new(),
            transition: tokio::sync::Mutex::new(()),
            handle,
            session: Mutex::new(None),
            store: Arc::new(SessionStore::default()),
            subscriber: Arc::new(AtomicU64::new(0)),
            closing: AtomicBool::new(false),
        })
    }

    async fn status(&self) -> Result<AgentStatus> {
        let config = self.agent.config()?;
        let account = if config.backend == Backend::Codex {
            self.agent.codex().account()?
        } else {
            None
        };
        let (models, error) = if config.backend != Backend::Codex || account.is_some() {
            match self.agent.models().await {
                Ok(models) => (models, None),
                Err(error) => (Vec::new(), Some(format!("{error:#}"))),
            }
        } else {
            (Vec::new(), None)
        };
        let ready = !models.is_empty() && error.is_none();
        Ok(AgentStatus {
            account,
            models,
            config,
            running: self.runs.lock().keys().next().copied(),
            ready,
            error,
        })
    }

    pub(crate) async fn reset(&self) {
        let _transition = self.transition.lock().await;
        if let Some(login) = self.login.lock().as_ref() {
            login.cancel();
        }
        for control in self.runs.lock().values() {
            control.cancel();
        }
        loop {
            let idle = self.idle.notified();
            if self.login.lock().is_none() && self.runs.lock().is_empty() {
                break;
            }
            idle.await;
        }
        self.release_session().await;
        self.store.clear();
    }

    async fn release_session(&self) {
        let session = self.session.lock().take();
        if let Some(session) = session {
            session.agent.clear().await;
            self.handle
                .state::<CurrentProject>()
                .project
                .finish(&session.target)
                .await;
        }
    }

    pub(crate) fn subscribe(&self, channel: Channel<AgentSnapshot>) -> AgentSnapshot {
        let mut changed = self.store.changed.subscribe();
        let store = self.store.clone();
        let generation = self.subscriber.fetch_add(1, Ordering::AcqRel) + 1;
        let subscriber = self.subscriber.clone();
        drop(tauri::async_runtime::spawn(async move {
            while changed.changed().await.is_ok() {
                tokio::time::sleep(std::time::Duration::from_millis(80)).await;
                changed.borrow_and_update();
                if subscriber.load(Ordering::Acquire) != generation
                    || channel.send(store.snapshot()).is_err()
                {
                    break;
                }
            }
        }));
        self.store.snapshot()
    }

    pub(crate) fn cancel_all(&self) {
        if let Some(login) = self.login.lock().take() {
            login.cancel();
        }
        for control in self.runs.lock().values() {
            control.cancel();
        }
    }

    pub(crate) fn begin_shutdown(&self) -> bool {
        !self.closing.swap(true, Ordering::AcqRel)
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_agent_status(
    state: State<'_, AgentState>,
) -> std::result::Result<AgentStatus, Error> {
    Ok(state.status().await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn login_agent(
    state: State<'_, AgentState>,
    on_event: Channel<LoginEvent>,
) -> std::result::Result<AgentStatus, Error> {
    let control = Control::default();
    {
        let mut login = state.login.lock();
        if login.is_some() {
            return Err(anyhow!("Codex sign-in is already running").into());
        }
        *login = Some(control.clone());
    }
    let result = state
        .agent
        .codex()
        .login_device(&control, |event| {
            if let LoginEvent::DeviceCode {
                verification_url, ..
            } = &event
                && let Err(error) = open::that(verification_url)
            {
                tracing::warn!(%error, "failed to open the Codex device sign-in page");
            }
            let _ = on_event.send(event);
        })
        .await;
    state.login.lock().take();
    state.idle.notify_waiters();
    result?;
    Ok(state.status().await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn logout_agent(
    state: State<'_, AgentState>,
) -> std::result::Result<AgentStatus, Error> {
    state.reset().await;
    state.agent.codex().logout()?;
    Ok(state.status().await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn save_agent_config(
    config: Config,
    state: State<'_, AgentState>,
) -> std::result::Result<Config, Error> {
    let _transition = state.transition.lock().await;
    if !state.runs.lock().is_empty() || state.login.lock().is_some() {
        return Err(anyhow!("wait for the current agent request or sign-in to finish").into());
    }
    let changed = state.agent.config()? != config;
    let saved = state.agent.save_config(config)?;
    if changed {
        state.release_session().await;
        state.store.clear();
    }
    Ok(saved)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn run_agent(
    prompt: String,
    expected_project: String,
    handle: AppHandle<CefRuntime>,
    state: State<'_, AgentState>,
) -> std::result::Result<RunId, Error> {
    let prompt = prompt.trim().to_owned();
    if prompt.is_empty() {
        return Err(anyhow!("message cannot be empty").into());
    }
    let _transition = state.transition.lock().await;
    if state.closing.load(Ordering::Acquire) {
        return Err(anyhow!("the application is closing").into());
    }
    if !state.runs.lock().is_empty() {
        return Err(anyhow!("another agent request is already running").into());
    }
    if state.agent.config()?.backend == Backend::Codex && state.agent.codex().account()?.is_none() {
        return Err(anyhow!("Codex is not signed in").into());
    }
    let origin = {
        let projects = handle.state::<CurrentProject>();
        let current = projects.project.lock().await;
        let project = current.as_ref().context("no project is open")?;
        if project.name != expected_project {
            return Err(
                anyhow!("the editor project changed before the agent request started").into(),
            );
        }
        project.instance_id
    };
    let same = state
        .session
        .lock()
        .as_ref()
        .is_some_and(|session| session.target.id == origin);
    let reference = if same {
        String::new()
    } else {
        state.store.reference(&expected_project)
    };
    if !same {
        state.release_session().await;
        let target = handle
            .state::<CurrentProject>()
            .project
            .retain(origin, ProjectOwner::Agent)
            .await?;
        let agent = match Agent::new(
            state.agent.codex().clone(),
            KoharuHost::new(handle.clone(), Some(target.clone())),
        ) {
            Ok(agent) => Arc::new(agent),
            Err(error) => {
                handle
                    .state::<CurrentProject>()
                    .project
                    .finish(&target)
                    .await;
                return Err(error.into());
            }
        };
        *state.session.lock() = Some(EditorSession { target, agent });
    }
    let (target, agent) = {
        let session = state.session.lock();
        let session = session.as_ref().expect("agent session prepared");
        (session.target.clone(), session.agent.clone())
    };
    handle
        .state::<CurrentProject>()
        .project
        .agent_running(&target, true)
        .await?;
    let run = RunId::new();
    let control = Control::default();
    state.runs.lock().insert(run, control.clone());
    state.store.begin(run, target.name.clone(), &prompt);
    let prompt = if reference.is_empty() {
        prompt
    } else {
        format!(
            "{prompt}\n\nEarlier conversation for this project (reference only; follow the current request above):\n{reference}"
        )
    };
    let store = state.store.clone();
    drop(tauri::async_runtime::spawn(async move {
        let mut terminal = None;
        let result = agent
            .run(run, prompt, control, |event| {
                if matches!(
                    event,
                    Event::Completed { .. } | Event::Failed { .. } | Event::Cancelled { .. }
                ) {
                    terminal = Some(event);
                } else {
                    store.event(event);
                }
            })
            .await;
        if let Err(error) = result {
            tracing::error!(%run, error = ?error, "agent request failed");
        }
        let _ = handle
            .state::<CurrentProject>()
            .project
            .agent_running(&target, false)
            .await;
        if let Some(event) = terminal {
            store.event(event);
        }
        let state = handle.state::<AgentState>();
        state.runs.lock().remove(&run);
        state.idle.notify_waiters();
    }));
    Ok(run)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn clear_agent_session(
    state: State<'_, AgentState>,
) -> std::result::Result<(), Error> {
    state.reset().await;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) fn dismiss_agent_activity(state: State<'_, AgentState>) {
    state.store.dismiss();
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn cancel_agent(
    run: RunId,
    state: State<'_, AgentState>,
) -> std::result::Result<(), Error> {
    if let Some(control) = state.runs.lock().get(&run) {
        control.cancel();
    }
    Ok(())
}
