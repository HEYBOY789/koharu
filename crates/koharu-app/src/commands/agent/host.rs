use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};

use anyhow::{Context as _, Result, anyhow, bail};
use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use koharu_agent::{Control, Host, Invocation, RunId, Tool, ToolCall};
use koharu_desktop::{Desktop, Frame};
use koharu_pipeline::{Committer, Operation, RunStatus, Scope, Stage, StageOutput, StopToken};
use koharu_scene::{Commit, EntityId, Revision, Snapshot};
use schemars::{JsonSchema, schema_for};
use serde::Deserialize;
use serde_json::{Value, json};
use tauri::{AppHandle, Manager as _};
use tauri_runtime_cef::CefRuntime;

use super::context::{self, InspectPage, Window};

use crate::commands::{
    ChannelExt as _,
    canvas::{CanvasChannel, Point},
    editing::{GeometryUpdate, TypographyUpdate},
    output,
    preferences::Preferences,
    processing::Processing,
    project::{CurrentProject, Project, ProjectLease, Typography},
};

#[derive(Clone)]
pub(super) struct KoharuHost {
    handle: AppHandle<CefRuntime>,
    binding: Option<Binding>,
    target: Option<ProjectLease>,
}

#[derive(Clone)]
struct Binding {
    instance: uuid::Uuid,
    control: Control,
    run: RunId,
    observed: Arc<parking_lot::Mutex<Option<Revision>>>,
}

impl Binding {
    fn check(&self, project: &Project) -> Result<()> {
        if self.control.is_cancelled() {
            bail!("Agent request was cancelled");
        }
        if project.instance_id != self.instance {
            bail!("Agent project has changed; start a new request");
        }
        Ok(())
    }
    fn check_revision(&self, project: &Project) -> Result<()> {
        anyhow::ensure!(
            self.observed.lock().as_ref() == Some(&project.revision()),
            "the project changed since it was inspected; read inspect_project/inspect_page again before editing"
        );
        Ok(())
    }
}

impl KoharuHost {
    pub(super) fn new(handle: AppHandle<CefRuntime>, target: Option<ProjectLease>) -> Self {
        Self {
            handle,
            binding: None,
            target,
        }
    }

    fn check_project(&self, project: &Project) -> Result<()> {
        self.binding
            .as_ref()
            .context("Agent host is not bound to a project")?
            .check(project)
    }

    fn target(&self) -> Result<&ProjectLease> {
        self.target
            .as_ref()
            .context("agent has no retained project")
    }

    fn observe(&self, project: &Project) {
        if let Some(binding) = &self.binding {
            *binding.observed.lock() = Some(project.revision());
        }
    }

    fn check_revision(&self, project: &Project) -> Result<()> {
        self.binding
            .as_ref()
            .context("agent host is unbound")?
            .check_revision(project)
    }

    fn progress(
        &self,
        phase: super::AgentPhase,
        page: Option<String>,
        element: Option<String>,
        stage: Option<Stage>,
    ) {
        if let Some(binding) = &self.binding {
            self.handle.state::<super::AgentState>().store.progress(
                binding.run,
                phase,
                page,
                element,
                stage,
            );
        }
    }

    async fn project_context(&self, query: &Window) -> Result<Value> {
        let preferences = Preferences::load()?;
        let configuration = json!({
            "pipeline": {
                "detection": preferences.pipeline.detection,
                "ocr": preferences.pipeline.ocr,
                "inpainting": preferences.pipeline.inpainting,
                "translation": {
                    "model": preferences.pipeline.translation.model,
                    "target_language": preferences.pipeline.translation.target_language,
                },
            },
            "typesetting": preferences.typesetting,
        });
        let projects = self.handle.state::<CurrentProject>();
        let (project, _) = projects.project.target(self.target()?).await?;
        self.check_project(&project)?;
        let result = context::overview(&project, configuration, query)?;
        self.observe(&project);
        Ok(result)
    }

