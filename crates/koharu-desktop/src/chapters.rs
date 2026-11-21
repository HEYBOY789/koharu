//! Chapter jobs compose summarization with the main pipeline. A job owns
//! its project session even while the editor shows a different one.

mod summarize;

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use anyhow::{Context as _, Result, bail, ensure};
use koharu_pipeline::{Event, Outcome, PipelineConfig, Request, RunId, Stage, Target};
use koharu_scene::{Chapter, ChapterId, Content, Edit, Op, ProjectId, Session};
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
                ChapterMode::Summarize => true,
                ChapterMode::SummarizeTranslate => {
                    chapter.summarize && (chapter.summary.is_none() || chapter.setting.is_none())
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
        mut config: PipelineConfig,
        stop: &CancellationToken,
    ) -> Result<()> {
        let current = session.lock().project();
        let chapter = current
            .metadata()
            .chapters
            .chapter(chapter.id)
            .context("chapter was removed")?;
        if let Some(setting) = &chapter.setting {
            config.translator.instructions.push_str("\n\n");
            config.translator.instructions.push_str(setting);
        }
        let mut groups = BTreeMap::<_, Vec<_>>::new();
        for &page in &chapter.pages {
            let mut blocks = current
                .layers_of(page)?
                .into_iter()
                .filter_map(|layer| {
                    if let Content::Block(block) = &layer.content {
                        Some(block)
                    } else {
                        None
                    }
                })
                .peekable();
            let stages = if blocks.peek().is_none() {
                vec![
                    Stage::Detect,
                    Stage::Recognize,
                    Stage::Translate,
                    Stage::Inpaint,
                ]
            } else if blocks.any(|block| {
                !block.source.content.text().trim().is_empty()
                    && block
                        .translation
                        .as_ref()
                        .is_none_or(|text| text.content.text().trim().is_empty())
            }) {
                vec![Stage::Translate, Stage::Inpaint]
            } else if !chapter.translated {
                // Some detected blocks have yet to be recognized.
                let pending = current.layers_of(page)?.into_iter().any(|layer| {
                    matches!(&layer.content, Content::Block(block) if block.source.content.0.is_empty())
                });
                if !pending {
                    continue;
                }
                vec![Stage::Recognize, Stage::Translate, Stage::Inpaint]
            } else {
                continue;
            };
            groups.entry(stages).or_default().push(page);
        }
        for (stages, pages) in groups {
            if stop.is_cancelled() {
                return Ok(());
            }
            let run = self.pipeline.run(
                Arc::clone(session),
                Request {
                    target: Target::Pages(pages),
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
