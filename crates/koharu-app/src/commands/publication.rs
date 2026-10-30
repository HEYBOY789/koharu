use anyhow::{Context as _, bail};
use koharu_pipeline::StopToken;
use koharu_scene::PublicationMetadata;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tauri::{AppHandle, Manager as _, State, WebviewWindow};
use tauri_runtime_cef::CefRuntime;
use tokio::{
    io::{AsyncBufReadExt as _, BufReader},
    process::Command,
};

use super::{
    ChannelExt as _, Error,
    output::{self, ExportFormat},
    processing::{Job, JobChannel, JobId, JobState, Processing},
    project::CurrentProject,
    summarizer,
};

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
            node: "node".to_owned(),
            script: String::new(),
            catalog_url: String::new(),
            backend: "codex".to_owned(),
            binary: None,
            model: None,
            offline: false,
        }
    }
}

#[derive(Serialize, Type)]
pub struct PublicationSettings {
    pub config: PublicationConfig,
    pub metadata: PublicationMetadata,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PublicationCatalog {
    pub url: String,
    pub site_url: String,
    pub story_count: u32,
    pub downloaded_at: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PublisherInspection {
    binary: Option<String>,
    catalog: Option<PublicationCatalog>,
    catalog_error: Option<String>,
}

#[derive(Serialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PublicationReadiness {
    pub node: Option<String>,
    pub script: Option<String>,
    pub binary: Option<String>,
    pub missing: Vec<String>,
    pub catalog: Option<PublicationCatalog>,
    pub catalog_error: Option<String>,
}

pub(super) fn node_executable(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    let path = Path::new(configured);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_owned());
    }
    let mut directories = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    if cfg!(windows) {
        if let Some(program_files) = std::env::var_os("ProgramFiles") {
            directories.push(PathBuf::from(program_files).join("nodejs"));
        }
    }
    let name = if configured.is_empty() || configured == "node" {
        if cfg!(windows) { "node.exe" } else { "node" }
    } else {
        configured
    };
    directories
        .into_iter()
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

pub(super) fn publisher_script(configured: &str) -> Option<PathBuf> {
    if !configured.trim().is_empty() {
        return Path::new(configured)
            .canonicalize()
            .ok()
            .filter(|path| path.is_file());
    }
    let mut roots = Vec::new();
    if let Ok(executable) = std::env::current_exe() {
        if let Some(parent) = executable.parent() {
            roots.push(parent.join("publisher"));
            roots.push(parent.to_owned());
        }
    }
    if let Ok(directory) = std::env::current_dir() {
        roots.push(directory);
    }
    roots
        .into_iter()
        .map(|root| root.join("scripts/publishing/main.ts"))
        .find(|path| path.is_file())
}

fn catalog_cache() -> anyhow::Result<PathBuf> {
    Ok(dirs::data_local_dir()
        .context("local data directory is unavailable")?
        .join("Koharu/publishing/catalog.json"))
}

