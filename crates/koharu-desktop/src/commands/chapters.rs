use std::sync::Arc;

use anyhow::Context as _;
use koharu_scene::{Chapter, ChapterId, Edit, Op, PageId, ProjectId};
use serde::Deserialize;
use specta::Type;
use tauri::State;

use super::Result;
use crate::{
    app::App,
    chapters::{ChapterJob, ChapterMode, SummarizerConfig},
};

#[derive(Deserialize, Type)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChapterEdit {
    Create {
        title: String,
        pages: Vec<PageId>,
    },
    Assign {
        chapter: ChapterId,
        pages: Vec<PageId>,
    },
    Rename {
        chapter: ChapterId,
        title: String,
    },
    Remove {
        chapter: ChapterId,
    },
    SetSummarize {
        chapter: ChapterId,
        summarize: bool,
    },
    SetSummary {
        chapter: ChapterId,
        summary: String,
    },
    SetPrompt {
        chapter: ChapterId,
        prompt: String,
    },
    SetWiki {
        slug: String,
        body: String,
    },
    RemoveWiki {
        slug: String,
    },
}

#[tauri::command]
#[specta::specta]
pub async fn edit_chapter(
    app: State<'_, Arc<App>>,
    action: ChapterEdit,
    project: ProjectId,
) -> Result<()> {
    let session = app.session()?;
    Ok(tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut session = session.lock();
        anyhow::ensure!(session.id() == project, "the open project changed");
        let mut metadata = session.project().metadata().clone();
        let chapters = &mut metadata.chapters;
        match action {
            ChapterEdit::Create { title, pages } => {
                let mut chapter = Chapter::new(title);
                for &page in &pages {
                    chapters.remove_page(page);
                }
                chapter.pages = pages;
                chapters.entries.push(chapter);
            }
            ChapterEdit::Assign { chapter, pages } => {
                anyhow::ensure!(
                    chapters.chapter(chapter).is_some(),
                    "chapter does not exist"
                );
                for &page in &pages {
                    chapters.remove_page(page);
                }
                let target = chapters
                    .chapter_mut(chapter)
                    .context("chapter does not exist")?;
                target.pages.extend(pages);
                target.translated = false;
            }
            ChapterEdit::Rename { chapter, title } => {
                chapters
                    .chapter_mut(chapter)
                    .context("chapter does not exist")?
                    .title = title;
            }
            ChapterEdit::Remove { chapter } => {
                chapters.entries.retain(|value| value.id != chapter);
                if let Some(source) = &mut metadata.source {
                    source.chapters.retain(|value| value.chapter != chapter);
                }
            }
            ChapterEdit::SetSummarize { chapter, summarize } => {
                chapters
                    .chapter_mut(chapter)
                    .context("chapter does not exist")?
                    .summarize = summarize;
            }
            ChapterEdit::SetSummary { chapter, summary } => {
                chapters
                    .chapter_mut(chapter)
                    .context("chapter does not exist")?
                    .summary = (!summary.trim().is_empty()).then_some(summary);
            }
            ChapterEdit::SetPrompt { chapter, prompt } => {
                chapters
                    .chapter_mut(chapter)
                    .context("chapter does not exist")?
                    .setting = (!prompt.trim().is_empty()).then_some(prompt);
            }
            ChapterEdit::SetWiki { slug, body } => metadata.wiki.upsert(&slug, body)?,
            ChapterEdit::RemoveWiki { slug } => {
                metadata.wiki.remove(&slug);
            }
        }
        session.apply(Edit {
            label: "chapter.edit".to_owned(),
            ops: vec![Op::SetProjectMetadata {
                metadata: Box::new(metadata),
            }],
        })?;
        Ok(())
    })
    .await??)
}

#[tauri::command]
#[specta::specta]
pub fn run_chapters(
    app: State<'_, Arc<App>>,
    project: ProjectId,
    chapter: ChapterId,
    from_here: bool,
    mode: ChapterMode,
) -> Result<()> {
    let (name, session) = {
        let open = app.project.lock();
        let open = open.as_ref().context("no project is open")?;
        if open.session.lock().id() != project {
            return Err(anyhow::anyhow!("the open project changed").into());
        }
        (open.name.clone(), Arc::clone(&open.session))
    };
    Ok(app
        .inner()
        .start_chapters(name, session, chapter, from_here, mode)?)
}

#[tauri::command]
#[specta::specta]
pub fn get_chapter_jobs(app: State<'_, Arc<App>>) -> Vec<ChapterJob> {
    app.chapter_jobs.list()
}

#[tauri::command]
#[specta::specta]
pub fn stop_chapter_job(app: State<'_, Arc<App>>, project: ProjectId) {
    app.chapter_jobs.stop(project);
}

#[tauri::command]
#[specta::specta]
pub fn get_summarizer_settings() -> Result<SummarizerConfig> {
    Ok(koharu_config::get()?)
}

#[tauri::command]
#[specta::specta]
pub fn save_summarizer_settings(settings: SummarizerConfig) -> Result<()> {
    Ok(koharu_config::update(|config| *config = settings)?)
}
