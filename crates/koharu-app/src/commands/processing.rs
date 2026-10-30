use std::{collections::HashMap, fmt, sync::Arc};

use anyhow::{Context as _, Result};
use koharu_pipeline::{Committer, Progress, RunStatus, StageOutput, StopToken};
use koharu_scene::Snapshot;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Manager as _, State, ipc::Channel};
use tauri_runtime_cef::CefRuntime;
use uuid::Uuid;

use super::{
    ChannelExt as _, Error,
    canvas::CanvasChannel,
    project::{CurrentProject, JobProject},
};
use koharu_desktop::Desktop;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize, Type)]
#[serde(transparent)]
pub struct JobId(Uuid);

impl JobId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Job {
    pub id: JobId,
    pub project: Option<String>,
    pub state: JobState,
    #[specta(type = f64)]
    pub completed: usize,
    #[specta(type = f64)]
    pub total: usize,
    pub page: Option<koharu_scene::EntityId>,
    pub chapter: Option<koharu_scene::ChapterId>,
    pub chapter_title: Option<String>,
    pub phase: Option<ChapterPhase>,
    pub stage: Option<koharu_pipeline::Stage>,
    pub model: Option<String>,
    pub detail: Option<String>,
    pub retry: Option<koharu_pipeline::RetryStatus>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChapterPhase {
    Import,
    Summarize,
    Translate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Running,
    Finished,
    Failed,
    Stopped,
}

#[derive(Default)]
pub(crate) struct Processing {
    pub(crate) admission: Arc<tokio::sync::Mutex<()>>,
    pub(crate) stops: Mutex<HashMap<JobId, StopToken>>,
    pub(crate) jobs: Mutex<HashMap<JobId, Job>>,
    pub(crate) inpainting_mask: Mutex<Option<koharu_pipeline::InpaintingMask>>,
}

impl Processing {
    pub(crate) fn try_admit(&self) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        self.admission
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("another process is already running"))
    }

    pub(crate) async fn wait(&self, stop: &StopToken) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let admission = self.admission.clone().lock_owned();
        tokio::pin!(admission);
        loop {
            if stop.stopped() {
                return None;
            }
            tokio::select! {
                permit = &mut admission => return (!stop.stopped()).then_some(permit),
                () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            }
        }
    }
    pub(crate) fn busy_for(&self, name: &str) -> bool {
        self.jobs.lock().values().any(|job| {
            job.state == JobState::Running
                && job.project.as_deref().is_none_or(|project| project == name)
        })
    }
}

#[derive(Default)]
pub(crate) struct JobChannel {
    pub(crate) channel: Mutex<Option<Channel<Job>>>,
}

pub(crate) struct PipelineCommitter {
    handle: AppHandle<CefRuntime>,
    project: JobProject,
}

impl PipelineCommitter {
    pub(crate) fn new(handle: AppHandle<CefRuntime>, project: JobProject) -> Self {
        Self { handle, project }
    }
}

#[async_trait::async_trait]
impl Committer for PipelineCommitter {
    async fn commit(&mut self, output: StageOutput) -> Result<Snapshot> {
        let projects = self.handle.state::<CurrentProject>();
        let (mut project, visible) = projects.project.target(&self.project).await?;
        let Some(commit) = project.commit_rebased(output.patch).await? else {
            return Ok(project.snapshot());
        };
        project.record_commit(&commit);
        let page = project.active_page();
        let snapshot = commit.snapshot.clone();
        if visible {
            let desktop = self.handle.state::<Desktop>();
            desktop.synchronize(&commit.snapshot, page, &commit).await?;
            let canvas = desktop.canvas_state();
            self.handle.state::<CanvasChannel>().channel.publish(canvas);
        }
        Ok(snapshot)
    }
}

pub(crate) fn progress_sink(
    handle: AppHandle<CefRuntime>,
    id: JobId,
) -> koharu_pipeline::ProgressSink {
    let progress = Arc::new(Mutex::new((0_usize, 0_usize)));
    Arc::new(move |event| {
        let retry = match &event {
            Progress::Retrying { retry, .. } => Some(retry.clone()),
            _ => None,
        };
        let update = match event {
            Progress::Started { pages, stages } => {
                let mut progress = progress.lock();
                *progress = (0, pages.len().saturating_mul(stages.len()));
                Some((0, progress.1, None, None, None))
            }
            Progress::Loading { page, stage, model }
            | Progress::Retrying {
                page, stage, model, ..
            } => {
                let progress = progress.lock();
                Some((progress.0, progress.1, Some(page), Some(stage), Some(model)))
            }
            Progress::Finished {
                page, stage, model, ..
            } => {
                let mut progress = progress.lock();
                progress.0 = progress.0.saturating_add(1).min(progress.1);
                Some((progress.0, progress.1, Some(page), Some(stage), Some(model)))
            }
            Progress::Skipped { page, stage } => {
                let mut progress = progress.lock();
                progress.0 = progress.0.saturating_add(1).min(progress.1);
                Some((progress.0, progress.1, Some(page), Some(stage), None))
            }
            Progress::Running { .. } => None,
        };
        if let Some((completed, total, page, stage, model)) = update {
            let job = {
                let processing = handle.state::<Processing>();
                let mut jobs = processing.jobs.lock();
                jobs.get_mut(&id).map(|job| {
                    job.completed = completed;
                    job.total = total;
                    // Other stages may keep progressing while translation waits.
                    // Keep its recovery visible until translation starts again.
                    if retry.is_some()
                        || job.retry.is_none()
                        || stage == Some(koharu_pipeline::Stage::Translation)
                    {
                        job.page = page;
                        job.stage = stage;
                        job.model = model;
                        job.retry = retry;
                    }
                    job.clone()
                })
            };
            if let Some(job) = job {
                handle.state::<JobChannel>().channel.publish(job);
            }
        }
    })
}

