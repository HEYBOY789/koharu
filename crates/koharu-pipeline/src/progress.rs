use std::sync::Arc;

use crate::Stage;
use koharu_scene::EntityId;

#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct RetryStatus {
    pub attempt: u8,
    pub max_attempts: u8,
    pub delay_seconds: u32,
}

#[derive(Clone, Debug)]
pub enum Progress {
    Started {
        pages: Vec<EntityId>,
        stages: Vec<Stage>,
    },
    Loading {
        page: EntityId,
        stage: Stage,
        model: String,
    },
    Running {
        page: EntityId,
        stage: Stage,
        model: String,
    },
    Retrying {
        page: EntityId,
        stage: Stage,
        model: String,
        retry: RetryStatus,
    },
    Finished {
        page: EntityId,
        stage: Stage,
        model: String,
        elapsed: std::time::Duration,
    },
    Skipped {
        page: EntityId,
        stage: Stage,
    },
}

pub type ProgressSink = Arc<dyn Fn(Progress) + Send + Sync>;

pub(crate) fn emit(sink: Option<&ProgressSink>, progress: Progress) {
    if let Some(sink) = sink {
        sink(progress);
    }
}