    async fn mutate<T>(
        &self,
        mutation: impl for<'project> FnOnce(
            &'project mut Project,
        ) -> Pin<
            Box<dyn Future<Output = Result<(Commit, T)>> + Send + 'project>,
        >,
    ) -> Result<Invocation>
    where
        T: serde::Serialize + Send,
    {
        let projects = self.handle.state::<CurrentProject>();
        let (mut project, visible) = projects.project.target(self.target()?).await?;
        self.check_project(&project)?;
        self.check_revision(&project)?;
        let (commit, value) = mutation(&mut project).await?;
        project.record_commit(&commit);
        project.reconcile_page();
        self.observe(&project);
        let revision = commit.revision;
        if visible {
            self.synchronize(commit, project.active_page()).await?;
        }
        Invocation::changed(
            json!({"revision": revision, "project": project.info(), "result": value}),
        )
    }

    async fn synchronize(&self, commit: Commit, page: Option<EntityId>) -> Result<()> {
        let desktop = self.handle.state::<Desktop>();
        desktop.synchronize(&commit.snapshot, page, &commit).await?;
        let canvas = desktop.canvas_state();
        self.handle.state::<CanvasChannel>().channel.publish(canvas);
        Ok(())
    }

    async fn run_pipeline(&self, arguments: RunPipeline, control: &Control) -> Result<Invocation> {
        let scope = arguments.scope()?;
        let operation = arguments.operation.into();
        let stop = StopToken::default();

        self.progress(super::AgentPhase::Waiting, None, None, None);
        let processing = self.handle.state::<Processing>();
        let _permit = tokio::select! {
            biased;
            () = control.cancelled() => { bail!("agent pipeline wait was cancelled"); },
            permit = processing.wait(&stop) => permit.context("agent pipeline wait was cancelled")?,
        };
        let snapshot = {
            let projects = self.handle.state::<CurrentProject>();
            let (project, _) = projects.project.target(self.target()?).await?;
            self.check_project(&project)?;
            self.check_revision(&project)?;
            project.snapshot()
        };
        let progress_host = self.clone();
        let labels = snapshot
            .pages()
            .filter_map(|page| page.page().ok().map(|value| (page.id(), value.label)))
            .collect::<std::collections::HashMap<_, _>>();
        let progress: koharu_pipeline::ProgressSink = Arc::new(move |event| {
            use koharu_pipeline::Progress;
            let (page, stage) = match event {
                Progress::Loading { page, stage, .. }
                | Progress::Running { page, stage, .. }
                | Progress::Finished { page, stage, .. }
                | Progress::Retrying { page, stage, .. }
                | Progress::Skipped { page, stage } => (labels.get(&page).cloned(), Some(stage)),
                _ => (None, None),
            };
            progress_host.progress(super::AgentPhase::Pipeline, page, None, stage);
        });
        let mut committer = AgentCommitter { host: self.clone() };
        let request = koharu_pipeline::Request {
            resume: false,
            operation,
            scope,
            stop: stop.clone(),
            progress: Some(progress),
            inpainting_mask: None,
        };
        let pipeline = self.handle.state::<koharu_pipeline::Pipeline>();
        let execution = pipeline.execute(snapshot, request, &mut committer);
        tokio::pin!(execution);
        let result = tokio::select! {
            result = &mut execution => result,
            () = control.cancelled() => { stop.stop(); execution.await },
        };
        let report = result.map_err(|error| anyhow!(error))?;
        if report.status == RunStatus::Stopped {
            bail!("pipeline processing was cancelled");
        }
        Invocation::changed(json!({
            "base_revision": report.base,
            "final_revision": report.final_revision,
            "completed": report.completed,
            "total": report.total,
            "elapsed_ms": report.elapsed.as_millis(),
        }))
    }
}

#[async_trait]
impl Host for KoharuHost {
    async fn bind(&self, run: RunId, control: Control) -> Result<Arc<dyn Host>> {
        if control.is_cancelled() {
            bail!("Agent request was cancelled");
        }
        let target = self.target()?.clone();
        let projects = self.handle.state::<CurrentProject>();
        drop(projects.project.target(&target).await?);
        Ok(Arc::new(Self {
            handle: self.handle.clone(),
            target: Some(target.clone()),
            binding: Some(Binding {
                instance: target.id,
                control,
                run,
                observed: Arc::new(parking_lot::Mutex::new(None)),
            }),
        }))
    }

    async fn context(&self) -> Result<Value> {
        self.project_context(&Window::default()).await
    }

