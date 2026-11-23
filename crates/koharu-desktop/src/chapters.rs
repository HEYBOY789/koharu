//! Chapter jobs compose summarization with the main pipeline. A job owns
//! its project session even while the editor shows a different one.

mod summarize;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use koharu_pipeline::{Event, Outcome, PipelineConfig, Request, RunId, Stage, Target};
use koharu_scene::{Chapter, ChapterId, Edit, Op, ProjectId, Session};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use tokio_util::sync::CancellationToken;

use crate::{app::App, events};

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChapterMode {
    Summarize,
    Translate,
    SummarizeTranslate,
    Resume,
}

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ChapterPhase {
    Summarizing,
    Translating,
    Finished,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ChapterJob {
    pub project: ProjectId,
    pub name: String,
    pub chapter: String,
    pub completed: usize,
    pub total: usize,
    pub phase: ChapterPhase,
    pub run: Option<RunId>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct ChapterJobs {
    jobs: Mutex<BTreeMap<ProjectId, (ChapterJob, CancellationToken)>>,
}

impl ChapterJobs {
    pub fn list(&self) -> Vec<ChapterJob> {
        self.jobs
            .lock()
            .values()
            .map(|(job, _)| job.clone())
            .collect()
    }

    pub fn stop(&self, project: ProjectId) {
        if let Some((_, stop)) = self.jobs.lock().get(&project) {
            stop.cancel();
        }
    }

    pub fn stop_all(&self) {
        for (_, stop) in self.jobs.lock().values() {
            stop.cancel();
        }
    }
}

impl App {
    pub fn start_chapters(
        self: &Arc<Self>,
        name: String,
        session: Arc<Mutex<Session>>,
        first: ChapterId,
        from_here: bool,
        mode: ChapterMode,
    ) -> Result<()> {
        let project = session.lock().project();
        let chapters = &project.metadata().chapters.entries;
        let at = chapters
            .iter()
            .position(|chapter| chapter.id == first)
            .context("chapter does not exist")?;
        let selected = if from_here {
            &chapters[at..]
        } else {
            &chapters[at..=at]
        };
        let ids = selected
            .iter()
            .map(|chapter| chapter.id)
            .collect::<Vec<_>>();
        self.start_chapter_ids(name, session, ids, mode)
    }

    pub fn start_chapter_ids(
        self: &Arc<Self>,
        name: String,
        session: Arc<Mutex<Session>>,
        ids: Vec<ChapterId>,
        mode: ChapterMode,
    ) -> Result<()> {
        let project = session.lock().project();
        let first = ids
            .first()
            .and_then(|id| project.metadata().chapters.chapter(*id))
            .context("no chapters selected")?;
        let config = koharu_config::get::<PipelineConfig>()?;
        let id = project.id();
        let job = ChapterJob {
            project: id,
            name,
            chapter: first.title.clone(),
            completed: 0,
            total: ids.len(),
            phase: if matches!(mode, ChapterMode::Translate) {
                ChapterPhase::Translating
            } else {
                ChapterPhase::Summarizing
            },
            run: None,
            error: None,
        };
        let stop = CancellationToken::new();
        {
            let mut jobs = self.chapter_jobs.jobs.lock();
            ensure!(
                jobs.get(&id).is_none_or(|(job, _)| !matches!(
                    job.phase,
                    ChapterPhase::Summarizing | ChapterPhase::Translating
                )),
                "a chapter job is running on this project"
            );
            jobs.insert(id, (job.clone(), stop.clone()));
        }
        self.emit(events::CHAPTER_JOB, job);
        let app = Arc::clone(self);
        tauri::async_runtime::spawn(async move {
            let result = app
                .run_chapters(id, &session, &ids, mode, config, &stop)
                .await;
            app.update_chapter_job(id, |job| {
                job.run = None;
                job.phase = match result {
                    _ if stop.is_cancelled() => ChapterPhase::Stopped,
                    Ok(()) => ChapterPhase::Finished,
                    Err(error) => {
                        job.error = Some(format!("{error:#}"));
                        ChapterPhase::Failed
                    }
                };
            });
        });
        Ok(())
    }

    fn update_chapter_job(&self, project: ProjectId, edit: impl FnOnce(&mut ChapterJob)) {
        let change = {
            let mut jobs = self.chapter_jobs.jobs.lock();
            jobs.get_mut(&project).map(|(job, _)| {
                edit(job);
                job.clone()
            })
        };
        if let Some(job) = change {
            self.emit(events::CHAPTER_JOB, job);
        }
    }

    async fn run_chapters(
        &self,
        project: ProjectId,
        session: &Arc<Mutex<Session>>,
        chapters: &[ChapterId],
        mode: ChapterMode,
        config: PipelineConfig,
        stop: &CancellationToken,
    ) -> Result<()> {
        for (index, &id) in chapters.iter().enumerate() {
            if stop.is_cancelled() {
                return Ok(());
            }
            let current = session.lock().project();
            let chapter = current
                .metadata()
                .chapters
                .chapter(id)
                .cloned()
                .context("chapter was removed")?;
            ensure!(
                !chapter.pages.is_empty(),
                "chapter {} has no pages",
                chapter.title
            );
            let summarize = match mode {
                ChapterMode::Summarize | ChapterMode::SummarizeTranslate => true,
                ChapterMode::Resume => {
                    chapter.summarize
                        && !(chapter
                            .summary
                            .as_ref()
                            .is_some_and(|text| !text.trim().is_empty())
                            && chapter
                                .setting
                                .as_ref()
                                .is_some_and(|text| !text.trim().is_empty())
                            && current
                                .metadata()
                                .wiki
                                .files
                                .iter()
                                .any(|file| !file.body.trim().is_empty()))
                }
                ChapterMode::Translate => false,
            };
            if summarize {
                self.update_chapter_job(project, |job| {
                    job.chapter.clone_from(&chapter.title);
                    job.phase = ChapterPhase::Summarizing;
                });
                summarize::run(session, chapter.clone(), &config, stop).await?;
            }
            if !matches!(mode, ChapterMode::Summarize) && !stop.is_cancelled() {
                self.update_chapter_job(project, |job| {
                    job.chapter.clone_from(&chapter.title);
                    job.phase = ChapterPhase::Translating;
                });
                self.translate_chapter(project, session, &chapter, config.clone(), stop)
                    .await?;
            }
            if !stop.is_cancelled() {
                self.update_chapter_job(project, |job| job.completed = index + 1);
            }
        }
        Ok(())
    }

    async fn translate_chapter(
        &self,
        project: ProjectId,
        session: &Arc<Mutex<Session>>,
        chapter: &Chapter,
        config: PipelineConfig,
        stop: &CancellationToken,
    ) -> Result<()> {
        let current = session.lock().project();
        let chapter = current
            .metadata()
            .chapters
            .chapter(chapter.id)
            .context("chapter was removed")?;
        let mut unprocessed = Vec::new();
        let mut pending = BTreeMap::<_, Vec<_>>::new();
        for &page in &chapter.pages {
            let blocks = current.blocks(page)?;
            if blocks.is_empty() && !chapter.translated {
                unprocessed.push(page);
            }
            for (id, block) in blocks {
                if block
                    .translation
                    .as_ref()
                    .is_some_and(|text| !text.content.text().trim().is_empty())
                {
                    continue;
                }
                let needs_ocr = block.source.content.text().trim().is_empty();
                if needs_ocr && chapter.translated {
                    continue;
                }
                let mut stages = Vec::new();
                if needs_ocr {
                    stages.push(Stage::Recognize);
                }
                stages.push(Stage::Translate);
                if !current.has_pixels(block.cleanup.id) {
                    stages.push(Stage::Inpaint);
                }
                pending.entry(stages).or_default().push(id);
            }
        }
        let mut groups = Vec::new();
        if !unprocessed.is_empty() {
            groups.push((
                vec![
                    Stage::Detect,
                    Stage::Recognize,
                    Stage::Translate,
                    Stage::Inpaint,
                ],
                Target::Pages(unprocessed),
            ));
        }
        groups.extend(
            pending
                .into_iter()
                .map(|(stages, blocks)| (stages, Target::Blocks(blocks))),
        );
        for (stages, target) in groups {
            if stop.is_cancelled() {
                return Ok(());
            }
            let run = self.pipeline.run(
                Arc::clone(session),
                Request {
                    target,
                    stages: stages.into_iter().collect(),
                    config: Some(config.clone()),
                },
            )?;
            self.update_chapter_job(project, |job| job.run = Some(run));
            loop {
                let history = tokio::select! {
                    _ = stop.cancelled() => {
                        self.pipeline.stop_run(run);
                        return Ok(());
                    }
                    history = self.pipeline.history(run, Duration::from_secs(30)) => history?,
                };
                match history.last() {
                    Some(Event::Ended {
                        outcome: Outcome::Done,
                        ..
                    }) => break,
                    Some(Event::Ended {
                        outcome: Outcome::Stopped,
                        ..
                    }) => {
                        stop.cancel();
                        return Ok(());
                    }
                    Some(Event::Ended {
                        outcome: Outcome::Failed { reason, .. },
                        ..
                    }) => {
                        bail!("{reason}");
                    }
                    _ => {}
                }
            }
            self.update_chapter_job(project, |job| job.run = None);
        }
        let mut session = session.lock();
        let mut metadata = session.project().metadata().clone();
        if let Some(done) = metadata.chapters.chapter_mut(chapter.id)
            && done.pages == chapter.pages
            && !done.translated
            && !stop.is_cancelled()
        {
            done.translated = true;
            session.apply(Edit {
                label: "chapter.translate".to_owned(),
                ops: vec![Op::SetProjectMetadata {
                    metadata: Box::new(metadata),
                }],
            })?;
        }
        Ok(())
    }
}

pub use summarize::SummarizerConfig;
