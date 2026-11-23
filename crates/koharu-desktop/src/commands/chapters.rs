use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::Context as _;
use koharu_scene::{Chapter, ChapterId, Edit, Op, PageId, ProjectId, ProjectMetadata, Session};
use serde::Deserialize;
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;

use super::Result;
use crate::{
    app::App,
    chapters::{ChapterJob, ChapterMode, SummarizerConfig},
};

#[tauri::command]
#[specta::specta]
pub fn get_project_metadata(
    app: State<'_, Arc<App>>,
    project: ProjectId,
) -> Result<ProjectMetadata> {
    let session = app.session()?;
    let current = session.lock().project();
    if current.id() != project {
        return Err(anyhow::anyhow!("the open project changed").into());
    }
    Ok(current.metadata().clone())
}

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
    SetSummarizeAll {
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
    AddWiki {
        slug: String,
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
                chapter.summarize = koharu_config::get::<SummarizerConfig>()?.summarize_default;
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
            ChapterEdit::SetSummarizeAll { summarize } => {
                for chapter in &mut chapters.entries {
                    chapter.summarize = summarize;
                }
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
            ChapterEdit::AddWiki { slug } => metadata.wiki.insert(&slug)?,
            ChapterEdit::RemoveWiki { slug } => {
                anyhow::ensure!(metadata.wiki.remove(&slug), "the wiki has no such file");
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

/// Merge Markdown files into the selected project's wiki in one undo step.
#[tauri::command]
#[specta::specta]
pub async fn import_wiki(
    app: State<'_, Arc<App>>,
    window: Window<CefRuntime>,
    project: ProjectId,
) -> Result<Vec<String>> {
    let session = app.session()?;
    if session.lock().id() != project {
        return Err(anyhow::anyhow!("the open project changed").into());
    }
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
    else {
        return Ok(Vec::new());
    };
    let root = folder.path().to_owned();
    Ok(tokio::task::spawn_blocking(move || {
        let files = read_wiki_folder(&root)?;
        merge_wiki(&mut session.lock(), files)
    })
    .await??)
}

fn read_wiki_folder(root: &Path) -> anyhow::Result<BTreeMap<String, String>> {
    let nested = root.join("wiki");
    let directory = if nested.is_dir() {
        nested.as_path()
    } else {
        root
    };
    let mut files = BTreeMap::new();
    for entry in fs_err::read_dir(directory)? {
        let path = entry?.path();
        if !path.is_file()
            || !path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
        {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let slug = stem
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join("_")
            .to_ascii_lowercase();
        if slug.is_empty() {
            continue;
        }
        let body = fs_err::read_to_string(&path)?;
        if !body.trim().is_empty() {
            anyhow::ensure!(
                !files.contains_key(&slug),
                "multiple Markdown files match the wiki name {slug}"
            );
            files.insert(slug, body);
        }
    }
    anyhow::ensure!(
        !files.is_empty(),
        "the folder has no non-empty Markdown files"
    );
    Ok(files)
}

fn merge_wiki(
    session: &mut Session,
    files: BTreeMap<String, String>,
) -> anyhow::Result<Vec<String>> {
    let mut metadata = session.project().metadata().clone();
    let mut imported = Vec::with_capacity(files.len());
    for (slug, body) in files {
        metadata.wiki.upsert(&slug, body)?;
        imported.push(slug);
    }
    session.apply(Edit {
        label: "chapter.wikiImport".into(),
        ops: vec![Op::SetProjectMetadata {
            metadata: Box::new(metadata),
        }],
    })?;
    Ok(imported)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_merges_markdown_and_can_be_undone() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let wiki = root.path().join("wiki");
        fs_err::create_dir(&wiki)?;
        fs_err::write(wiki.join("Power System.md"), "# Rules\n")?;
        fs_err::write(wiki.join("Glossary.md"), "# Glossary\n")?;
        fs_err::write(wiki.join("ignored.txt"), "ignored")?;
        fs_err::write(wiki.join("empty.md"), "\n")?;
        let mut session = Session::create(&root.path().join("test.khr"))?;
        let before = session.project().metadata().wiki.clone();
        let imported = merge_wiki(&mut session, read_wiki_folder(root.path())?)?;
        assert_eq!(imported, ["glossary", "power_system"]);
        let project = session.project();
        assert_eq!(
            project.metadata().wiki.file("power_system"),
            Some("# Rules\n")
        );
        assert_eq!(project.metadata().wiki.file("world"), before.file("world"));
        let backup = tempfile::tempdir()?;
        super::super::export::write_wiki(&project.metadata().wiki, backup.path())?;
        assert_eq!(
            read_wiki_folder(backup.path())?,
            read_wiki_folder(root.path())?
        );
        session.undo()?;
        assert_eq!(session.project().metadata().wiki, before);
        Ok(())
    }
}