    fn tools(&self) -> Vec<Tool> {
        static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
        TOOLS
            .get_or_init(|| {
                vec![
                    definition::<Window>(
                        "inspect_project",
                        "Read the current project overview and a batch of page IDs. Follow pagination.next_offset until null to list every page. Layer content is read separately with inspect_page.",
                    ),
                    definition::<InspectPage>(
                        "inspect_page",
                        "Read one page's semantic layer and analysis-region entries, including text, font and geometry. Follow pagination.next_offset until null for all entries. No image rendering is performed.",
                    ),
                    definition::<Window>(
                        "list_fonts",
                        "List available font family names. Follow pagination.next_offset until null for all fonts.",
                    ),
                    definition::<ViewPage>(
                        "view_page",
                        "Render and inspect one page image. Call this only for pages whose visual appearance matters to the request.",
                    ),
                    definition::<RenamePage>("rename_page", "Rename a project page."),
                    definition::<MovePage>("move_page", "Move a page to a zero-based project index."),
                    definition::<DeletePages>("delete_pages", "Delete pages and their contents."),
                    definition::<AddTextBox>("add_text_box", "Add a paragraph text box to a page."),
                    definition::<SetText>("set_source_text", "Replace an element's source text."),
                    definition::<SetTranslation>(
                        "set_translation",
                        "Set an element's translation, or pass null to remove it.",
                    ),
                    definition::<SetTypography>(
                        "set_typography",
                        "Replace an element's typography settings. Font is a family name returned by list_fonts.",
                    ),
                    definition::<SetGeometry>(
                        "set_geometry",
                        "Replace an element's page-space polygon geometry.",
                    ),
                    definition::<SetVisibility>(
                        "set_visibility",
                        "Change visibility and/or opacity for elements.",
                    ),
                    definition::<DeleteElements>("delete_elements", "Delete elements and descendants."),
                    definition::<MoveElement>(
                        "move_element",
                        "Move an element under a page or element at a zero-based index.",
                    ),
                    definition::<RunPipeline>(
                        "run_pipeline",
                        "Run Koharu's configured processing pipeline for the whole project, selected pages, or selected text elements.",
                    ),
                ]
            })
            .clone()
    }

