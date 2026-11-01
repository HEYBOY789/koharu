use super::{
    ChannelExt as _, Error, chapter,
    import::{self, ChapterSource, ImportedChapter},
    lifecycle,
    processing::{ChapterPhase, Job, JobChannel, JobId, JobState, Processing},
    project::{CurrentProject, Project, ProjectLibrary},
    publication::{self, PublicationConfig},
    summarizer,
};
use crate::commands::project::ProjectOwner;
use anyhow::{Context as _, Result, bail};
use koharu_pipeline::StopToken;
use koharu_scene::{
    AssetInput, AssetMetadata, AssetRole, BlobId, ImportedSource, PublicationMetadata,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::{
    path::{Component, Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tauri::{AppHandle, Manager as _};
use tauri_runtime_cef::CefRuntime;
use tokio::{
    io::{AsyncBufReadExt as _, BufReader},
    process::Command,
};

#[derive(Deserialize)]
struct SourceChapter {
    url: String,
    title: String,
    number: f64,
    pages: Vec<String>,
}
#[derive(Deserialize)]
struct SourceManifest {
    version: u8,
    url: String,
    #[serde(rename = "catalogURL")]
    catalog_url: Option<String>,
    metadata: PublicationMetadata,
    description: String,
    fields: Vec<String>,
    cover: Option<String>,
    chapters: Vec<SourceChapter>,
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct SourceImportOptions {
    pub language: Option<String>,
    pub download_url: Option<String>,
    pub project_name: Option<String>,
    pub summarize: bool,
    pub translate: bool,
    pub chapters: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct SourceChapterPreview {
    pub url: String,
    pub title: String,
    pub number: f64,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SourcePreview {
    pub title: String,
    pub source_url: String,
    pub chapters: Vec<SourceChapterPreview>,
}

#[derive(Deserialize)]
struct SourceSurvey {
    version: u8,
    url: String,
    #[serde(rename = "catalogURL")]
    catalog_url: Option<String>,
    metadata: PublicationMetadata,
    chapters: Vec<SourceChapterPreview>,
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn preview_source_url(
    url: String,
    mut config: PublicationConfig,
    mut options: SourceImportOptions,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<Option<SourcePreview>, Error> {
    let result: Result<Option<SourcePreview>> = async {
        let (url, catalog) = source_address(&url, options.language.as_deref())?;
        if let Some(reader) = &options.download_url {
            anyhow::ensure!(catalog, "a source override requires a catalog URL");
            reader_address(reader)?;
        }
        options.chapters = None;
        let checked = publication::inspect(&config, false).await?;
        anyhow::ensure!(
            checked.missing.is_empty(),
            "missing local tools: {}",
            checked.missing.join(", ")
        );
        config.node = checked.node.context("Node is missing")?;
        config.script = checked.script.context("publisher toolkit is missing")?;
        config.binary = checked.binary;
        let id = JobId::new();
        let stop = StopToken::default();
        let job = Job {
            id,
            project: None,
            state: JobState::Queued,
            completed: 0,
            total: 0,
            page: None,
            chapter: None,
            chapter_title: Some(url.clone()),
            chapter_progress: None,
            phase: Some(ChapterPhase::Inspect),
            stage: None,
            model: config.model.clone(),
            detail: None,
            retry: None,
            error: None,
        };
        let processing = handle.state::<Processing>();
        {
            let mut jobs = processing.jobs.lock();
            anyhow::ensure!(
                !jobs.values().any(|other| matches!(
                    other.state,
                    JobState::Running | JobState::Queued
                ) && other.chapter_title.as_deref() == Some(&url)),
                "this source is already queued or running"
            );
            jobs.insert(id, job.clone());
            processing.stops.lock().insert(id, stop.clone());
        }
        handle.state::<JobChannel>().channel.publish(job);
        let result: Result<SourcePreview> = async {
            let _permit = processing
                .wait(&stop)
                .await
                .context("source preview stopped while queued")?;
            if let Some(job) = processing.jobs.lock().get_mut(&id) {
                job.state = JobState::Running;
                handle.state::<JobChannel>().channel.publish(job.clone());
            }
            let output =
                run_source_command(&handle, id, &url, &config, &options, &stop, true).await?;
            let bytes = tokio::fs::read(output.join("preview.json")).await?;
            anyhow::ensure!(
                bytes.len() < 16 * 1024 * 1024,
                "source preview is too large"
            );
            let survey: SourceSurvey = serde_json::from_slice(&bytes)?;
            anyhow::ensure!(
                survey.version == 1
                    && !survey.chapters.is_empty()
                    && survey.chapters.len() <= 20000,
                "invalid chapter survey"
            );
            anyhow::ensure!(
                if catalog {
                    survey.catalog_url.as_deref() == Some(&url)
                        && reader_address(&survey.url).is_ok()
                } else {
                    survey.catalog_url.is_none() && survey.url == url
                },
                "source survey does not match the requested manga"
            );
            Ok(SourcePreview {
                title: survey.metadata.title,
                source_url: survey.url,
                chapters: survey.chapters,
            })
        }
        .await;
        processing.stops.lock().remove(&id);
        if let Some(job) = processing.jobs.lock().get_mut(&id) {
            job.state = if stop.stopped() {
                JobState::Stopped
            } else if result.is_ok() {
                JobState::Finished
            } else {
                JobState::Failed
            };
            job.error = result.as_ref().err().map(|error| format!("{error:#}"));
            handle.state::<JobChannel>().channel.publish(job.clone());
        }
        if stop.stopped() {
            Ok(None)
        } else {
            result.map(Some)
        }
    }
    .await;
    result.map_err(Into::into)
}

fn source_address(url: &str, language: Option<&str>) -> Result<(String, bool)> {
    let mut address = tauri::Url::parse(url.trim()).context("invalid manga URL")?;
    anyhow::ensure!(
        matches!(address.scheme(), "http" | "https")
            && address.username().is_empty()
            && address.password().is_none(),
        "enter an HTTP(S) manga URL without embedded credentials"
    );
    address.set_fragment(None);
    let direct = matches!(
        address.host_str(),
        Some("mangadex.org" | "www.mangadex.org")
    );
    if direct {
        let parts = address
            .path_segments()
            .context("invalid MangaDex URL")?
            .collect::<Vec<_>>();
        anyhow::ensure!(
            parts.first() == Some(&"title"),
            "enter a MangaDex title URL"
        );
        let id = uuid::Uuid::parse_str(parts.get(1).context("MangaDex title ID missing")?)?;
        let saved_language = address
            .query_pairs()
            .find(|(key, _)| key == "lang")
            .map(|(_, value)| value.into_owned());
        let language = language.or(saved_language.as_deref()).unwrap_or("en");
        anyhow::ensure!(
            super::mangadex::valid_language(language),
            "invalid source language"
        );
        return Ok((
            format!("https://mangadex.org/title/{id}?lang={language}"),
            true,
        ));
    }
    Ok((address.to_string(), false))
}

fn reader_address(url: &str) -> Result<String> {
    let (address, catalog) = source_address(url, None)?;
    let parsed = tauri::Url::parse(&address)?;
    anyhow::ensure!(
        !catalog
            && !parsed
                .host_str()
                .is_some_and(|host| host.ends_with(".mangadex.org")),
        "choose a reading website as the download source; MangaDex is only the catalog"
    );
    Ok(address)
}

fn validate_manifest_source(manifest: &SourceManifest, requested: &str) -> Result<()> {
    let catalog = source_address(requested, None)?.1;
    let matches = if catalog {
        manifest.catalog_url.as_deref() == Some(requested) && reader_address(&manifest.url).is_ok()
    } else {
        manifest.catalog_url.is_none() && manifest.url == requested
    };
    anyhow::ensure!(
        manifest.version == 1
            && matches
            && !manifest.chapters.is_empty()
            && manifest.chapters.len() <= 20000,
        "source manifest does not match the requested manga"
    );
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_source_import_settings() -> std::result::Result<PublicationConfig, Error> {
    Ok(koharu_config::load::<PublicationConfig>("publication")?
        .read()?
        .clone())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn import_source_url(
    url: String,
    mut config: PublicationConfig,
    options: SourceImportOptions,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<JobId, Error> {
    let (url, catalog) = source_address(&url, options.language.as_deref())?;
    if let Some(download_url) = &options.download_url {
        if !catalog {
            return Err(
                anyhow::anyhow!("a download source override requires a catalog URL").into(),
            );
        }
        reader_address(download_url)?;
    }
    if options.summarize {
        let summary = summarizer::SummarizerConfig::load()?.read()?.clone();
        summary.resolved_binary()?;
        summarizer::check_skill(&summary)?;
    }
    let checked = publication::inspect(&config, false).await?;
    if !checked.missing.is_empty() {
        return Err(anyhow::anyhow!("missing local tools: {}", checked.missing.join(", ")).into());
    }
    config.node = checked.node.context("Node is missing")?;
    config.script = checked.script.context("publisher toolkit is missing")?;
    config.binary = checked.binary;
    let settings = koharu_config::load::<PublicationConfig>("publication")?;
    {
        let mut saved = settings.write()?;
        *saved = config.clone();
        saved.save()?;
    }
    let id = JobId::new();
    let stop = StopToken::default();
    let processing = handle.state::<Processing>();
    let job = Job {
        id,
        project: options.project_name.clone(),
        state: JobState::Queued,
        completed: 0,
        total: 0,
        page: None,
        chapter: None,
        chapter_title: Some(url.clone()),
        chapter_progress: None,
        phase: Some(ChapterPhase::Import),
        stage: None,
        model: config.model.clone(),
        detail: Some(url.clone()),
        retry: None,
        error: None,
    };
    {
        let mut jobs = processing.jobs.lock();
        if jobs.values().any(|other| {
            matches!(other.state, JobState::Running | JobState::Queued)
                && (other.detail.as_deref() == Some(url.as_str())
                    || job.project.is_some() && job.project == other.project)
        }) {
            return Err(anyhow::anyhow!("this source/project is already queued or running").into());
        }
        processing.stops.lock().insert(id, stop.clone());
        jobs.insert(id, job.clone());
    }
    handle.state::<JobChannel>().channel.publish(job.clone());
    drop(tokio::spawn(async move {
        let processing = handle.state::<Processing>();
        let permit = processing.wait(&stop).await;
        let result = if permit.is_some() {
            if let Some(job) = processing.jobs.lock().get_mut(&id) {
                job.state = JobState::Running;
                handle.state::<JobChannel>().channel.publish(job.clone());
            }
            download_and_import(&handle, id, &url, &config, &options, &stop).await
        } else {
            Err(anyhow::anyhow!("source import stopped while queued"))
        };
        let processing = handle.state::<Processing>();
        processing.stops.lock().remove(&id);
        let mut finished = processing.jobs.lock().get(&id).cloned().unwrap_or(job);
        finished.state = if stop.stopped() {
            JobState::Stopped
        } else if result.is_ok() {
            JobState::Finished
        } else {
            JobState::Failed
        };
        finished.error = result.err().map(|error| format!("{error:#}"));
        finished.retry = None;
        if finished.state == JobState::Finished {
            finished.completed = finished.total;
        }
        processing.jobs.lock().insert(id, finished.clone());
        handle.state::<JobChannel>().channel.publish(finished);
    }));
    Ok(id)
}

async fn run_source_command(
    handle: &AppHandle<CefRuntime>,
    id: JobId,
    url: &str,
    config: &PublicationConfig,
    options: &SourceImportOptions,
    stop: &StopToken,
    scan_only: bool,
) -> Result<PathBuf> {
    let home = dirs::data_local_dir()
        .context("local data directory is unavailable")?
        .join("Koharu");
    let output = home
        .join("source-import")
        .join(BlobId::for_bytes(url.as_bytes()).to_string());
    tokio::fs::create_dir_all(&output).await?;
    let script = PathBuf::from(&config.script).canonicalize()?;
    let mut command = Command::new(&config.node);
    command
        .current_dir(script.ancestors().nth(3).context("invalid toolkit path")?)
        .args(["--import", "tsx"])
        .arg(&script)
        .arg("download-source")
        .arg("--url")
        .arg(url)
        .arg("--output")
        .arg(&output)
        .arg("--cache")
        .arg(home.join("source-recipes"))
        .arg("--backend")
        .arg(&config.backend)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(binary) = &config.binary {
        command.arg("--binary").arg(binary);
    }
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    if let Some(language) = &options.language {
        command.arg("--language").arg(language);
    }
    if let Some(download_url) = &options.download_url {
        command.arg("--download-url").arg(download_url);
    }
    if scan_only {
        command.arg("--scan-only");
    }
    if let Some(chapters) = &options.chapters {
        anyhow::ensure!(
            !chapters.is_empty() && chapters.len() <= 20000,
            "choose at least one chapter"
        );
        let selection = output.join(format!("selection-{id}.json"));
        tokio::fs::write(&selection, serde_json::to_vec(chapters)?).await?;
        command.arg("--chapters-file").arg(selection);
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .context("could not start the source importer")?;
    let pid = child.id();
    let mut stdout = BufReader::new(child.stdout.take().context("import stdout missing")?).lines();
    let mut stderr = BufReader::new(child.stderr.take().context("import stderr missing")?).lines();
    let (mut open_out, mut open_err, mut last) = (true, true, String::new());
    while open_out || open_err {
        tokio::select! {
            line = stdout.next_line(), if open_out => match line? { Some(line) => { last = line; report(handle,id,&last); }, None => open_out = false },
            line = stderr.next_line(), if open_err => match line? { Some(line) => { last = line; report(handle,id,&last); }, None => open_err = false },
            () = tokio::time::sleep(Duration::from_millis(250)) => if stop.stopped() { summarizer::stop_process(&mut child,pid).await; bail!("source import stopped; downloaded pages are retained for resume") },
        }
    }
    if !child.wait().await?.success() {
        bail!("source importer failed: {last}");
    }
    anyhow::ensure!(!stop.stopped(), "source import stopped");
    Ok(output)
}

async fn download_and_import(
    handle: &AppHandle<CefRuntime>,
    id: JobId,
    url: &str,
    config: &PublicationConfig,
    options: &SourceImportOptions,
    stop: &StopToken,
) -> Result<()> {
    let output = run_source_command(handle, id, url, config, options, stop, false).await?;
    let bytes = tokio::fs::read(output.join("source.json")).await?;
    anyhow::ensure!(
        bytes.len() < 16 * 1024 * 1024,
        "source manifest is too large"
    );
    let manifest: SourceManifest =
        serde_json::from_slice(&bytes).context("invalid source manifest")?;
    validate_manifest_source(&manifest, url)?;
    if let Some(selected) = &options.chapters {
        let expected = selected.iter().collect::<std::collections::HashSet<_>>();
        let actual = manifest
            .chapters
            .iter()
            .map(|chapter| &chapter.url)
            .collect::<std::collections::HashSet<_>>();
        anyhow::ensure!(
            expected.len() == selected.len()
                && expected == actual
                && actual.len() == manifest.chapters.len(),
            "downloaded chapter selection does not match the request"
        );
    }

    let library = handle.state::<ProjectLibrary>().inner().clone();
    {
        let current = handle.state::<super::project::CurrentProject>();
        let current = current.project.lock().await;
        if let Some(project) = current.as_ref() {
            if project
                .snapshot()
                .project_component::<ImportedSource>()?
                .is_some_and(|source| source.url == manifest.url)
            {
                bail!(
                    "close this manga project before importing its URL again; the downloaded cache is retained"
                );
            }
        }
    }
    let project =
        open_import_project_named(&library, &manifest, options.project_name.as_deref()).await?;
    if let Some(job) = handle.state::<Processing>().jobs.lock().get_mut(&id) {
        job.project = Some(project.name.clone());
    }
    let summarize = if options.summarize || options.translate {
        options.summarize
    } else {
        summarizer::SummarizerConfig::load()?
            .read()?
            .summarize_default
    };
    let project = populate_project(
        project,
        &manifest,
        &output,
        summarize,
        stop,
        |completed, total, detail| {
            report(
                handle,
                id,
                &serde_json::json!({"completed":completed,"total":total,"detail":detail})
                    .to_string(),
            );
        },
    )
    .await?;
    if options.summarize || options.translate {
        let chapters = project
            .snapshot()
            .chapters()?
            .entries
            .iter()
            .map(|chapter| chapter.id)
            .collect();
        let target = handle
            .state::<CurrentProject>()
            .project
            .retain_background(project, ProjectOwner::Pipeline)
            .await?;
        let outcome = async {
            let config = summarizer::SummarizerConfig::load()?.read()?.clone();
            let language = koharu_pipeline::PipelineConfig::load()?
                .read()?
                .translation
                .target_language
                .to_string();
            chapter::run_bound(
                handle.clone(),
                id,
                target.clone(),
                chapters,
                Some(options.summarize),
                options.translate,
                true,
                config,
                language,
                stop.clone(),
            )
            .await
        }
        .await;
        handle
            .state::<CurrentProject>()
            .project
            .finish(&target)
            .await;
        outcome?;
        return Ok(());
    }
    // MangaDex creation stays in the background; the existing generic URL flow still opens its project.
    if source_address(url, None)?.1 {
        return Ok(());
    }
    let final_progress = handle.state::<Processing>().jobs.lock().get(&id).cloned();
    handle.state::<Processing>().stops.lock().remove(&id);
    lifecycle::replace_project(handle, project).await?;
    if let Some(mut job) = final_progress {
        job.completed = manifest.chapters.len();
        job.total = manifest.chapters.len();
        handle.state::<Processing>().jobs.lock().insert(id, job);
    }
    Ok(())
}

async fn populate_project(
    mut project: Project,
    manifest: &SourceManifest,
    output: &Path,
    summarize: bool,
    stop: &StopToken,
    progress: impl Fn(usize, usize, &str),
) -> Result<Project> {
    let root = output.canonicalize()?;
    for (index, chapter) in manifest.chapters.iter().enumerate() {
        anyhow::ensure!(
            !stop.stopped(),
            "source import stopped; imported chapters are retained"
        );
        let snapshot = project.snapshot();
        let chapters = snapshot.chapters()?;
        let previous = snapshot
            .project_component::<ImportedSource>()?
            .context("source metadata missing")?;
        if previous.chapters.iter().any(|item| {
            item.url == chapter.url
                && chapters
                    .entries
                    .iter()
                    .any(|entry| entry.id == item.chapter)
        }) {
            continue;
        }
        anyhow::ensure!(
            chapter.number.is_finite()
                && chapter.number >= 0.0
                && !chapter.pages.is_empty()
                && chapter.pages.len() <= 2000,
            "invalid source chapter"
        );
        progress(
            index,
            manifest.chapters.len(),
            &format!("Importing Chapter {}", chapter.number),
        );
        let files = chapter
            .pages
            .iter()
            .map(|relative| image_path(&root, relative))
            .collect::<Result<Vec<_>>>()?;
        let chapter_bytes = files.iter().try_fold(0_u64, |total, file| {
            Ok::<_, anyhow::Error>(total + std::fs::metadata(file)?.len())
        })?;
        anyhow::ensure!(
            chapter_bytes <= 512 * 1024 * 1024,
            "chapter exceeds the 512 MiB import memory limit"
        );
        let mut pages = tokio_rayon::spawn(move || import::import_presorted(files)).await?;
        for (index, page) in pages.iter_mut().enumerate() {
            page.name = format!("{:03}", index + 1);
        }
        anyhow::ensure!(!stop.stopped(), "source import stopped");
        let group = ImportedChapter {
            title: chapter.title.chars().take(300).collect(),
            pages,
            source: Some(ChapterSource {
                url: chapter.url.clone(),
                title: chapter.title.clone(),
                number: chapter.number,
            }),
        };
        let (commit, first) = project.import_chapters(vec![group], summarize).await?;
        project.record_commit(&commit);
        if project.active_page().is_none() {
            if let Some(first) = first {
                project.select_page(first)?;
            }
        }
    }
    if let (Some(relative), Some(page)) = (
        &manifest.cover,
        project.snapshot().pages().next().map(|page| page.id()),
    ) {
        let file = image_path(&root, relative)?;
        let mut pages = tokio_rayon::spawn(move || import::import_presorted(vec![file])).await?;
        let cover = pages.pop().context("cover image missing")?;
        let role = AssetRole::new("cover")?;
        let patch = project.snapshot().patch(|edit| {
            edit.set_asset(
                page,
                &role,
                AssetInput::new(
                    cover.bytes,
                    cover.format.to_mime_type(),
                    AssetMetadata {
                        width: Some(cover.width),
                        height: Some(cover.height),
                        attributes: Default::default(),
                    },
                ),
            )
        })?;
        if let Some(commit) = project.commit_rebased(patch).await? {
            project.record_commit(&commit);
        }
    }
    anyhow::ensure!(!stop.stopped(), "source import stopped");
    Ok(project)
}

async fn open_import_project_named(
    library: &ProjectLibrary,
    manifest: &SourceManifest,
    name: Option<&str>,
) -> Result<Project> {
    let name = name
        .unwrap_or(&manifest.metadata.title)
        .chars()
        .filter(|c| !c.is_control() && !r#"<>:"/\|?*"#.contains(*c))
        .take(120)
        .collect::<String>()
        .trim()
        .trim_end_matches('.')
        .to_owned();
    anyhow::ensure!(
        !name.is_empty(),
        "project name is empty after removing invalid characters"
    );
    let exists = library.list()?.iter().any(|project| project.name == name);
    let project = if exists {
        library.open(&name).await?
    } else {
        library.create(&name).await?
    };
    bind_source_metadata(project, manifest, exists).await
}

async fn bind_source_metadata(
    mut project: Project,
    manifest: &SourceManifest,
    exists: bool,
) -> Result<Project> {
    if exists {
        let source = project
            .snapshot()
            .project_component::<ImportedSource>()?
            .context("a different project already uses this manga name")?;
        anyhow::ensure!(
            source.url == manifest.url,
            "a different source already uses this project name"
        );
    } else {
        let source = ImportedSource {
            url: manifest.url.clone(),
            description: manifest.description.clone(),
            fields: manifest.fields.clone(),
            chapters: Vec::new(),
        };
        let patch = project.snapshot().patch(|edit| {
            edit.set_project(&manifest.metadata)?;
            edit.set_project(&source)
        })?;
        if let Some(commit) = project.commit_rebased(patch).await? {
            project.record_commit(&commit);
        }
    }
    Ok(project)
}

fn image_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    anyhow::ensure!(
        relative.starts_with("images/")
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "invalid source image path"
    );
    let resolved = root.join(path).canonicalize()?;
    anyhow::ensure!(
        resolved.starts_with(root)
            && resolved.is_file()
            && std::fs::metadata(&resolved)?.len() <= 50 * 1024 * 1024,
        "unsafe source image"
    );
    Ok(resolved)
}

fn report(handle: &AppHandle<CefRuntime>, id: JobId, line: &str) {
    let raw = line.strip_prefix("KOHARU_SOURCE_PROGRESS ").unwrap_or(line);
    let progress = serde_json::from_str::<serde_json::Value>(raw).ok();
    let processing = handle.state::<Processing>();
    let mut jobs = processing.jobs.lock();
    if let Some(job) = jobs.get_mut(&id) {
        if let Some(progress) = progress
            .filter(|value| value.get("completed").is_some() && value.get("total").is_some())
        {
            job.completed = progress["completed"].as_u64().unwrap_or(0).min(20000) as usize;
            job.total = progress["total"].as_u64().unwrap_or(0).min(20000) as usize;
            job.detail = progress["detail"]
                .as_str()
                .map(|text| text.chars().take(400).collect());
        } else {
            job.detail = Some(line.chars().take(400).collect());
        }
        handle.state::<JobChannel>().channel.publish(job.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn mangadex_source_identity_preserves_selected_language_across_url_variants() {
        let id = "00000000-0000-0000-0000-000000000001";
        let url = format!("https://www.mangadex.org/title/{id}/arbitrary-slug?lang=ja#chapter");
        assert_eq!(
            source_address(&url, Some("en")).unwrap(),
            (format!("https://mangadex.org/title/{id}?lang=en"), true)
        );
        assert_eq!(
            source_address(&url, None).unwrap().0,
            format!("https://mangadex.org/title/{id}?lang=ja")
        );
        assert!(source_address("https://mangadex.org/chapter/not-a-title", None).is_err());
        assert!(source_address(&url, Some("../vi")).is_err());
        assert!(source_address("file:///manga", None).is_err());
    }

    #[test]
    fn catalog_import_requires_a_separate_reader_source_and_matching_catalog() -> Result<()> {
        let catalog = "https://mangadex.org/title/00000000-0000-0000-0000-000000000001?lang=en";
        let mut manifest: SourceManifest = serde_json::from_value(serde_json::json!({
            "version":1,"url":"https://books.example/manga/test","catalogURL":catalog,
            "metadata":{"title":"Test","slug":"test","alternativeTitles":"","author":"","releaseYear":null,"country":"","status":"ongoing","genres":[]},
            "description":"","fields":[],"cover":null,
            "chapters":[{"url":"https://books.example/chapter-1","title":"Chapter 1","number":1,"pages":["images/1.png"]}]
        }))?;
        validate_manifest_source(&manifest, catalog)?;
        assert!(validate_manifest_source(&manifest, "https://books.example/manga/test").is_err());
        manifest.catalog_url = Some(format!("{catalog}x"));
        assert!(validate_manifest_source(&manifest, catalog).is_err());
        manifest.catalog_url = Some(catalog.into());
        manifest.url = catalog.into();
        assert!(validate_manifest_source(&manifest, catalog).is_err());
        assert!(reader_address("https://api.mangadex.org/manga/test").is_err());
        Ok(())
    }

    #[tokio::test]
    async fn ordered_pages_cover_and_provenance_resume_together() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("download");
        std::fs::create_dir_all(root.join("images"))?;
        for (name, width) in [("z.png", 11), ("a.png", 17), ("cover.png", 23)] {
            let mut data = Cursor::new(Vec::new());
            image::RgbImage::new(width, 29).write_to(&mut data, image::ImageFormat::Png)?;
            std::fs::write(root.join("images").join(name), data.into_inner())?;
        }
        let manifest: SourceManifest = serde_json::from_value(serde_json::json!({
            "version":1,"url":"https://books.example/book","metadata":{
                "title":"Source test","slug":"source-test","alternativeTitles":"Alias","author":"Author","releaseYear":2024,"country":"","status":"completed","genres":[]
            },"description":"Original synopsis","fields":["Publisher: Test"],"cover":"images/cover.png",
            "chapters":[{"url":"https://books.example/one","title":"Chapter 1: Original","number":1,"pages":["images/z.png","images/a.png"]}]
        }))?;
        let mut project =
            Project::create("Source test".into(), temp.path().join("test.khrproj")).await?;
        let patch = project.snapshot().patch(|edit| {
            edit.set_project(&manifest.metadata)?;
            edit.set_project(&ImportedSource {
                url: manifest.url.clone(),
                description: manifest.description.clone(),
                fields: manifest.fields.clone(),
                chapters: Vec::new(),
            })
        })?;
        project.commit_rebased(patch).await?;
        let stop = StopToken::default();
        let project =
            populate_project(project, &manifest, &root, false, &stop, |_, _, _| {}).await?;
        drop(project);
        let project = Project::open("Source test".into(), temp.path().join("test.khrproj")).await?;
        let project =
            populate_project(project, &manifest, &root, false, &stop, |_, _, _| {}).await?;
        let snapshot = project.snapshot();
        let chapters = snapshot.chapters()?;
        assert_eq!(chapters.entries.len(), 1);
        assert_eq!(chapters.entries[0].pages.len(), 2);
        let source = AssetRole::new("source")?;
        let first = snapshot
            .asset(chapters.entries[0].pages[0], &source)?
            .context("first source")?;
        let second = snapshot
            .asset(chapters.entries[0].pages[1], &source)?
            .context("second source")?;
        assert_eq!(first.metadata.width, Some(11));
        assert_eq!(second.metadata.width, Some(17));
        let cover = snapshot
            .asset(chapters.entries[0].pages[0], &AssetRole::new("cover")?)?
            .context("cover")?;
        assert_eq!(cover.metadata.width, Some(23));
        let provenance = snapshot
            .project_component::<ImportedSource>()?
            .context("provenance")?;
        assert_eq!(provenance.chapters.len(), 1);
        assert_eq!(provenance.chapters[0].chapter, chapters.entries[0].id);
        assert_eq!(provenance.chapters[0].title, "Chapter 1: Original");
        assert_eq!(provenance.description, "Original synopsis");
        Ok(())
    }

    #[tokio::test]
    #[ignore = "manual acceptance: imports KOHARU_SOURCE_FIXTURE into the user's project library"]
    async fn import_downloaded_manga() -> Result<()> {
        let root = PathBuf::from(std::env::var("KOHARU_SOURCE_FIXTURE")?);
        let manifest: SourceManifest =
            serde_json::from_slice(&std::fs::read(root.join("source.json"))?)?;
        eprintln!(
            "Source cache key: {}",
            BlobId::for_bytes(manifest.url.as_bytes())
        );
        let project = if let Ok(target) = std::env::var("KOHARU_SOURCE_PROJECT") {
            let path = PathBuf::from(target);
            let exists = path.join("state-a.khr").is_file() || path.join("state-b.khr").is_file();
            let project = if exists {
                Project::open(manifest.metadata.title.clone(), path).await?
            } else {
                Project::create(manifest.metadata.title.clone(), path).await?
            };
            bind_source_metadata(project, &manifest, exists).await?
        } else {
            open_import_project_named(&ProjectLibrary::new()?, &manifest, None).await?
        };
        let summarize = summarizer::SummarizerConfig::load()?
            .read()?
            .summarize_default;
        let project = populate_project(
            project,
            &manifest,
            &root,
            summarize,
            &StopToken::default(),
            |completed, total, detail| eprintln!("{completed}/{total} {detail}"),
        )
        .await?;
        let snapshot = project.snapshot();
        assert_eq!(snapshot.chapters()?.entries.len(), manifest.chapters.len());
        assert_eq!(
            snapshot.pages().count(),
            manifest
                .chapters
                .iter()
                .map(|chapter| chapter.pages.len())
                .sum::<usize>()
        );
        eprintln!(
            "Imported {}: {} chapters, {} pages",
            project.name,
            manifest.chapters.len(),
            snapshot.pages().count()
        );
        Ok(())
    }
}
