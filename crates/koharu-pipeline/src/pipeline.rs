use std::sync::Arc;

use anyhow::{Context as _, Result};
use arc_swap::ArcSwap;
use koharu_config::Config;
use koharu_scene::Snapshot;

use crate::{
    Committer, PipelineConfig, PipelineError, Report, Request, ResourceSnapshot,
    execution::Execution, resources::ResourceMonitor, stage_runner::StageRunner,
};

#[derive(Clone)]
pub struct Pipeline {
    current: Arc<ArcSwap<StageRunner>>,
    resources: Arc<ResourceMonitor>,
    execution: Arc<crate::admission::Admission>,
}

impl Pipeline {
    #[cfg(test)]
    pub(crate) fn test_processors(
        processors: Vec<(crate::Stage, Box<dyn crate::stages::StageProcessor>)>,
    ) -> Self {
        let device = koharu_ml::Device::cpu();
        let resources = ResourceMonitor::new(&device);
        let translator = koharu_translator::Translator::from_config(
            device.clone(),
            Config::memory(Default::default()),
        )
        .unwrap();
        let mut runner = StageRunner::new(
            &PipelineConfig::default(),
            translator,
            &device,
            resources.clone(),
        )
        .unwrap();
        for (stage, processor) in processors {
            runner.replace(stage, processor);
        }
        runner.set_retry_delays(vec![std::time::Duration::from_millis(1); 3]);
        Self {
            current: Arc::new(ArcSwap::from_pointee(runner)),
            resources,
            execution: Arc::default(),
        }
    }
    pub fn pending_pages(
        &self,
        scene: &Snapshot,
        pages: &[koharu_scene::EntityId],
    ) -> Result<Vec<koharu_scene::EntityId>> {
        self.current.load().pending_pages(scene, pages)
    }
    pub fn load(device: koharu_ml::Device) -> Result<Self> {
        Self::from_config(
            PipelineConfig::load()?,
            koharu_translator::ProvidersConfig::load()?,
            device,
        )
    }

    #[tracing::instrument(skip_all)]
    pub fn from_config(
        config: Config<PipelineConfig>,
        providers: Config<koharu_translator::ProvidersConfig>,
        device: koharu_ml::Device,
    ) -> Result<Self> {
        let translator = koharu_translator::Translator::from_config(device.clone(), providers)?;
        let resources = ResourceMonitor::new(&device);
        let runner = {
            let value = config.read()?;
            StageRunner::new(&value, translator.clone(), &device, resources.clone())?
        };
        let current = Arc::new(ArcSwap::from_pointee(runner));
        let watched = current.clone();
        let watched_resources = resources.clone();
        let _watcher = tokio::runtime::Handle::try_current()
            .context("pipeline requires a Tokio runtime")?
            .spawn(async move {
                let mut changes = config.subscribe();
                while changes.changed().await.is_ok() {
                    let runner = config.read().and_then(|value| {
                        StageRunner::new(
                            &value,
                            translator.clone(),
                            &device,
                            watched_resources.clone(),
                        )
                    });
                    match runner {
                        Ok(runner) => watched.store(Arc::new(runner)),
                        Err(error) => tracing::error!(%error, "failed to reload pipeline"),
                    }
                }
            });
        Ok(Self {
            current,
            resources,
            execution: Arc::default(),
        })
    }

    pub fn subscribe_resources(&self) -> tokio::sync::watch::Receiver<ResourceSnapshot> {
        self.resources.start();
        self.resources.subscribe()
    }

    /// External chapter summarizers share inference admission with page models.
    pub async fn reserve_inference(&self, stop: &crate::StopToken) -> Option<impl Send + use<>> {
        self.execution.acquire(false, stop).await
    }

    #[tracing::instrument(skip_all)]
    pub async fn execute(
        &self,
        snapshot: Snapshot,
        request: Request,
        committer: &mut dyn Committer,
    ) -> std::result::Result<Report, PipelineError> {
        let interactive = request.inpainting_mask.is_some()
            && request.operation
                == crate::Operation::Only {
                    stage: crate::Stage::Inpainting,
                };
        let permit = self.execution.acquire(interactive, &request.stop).await;
        let execution = Execution::new(
            self.current.load_full(),
            self.resources.clone(),
            snapshot,
            request,
            committer,
        )?;
        match permit {
            Some(permit) => execution.run(permit).await,
            None => Ok(execution.report(crate::RunStatus::Stopped)),
        }
    }
}