pub(super) async fn inspect(
    config: &PublicationConfig,
    refresh: bool,
) -> anyhow::Result<PublicationReadiness> {
    let node = node_executable(&config.node);
    let script = publisher_script(&config.script);
    let mut status = PublicationReadiness {
        node: node
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        script: script
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        binary: None,
        missing: Vec::new(),
        catalog: None,
        catalog_error: None,
    };
    if node.is_none() {
        status.missing.push("node".to_owned());
    }
    if script.is_none() {
        status.missing.push("script".to_owned());
    }
    let (Some(node), Some(script)) = (node, script) else {
        return Ok(status);
    };
    let root = script
        .ancestors()
        .nth(3)
        .context("invalid publisher script location")?;
    let mut command = Command::new(node);
    command
        .current_dir(root)
        .args(["--import", "tsx"])
        .arg(script.as_path())
        .arg("inspect")
        .arg("--url")
        .arg(&config.catalog_url)
        .arg("--cache")
        .arg(catalog_cache()?)
        .arg("--backend")
        .arg(&config.backend)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    if let Some(binary) = config
        .binary
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        command.arg("--binary").arg(binary);
    }
    if refresh {
        command.arg("--refresh");
    }
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = tokio::time::timeout(Duration::from_secs(40), command.output())
        .await
        .context("catalog check timed out")??;
    if !output.status.success() {
        bail!(
            "publisher check failed: {}",
            String::from_utf8_lossy(&output.stderr)
                .chars()
                .take(1200)
                .collect::<String>()
        );
    }
    let checked: PublisherInspection =
        serde_json::from_slice(&output.stdout).context("publisher check returned invalid data")?;
    status.binary = checked.binary;
    if status.binary.is_none() {
        status.missing.push("binary".to_owned());
    }
    status.catalog = checked.catalog;
    status.catalog_error = checked.catalog_error;
    Ok(status)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn inspect_publication(
    config: PublicationConfig,
    refresh: bool,
) -> std::result::Result<PublicationReadiness, Error> {
    let status = inspect(&config, refresh).await?;
    if refresh && status.catalog_error.is_none() && status.catalog.is_some() {
        let settings = koharu_config::load::<PublicationConfig>("publication")?;
        let mut saved = settings.write()?;
        *saved = config;
        saved.save()?;
    }
    Ok(status)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_publication_settings(
    project: State<'_, CurrentProject>,
) -> std::result::Result<PublicationSettings, Error> {
    let config = koharu_config::load::<PublicationConfig>("publication")?
        .read()?
        .clone();
    let project = project.project.lock().await;
    let project = project.as_ref().context("no project is open")?;
    let metadata = project
        .snapshot()
        .project_component::<PublicationMetadata>()?
        .unwrap_or_else(|| PublicationMetadata {
            title: project.name.clone(),
            ..PublicationMetadata::default()
        });
    Ok(PublicationSettings { config, metadata })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn pick_publication_file(
    window: WebviewWindow<CefRuntime>,
    kind: String,
) -> std::result::Result<Option<String>, Error> {
    let picker = rfd::AsyncFileDialog::new().set_parent(&window);
    let picker = match kind.as_str() {
        "script" => picker.add_filter("Publisher", &["ts"]),
        "node" => picker.add_filter("Executable", &["exe"]),
        "binary" => picker.add_filter("Harness", &["exe", "js", "mjs", "cjs"]),
        _ => return Err(anyhow::anyhow!("unknown publication file kind").into()),
    };
    Ok(picker
        .pick_file()
        .await
        .map(|file| file.path().to_string_lossy().into_owned()))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn prepare_publication(
    window: WebviewWindow<CefRuntime>,
    handle: AppHandle<CefRuntime>,
    metadata: PublicationMetadata,
    mut config: PublicationConfig,
) -> std::result::Result<Option<JobId>, Error> {
    // Fail before exporting any pages. Online runs refresh once here; the build uses that snapshot.
    let checked = inspect(&config, !config.offline).await?;
    if !checked.missing.is_empty() {
        return Err(
            anyhow::anyhow!("missing publishing tools: {}", checked.missing.join(", ")).into(),
        );
    }
    if let Some(error) = checked.catalog_error {
        return Err(anyhow::anyhow!("catalog check failed: {error}").into());
    }
    if checked.catalog.is_none() {
        return Err(anyhow::anyhow!(
            "download this website's catalog before using the saved version"
        )
        .into());
    }
    config.node = checked.node.context("Node is missing")?;
    config.script = checked.script.context("publisher is missing")?;
    config.binary = checked.binary;
    let Some(destination) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
        .map(|file| file.path().to_owned())
    else {
        return Ok(None);
    };
    let (snapshot, name) = {
        let projects = handle.state::<CurrentProject>();
        let mut projects = projects.project.lock().await;
        let project = projects.as_mut().context("no project is open")?;
        if !handle.state::<Processing>().stops.lock().is_empty() {
            return Err(anyhow::anyhow!("another process is already running").into());
        }
        let snapshot = project.snapshot();
        let patch = snapshot.patch(|edit| edit.set_project(&metadata))?;
        if let Some(commit) = project.commit_rebased(patch).await? {
            project.record_commit(&commit);
        }
        (project.snapshot(), project.name.clone())
    };
    let settings = koharu_config::load::<PublicationConfig>("publication")?;
    {
        let mut value = settings.write()?;
        *value = config.clone();
        value.save()?;
    }
    let id = JobId::new();
    let stop = StopToken::default();
    let processing = handle.state::<Processing>();
    let permit = processing.try_admit()?;
    {
        let mut stops = processing.stops.lock();
        stops.insert(id, stop.clone());
    }
    let job = Job {
        id,
        project: Some(name.clone()),
        state: JobState::Running,
        completed: 0,
        total: 1,
        page: None,
        chapter: None,
        chapter_title: Some(name.clone()),
        phase: None,
        stage: None,
        model: config.model.clone(),
        detail: Some(destination.to_string_lossy().into_owned()),
        retry: None,
        error: None,
    };
    processing.jobs.lock().insert(id, job.clone());
    handle.state::<JobChannel>().channel.publish(job);
    let task_handle = handle.clone();
    drop(tokio::spawn(async move {
        let _permit = permit;
        let result = async {
            let temp = tempfile::Builder::new().prefix("koharu-publication-").tempdir_in(&destination)?;
            let source = temp.path().join("source");
            tokio::fs::create_dir(&source).await?;
            let desktop = task_handle.state::<koharu_desktop::Desktop>();
            output::export_snapshot(name, snapshot.clone(), ExportFormat::Png, source.clone(), &desktop, Some(&stop)).await?;
            if stop.stopped() { bail!("publication stopped") }
            tokio::fs::create_dir(source.join("wiki")).await?;
            for file in snapshot.wiki()?.files {
                tokio::fs::write(source.join("wiki").join(format!("{}.md", file.slug)), file.body).await?;
            }
            let metadata_file = temp.path().join("metadata.json");
            let mut exported_metadata = serde_json::to_value(&metadata)?;
            let cover_role = koharu_scene::AssetRole::new("cover")?;
            for page in snapshot.pages() {
                if let Some(cover) = snapshot.asset(page.id(), &cover_role)? {
                    // Keep the cover outside the chapter tree: it is not a reader page.
                    let bytes = snapshot.read_blob(cover.blob).await?;
                    tokio::fs::write(temp.path().join("cover.image"), bytes).await?;
                    exported_metadata["coverFile"] = serde_json::json!("cover.image");
                    break;
                }
            }
            tokio::fs::write(&metadata_file, serde_json::to_vec(&exported_metadata)?).await?;
            let output = destination.join(format!("{}-{}", metadata.slug, uuid::Uuid::new_v4()));
            let cache = catalog_cache()?;
            let script = PathBuf::from(&config.script).canonicalize()?;
            let root = script.ancestors().nth(3).context("invalid publisher script location")?;
            let mut command = Command::new(&config.node);
            command.current_dir(root).args(["--import", "tsx"]).arg(&script).arg("build")
                .arg("--source").arg(&source).arg("--metadata").arg(metadata_file).arg("--output").arg(&output)
                .arg("--url").arg(&config.catalog_url).arg("--cache").arg(cache).arg("--backend").arg(&config.backend)
                .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            if let Some(binary) = &config.binary { command.arg("--binary").arg(binary); }
            if let Some(model) = &config.model { command.arg("--model").arg(model); }
            command.arg("--offline");
            #[cfg(windows)]
            command.creation_flags(0x08000000);
            let mut child = command.spawn().context("could not start local publisher")?;
            let pid = child.id();
            let mut stdout = BufReader::new(child.stdout.take().context("publisher stdout is missing")?).lines();
            let mut stderr = BufReader::new(child.stderr.take().context("publisher stderr is missing")?).lines();
            let mut open_out = true;
            let mut open_err = true;
            let mut last = String::new();
            while open_out || open_err {
                tokio::select! {
                    line = stdout.next_line(), if open_out => { match line? { Some(line) => { last = line; update(&task_handle, id, &last); }, None => open_out = false } }
                    line = stderr.next_line(), if open_err => { match line? { Some(line) => { last = line; update(&task_handle, id, &last); }, None => open_err = false } }
                    () = tokio::time::sleep(Duration::from_millis(400)) => { if stop.stopped() { summarizer::stop_process(&mut child, pid).await; bail!("publication stopped") } }
                }
            }
            if !child.wait().await?.success() { bail!("local publisher failed: {last}") }
            let projects = task_handle.state::<CurrentProject>();
            let projects = projects.project.lock().await;
            let current = projects.as_ref().context("project was closed during publication")?.snapshot();
            if current.project_id() != snapshot.project_id() || current.revision() != snapshot.revision() { bail!("project changed during publication; prepare again from its latest revision") }
            if !output.join("editorial.json").is_file() { bail!("publisher did not produce editorial.json") }
            update(&task_handle, id, &output.to_string_lossy());
            Ok::<(), anyhow::Error>(())
        }.await;
        let processing = task_handle.state::<Processing>();
        processing.stops.lock().remove(&id);
        let job = processing.jobs.lock().get_mut(&id).map(|job| {
            job.state = if stop.stopped() {
                JobState::Stopped
            } else if result.is_ok() {
                JobState::Finished
            } else {
                JobState::Failed
            };
            job.completed = usize::from(result.is_ok());
            job.error = result.err().map(|error| format!("{error:#}"));
            job.clone()
        });
        if let Some(job) = job {
            task_handle.state::<JobChannel>().channel.publish(job);
        }
    }));
    Ok(Some(id))
}

fn update(handle: &AppHandle<CefRuntime>, id: JobId, detail: &str) {
    let processing = handle.state::<Processing>();
    let job = processing.jobs.lock().get_mut(&id).map(|job| {
        job.detail = Some(detail.chars().take(400).collect());
        job.clone()
    });
    if let Some(job) = job {
        handle.state::<JobChannel>().channel.publish(job);
    }
}