    async fn invoke(&self, call: ToolCall, control: &Control) -> Result<Invocation> {
        if control.is_cancelled() {
            bail!("Agent request was cancelled");
        }
        {
            let args: Value = serde_json::from_str(&call.arguments)?;
            let element = args["element"]
                .as_str()
                .map(str::to_owned)
                .or_else(|| args["elements"][0].as_str().map(str::to_owned));
            let requested = args["page"]
                .as_str()
                .or_else(|| args["pages"][0].as_str())
                .map(str::to_owned)
                .or_else(|| element.clone());
            let projects = self.handle.state::<CurrentProject>();
            let (project, _) = projects.project.target(self.target()?).await?;
            self.check_project(&project)?;
            let snapshot = project.snapshot();
            let page = requested
                .and_then(|id| entity(&id).ok())
                .and_then(|mut id| {
                    loop {
                        if let Ok(page) = snapshot.page(id).and_then(|page| page.page()) {
                            break Some(page.label);
                        }
                        match snapshot.parent(id) {
                            Ok(Some(parent)) => id = parent,
                            _ => break None,
                        }
                    }
                });
            self.progress(super::AgentPhase::Tool, page, element, None);
        }
        match call.name.as_str() {
            "inspect_project" => {
                let query: Window = arguments(&call)?;
                Invocation::read(self.project_context(&query).await?)
            }
            "inspect_page" => {
                let query: InspectPage = arguments(&call)?;
                let page = entity(&query.page)?;
                let projects = self.handle.state::<CurrentProject>();
                let (project, _) = projects.project.target(self.target()?).await?;
                self.check_project(&project)?;
                let result = context::page(&project.snapshot(), page, query.offset, query.limit)?;
                self.observe(&project);
                Invocation::read(result)
            }
            "list_fonts" => {
                let query: Window = arguments(&call)?;
                {
                    let projects = self.handle.state::<CurrentProject>();
                    let (project, _) = projects.project.target(self.target()?).await?;
                    self.check_project(&project)?;
                }
                let fonts = self
                    .handle
                    .state::<Desktop>()
                    .renderer()
                    .available_fonts()
                    .await?;
                Invocation::read(context::fonts(
                    fonts.into_iter().map(|font| font.name).collect(),
                    &query,
                )?)
            }
            "view_page" => {
                let arguments: ViewPage = arguments(&call)?;
                let page = entity(&arguments.page)?;
                let (label, snapshot) = {
                    let projects = self.handle.state::<CurrentProject>();
                    let (project, _) = projects.project.target(self.target()?).await?;
                    self.check_project(&project)?;
                    self.observe(&project);
                    let snapshot = project.snapshot();
                    let label = snapshot.page(page)?.page()?.label;
                    (label, snapshot)
                };
                let desktop = self.handle.state::<Desktop>();
                let renderer = desktop.renderer();
                let rasterizer = desktop.rasterizer().await?;
                let bytes =
                    output::rendered_preview(&renderer, rasterizer, &snapshot, page).await?;
                Ok(
                    Invocation::read(json!({ "page": page, "label": label }))?.with_image(
                        format!("Rendered page {label} ({page})"),
                        format!("data:image/webp;base64,{}", STANDARD.encode(bytes)),
                    ),
                )
            }
            "rename_page" => {
                let arguments: RenamePage = arguments(&call)?;
                let page = entity(&arguments.page)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project.rename_page(page, arguments.label).await?,
                            json!({ "page": page }),
                        ))
                    })
                })
                .await
            }
            "move_page" => {
                let arguments: MovePage = arguments(&call)?;
                let page = entity(&arguments.page)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project.move_page(page, arguments.index).await?,
                            json!({ "page": page }),
                        ))
                    })
                })
                .await
            }
            "delete_pages" => {
                let arguments: DeletePages = arguments(&call)?;
                let pages = entities(&arguments.pages)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project.delete_pages(pages.clone()).await?,
                            json!({ "pages": pages }),
                        ))
                    })
                })
                .await
            }
            "add_text_box" => {
                let arguments: AddTextBox = arguments(&call)?;
                let page = entity(&arguments.page)?;
                let frame = Frame {
                    x: arguments.x,
                    y: arguments.y,
                    width: arguments.width,
                    height: arguments.height,
                    angle_degrees: arguments.angle_degrees,
                };
                self.mutate(|project| {
                    Box::pin(async move {
                        let (commit, element) = project.add_text_box(page, frame).await?;
                        Ok((commit, json!({ "page": page, "element": element })))
                    })
                })
                .await
            }
            "set_source_text" => {
                let arguments: SetText = arguments(&call)?;
                let element = entity(&arguments.element)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project.set_source_text(element, arguments.text).await?,
                            json!({ "element": element }),
                        ))
                    })
                })
                .await
            }
            "set_translation" => {
                let arguments: SetTranslation = arguments(&call)?;
                let element = entity(&arguments.element)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project.set_translation(element, arguments.text).await?,
                            json!({ "element": element }),
                        ))
                    })
                })
                .await
            }
            "set_typography" => {
                let arguments: SetTypography = arguments(&call)?;
                let element = entity(&arguments.element)?;
                let typography = Typography {
                    preferred_font: arguments.preferred_font,
                    font_weight: arguments.font_weight,
                    font_style: arguments.font_style.map(Into::into),
                    size: arguments.size,
                    auto_fit: arguments.auto_fit,
                    color: arguments.color,
                    stroke_color: arguments.stroke_color,
                    stroke_width: arguments.stroke_width,
                    alignment: arguments.alignment.map(Into::into),
                    writing_mode: arguments.writing_mode.map(Into::into),
                };
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project
                                .set_typography(vec![TypographyUpdate {
                                    layer: element,
                                    typography,
                                }])
                                .await?,
                            json!({ "element": element }),
                        ))
                    })
                })
                .await
            }
            "set_geometry" => {
                let arguments: SetGeometry = arguments(&call)?;
                let element = entity(&arguments.element)?;
                let points = arguments
                    .points
                    .into_iter()
                    .map(|point| Point {
                        x: point.x,
                        y: point.y,
                    })
                    .collect();
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project
                                .set_geometry(vec![GeometryUpdate {
                                    layer: element,
                                    points: Some(points),
                                }])
                                .await?,
                            json!({ "element": element }),
                        ))
                    })
                })
                .await
            }
            "set_visibility" => {
                let arguments: SetVisibility = arguments(&call)?;
                let elements = entities(&arguments.elements)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project
                                .set_visibility(
                                    elements.clone(),
                                    arguments.visible,
                                    arguments.opacity,
                                )
                                .await?,
                            json!({ "elements": elements }),
                        ))
                    })
                })
                .await
            }
            "delete_elements" => {
                let arguments: DeleteElements = arguments(&call)?;
                let elements = entities(&arguments.elements)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project.delete_layers(elements.clone()).await?,
                            json!({ "elements": elements }),
                        ))
                    })
                })
                .await
            }
            "move_element" => {
                let arguments: MoveElement = arguments(&call)?;
                let element = entity(&arguments.element)?;
                let parent = entity(&arguments.parent)?;
                self.mutate(|project| {
                    Box::pin(async move {
                        Ok((
                            project.move_layer(element, parent, arguments.index).await?,
                            json!({ "element": element, "parent": parent }),
                        ))
                    })
                })
                .await
            }
            "run_pipeline" => self.run_pipeline(arguments(&call)?, control).await,
            name => bail!("unknown Koharu tool {name}"),
        }
    }
}

