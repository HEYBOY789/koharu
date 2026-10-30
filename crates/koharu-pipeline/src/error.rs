use std::{error::Error as StdError, fmt, time::Duration};

use crate::Stage;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorKind {
    InvalidInput,
    ModelLoad,
    Processing,
    Commit,
    InvalidOutput,
}

#[derive(Debug)]
pub struct PipelineError {
    pub stage: Option<Stage>,
    pub kind: ErrorKind,
    source: anyhow::Error,
}

impl PipelineError {
    pub(crate) fn translation_retry_delay(&self, backoff: Duration) -> Option<Duration> {
        if self.stage != Some(Stage::Translation) || self.kind != ErrorKind::Processing {
            return None;
        }
        self.source
            .chain()
            .filter_map(|source| source.downcast_ref::<koharu_translator::Error>())
            .find_map(|error| error.retry_delay(backoff))
    }

    pub(crate) fn new(
        kind: ErrorKind,
        stage: Option<Stage>,
        source: impl Into<anyhow::Error>,
    ) -> Self {
        Self {
            stage,
            kind,
            source: source.into(),
        }
    }
}

impl fmt::Display for PipelineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.stage {
            Some(stage) if formatter.alternate() => {
                write!(formatter, "pipeline {stage} failed: {:#}", self.source)
            }
            Some(stage) => write!(formatter, "pipeline {stage} failed: {}", self.source),
            None if formatter.alternate() => {
                write!(formatter, "pipeline failed: {:#}", self.source)
            }
            None => write!(formatter, "pipeline failed: {}", self.source),
        }
    }
}

impl StdError for PipelineError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        Some(self.source.as_ref())
    }
}
