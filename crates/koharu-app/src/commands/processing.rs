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
    project::{CurrentProject, ProjectLease, ProjectOwner},
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
    pub chapter_progress: Option<ChapterProgress>,
    pub phase: Option<ChapterPhase>,
    pub stage: Option<koharu_pipeline::Stage>,
    pub model: Option<String>,
    pub detail: Option<String>,
    pub retry: Option<koharu_pipeline::RetryStatus>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ChapterProgress {
    #[specta(type = f64)]
    pub completed: usize,
    #[specta(type = f64)]
    pub total: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChapterPhase {
    Inspect,
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
    source_admission: Arc<tokio::sync::Mutex<()>>,
    pub(crate) stops: Mutex<HashMap<JobId, StopToken>>,
    pub(crate) jobs: Mutex<HashMap<JobId, Job>>,
}

impl Processing {
    pub(crate) fn try_admit(&self) -> Result<tokio::sync::OwnedMutexGuard<()>> {
        self.admission
            .clone()
            .try_lock_owned()
            .map_err(|_| anyhow::anyhow!("another process is already running"))
    }

    pub(crate) async fn wait(&self, stop: &StopToken) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        Self::wait_on(&self.admission, stop).await
    }

    pub(crate) async fn wait_source(
        &self,
        stop: &StopToken,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        Self::wait_on(&self.source_admission, stop).await
    }

    async fn wait_on(
        lane: &Arc<tokio::sync::Mutex<()>>,
        stop: &StopToken,
    ) -> Option<tokio::sync::OwnedMutexGuard<()>> {
        let admission = lane.clone().lock_owned();
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
            matches!(job.state, JobState::Running | JobState::Queued)
                && job.phase != Some(ChapterPhase::Inspect)
                && (job.project.as_deref() == Some(name)
                    || job.project.is_none() && job.phase != Some(ChapterPhase::Import))
        })
    }
}

#[derive(Default)]
pub(crate) struct JobChannel {
    pub(crate) channel: Mutex<Option<Channel<Job>>>,
}

pub(crate) struct PipelineCommitter {
    handle: AppHandle<CefRuntime>,
    project: ProjectLease,
}

