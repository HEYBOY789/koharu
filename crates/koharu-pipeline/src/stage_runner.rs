use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use koharu_scene::{EntityId, Patch};

use crate::{
    ErrorKind, PipelineConfig, PipelineError, Progress, ProgressSink, Stage, StopToken,
    accelerator::AcceleratorGate,
    progress,
    resources::ResourceMonitor,
    stages::{StageInput, Stages},
};

pub(crate) struct StageRunner {
    stages: Stages,
    accelerator: AcceleratorGate,
    retry_delays: Vec<Duration>,
}

impl StageRunner {
    #[cfg(test)]
    pub(crate) fn set_retry_delays(&mut self, delays: Vec<Duration>) {
        self.retry_delays = delays;
    }
    #[cfg(test)]
    pub(crate) fn replace_translation(
        &mut self,
        processor: Box<dyn crate::stages::StageProcessor>,
    ) {
        self.stages.replace_translation(processor);
    }
    pub(crate) fn pending_pages(
        &self,
        scene: &koharu_scene::Snapshot,
        pages: &[EntityId],
    ) -> Result<Vec<EntityId>> {
        self.stages.pending_pages(scene, pages)
    }
    pub(crate) fn new(
        config: &PipelineConfig,
        translator: koharu_translator::Translator,
        device: &koharu_ml::Device,
        resources: Arc<ResourceMonitor>,
    ) -> Result<Self> {
        Ok(Self {
            stages: Stages::new(config, translator, device)?,
            accelerator: AcceleratorGate::new(device, resources),
            retry_delays: [15, 30, 60, 120, 300]
                .into_iter()
                .map(Duration::from_secs)
                .collect(),
        })
    }

    #[tracing::instrument(skip_all)]
    pub(crate) async fn run(&self, job: StageJob) -> StageCompletion {
        let started = Instant::now();
        let page = job.input.page();
        let model = self.stages.model(job.stage).to_owned();
        let outcome = self.run_with_recovery(&job, &model).await;
        StageCompletion {
            page,
            stage: job.stage,
            model,
            elapsed: started.elapsed(),
            outcome,
        }
    }

    async fn run_with_recovery(
        &self,
        job: &StageJob,
        model: &str,
    ) -> std::result::Result<StageOutcome, PipelineError> {
        let mut retries = 0;
        loop {
            let failure = match self.run_with_memory_recovery(job, model).await {
                Ok(outcome) => return Ok(outcome),
                Err(failure) => failure,
            };
            if job.stop.stopped() {
                return Ok(StageOutcome::Stopped);
            }
            let Some(delay) = self
                .retry_delays
                .get(retries)
                .and_then(|&backoff| failure.translation_retry_delay(backoff))
            else {
                return Err(failure);
            };
            retries += 1;
            progress::emit(
                job.progress.as_ref(),
                Progress::Retrying {
                    page: job.input.page(),
                    stage: job.stage,
                    model: model.to_owned(),
                    retry: crate::RetryStatus {
                        attempt: retries as u8,
                        max_attempts: self.retry_delays.len() as u8,
                        delay_seconds: delay.as_secs() as u32,
                    },
                },
            );
            tracing::warn!(stage = %job.stage, page = %job.input.page(), attempt = retries, delay_seconds = delay.as_secs(), "waiting to retry a temporary translation outage");
            // The failed attempt released the accelerator lane. Keep the same
            // page job and inputs so committed pages and chapter wiki stay intact.
            if !wait_for_retry(delay, &job.stop).await {
                return Ok(StageOutcome::Stopped);
            }
        }
    }