struct AgentCommitter {
    host: KoharuHost,
}

#[async_trait]
impl Committer for AgentCommitter {
    async fn commit(&mut self, output: StageOutput) -> Result<Snapshot> {
        let projects = self.host.handle.state::<CurrentProject>();
        let (mut project, visible) = projects.project.target(self.host.target()?).await?;
        self.host.check_project(&project)?;
        let Some(commit) = project.commit_rebased(output.patch).await? else {
            return Ok(project.snapshot());
        };
        project.record_commit(&commit);
        self.host.observe(&project);
        let snapshot = commit.snapshot.clone();
        if visible {
            self.host.synchronize(commit, project.active_page()).await?;
        }
        Ok(snapshot)
    }
}

fn definition<T: JsonSchema>(name: &str, description: &str) -> Tool {
    Tool::new(
        name,
        description,
        serde_json::to_value(schema_for!(T)).expect("tool argument schema must serialize"),
    )
}

fn arguments<T: for<'de> Deserialize<'de>>(call: &ToolCall) -> Result<T> {
    serde_json::from_str(&call.arguments)
        .with_context(|| format!("invalid arguments for {}", call.name))
}

fn entity(value: &str) -> Result<EntityId> {
    serde_json::from_value(Value::String(value.to_owned()))
        .with_context(|| format!("invalid entity ID {value}"))
}