impl PipelineCommitter {
    pub(crate) fn new(handle: AppHandle<CefRuntime>, project: ProjectLease) -> Self {
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
                    job.state = JobState::Running;
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
pub(crate) async fn process(
    handle: AppHandle<CefRuntime>,
    scope: koharu_pipeline::Scope,
    operation: koharu_pipeline::Operation,
    project: State<'_, CurrentProject>,
) -> std::result::Result<JobId, Error> {
    let origin = {
        let current = project.project.lock().await;
        let current = current.as_ref().context("no project is open")?;
        current.instance_id
    };
    let target = project
        .project
        .retain(origin, ProjectOwner::Pipeline)
        .await?;
    Ok(start_process(
        handle,
        target,
        koharu_pipeline::Request {
            scope,
            operation,
            ..Default::default()
        },
    ))
}

pub(super) fn start_process(
    handle: AppHandle<CefRuntime>,
    target: ProjectLease,
    mut request: koharu_pipeline::Request,
) -> JobId {
    let id = JobId::new();
    let stop = request.stop.clone();
    let processing = handle.state::<Processing>();
    processing.stops.lock().insert(id, stop.clone());
    let job = Job {
        id,
        project: Some(target.name.clone()),
        state: JobState::Queued,
        completed: 0,
        total: 0,
        page: request.inpainting_mask.as_ref().map(|mask| mask.page),
        chapter: None,
        chapter_title: None,
        chapter_progress: None,
        phase: None,
        stage: None,
        model: None,
        detail: None,
        retry: None,
        error: None,
    };
    processing.jobs.lock().insert(id, job.clone());
    handle.state::<JobChannel>().channel.publish(job);

    let pipeline = handle.state::<koharu_pipeline::Pipeline>().inner().clone();
    let task_handle = handle.clone();
    let interactive = request.inpainting_mask.is_some()
        && request.operation
            == koharu_pipeline::Operation::Only {
                stage: koharu_pipeline::Stage::Inpainting,
            };
    request.progress = Some(progress_sink(task_handle.clone(), id));
    drop(tokio::spawn(async move {
        let result: Result<bool> = async {
            let processing = task_handle.state::<Processing>();
            // Ordinary workflows stay FIFO across whole projects. A manual
            // Remove stroke borrows the pipeline lane at a drained stage boundary.
            let _permit = if interactive {
                None
            } else {
                let Some(permit) = processing.wait(&stop).await else {
                    return Ok(true);
                };
                Some(permit)
            };
            let snapshot = task_handle
                .state::<CurrentProject>()
                .project
                .snapshot(&target)
                .await?;
            let mut committer = PipelineCommitter::new(task_handle.clone(), target.clone());
            let report = pipeline
                .execute(snapshot, request, &mut committer)
                .await
                .map_err(|error| {
                    tracing::error!(stage = ?error.stage, %error, "processing failed");
                    error
                })?;
            Ok(report.status == RunStatus::Stopped)
        }
        .await;
        let (stopped, error) = match result {
            Ok(stopped) => (stopped, None),
            Err(error) => (false, Some(format!("{error:#}"))),
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
    id
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

    #[test]
    fn preview_and_other_downloads_do_not_block_project_edits_but_queued_work_reserves_its_target()
    {
        let processing = Processing::default();
        let id = JobId::new();
        let mut job = Job {
            id,
            project: Some("Imported".into()),
            state: JobState::Running,
            completed: 0,
            total: 0,
            page: None,
            chapter: None,
            chapter_title: None,
            chapter_progress: None,
            phase: Some(ChapterPhase::Import),
            stage: None,
            model: None,
            detail: None,
            retry: None,
            error: None,
        };
        processing.jobs.lock().insert(id, job.clone());
        assert!(!processing.busy_for("Editing"));
        assert!(processing.busy_for("Imported"));
        job.phase = Some(ChapterPhase::Inspect);
        job.project = None;
        processing.jobs.lock().insert(id, job.clone());
        assert!(!processing.busy_for("Editing"));
        job.phase = Some(ChapterPhase::Translate);
        job.project = Some("Imported".into());
        job.state = JobState::Queued;
        processing.jobs.lock().insert(id, job.clone());
        assert!(!processing.busy_for("Editing"));
        assert!(processing.busy_for("Imported"));
        job.state = JobState::Stopped;
        processing.jobs.lock().insert(id, job);
        assert!(!processing.busy_for("Imported"));
    }

    #[tokio::test]
    async fn source_work_is_independent_of_inference_but_sources_still_run_one_at_a_time() {
        let processing = Processing::default();
        let inference = processing.try_admit().unwrap();
        let stop = StopToken::default();
        let source = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            processing.wait_source(&stop),
        )
        .await
        .expect("an active translation must not block downloads")
        .unwrap();
        let next_source = processing.wait_source(&stop);
        tokio::pin!(next_source);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut next_source)
                .await
                .is_err()
        );
        drop(inference);
        let inference = processing
            .try_admit()
            .expect("an active download must not block translation");
        drop(source);
        let next_source = tokio::time::timeout(std::time::Duration::from_millis(100), next_source)
            .await
            .unwrap()
            .unwrap();
        assert!(
            processing.try_admit().is_err(),
            "the source must not release inference admission"
        );
        drop(next_source);
        drop(inference);
    }

    #[tokio::test]
    async fn stopped_source_waiter_does_not_release_other_sources_or_inference() {
        let processing = Processing::default();
        let inference = processing.try_admit().unwrap();
        let source = processing.wait_source(&StopToken::default()).await.unwrap();
        let stop = StopToken::default();
        let waiting = processing.wait_source(&stop);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut waiting)
                .await
                .is_err()
        );
        stop.stop();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(300), waiting)
                .await
                .unwrap()
                .is_none()
        );
        assert!(processing.try_admit().is_err());
        let next_stop = StopToken::default();
        let next = processing.wait_source(&next_stop);
        tokio::pin!(next);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), &mut next)
                .await
                .is_err()
        );
        drop(source);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), next)
                .await
                .unwrap()
                .is_some()
        );
        drop(inference);
    }

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
