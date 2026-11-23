//! Source surveys and downloads use the configured publishing toolkit.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result, bail, ensure};
use koharu_scene::{
    Chapter, Edit, Hash, ImportedSource, ImportedSourceChapter, Op, PageId, PublicationMetadata,
    Session, Uuid,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use crate::{app::App, chapters::ChapterMode, events};

#[derive(Clone, Copy, Debug, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SourcePhase {
    Queued,
    Downloading,
    Finished,
    Stopped,
    Failed,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct SourceJob {
    pub id: Uuid,
    pub name: String,
    pub phase: SourcePhase,
    pub progress: Option<SourceProgress>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
pub struct SourceProgress {
    pub completed: u32,
    pub total: u32,
    pub detail: String,
}

#[derive(Default)]
pub struct SourceJobs {
    pub queue: tokio::sync::Mutex<()>,
    jobs: Mutex<BTreeMap<Uuid, (SourceJob, CancellationToken)>>,
}

impl SourceJobs {
    fn update(&self, app: &App, id: Uuid, phase: SourcePhase, error: Option<String>) {
        let status = {
            let mut jobs = self.jobs.lock();
            jobs.get_mut(&id).map(|(job, _)| {
                job.phase = phase;
                job.error = error;
                job.clone()
            })
        };
        if let Some(job) = status {
            app.emit(events::SOURCE_JOB, job);
        }
    }

    fn progress(&self, app: &App, id: Uuid, progress: SourceProgress) {
        let status = self.jobs.lock().get_mut(&id).map(|(job, _)| {
            job.progress = Some(progress);
            job.clone()
        });
        if let Some(job) = status {
            app.emit(events::SOURCE_JOB, job);
        }
    }
}

#[tauri::command]
#[specta::specta]
pub fn get_source_jobs(app: State<'_, Arc<App>>) -> Vec<SourceJob> {
    app.source_import
        .jobs
        .lock()
        .values()
        .map(|(job, _)| job.clone())
        .collect()
}

#[tauri::command]
#[specta::specta]
pub fn stop_source_job(app: State<'_, Arc<App>>, job: Uuid) {
    if let Some((_, stop)) = app.source_import.jobs.lock().get(&job) {
        stop.cancel();
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(default)]
pub struct PublicationConfig {
    pub node: String,
    pub script: String,
    pub catalog_url: String,
    pub backend: String,
    pub binary: Option<String>,
    pub model: Option<String>,
    pub offline: bool,
}

impl Default for PublicationConfig {
    fn default() -> Self {
        Self {
            node: "node".into(),
            script: String::new(),
            catalog_url: String::new(),
            backend: "codex".into(),
            binary: None,
            model: None,
            offline: false,
        }
    }
}

impl koharu_config::Section for PublicationConfig {
    const NAME: &'static str = "publication";
}

#[derive(Serialize, Type)]
pub struct PublicationReadiness {
    pub node: Option<String>,
    pub script: Option<String>,
    pub binary: Option<String>,
    pub missing: Vec<String>,
}

#[derive(Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PublicationFile {
    Script,
    Node,
    Binary,
}

#[tauri::command]
#[specta::specta]
pub fn inspect_publication(config: PublicationConfig) -> PublicationReadiness {
    let node = executable(&config.node, "node");
    let script = toolkit_script(&config);
    let binary = executable(
        config.binary.as_deref().unwrap_or_default(),
        &config.backend,
    );
    let missing = [
        ("node", node.is_none()),
        ("script", script.is_none()),
        ("binary", binary.is_none() && config.binary.is_some()),
    ]
    .into_iter()
    .filter(|(_, missing)| *missing)
    .map(|(name, _)| name.into())
    .collect();
    let display = |path: PathBuf| path.to_string_lossy().into_owned();
    PublicationReadiness {
        node: node.map(display),
        script: script.map(display),
        binary: binary.map(display),
        missing,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn pick_publication_file(
    window: Window<CefRuntime>,
    kind: PublicationFile,
) -> Option<String> {
    let dialog = rfd::AsyncFileDialog::new().set_parent(&window);
    let dialog = match kind {
        PublicationFile::Script => dialog.add_filter("Script", &["ts", "js", "mjs"]),
        PublicationFile::Node | PublicationFile::Binary => dialog,
    };
    dialog
        .pick_file()
        .await
        .map(|file| file.path().to_string_lossy().into_owned())
}

pub(super) fn toolkit_script(config: &PublicationConfig) -> Option<PathBuf> {
    if !config.script.trim().is_empty() {
        return Path::new(&config.script)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file());
    }
    let executable = std::env::current_exe().ok();
    let roots = executable
        .as_deref()
        .and_then(Path::parent)
        .into_iter()
        .flat_map(|parent| [parent.join("publisher"), parent.to_owned()])
        .chain(std::env::current_dir().ok());
    roots
        .map(|root| root.join("scripts/publishing/main.ts"))
        .find(|file| file.is_file())
}

pub(super) fn executable(configured: &str, default: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    let path = Path::new(configured);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_owned());
    }
    let name = if configured.is_empty() {
        default
    } else {
        configured
    };
    let path_env = std::env::var_os("PATH")?;
    let mut roots = std::env::split_paths(&path_env).collect::<Vec<_>>();
    if cfg!(windows) {
        if let Some(home) = dirs::home_dir() {
            roots.extend([
                home.join(".pi/agent/bin"),
                home.join("AppData/Local/agy/bin"),
            ]);
        }
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            roots.push(PathBuf::from(program_files).join("nodejs"));
        }
    }
    roots
        .into_iter()
        .flat_map(|root| {
            [
                root.join(name),
                root.join(format!("{name}.exe")),
                root.join(format!("{name}.cmd")),
            ]
        })
        .find(|file| file.is_file())
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
struct SourceChapter {
    #[serde(flatten)]
    info: SourceChapterPreview,
    pages: Vec<String>,
}

#[derive(Deserialize)]
struct Manifest<T> {
    version: u8,
    url: String,
    #[serde(rename = "catalogURL")]
    catalog_url: Option<String>,
    metadata: PublicationMetadata,
    #[serde(default)]
    description: String,
    #[serde(default)]
    fields: Vec<String>,
    cover: Option<String>,
    chapters: Vec<T>,
}

fn source_address(url: &str, language: Option<&str>) -> Result<(String, bool)> {
    let mut address = tauri::Url::parse(url.trim()).context("invalid manga URL")?;
    ensure!(
        matches!(address.scheme(), "http" | "https")
            && address.username().is_empty()
            && address.password().is_none()
            && address.host().is_some(),
        "enter an HTTP(S) manga URL without embedded credentials"
    );
    address.set_fragment(None);
    let catalog = matches!(
        address.host_str(),
        Some("mangadex.org" | "www.mangadex.org")
    );
    if catalog {
        let mut path = address.path_segments().context("invalid MangaDex URL")?;
        ensure!(path.next() == Some("title"), "enter a MangaDex title URL");
        let id = Uuid::parse_str(path.next().context("MangaDex title ID missing")?)?;
        let saved = address
            .query_pairs()
            .find(|(key, _)| key == "lang")
            .map(|(_, value)| value);
        let language = language.or(saved.as_deref()).unwrap_or("en");
        ensure!(
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
    let (url, catalog) = source_address(url, None)?;
    let address = tauri::Url::parse(&url)?;
    ensure!(
        !catalog
            && !address
                .host_str()
                .is_some_and(|host| host.ends_with(".mangadex.org")),
        "choose a reading website for the source override"
    );
    Ok(url)
}

fn manifest_matches<T>(value: &Manifest<T>, requested: &str, catalog: bool) -> Result<()> {
    let matches = if catalog {
        value.catalog_url.as_deref() == Some(requested) && reader_address(&value.url).is_ok()
    } else {
        value.catalog_url.is_none() && value.url == requested
    };
    ensure!(
        matches
            && value.version == 1
            && !value.chapters.is_empty()
            && value.chapters.len() <= 20_000,
        "source manifest does not match the manga or contains no chapters"
    );
    Ok(())
}

fn read<T: DeserializeOwned>(file: &Path) -> Result<T> {
    let metadata = std::fs::symlink_metadata(file)?;
    ensure!(
        metadata.is_file() && metadata.len() <= 16 * 1024 * 1024,
        "invalid toolkit manifest"
    );
    Ok(serde_json::from_slice(&std::fs::read(file)?)?)
}

async fn run(
    url: &str,
    catalog: bool,
    config: &PublicationConfig,
    options: &SourceImportOptions,
    job: Option<(&App, Uuid)>,
) -> Result<PathBuf> {
    let preview = job.is_none();
    let script = toolkit_script(config).context("choose the publishing toolkit script")?;
    ensure!(script.is_file(), "publishing toolkit script is not a file");
    let toolkit = script
        .ancestors()
        .nth(3)
        .context("invalid toolkit script path")?;
    let local = dirs::data_local_dir()
        .context("local data folder is unavailable")?
        .join("Koharu");
    let output = local
        .join("source-import")
        .join(Hash::of(url.as_bytes()).to_string());
    tokio::fs::create_dir_all(&output).await?;
    let selection = tempfile::tempdir()?;
    let mut command =
        Command::new(executable(&config.node, "node").context("Node executable is missing")?);
    command
        .current_dir(toolkit)
        .args(["--import", "tsx"])
        .arg(&script)
        .args(["download-source", "--url", url, "--output"])
        .arg(&output)
        .arg("--cache")
        .arg(local.join("source-recipes"))
        .args(["--backend", &config.backend])
        .stdin(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    if let Some(binary) = &config.binary {
        command.arg("--binary").arg(binary);
    }
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    if let Some(language) = &options.language {
        command.arg("--language").arg(language);
    }
    if let Some(override_url) = &options.download_url {
        ensure!(catalog, "a source override requires a MangaDex title");
        command
            .arg("--download-url")
            .arg(reader_address(override_url)?);
    }
    if preview {
        command.arg("--scan-only");
    } else if let Some(chapters) = &options.chapters {
        ensure!(
            !chapters.is_empty() && chapters.len() <= 20_000,
            "choose at least one chapter"
        );
        let file = selection.path().join("chapters.json");
        tokio::fs::write(&file, serde_json::to_vec(chapters)?).await?;
        command.arg("--chapters-file").arg(file);
    }
    let log_path = selection.path().join("import.log");
    let log = std::fs::File::create(&log_path)?;
    let mut process = command
        .stdout(Stdio::piped())
        .stderr(log)
        .spawn()
        .context("start the source importer")?;
    let mut lines = BufReader::new(process.stdout.take().context("read source progress")?).lines();
    let mut last_output = String::new();
    let wait = async {
        while let Some(line) = lines.next_line().await? {
            if let Some((app, id)) = job
                && let Some(data) = line.strip_prefix("KOHARU_SOURCE_PROGRESS ")
                && let Ok(progress) = serde_json::from_str::<SourceProgress>(data)
            {
                app.source_import.progress(app, id, progress);
            } else if !line.trim().is_empty() {
                last_output = line.chars().take(512).collect();
            }
        }
        process.wait().await
    };
    let status = if preview {
        tokio::time::timeout(Duration::from_secs(4 * 60), wait)
            .await
            .context("source preview timed out")??
    } else {
        wait.await?
    };
    if !status.success() {
        use std::io::{Read as _, Seek as _};
        let mut file = std::fs::File::open(&log_path)?;
        file.seek(std::io::SeekFrom::Start(
            file.metadata()?.len().saturating_sub(2048),
        ))?;
        let mut tail = Vec::new();
        file.take(2048).read_to_end(&mut tail)?;
        bail!(
            "source importer exited with {status}: {}",
            if tail.is_empty() {
                last_output.trim().to_owned()
            } else {
                String::from_utf8_lossy(&tail).trim().to_owned()
            }
        );
    }
    Ok(output)
}

#[tauri::command]
#[specta::specta]
pub fn get_source_import_settings() -> super::Result<PublicationConfig> {
    Ok(koharu_config::get()?)
}

#[tauri::command]
#[specta::specta]
pub fn save_source_import_settings(config: PublicationConfig) -> super::Result<()> {
    Ok(koharu_config::update(|saved| *saved = config)?)
}

pub(super) async fn preview(
    app: &App,
    url: &str,
    config: &PublicationConfig,
    options: &SourceImportOptions,
) -> Result<SourcePreview> {
    let (url, catalog) = source_address(url, options.language.as_deref())?;
    let _queue = app.source_import.queue.lock().await;
    let output = run(&url, catalog, config, options, None).await?;
    let manifest: Manifest<SourceChapterPreview> = read(&output.join("preview.json"))?;
    manifest_matches(&manifest, &url, catalog)?;
    Ok(SourcePreview {
        title: manifest.metadata.title,
        source_url: manifest.url,
        chapters: manifest.chapters,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn preview_source_url(
    app: State<'_, Arc<App>>,
    url: String,
    config: PublicationConfig,
    options: SourceImportOptions,
) -> super::Result<Option<SourcePreview>> {
    Ok(Some(preview(&app, &url, &config, &options).await?))
}

#[tauri::command]
#[specta::specta]
pub async fn import_source_url(
    app: State<'_, Arc<App>>,
    url: String,
    config: PublicationConfig,
    options: SourceImportOptions,
) -> super::Result<Uuid> {
    let (url, catalog) = source_address(&url, options.language.as_deref())?;
    let app = Arc::clone(app.inner());
    let id = Uuid::now_v7();
    let stop = CancellationToken::new();
    let job = SourceJob {
        id,
        name: options
            .project_name
            .clone()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| url.clone()),
        phase: SourcePhase::Queued,
        progress: None,
        error: None,
    };
    app.source_import
        .jobs
        .lock()
        .insert(id, (job.clone(), stop.clone()));
    app.emit(events::SOURCE_JOB, job);
    tauri::async_runtime::spawn(async move {
        let result = import(&app, id, url, catalog, config, options, &stop).await;
        let (phase, error) = match result {
            _ if stop.is_cancelled() => (SourcePhase::Stopped, None),
            Ok(()) => (SourcePhase::Finished, None),
            Err(error) => (SourcePhase::Failed, Some(format!("{error:#}"))),
        };
        app.source_import.update(&app, id, phase, error);
    });
    Ok(id)
}

async fn import(
    app: &Arc<App>,
    id: Uuid,
    url: String,
    catalog: bool,
    config: PublicationConfig,
    options: SourceImportOptions,
    stop: &CancellationToken,
) -> Result<()> {
    let queue = tokio::select! {
        _ = stop.cancelled() => return Ok(()),
        queue = app.source_import.queue.lock() => queue,
    };
    app.source_import
        .update(app, id, SourcePhase::Downloading, None);
    let output = tokio::select! {
        _ = stop.cancelled() => return Ok(()),
        output = run(&url, catalog, &config, &options, Some((app, id))) => output?,
    };
    let saved = tokio::task::spawn_blocking({
        let app = Arc::clone(app);
        let options = options.clone();
        let stop = stop.clone();
        move || -> Result<_> {
            let manifest: Manifest<SourceChapter> = read(&output.join("source.json"))?;
            manifest_matches(&manifest, &url, catalog)?;
            ensure!(
                options.chapters.as_ref().is_none_or(|selected| {
                    let expected: BTreeSet<_> = selected.iter().collect();
                    let actual: BTreeSet<_> =
                        manifest.chapters.iter().map(|ch| &ch.info.url).collect();
                    actual == expected && actual.len() == manifest.chapters.len()
                }),
                "the download does not match the selected chapters"
            );
            let name = options
                .project_name
                .clone()
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| sanitize_filename::sanitize(&manifest.metadata.title));
            let summarize = if options.summarize || options.translate {
                options.summarize
            } else {
                koharu_config::get::<crate::chapters::SummarizerConfig>()?.summarize_default
            };
            let path = app.library.path(&name)?;
            let result = if path.is_file() {
                let session = app.library.open(&name)?;
                populate(&mut session.lock(), &manifest, &output, summarize, &stop)?
            } else {
                let staging = tempfile::tempdir_in(path.parent().context("library has no root")?)?;
                let pending = staging.path().join("project.khr");
                let mut session = Session::create(&pending)?;
                let first = populate(&mut session, &manifest, &output, summarize, &stop)?;
                if stop.is_cancelled() {
                    return Ok((name, Vec::new()));
                }
                drop(session);
                std::fs::hard_link(pending, path)?;
                first
            };
            Ok((name, result))
        }
    })
    .await??;
    drop(queue);
    if stop.is_cancelled() {
        return Ok(());
    }
    let (name, chapters) = saved;
    if !chapters.is_empty() {
        let mode = match (options.summarize, options.translate) {
            (true, true) => Some(ChapterMode::Resume),
            (true, false) => Some(ChapterMode::Summarize),
            (false, true) => Some(ChapterMode::Translate),
            (false, false) => None,
        };
        if let Some(mode) = mode {
            app.start_chapter_ids(name.clone(), app.library.open(&name)?, chapters, mode)?;
        }
    }
    Ok(())
}

fn populate(
    session: &mut Session,
    manifest: &Manifest<SourceChapter>,
    output: &Path,
    summarize: bool,
    stop: &CancellationToken,
) -> Result<Vec<koharu_scene::ChapterId>> {
    let project = session.project();
    let mut metadata = project.metadata().clone();
    let mut source = metadata.source.take().unwrap_or_else(|| ImportedSource {
        url: manifest.url.clone(),
        description: manifest.description.clone(),
        fields: manifest.fields.clone(),
        chapters: Vec::new(),
    });
    ensure!(
        source.url == manifest.url
            && (project.pages().next().is_none() || project.metadata().source.is_some()),
        "this name is used for a different manga project"
    );
    metadata.publication = Some(manifest.metadata.clone());
    let mut imported = Vec::new();
    let mut index = project.pages().count();
    let mut cover = manifest.cover.as_deref();
    for chapter in &manifest.chapters {
        if stop.is_cancelled() {
            return Ok(imported);
        }
        if let Some(saved) = source
            .chapters
            .iter()
            .find(|saved| saved.url == chapter.info.url)
        {
            imported.push(saved.chapter);
            continue;
        }
        ensure!(!chapter.pages.is_empty(), "chapter has no downloaded pages");
        let mut value = Chapter::new(chapter.info.title.clone());
        value.summarize = summarize;
        imported.push(value.id);
        let mut ops = Vec::with_capacity(chapter.pages.len() * 2 + 2);
        if let Some(relative) = cover.take() {
            let bytes = std::fs::read(image_path(output, relative)?)?;
            metadata.cover = Some(Hash::of(&bytes));
            ops.push(Op::AddFile { bytes });
        }
        for (within, file) in chapter.pages.iter().enumerate() {
            let bytes = std::fs::read(image_path(output, file)?)?;
            let background = Some(Hash::of(&bytes));
            let id = PageId(Uuid::now_v7());
            value.pages.push(id);
            metadata
                .page_labels
                .insert(id, format!("{:03}", within + 1));
            ops.push(Op::AddFile { bytes });
            ops.push(Op::AddPage {
                id: Some(id.into()),
                index,
                background,
                size: None,
            });
            index += 1;
        }
        source.chapters.push(ImportedSourceChapter {
            chapter: value.id,
            url: chapter.info.url.clone(),
            title: chapter.info.title.clone(),
            number: chapter.info.number,
        });
        metadata.chapters.entries.push(value);
        metadata.source = Some(source.clone());
        ops.push(Op::SetProjectMetadata {
            metadata: Box::new(metadata.clone()),
        });
        session.apply(Edit {
            label: "sourceImport.start".into(),
            ops,
        })?;
    }
    Ok(imported)
}

fn image_path(root: &Path, path: &str) -> Result<PathBuf> {
    let path = Path::new(path);
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::Normal(_))),
        "a downloaded page path leaves the source folder"
    );
    let root = root.canonicalize()?;
    let page = root.join(path).canonicalize()?;
    ensure!(
        page.starts_with(&root) && page.is_file(),
        "downloaded page path is invalid"
    );
    Ok(page)
}