#[tauri::command]
#[specta::specta]
#[allow(clippy::too_many_arguments)]
pub(crate) async fn process(
    handle: AppHandle<CefRuntime>,
    scope: koharu_pipeline::Scope,
    operation: koharu_pipeline::Operation,
    project: State<'_, CurrentProject>,
    processing: State<'_, Processing>,
    job_channel: State<'_, JobChannel>,
) -> std::result::Result<JobId, Error> {
    let (snapshot, origin) = {
        let current = project.project.lock().await;
        let current = current.as_ref().context("no project is open")?;
        (current.snapshot(), current.instance_id)
    };
    let id = JobId::new();
    let stop = StopToken::default();
    let permit = processing.try_admit()?;
    {
        let mut stops = processing.stops.lock();
        stops.insert(id, stop.clone());
    }
    let target = match project.project.bind(origin).await {
        Ok(target) => target,
        Err(error) => {
            processing.stops.lock().remove(&id);
            return Err(error.into());
        }
    };
    let job = Job {
        id,
        project: Some(target.name.clone()),
        state: JobState::Running,
        completed: 0,
        total: 0,
        page: None,
        chapter: None,
        chapter_title: None,
        phase: None,
        stage: None,
        model: None,
        detail: None,
        retry: None,
        error: None,
    };
    processing.jobs.lock().insert(id, job.clone());
    job_channel.channel.publish(job);

    let pipeline = handle.state::<koharu_pipeline::Pipeline>().inner().clone();
    let task_handle = handle.clone();
    let inpainting_mask = processing.inpainting_mask.lock().take();
    drop(tokio::spawn(async move {
        let _permit = permit;
        let request = koharu_pipeline::Request {
            resume: false,
            operation,
            scope,
            stop: stop.clone(),
            progress: Some(progress_sink(task_handle.clone(), id)),
            inpainting_mask,
        };
        let mut committer = PipelineCommitter::new(task_handle.clone(), target.clone());
        let result = pipeline.execute(snapshot, request, &mut committer).await;
        let (stopped, error) = match result {
            Ok(report) => (report.status == RunStatus::Stopped, None),
            Err(error) => {
                tracing::error!(stage = ?error.stage, %error, "processing failed");
                (false, Some(format!("{error:#}")))
            }
        };
        task_handle
            .state::<CurrentProject>()
            .project
            .finish(&target)
            .await;
        task_handle.state::<Processing>().stops.lock().remove(&id);
        let job = task_handle
            .state::<Processing>()
            .jobs
            .lock()
            .get_mut(&id)
            .map(|job| {
                job.state = if stopped {
                    JobState::Stopped
                } else if error.is_some() {
                    JobState::Failed
                } else {
                    JobState::Finished
                };
                job.error = error;
                job.retry = None;
                job.clone()
            });
        if let Some(job) = job {
            task_handle.state::<JobChannel>().channel.publish(job);
        }
    }));
    Ok(id)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn stop_job(
    job: JobId,
    processing: State<'_, Processing>,
) -> std::result::Result<(), Error> {
    let stops = processing.stops.lock();
    let stop = stops
        .get(&job)
        .with_context(|| format!("job {job} is not running"))?;
    stop.stop();
    Ok(())
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    #[tokio::test]
    async fn import_queue_waits_for_current_workflow_and_cancellation_does_not_block_the_next() {
        let processing = Arc::new(Processing::default());
        let current = processing.try_admit().unwrap();
        let stopped = StopToken::default();
        let waiting = {
            let processing = processing.clone();
            let stop = stopped.clone();
            tokio::spawn(async move { processing.wait(&stop).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!waiting.is_finished());
        assert!(processing.try_admit().is_err());
        stopped.stop();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), waiting)
                .await
                .unwrap()
                .unwrap()
                .is_none()
        );
        let next = {
            let processing = processing.clone();
            tokio::spawn(async move { processing.wait(&StopToken::default()).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        assert!(!next.is_finished());
        drop(current);
        let running = tokio::time::timeout(std::time::Duration::from_millis(300), next)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(processing.try_admit().is_err());
        drop(running);
        assert!(processing.try_admit().is_ok());
    }
}
