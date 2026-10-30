use anyhow::{Context as _, Result};
use koharu_desktop::{CanvasState, Desktop};
use koharu_scene::ChapterId;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use specta::Type;
use strum::{EnumMessage as _, IntoEnumIterator as _};
use tauri::{AppHandle, Manager as _, State, WebviewWindow, ipc::Channel};
use tauri_runtime_cef::CefRuntime;

use super::{
    ChannelExt as _, Error,
    agent::AgentState,
    canvas::CanvasChannel,
    import,
    preferences::Preferences,
    processing::{Job, JobChannel, Processing},
    project::{
        CurrentProject, Page, PageSummary, Project, ProjectInfo, ProjectLibrary, ProjectSummary,
    },
    summarizer::SummarizerConfig,
};

#[derive(Clone, Debug, Serialize, Type)]
pub struct StartupState {
    pub preferences: Preferences,
    pub jobs: Vec<Job>,
    pub canvas: CanvasState,
    pub agent: super::agent::AgentSnapshot,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct PageSelection {
    pub project: ProjectInfo,
    pub page: Page,
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum PageImportSource {
    Files,
    Folder,
}

pub(crate) struct Initialization {
    ready: tokio::sync::watch::Sender<bool>,
}

impl Default for Initialization {
    fn default() -> Self {
        let (ready, _) = tokio::sync::watch::channel(false);
        Self { ready }
    }
}

impl Initialization {
    pub(crate) fn ready(&self) {
        self.ready.send_replace(true);
    }

    async fn wait(&self) -> Result<()> {
        let mut ready = self.ready.subscribe();
        while !*ready.borrow_and_update() {
            ready
                .changed()
                .await
                .context("startup state closed before initialization completed")?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct Download {
    #[specta(type = f64)]
    pub id: u64,
    pub state: DownloadState,
    pub name: Option<String>,
    #[specta(type = f64)]
    pub completed: u64,
    #[specta(type = f64)]
    pub total: u64,
    pub error: Option<String>,
}

#[derive(Default)]
pub(crate) struct DownloadChannel {
    pub(crate) channel: Mutex<Option<Channel<Download>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum DownloadState {
    Running,
    Finished,
    Failed,
}

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct ModelResources {
    #[specta(type = f64)]
    pub process_memory: u64,
    #[specta(type = f64)]
    pub system_memory: u64,
    pub process_cpu: f32,
    pub devices: Vec<DeviceResources>,
}

#[derive(Default)]
pub(crate) struct ResourceChannel {
    pub(crate) channel: Mutex<Option<Channel<ModelResources>>>,
}

#[derive(Default)]
pub(crate) struct ProjectChannel {
    pub(crate) channel: Mutex<Option<Channel<Option<ProjectInfo>>>>,
}

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct DeviceResources {
    pub name: String,
    pub selected: bool,
    #[specta(type = Option<f64>)]
    pub memory_budget: Option<u64>,
    #[specta(type = Option<f64>)]
    pub memory_used: Option<u64>,
    pub utilization: Option<f32>,
}

impl From<koharu_pipeline::ResourceSnapshot> for ModelResources {
    fn from(value: koharu_pipeline::ResourceSnapshot) -> Self {
        Self {
            process_memory: value.process_memory_bytes,
            system_memory: value.system_memory_bytes,
            process_cpu: value.process_cpu_percent,
            devices: value
                .devices
                .into_iter()
                .map(|device| DeviceResources {
                    name: device.name,
                    selected: device.selected,
                    memory_budget: device.memory_budget_bytes,
                    memory_used: device.memory_used_bytes,
                    utilization: device.utilization_percent,
                })
                .collect(),
        }
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn subscribe(
    handle: AppHandle<CefRuntime>,
    on_canvas: Channel<CanvasState>,
    on_job: Channel<Job>,
    on_download: Channel<Download>,
    on_resources: Channel<ModelResources>,
    on_project: Channel<Option<ProjectInfo>>,
    on_agent: Channel<super::agent::AgentSnapshot>,
) -> std::result::Result<StartupState, Error> {
    handle.state::<Initialization>().wait().await?;

    *handle.state::<CanvasChannel>().channel.lock() = Some(on_canvas);
    *handle.state::<JobChannel>().channel.lock() = Some(on_job);
    *handle.state::<DownloadChannel>().channel.lock() = Some(on_download);
    *handle.state::<ResourceChannel>().channel.lock() = Some(on_resources);
    *handle.state::<ProjectChannel>().channel.lock() = Some(on_project);

    let canvas = handle.state::<Desktop>().canvas_state();
    let preferences = Preferences::load()?;
    Ok(StartupState {
        preferences,
        jobs: handle
            .state::<Processing>()
            .jobs
            .lock()
            .values()
            .cloned()
            .collect(),
        canvas,
        agent: handle.state::<AgentState>().subscribe(on_agent),
    })
}

pub(super) async fn replace_project(handle: &AppHandle<CefRuntime>, opened: Project) -> Result<()> {
    prepare_project_switch(handle).await;
    handle
        .state::<CurrentProject>()
        .project
        .replace(opened)
        .await;
    show_current_project(handle).await
}

async fn prepare_project_switch(handle: &AppHandle<CefRuntime>) {
    handle.state::<Processing>().inpainting_mask.lock().take();
}

async fn show_current_project(handle: &AppHandle<CefRuntime>) -> Result<()> {
    let projects = handle.state::<CurrentProject>();
    let current = projects.project.lock().await;
    let desktop = handle.state::<Desktop>();
    if let Some(project) = current.as_ref() {
        desktop
            .show_page(&project.snapshot(), project.active_page())
            .await?;
    } else {
        desktop.clear().await;
    }
    let canvas = desktop.canvas_state();
    handle.state::<CanvasChannel>().channel.publish(canvas);
    handle
        .state::<ProjectChannel>()
        .channel
        .publish(current.as_ref().map(Project::info));
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_project(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Option<ProjectInfo>, Error> {
    Ok(project.project.lock().await.as_ref().map(Project::info))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_pages(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Vec<PageSummary>, Error> {
    let snapshot = project
        .project
        .lock()
        .await
        .as_ref()
        .context("no project is open")?
        .snapshot();
    Ok(Project::pages(&snapshot)?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_page(
    project: State<'_, CurrentProject>,
) -> std::result::Result<Option<Page>, Error> {
    let current = {
        let project = project.project.lock().await;
        project
            .as_ref()
            .map(|project| (project.snapshot(), project.active_page()))
    };
    Ok(current
        .and_then(|(snapshot, page)| page.map(|page| (snapshot, page)))
        .map(|(snapshot, page)| Project::page(&snapshot, page))
        .transpose()?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn list_projects(
    library: State<'_, ProjectLibrary>,
) -> std::result::Result<Vec<ProjectSummary>, Error> {
    Ok(library.list()?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn create_project(
    name: String,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<(), Error> {
    let library = handle.state::<ProjectLibrary>().inner().clone();
    let opened = library.create(&name).await?;
    replace_project(&handle, opened).await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn open_project(
    name: String,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<(), Error> {
    if handle
        .state::<Processing>()
        .jobs
        .lock()
        .values()
        .any(|job| {
            job.state == super::processing::JobState::Running
                && job.phase == Some(super::processing::ChapterPhase::Import)
                && job.project.as_deref() == Some(name.as_str())
        })
    {
        return Err(
            anyhow::anyhow!("wait until this project's download/import has finished").into(),
        );
    }
    let library = handle.state::<ProjectLibrary>().inner().clone();
    prepare_project_switch(&handle).await;
    handle
        .state::<CurrentProject>()
        .project
        .open(&library, &name)
        .await?;
    show_current_project(&handle).await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn close_project(handle: AppHandle<CefRuntime>) -> std::result::Result<(), Error> {
    close_current_project(&handle).await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn delete_project(
    name: String,
    handle: AppHandle<CefRuntime>,
) -> std::result::Result<(), Error> {
    if handle.state::<Processing>().jobs.lock().values().any(|job|
        matches!(job.state, super::processing::JobState::Running | super::processing::JobState::Queued)
        && job.phase == Some(super::processing::ChapterPhase::Import)
        && job.project.as_deref() == Some(name.as_str())) {
        return Err(anyhow::anyhow!("stop this project's queued download/import before deleting it").into());
    }
    let library = handle.state::<ProjectLibrary>().inner().clone();
    let active = handle
        .state::<CurrentProject>()
        .project
        .delete(&library, &name)
        .await?;
    if active {
        handle.state::<AgentState>().reset().await;
        show_current_project(&handle).await?;
    }
    Ok(())
}

async fn close_current_project(handle: &AppHandle<CefRuntime>) -> Result<()> {
    prepare_project_switch(handle).await;
    handle.state::<CurrentProject>().project.close().await;
    show_current_project(handle).await
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn import(
    source: PageImportSource,
    chapter: Option<ChapterId>,
    window: WebviewWindow<CefRuntime>,
    desktop: State<'_, Desktop>,
    project: State<'_, CurrentProject>,
    processing: State<'_, Processing>,
    canvas_channel: State<'_, CanvasChannel>,
) -> std::result::Result<(), Error> {
    let busy = project
        .project
        .lock()
        .await
        .as_ref()
        .is_some_and(|project| processing.busy_for(&project.name));
    if busy {
        return Err(anyhow::anyhow!("pages cannot be imported while processing is running").into());
    }
    let extensions = import::Format::iter()
        .flat_map(|format| format.get_serializations())
        .collect::<Vec<_>>();
    let dialog = rfd::AsyncFileDialog::new()
        .add_filter("Images, archives, and PDF", &extensions)
        .set_parent(&window);
    let imported = match source {
        PageImportSource::Files => {
            let Some(files) = dialog.pick_files().await else {
                return Ok(());
            };
            let paths = files
                .into_iter()
                .map(|file| file.path().to_owned())
                .collect::<Vec<_>>();
            let pages = tokio_rayon::spawn(move || import::import(paths)).await?;
            if pages.is_empty() {
                return Err(
                    anyhow::anyhow!("no supported images were found in the selection").into(),
                );
            }
            Import::Pages(pages)
        }
        PageImportSource::Folder => {
            let Some(folder) = dialog.pick_folder().await else {
                return Ok(());
            };
            let root = folder.path().to_owned();
            if chapter.is_some() {
                Import::Pages(tokio_rayon::spawn(move || import::import_flat(&root)).await?)
            } else {
                Import::Chapters(tokio_rayon::spawn(move || import::import_folder(&root)).await?)
            }
        }
    };
    let summarize = SummarizerConfig::load()?.read()?.summarize_default;
    let (commit, page) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        let active_chapter = project
            .active_page
            .and_then(|page| project.snapshot().chapter_for(page).ok().flatten())
            .map(|chapter| chapter.id);
        let (commit, page) = match imported {
            Import::Pages(pages) => {
                project
                    .import_pages(pages, chapter.or(active_chapter), summarize)
                    .await?
            }
            Import::Chapters(chapters) => project.import_chapters(chapters, summarize).await?,
        };
        project.record(vec![commit.revision]);
        if let Some(page) = page {
            project.select_page(page)?;
        }
        let active = project.active_page();
        (commit, active)
    };
    desktop.synchronize(&commit.snapshot, page, &commit).await?;
    let canvas = desktop.canvas_state();
    canvas_channel.channel.publish(canvas);
    Ok(())
}

enum Import {
    Pages(Vec<import::ImportedPage>),
    Chapters(Vec<import::ImportedChapter>),
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn select_page(
    desktop: State<'_, Desktop>,
    page: koharu_scene::EntityId,
    project: State<'_, CurrentProject>,
    canvas_channel: State<'_, CanvasChannel>,
) -> std::result::Result<PageSelection, Error> {
    let (snapshot, project_info, selected_page) = {
        let mut project = project.project.lock().await;
        let project = project.as_mut().context("no project is open")?;
        project.select_page(page)?;
        let snapshot = project.snapshot();
        let project_info = project.info();
        let selected_page = Project::page(&snapshot, page)?;
        (snapshot, project_info, selected_page)
    };
    if desktop.show_page(&snapshot, Some(page)).await? {
        let canvas = desktop.canvas_state();
        canvas_channel.channel.publish(canvas);
    }
    Ok(PageSelection {
        project: project_info,
        page: selected_page,
    })
}
