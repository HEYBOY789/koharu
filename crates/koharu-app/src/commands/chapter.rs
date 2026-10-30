use std::path::Path;

use anyhow::{Context as _, Result, bail};
use image::codecs::jpeg::JpegEncoder;
use koharu_pipeline::{Operation, PipelineConfig, Request, RunStatus, Scope, StopToken};
use koharu_scene::{AssetRole, ChapterId, EntityId};
use serde::Serialize;
use specta::Type;
use tauri::{AppHandle, Manager as _, State, WebviewWindow};
use tauri_runtime_cef::CefRuntime;

use super::{
    ChannelExt as _, Error,
    canvas::CanvasChannel,
    processing::{
        ChapterPhase, Job, JobChannel, JobId, JobState, PipelineCommitter, Processing,
        progress_sink,
    },
    project::{CurrentProject, ProjectLease, ProjectOwner},
    summarizer::{self, SummarizerConfig},
};
use koharu_desktop::Desktop;

#[derive(Clone, Debug, Serialize, Type)]
pub struct ChapterInfo {
    pub id: ChapterId,
    pub title: String,
    pub pages: Vec<EntityId>,
    pub has_summary: bool,
    pub has_setting: bool,
    pub summarize: bool,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct ChapterDetail {
    pub id: ChapterId,
    pub title: String,
    pub pages: Vec<EntityId>,
    pub summary: Option<String>,
    pub setting: Option<String>,
    pub wiki: Wiki,
    pub summarize: bool,
}

pub use koharu_scene::Wiki;

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_chapters(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Vec<ChapterInfo>, Error> {
    let project = project.project.lock().await;
    let project = project.as_ref().context("no project is open")?;
    let snapshot = project.snapshot();
    let chapters = snapshot.chapters()?;
    let mut infos = Vec::new();
    for chapter in chapters.entries {
        let pages = snapshot.chapter_pages(chapter.id)?;
        infos.push(ChapterInfo {
            id: chapter.id,
            title: chapter.title,
            pages,
            has_summary: chapter
                .summary
                .is_some_and(|summary| !summary.trim().is_empty()),
            has_setting: chapter
                .setting
                .is_some_and(|setting| !setting.trim().is_empty()),
            summarize: chapter.summarize,
        });
    }
    Ok(infos)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_chapter(
    chapter: ChapterId,
    project: State<'_, CurrentProject>,
) -> std::result::Result<ChapterDetail, Error> {
    let project = project.project.lock().await;
    let project = project.as_ref().context("no project is open")?;
    let snapshot = project.snapshot();
    let value = snapshot
        .chapter(chapter)?
        .context("chapter does not exist")?;
    Ok(ChapterDetail {
        id: value.id,
        title: value.title,
        pages: snapshot.chapter_pages(chapter)?,
        summary: value.summary,
        setting: value.setting,
        wiki: snapshot.wiki()?,
        summarize: value.summarize,
    })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn create_chapter(
    title: String,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let summarize = SummarizerConfig::load()?.read()?.summarize_default;
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let (commit, _) = project.create_chapter(title, summarize).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn rename_chapter(
    chapter: ChapterId,
    title: String,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.rename_chapter(chapter, title).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn assign_page(
    page: EntityId,
    chapter: ChapterId,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.assign_page(page, chapter).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn move_chapter(
    chapter: ChapterId,
    index: u32,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.move_chapter(chapter, index as usize).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_chapter_summarize(
    chapter: Option<ChapterId>,
    summarize: bool,
    project: State<'_, CurrentProject>,
    processing: State<'_, Processing>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    if processing.busy_for(&project.name) {
        return Err(anyhow::anyhow!(
            "wait for the running workflow before changing chapter summary settings"
        )
        .into());
    }
    if let Some(commit) = project.set_chapter_summarize(chapter, summarize).await? {
        project.record_commit(&commit);
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_chapter_summary(
    chapter: ChapterId,
    summary: Option<String>,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.set_chapter_summary(chapter, summary).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_chapter_setting(
    chapter: ChapterId,
    setting: Option<String>,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.set_chapter_setting(chapter, setting).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn set_wiki(
    file: String,
    text: String,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.set_wiki(file, text).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn add_wiki_file(
    file: String,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.add_wiki_file(file).await?;
    project.record_commit(&commit);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_wiki_file(
    file: String,
    project: State<'_, CurrentProject>,
) -> std::result::Result<(), Error> {
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let commit = project.delete_wiki_file(file).await?;
    project.record_commit(&commit);
    Ok(())
}

/// Seeds the wiki from a folder of Markdown files so an existing series wiki
/// does not have to be retyped. Files match by name; an empty file is skipped.
#[tauri::command]
#[specta::specta]
pub(crate) async fn import_wiki(
    window: WebviewWindow<CefRuntime>,
    project: State<'_, CurrentProject>,
) -> std::result::Result<Vec<String>, Error> {
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
    else {
        return Ok(Vec::new());
    };
    let root = folder.path().to_owned();
    let files = tokio_rayon::spawn(move || read_wiki_folder(&root)).await?;
    if files.is_empty() {
        return Err(anyhow::anyhow!("the folder has no Markdown files").into());
    }
    let mut project = project.project.lock().await;
    let project = project.as_mut().context("no project is open")?;
    let (commit, imported) = project.import_wiki(files).await?;
    project.record_commit(&commit);
    Ok(imported)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_chapter(
    chapter: ChapterId,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> std::result::Result<(), Error> {
    let (commit, page) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        let commit = project.delete_chapter(chapter).await?;
        project.record_commit(&commit);
        project.reconcile_page();
        (commit, project.active_page())
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    canvas_channel.channel.publish(desktop.canvas_state());
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn summarize_chapter(
    chapter: ChapterId,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<JobId, Error> {
    start(handle, vec![chapter], Some(true), false, false).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn translate_chapter(
    chapter: ChapterId,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<JobId, Error> {
    start(handle, vec![chapter], Some(false), true, false).await
}

/// `summarize = None` follows each chapter's toggle, `Some` forces the choice.
#[tauri::command]
#[specta::specta]
pub(crate) async fn run_chapters(
    chapters: Vec<ChapterId>,
    summarize: Option<bool>,
    translate: bool,
    resume: bool,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<JobId, Error> {
    start(handle, chapters, summarize, translate, resume).await
}

async fn start(
    handle: AppHandle<CefRuntime>,
    chapters: Vec<ChapterId>,
    summarize: Option<bool>,
    translate: bool,
    resume: bool,
) -> std::result::Result<JobId, Error> {
    if chapters.is_empty() {
        return Err(anyhow::anyhow!("no chapters were selected").into());
    }
    let config = SummarizerConfig::load()?.read()?.clone();
    let language = PipelineConfig::load()?
        .read()?
        .translation
        .target_language
        .to_string();
    let (needs_summary, origin) = {
        let project = handle.state::<CurrentProject>();
        let project = project.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        let snapshot = project.snapshot();
        let mut needed = false;
        for chapter in &chapters {
            let value = snapshot
                .chapter(*chapter)?
                .context("chapter does not exist")?;
            if (!resume || !value.pages.is_empty())
                && summarize.unwrap_or(value.summarize)
                && !(resume && summary_ready(&snapshot, *chapter)?)
            {
                needed = true;
                break;
            }
        }
        (needed, project.instance_id)
    };
    if needs_summary {
        config.resolved_binary()?;
        summarizer::check_skill(&config)?;
    }
    let first_title = {
        let project = handle.state::<CurrentProject>();
        let project = project.project.lock().await;
        let project = project.as_ref().context("no project is open")?;
        let snapshot = project.snapshot();
        if project.instance_id != origin {
            return Err(anyhow::anyhow!("the project changed before processing started").into());
        }
        snapshot
            .chapter(chapters[0])?
            .context("chapter does not exist")?
            .title
    };
    let id = JobId::new();
    let stop = StopToken::default();
    let permit = handle.state::<Processing>().try_admit()?;
    {
        let processing = handle.state::<Processing>();
        let mut stops = processing.stops.lock();
        stops.insert(id, stop.clone());
    }
    let target = match handle.state::<CurrentProject>().project.retain(origin, ProjectOwner::Pipeline).await {
        Ok(target) => target,
        Err(error) => {
            handle.state::<Processing>().stops.lock().remove(&id);
            return Err(error.into());
        }
    };
    let job = Job {
        id,
        project: Some(target.name.clone()),
        state: JobState::Running,
        completed: 0,
        total: chapters.len(),
        page: None,
        chapter: chapters.first().copied(),
        chapter_title: Some(first_title),
        phase: if needs_summary {
            Some(ChapterPhase::Summarize)
        } else {
            Some(ChapterPhase::Translate)
        },
        stage: None,
        model: config.model.clone(),
        detail: None,
        retry: None,
        error: None,
    };
    handle
        .state::<Processing>()
        .jobs
        .lock()
        .insert(id, job.clone());
    handle.state::<JobChannel>().channel.publish(job);

    let task = handle.clone();
    drop(tokio::spawn(async move {
        let _permit = permit;
        let outcome = run_bound(
            task.clone(),
            id,
            target.clone(),
            chapters,
            summarize,
            translate,
            resume,
            config,
            language,
            stop,
        )
        .await;
        let (stopped, failure) = match outcome {
            Ok(stopped) => (stopped, None),
            Err(error) => (false, Some(format!("{error:#}"))),
        };
        task.state::<CurrentProject>().project.finish(&target).await;
        task.state::<Processing>().stops.lock().remove(&id);
        let job = task
            .state::<Processing>()
            .jobs
            .lock()
            .get_mut(&id)
            .map(|job| {
                job.state = if stopped {
                    JobState::Stopped
                } else if failure.is_some() {
                    JobState::Failed
                } else {
                    JobState::Finished
                };
                job.detail = None;
                job.error = failure;
                job.retry = None;
                job.clone()
            });
        if let Some(job) = job {
            task.state::<JobChannel>().channel.publish(job);
        }
    }));
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn run_bound(
    task: AppHandle<CefRuntime>,
    id: JobId,
    target: ProjectLease,
    chapters: Vec<ChapterId>,
    summarize: Option<bool>,
    translate: bool,
    resume: bool,
    config: SummarizerConfig,
    language: String,
    stop: StopToken,
) -> Result<bool> {
    let pipeline = task.state::<koharu_pipeline::Pipeline>().inner().clone();
    let mut failure = None;
    let mut stopped = false;
    let outcome: Result<()> = async {
        for (index, chapter) in chapters.iter().copied().enumerate() {
            if stop.stopped() {
                stopped = true;
                break;
            }
            let (title, toggle, pages, summarized) = {
                let projects = task.state::<CurrentProject>();
                let (project, _) = projects.project.target(&target).await?;
                let snapshot = project.snapshot();
                match snapshot.chapter(chapter)? {
                    Some(value) => (
                        value.title,
                        value.summarize,
                        snapshot.chapter_pages(chapter)?,
                        summary_ready(&snapshot, chapter)?,
                    ),
                    None => continue,
                }
            };
            update(&task, id, |job| {
                job.chapter = Some(chapter);
                job.chapter_title = Some(title.clone());
                job.completed = index;
                job.total = chapters.len();
                job.page = None;
                job.stage = None;
                job.detail = None;
                job.retry = None;
            });
            if resume && pages.is_empty() {
                update(&task, id, |job| job.completed = index + 1);
                continue;
            }
            let do_summary = summarize.unwrap_or(toggle) && !(resume && summarized);
            if do_summary {
                update(&task, id, |job| job.phase = Some(ChapterPhase::Summarize));
                match summarize_one(&task, &target, id, chapter, &config, &language, &stop).await {
                    Ok(true) => {}
                    Ok(false) => {
                        stopped = true;
                        break;
                    }
                    Err(error) => {
                        failure = Some(format!("{error:#}"));
                        break;
                    }
                }
            }
            if !translate {
                update(&task, id, |job| job.completed = index + 1);
                continue;
            }
            update(&task, id, |job| {
                job.phase = Some(ChapterPhase::Translate);
                job.detail = None;
            });
            let snapshot = task
                .state::<CurrentProject>()
                .project
                .snapshot(&target)
                .await?;
            let pages = if pages.is_empty() {
                snapshot.chapter_pages(chapter)?
            } else {
                pages
            };
            let pages = if resume {
                pipeline.pending_pages(&snapshot, &pages)?
            } else {
                pages
            };
            if pages.is_empty() {
                update(&task, id, |job| job.completed = index + 1);
                continue;
            }
            let request = Request {
                resume,
                operation: Operation::Full,
                scope: Scope::Pages(pages),
                stop: stop.clone(),
                progress: Some(progress_sink(task.clone(), id)),
                inpainting_mask: None,
            };
            let mut committer = PipelineCommitter::new(task.clone(), target.clone());
            match pipeline.execute(snapshot, request, &mut committer).await {
                Ok(report) if report.status == RunStatus::Stopped => {
                    stopped = true;
                    break;
                }
                Ok(_) => {}
                Err(error) => {
                    failure = Some(format!("{:#}", error));
                    break;
                }
            }
            update(&task, id, |job| job.completed = index + 1);
        }
        Ok(())
    }
    .await;
    if let Err(error) = outcome {
        if failure.is_none() {
            tracing::error!(%error, "chapter run failed");
            failure = Some(format!("{error:#}"));
        }
    }
    if let Some(error) = failure {
        bail!("{error}");
    }
    Ok(stopped)
}

fn summary_ready(snapshot: &koharu_scene::Snapshot, chapter: ChapterId) -> Result<bool> {
    let value = snapshot
        .chapter(chapter)?
        .context("chapter does not exist")?;
    Ok(value
        .summary
        .as_ref()
        .is_some_and(|text| !text.trim().is_empty())
        && value
            .setting
            .as_ref()
            .is_some_and(|text| !text.trim().is_empty())
        && snapshot
            .wiki()?
            .files
            .iter()
            .any(|file| !file.body.trim().is_empty()))
}

/// Returns `false` when cancellation was requested.
async fn summarize_one(
    handle: &AppHandle<CefRuntime>,
    target: &ProjectLease,
    id: JobId,
    chapter: ChapterId,
    config: &SummarizerConfig,
    language: &str,
    stop: &StopToken,
) -> Result<bool> {
    let (path, title, labels, wiki, snapshot) = {
        let projects = handle.state::<CurrentProject>();
        let (project, _) = projects.project.target(target).await?;
        let snapshot = project.snapshot();
        let value = snapshot
            .chapter(chapter)?
            .context("chapter does not exist")?;
        let pages = snapshot.chapter_pages(chapter)?;
        if pages.is_empty() {
            bail!("chapter `{}` has no pages", value.title);
        }
        let mut labels = Vec::with_capacity(pages.len());
        for page in &pages {
            labels.push((*page, snapshot.page(*page)?.page()?.label));
        }
        (
            project.path.clone(),
            value.title,
            labels,
            snapshot.wiki()?,
            snapshot,
        )
    };
    let workspace = path.join("summaries").join(chapter.to_string());
    if workspace.exists() {
        tokio::fs::remove_dir_all(&workspace)
            .await
            .with_context(|| format!("failed to clear {}", workspace.display()))?;
    }
    tokio::fs::create_dir_all(workspace.join("pages")).await?;
    tokio::fs::create_dir_all(workspace.join("wiki")).await?;
    tokio::fs::create_dir_all(workspace.join("chapters")).await?;
    let source = AssetRole::new("source")?;
    for (index, (page, _)) in labels.iter().enumerate() {
        let Some(asset) = snapshot.asset(*page, &source)? else {
            bail!("page {} has no source image", index + 1);
        };
        let bytes = snapshot.read_blob(asset.blob).await?;
        let target = workspace
            .join("pages")
            .join(format!("{:03}.jpg", index + 1));
        tokio_rayon::spawn(move || export_page(bytes.as_ref(), &target))
            .await
            .map_err(|error| error.context("failed to export a chapter page"))?;
    }
    for (page, label) in &labels {
        tracing::trace!(page = %page, %label, "exported chapter page");
    }
    let manifest = labels
        .iter()
        .enumerate()
        .map(|(index, (_, label))| format!("{}. {label}", index + 1))
        .collect::<Vec<_>>()
        .join("\n");
    tokio::fs::write(workspace.join("pages/manifest.md"), &manifest).await?;
    let mut wiki = wiki;
    wiki.ensure_defaults();
    for file in &wiki.files {
        tokio::fs::write(
            workspace.join("wiki").join(format!("{}.md", file.slug)),
            wiki_doc(&file.slug, &file.body),
        )
        .await?;
    }
    tokio::fs::write(workspace.join("setting.md"), "# Translation setting\n\n").await?;
    tokio::fs::write(
        workspace.join("AGENTS.md"),
        "Write `chapters/chapter.md` and `setting.md`. Follow the manga-summarizer skill. \
         Never edit `pages/`. Read every file in `wiki/` first, rewrite each one with new facts \
         appended, and add a new wiki file when the series needs a category the current files do \
         not cover. `wiki/style.md` holds the translation conventions every chapter must follow.\n",
    )
    .await?;
    let skill = summarizer::install_skill(&workspace, config)?;
    let prompt = format!(
        "/skill:manga-summarizer\n\n\
         Summarize exactly one chapter: \"{title}\".\n\
         Read `.agents/skills/manga-summarizer/SKILL.md` and follow it completely.\n\n\
         Ordered pages:\n{manifest}\n\n\
         Required outputs: `chapters/chapter.md` and `setting.md`.\n\
         Rewrite every existing file in `wiki/` and create another wiki file only when this \
         chapter establishes a category the existing files cannot hold.\n\
         Keep `wiki/style.md` current: the translation conventions there must match what \
         `setting.md` asks for.\n\
         Write in {language}. `setting.md` must be a translation system prompt, not a summary.\n\
         Do not read another chapter and do not edit `pages/`."
    );
    let stopped = summarizer::run_agent(
        handle.clone(),
        id,
        config,
        &workspace,
        &skill,
        &prompt,
        stop.clone(),
    )
    .await?;
    if stopped {
        return Ok(false);
    }
    let summary = read_summary(&workspace).await?;
    let setting = tokio::fs::read_to_string(workspace.join("setting.md"))
        .await
        .context("the summarizer did not write setting.md")?;
    if setting.trim().len() < 40 {
        bail!("the summarizer wrote an empty setting.md");
    }
    let mut updated = wiki.clone();
    let mut entries = tokio::fs::read_dir(workspace.join("wiki"))
        .await
        .context("the summarizer left no wiki directory")?;
    let mut seen = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("md") {
            continue;
        }
        let Some(slug) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        let text = tokio::fs::read_to_string(&path).await?;
        if text.trim().is_empty() {
            continue;
        }
        updated
            .upsert(slug, text)
            .with_context(|| format!("the summarizer wrote an invalid wiki file name: {slug}"))?;
        seen.push(slug.to_owned());
    }
    if seen.is_empty() {
        bail!("the summarizer did not write any wiki file");
    }

    if stop.stopped() {
        return Ok(false);
    }
    {
        let projects = handle.state::<CurrentProject>();
        let (mut project, _) = projects.project.target(target).await?;
        if stop.stopped() {
            return Ok(false);
        }
        let snapshot = project.snapshot();
        let mut chapters = snapshot.chapters()?;
        let value = chapters
            .chapter_mut(chapter)
            .context("the chapter was deleted while summarizing")?;
        value.summary = Some(summary);
        value.setting = Some(setting);
        let patch = snapshot.patch(|edit| {
            edit.set_project(&chapters)?;
            edit.set_project(&updated)?;
            Ok(())
        })?;
        if let Some(commit) = project.commit_rebased(patch).await? {
            project.record_commit(&commit);
        }
    }
    Ok(true)
}

fn update(handle: &AppHandle<CefRuntime>, id: JobId, f: impl FnOnce(&mut Job)) {
    let job = {
        let processing = handle.state::<Processing>();
        let mut jobs = processing.jobs.lock();
        jobs.get_mut(&id).map(|job| {
            f(job);
            job.clone()
        })
    };
    if let Some(job) = job {
        handle.state::<JobChannel>().channel.publish(job);
    }
}

/// Seeds an empty category with a heading and, for characters, the status tags
/// the skill contract expects.
fn wiki_doc(slug: &str, value: &str) -> String {
    if !value.trim().is_empty() {
        return value.to_owned();
    }
    match slug {
        "world" => "# World\n\n".to_owned(),
        "characters" => {
            "# Characters\n\nTag status as [ALIVE], [INJURED], [DEAD — chapter], or [TRANSFORMED].\n\n"
                .to_owned()
        }
        "glossary" => "# Glossary and address forms\n\n".to_owned(),
        "timeline" => "# Timeline\n\n".to_owned(),
        "style" => {
            "# Translation style\n\nConventions every chapter follows: voice register per \
             character, sentence length, profanity level, honorifics, sound effects, and terms \
             to localize.\n\n"
                .to_owned()
        }
        other => format!("# {}\n\n", other.replace('_', " ")),
    }
}

/// Reads a wiki folder as `(slug, body)` pairs. A picked series folder is
/// accepted too, in which case its `wiki/` subdirectory is used.
fn read_wiki_folder(root: &Path) -> Result<Vec<(String, String)>> {
    let nested = root.join("wiki");
    let directory = if nested.is_dir() {
        nested
    } else {
        root.to_owned()
    };
    let mut files = Vec::new();
    for entry in std::fs::read_dir(&directory)
        .with_context(|| format!("failed to read {}", directory.display()))?
    {
        let path = entry?.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("md") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        let slug = wiki_slug(stem);
        if slug.is_empty() {
            continue;
        }
        let body = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if body.trim().is_empty() {
            continue;
        }
        files.push((slug, body));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

/// Lowercases a file name and keeps only characters a wiki slug allows.
fn wiki_slug(stem: &str) -> String {
    let mut slug = String::with_capacity(stem.len());
    for character in stem.trim().to_lowercase().chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character);
        } else if !slug.ends_with('_') {
            slug.push('_');
        }
    }
    slug.trim_matches('_').to_owned()
}

fn export_page(bytes: &[u8], target: &Path) -> Result<()> {
    let image = image::load_from_memory(bytes).context("failed to decode a chapter page")?;
    let image = if image.width() > 1600 || image.height() > 1600 {
        image.thumbnail(1600, 1600)
    } else {
        image
    };
    let rgb = image.to_rgb8();
    let mut file = std::fs::File::create(target)
        .with_context(|| format!("failed to create {}", target.display()))?;
    JpegEncoder::new_with_quality(&mut file, 88)
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .context("failed to encode a chapter page")?;
    Ok(())
}

async fn read_summary(workspace: &Path) -> Result<String> {
    let preferred = workspace.join("chapters/chapter.md");
    let path = if preferred.is_file() {
        preferred
    } else {
        let mut entries = tokio::fs::read_dir(workspace.join("chapters")).await?;
        let mut found = None;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) == Some("md") {
                found = Some(path);
                break;
            }
        }
        found.context("the summarizer did not write chapters/chapter.md")?
    };
    let summary = tokio::fs::read_to_string(path).await?;
    if summary.trim().len() < 300 {
        bail!("the summarizer wrote a summary shorter than 300 characters");
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::read_wiki_folder;

    #[tokio::test]
    async fn resume_reuses_only_committed_summary_prompt_and_wiki_outputs() {
        use koharu_scene::{Chapter, Chapters, Session, Wiki, WikiFile};
        let mut session = Session::memory().await.unwrap();
        let mut chapter = Chapter::new("Chapter 1");
        let id = chapter.id;
        chapter.summary = Some("Existing summary".into());
        let patch = session
            .snapshot()
            .patch(|edit| {
                edit.set_project(&Chapters {
                    entries: vec![chapter.clone()],
                })
            })
            .unwrap();
        session.commit(patch).await.unwrap();
        assert!(!super::summary_ready(&session.snapshot(), id).unwrap());
        chapter.setting = Some("Translate using the saved conventions".into());
        let patch = session
            .snapshot()
            .patch(|edit| {
                edit.set_project(&Chapters {
                    entries: vec![chapter.clone()],
                })
            })
            .unwrap();
        session.commit(patch).await.unwrap();
        assert!(!super::summary_ready(&session.snapshot(), id).unwrap());
        let wiki = Wiki {
            files: vec![WikiFile {
                slug: "characters".into(),
                body: "Saved character facts".into(),
            }],
        };
        let patch = session
            .snapshot()
            .patch(|edit| edit.set_project(&wiki))
            .unwrap();
        session.commit(patch).await.unwrap();
        assert!(super::summary_ready(&session.snapshot(), id).unwrap());
        assert_eq!(session.snapshot().wiki().unwrap(), wiki);
    }

    /// A picked series folder must resolve its `wiki/` subdirectory, and file
    /// names must become the slugs the wiki validates.
    #[test]
    fn an_imported_wiki_folder_becomes_slugged_documents() {
        let root = std::env::temp_dir().join(format!("koharu-wiki-{}", std::process::id()));
        let wiki = root.join("wiki");
        std::fs::create_dir_all(&wiki).unwrap();
        std::fs::write(wiki.join("Characters.md"), "# Characters\n\nAoi").unwrap();
        std::fs::write(wiki.join("power_system.md"), "# Powers").unwrap();
        std::fs::write(wiki.join("empty.md"), "   \n").unwrap();
        std::fs::write(wiki.join("notes.txt"), "ignored").unwrap();
        let files = read_wiki_folder(&root).unwrap();
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            files,
            vec![
                ("characters".to_owned(), "# Characters\n\nAoi".to_owned()),
                ("power_system".to_owned(), "# Powers".to_owned()),
            ]
        );
    }
}