    async fn run_with_memory_recovery(
        &self,
        job: &StageJob,
        model: &str,
    ) -> std::result::Result<StageOutcome, PipelineError> {
        if job.stop.stopped() {
            return Ok(StageOutcome::Stopped);
        }
        let skip = self.stages.skip(job.stage, &job.input).map_err(|error| {
            self.stage_error(
                job.stage,
                model,
                AttemptFailure {
                    kind: ErrorKind::Processing,
                    error,
                },
            )
        })?;
        if skip {
            return Ok(StageOutcome::Skipped);
        }
        let permit = self.accelerator.acquire().await;
        if job.stop.stopped() {
            return Ok(StageOutcome::Stopped);
        }
        let first = self.load_and_process(job, model).await;
        let failure = match first {
            Ok(outcome) => return Ok(outcome),
            Err(failure) if is_out_of_memory(&failure.error) && !job.stop.stopped() => failure,
            Err(failure) => return Err(self.stage_error(job.stage, model, failure)),
        };

        drop(permit);
        tracing::warn!(stage = %job.stage, page = %job.input.page(), error = %failure.error, "retrying stage after memory pressure");
        let _permit = self.accelerator.recover(job.stage, &self.stages).await;
        if job.stop.stopped() {
            return Ok(StageOutcome::Stopped);
        }
        match self.load_and_process(job, model).await {
            Ok(outcome) => Ok(outcome),
            Err(failure) => Err(self.stage_error(job.stage, model, failure)),
        }
    }

    async fn load_and_process(
        &self,
        job: &StageJob,
        model: &str,
    ) -> std::result::Result<StageOutcome, AttemptFailure> {
        progress::emit(
            job.progress.as_ref(),
            Progress::Loading {
                page: job.input.page(),
                stage: job.stage,
                model: model.to_owned(),
            },
        );
        self.stages
            .load(job.stage)
            .await
            .map_err(|error| AttemptFailure {
                kind: ErrorKind::ModelLoad,
                error,
            })?;
        if job.stop.stopped() {
            return Ok(StageOutcome::Stopped);
        }
        progress::emit(
            job.progress.as_ref(),
            Progress::Running {
                page: job.input.page(),
                stage: job.stage,
                model: model.to_owned(),
            },
        );
        self.stages
            .process(job.stage, job.input.clone())
            .await
            .map(|patch| {
                if patch.is_empty() {
                    StageOutcome::Skipped
                } else {
                    StageOutcome::Patch(patch)
                }
            })
            .map_err(|error| AttemptFailure {
                kind: ErrorKind::Processing,
                error,
            })
    }

    fn stage_error(&self, stage: Stage, model: &str, failure: AttemptFailure) -> PipelineError {
        self.stages.unload(stage);
        let message = match failure.kind {
            ErrorKind::ModelLoad => format!("failed to load {model}"),
            _ => format!("{model} failed"),
        };
        PipelineError::new(failure.kind, Some(stage), failure.error.context(message))
    }
}

async fn wait_for_retry(delay: Duration, stop: &StopToken) -> bool {
    let deadline = tokio::time::Instant::now() + delay;
    loop {
        if stop.stopped() {
            return false;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return true;
        }
        tokio::time::sleep(remaining.min(Duration::from_millis(100))).await;
    }
}

struct AttemptFailure {
    kind: ErrorKind,
    error: anyhow::Error,
}

fn is_out_of_memory(error: &anyhow::Error) -> bool {
    error.chain().any(|source| {
        let message = source.to_string().to_ascii_lowercase();
        message.contains("out of memory")
            || message.contains("cuda_error_out_of_memory")
            || message.contains("not enough memory")
    })
}

pub(crate) struct StageJob {
    stage: Stage,
    input: StageInput,
    stop: StopToken,
    progress: Option<ProgressSink>,
}

impl StageJob {
    pub(crate) fn new(
        stage: Stage,
        input: StageInput,
        stop: StopToken,
        progress: Option<ProgressSink>,
    ) -> Self {
        Self {
            stage,
            input,
            stop,
            progress,
        }
    }
}

pub(crate) enum StageOutcome {
    Patch(Patch),
    Skipped,
    Stopped,
}

pub(crate) struct StageCompletion {
    pub(crate) page: EntityId,
    pub(crate) stage: Stage,
    pub(crate) model: String,
    pub(crate) elapsed: Duration,
    pub(crate) outcome: std::result::Result<StageOutcome, PipelineError>,
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn automatic_recovery_wait_can_be_cancelled_without_waiting_minutes() {
        use std::time::Duration;
        let stop = crate::StopToken::default();
        let waiting_stop = stop.clone();
        let waiting = tokio::spawn(async move {
            super::wait_for_retry(Duration::from_secs(300), &waiting_stop).await
        });
        tokio::task::yield_now().await;
        stop.stop();
        assert!(
            !tokio::time::timeout(Duration::from_millis(500), waiting)
                .await
                .unwrap()
                .unwrap()
        );
    }
}