fn entities(values: &[String]) -> Result<Vec<EntityId>> {
    values.iter().map(|value| entity(value)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn agent_binding_preserves_undo_and_rejects_cancelled_or_reopened_projects() -> Result<()>
    {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("fixture.khrproj");
        let mut project = Project::create("fixture".into(), path.clone()).await?;
        let control = Control::default();
        let binding = Binding {
            instance: project.instance_id,
            control: control.clone(),
            run: RunId::new(),
            observed: Arc::new(parking_lot::Mutex::new(None)),
        };
        let mut page = None;
        let patch = project.snapshot().patch(|edit| {
            page = Some(edit.add_page(
                koharu_scene::PageDraft::new("p1", 100.0, 100.0),
                koharu_scene::At::End,
            )?);
            Ok(())
        })?;
        project.session.commit(patch).await?;
        let page = page.context("page")?;
        let (commit, element) = project
            .add_text_box(
                page,
                Frame {
                    x: 10.0,
                    y: 10.0,
                    width: 80.0,
                    height: 50.0,
                    angle_degrees: 0.0,
                },
            )
            .await?;
        project.record_commit(&commit);
        drop(commit);
        let before = serde_json::to_value(Project::page(&project.snapshot(), page)?)?;
        binding.check(&project)?;
        *binding.observed.lock() = Some(project.revision());
        binding.check_revision(&project)?;
        let commit = project
            .set_translation(element, Some("Edited through agent tools".into()))
            .await?;
        project.record_commit(&commit);
        drop(commit);
        assert!(binding.check_revision(&project).is_err());
        *binding.observed.lock() = Some(project.revision());
        binding.check_revision(&project)?;
        assert_ne!(
            serde_json::to_value(Project::page(&project.snapshot(), page)?)?,
            before
        );
        project.undo().await?;
        assert_eq!(
            serde_json::to_value(Project::page(&project.snapshot(), page)?)?,
            before
        );
        control.cancel();
        assert!(binding.check(&project).is_err());
        let old = Binding {
            instance: project.instance_id,
            control: Control::default(),
            run: RunId::new(),
            observed: Arc::new(parking_lot::Mutex::new(None)),
        };
        drop(project);
        let reopened = Project::open("fixture".into(), path).await?;
        assert!(old.check(&reopened).is_err());
        Ok(())
    }
}

#[derive(Deserialize, JsonSchema)]
struct ViewPage {
    page: String,
}

#[derive(Deserialize, JsonSchema)]
struct RenamePage {
    page: String,
    label: String,
}

#[derive(Deserialize, JsonSchema)]
struct MovePage {
    page: String,
    index: usize,
}

#[derive(Deserialize, JsonSchema)]
struct DeletePages {
    pages: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct AddTextBox {
    page: String,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    #[serde(default)]
    angle_degrees: f32,
}

#[derive(Deserialize, JsonSchema)]
struct SetText {
    element: String,
    text: String,
}

#[derive(Deserialize, JsonSchema)]
struct SetTranslation {
    element: String,
    text: Option<String>,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AgentTextAlignment {
    Start,
    Center,
    End,
    Justify,
}

impl From<AgentTextAlignment> for koharu_scene::TextAlignment {
    fn from(value: AgentTextAlignment) -> Self {
        match value {
            AgentTextAlignment::Start => Self::Start,
            AgentTextAlignment::Center => Self::Center,
            AgentTextAlignment::End => Self::End,
            AgentTextAlignment::Justify => Self::Justify,
        }
    }
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AgentWritingMode {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AgentFontStyle {
    Normal,
    Italic,
    Oblique,
}

impl From<AgentFontStyle> for koharu_scene::FontStyle {
    fn from(value: AgentFontStyle) -> Self {
        match value {
            AgentFontStyle::Normal => Self::Normal,
            AgentFontStyle::Italic => Self::Italic,
            AgentFontStyle::Oblique => Self::Oblique,
        }
    }
}

impl From<AgentWritingMode> for koharu_scene::WritingMode {
    fn from(value: AgentWritingMode) -> Self {
        match value {
            AgentWritingMode::Horizontal => Self::Horizontal,
            AgentWritingMode::Vertical => Self::Vertical,
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct SetTypography {
    element: String,
    preferred_font: Option<String>,
    font_weight: Option<u16>,
    font_style: Option<AgentFontStyle>,
    size: Option<f32>,
    auto_fit: bool,
    color: Option<[u8; 4]>,
    stroke_color: Option<[u8; 4]>,
    stroke_width: Option<f32>,
    alignment: Option<AgentTextAlignment>,
    writing_mode: Option<AgentWritingMode>,
}

#[derive(Deserialize, JsonSchema)]
struct SetGeometry {
    element: String,
    points: Vec<AgentPoint>,
}

#[derive(Deserialize, JsonSchema)]
struct AgentPoint {
    x: f64,
    y: f64,
}

#[derive(Deserialize, JsonSchema)]
struct SetVisibility {
    elements: Vec<String>,
    visible: Option<bool>,
    opacity: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
struct DeleteElements {
    elements: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
struct MoveElement {
    element: String,
    parent: String,
    index: usize,
}

#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum AgentPipelineOperation {
    Full,
    Detection,
    Ocr,
    Translation,
    Inpainting,
}

impl From<AgentPipelineOperation> for Operation {
    fn from(value: AgentPipelineOperation) -> Self {
        match value {
            AgentPipelineOperation::Full => Self::Full,
            AgentPipelineOperation::Detection => Self::Only {
                stage: Stage::Detection,
            },
            AgentPipelineOperation::Ocr => Self::Only { stage: Stage::Ocr },
            AgentPipelineOperation::Translation => Self::Only {
                stage: Stage::Translation,
            },
            AgentPipelineOperation::Inpainting => Self::Only {
                stage: Stage::Inpainting,
            },
        }
    }
}

#[derive(Deserialize, JsonSchema)]
struct RunPipeline {
    operation: AgentPipelineOperation,
    #[serde(default)]
    pages: Vec<String>,
    #[serde(default)]
    elements: Vec<String>,
}

impl RunPipeline {
    fn scope(&self) -> Result<Scope> {
        match (self.pages.is_empty(), self.elements.is_empty()) {
            (true, true) => Ok(Scope::Project),
            (false, true) => Ok(Scope::Pages(entities(&self.pages)?)),
            (true, false) => Ok(Scope::Entities(entities(&self.elements)?)),
            (false, false) => bail!("pipeline scope cannot contain both pages and elements"),
        }
    }
}
