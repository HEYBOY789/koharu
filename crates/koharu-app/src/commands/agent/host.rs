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

use super::{
    context::{self, InspectChapter, InspectPage, Window},
    scope::{self, EditRange},
};

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
    pages: Arc<parking_lot::Mutex<Option<Vec<EntityId>>>>,
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
        targets: Vec<EntityId>,
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
        scope::check(
            &project.snapshot(),
            self.binding.as_ref().unwrap().pages.lock().as_deref(),
            &targets,
        )?;
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
            let targets = match &scope {
                Scope::Pages(ids) | Scope::Entities(ids) => ids,
                _ => bail!("explicit pipeline scope is required"),
            };
            scope::check(
                &project.snapshot(),
                self.binding.as_ref().unwrap().pages.lock().as_deref(),
                targets,
            )?;
            project.snapshot()
        };
        let progress_host = self.clone();
        let positions = context::Locations::new(&snapshot)?;
        let labels = snapshot
            .pages()
            .filter_map(|page| {
                page.page()
                    .ok()
                    .map(|value| (page.id(), positions.label(page.id()).unwrap_or(value.label)))
            })
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
            resume: arguments.resume,
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
                pages: Arc::new(parking_lot::Mutex::new(None)),
            }),
        }))
    }

    async fn context(&self) -> Result<Value> {
        self.project_context(&Window::default()).await
    }

    fn tools(&self) -> Vec<Tool> {
        editor_tools()
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
                            break context::Locations::new(&snapshot)
                                .and_then(|positions| positions.label(id))
                                .ok()
                                .or(Some(page.label));
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
            "inspect_chapters" => {
                let query: Window = arguments(&call)?;
                let projects = self.handle.state::<CurrentProject>();
                let (project, _) = projects.project.target(self.target()?).await?;
                self.check_project(&project)?;
                self.observe(&project);
                Invocation::read(context::chapters(&project.snapshot(), &query)?)
            }
            "inspect_chapter" => {
                let query: InspectChapter = arguments(&call)?;
                let projects = self.handle.state::<CurrentProject>();
                let (project, _) = projects.project.target(self.target()?).await?;
                self.check_project(&project)?;
                self.observe(&project);
                Invocation::read(context::chapter(&project.snapshot(), &query)?)
            }
            "set_edit_scope" => {
                let query: EditRange = arguments(&call)?;
                let projects = self.handle.state::<CurrentProject>();
                let (project, _) = projects.project.target(self.target()?).await?;
                self.check_project(&project)?;
                self.check_revision(&project)?;
                let snapshot = project.snapshot();
                let pages = query.resolve(&snapshot)?;
                let result = json!({"first":context::location(&snapshot, pages[0])?, "last":context::location(&snapshot, *pages.last().unwrap())?, "page_count":pages.len()});
                *self.binding.as_ref().unwrap().pages.lock() = Some(pages);
                Invocation::read(result)
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
                self.mutate(vec![page], |project| {
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
                let allowed = self.binding.as_ref().unwrap().pages.lock().clone();
                self.mutate(vec![page], |project| {
                    Box::pin(async move {
                        Ok((
                            {
                                let snapshot = project.snapshot();
                                let destination = snapshot
                                    .pages()
                                    .nth(arguments.index)
                                    .context("destination project index is out of bounds")?
                                    .id();
                                scope::check(&snapshot, allowed.as_deref(), &[destination])?;
                                project.move_page(page, arguments.index).await?
                            },
                            context::location(&project.snapshot(), page)?,
                        ))
                    })
                })
                .await
            }
            "delete_pages" => {
                let arguments: DeletePages = arguments(&call)?;
                let pages = entities(&arguments.pages)?;
                self.mutate(pages.clone(), |project| {
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
                self.mutate(vec![page], |project| {
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
                self.mutate(vec![element], |project| {
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
                self.mutate(vec![element], |project| {
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
                self.mutate(vec![element], |project| {
                    Box::pin(async move {
                        let typography = arguments.merge(&project.snapshot(), element)?;
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
                let points = arguments.points.map(|points| {
                    points
                        .into_iter()
                        .map(|point| Point {
                            x: point.x,
                            y: point.y,
                        })
                        .collect()
                });
                self.mutate(vec![element], |project| {
                    Box::pin(async move {
                        Ok((
                            project
                                .set_geometry(vec![GeometryUpdate {
                                    layer: element,
                                    points,
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
                self.mutate(elements.clone(), |project| {
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
                self.mutate(elements.clone(), |project| {
                    Box::pin(async move {
                        Ok((
                            project.delete_layers(elements.clone()).await?,
                            json!({ "elements": elements }),
                        ))
                    })
                })
                .await
            }
            "reorder_element" => {
                let arguments: ReorderElement = arguments(&call)?;
                let element = entity(&arguments.element)?;
                let parent = entity(&arguments.parent)?;
                self.mutate(vec![element, parent], |project| {
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

fn editor_tools() -> Vec<Tool> {
    static TOOLS: OnceLock<Vec<Tool>> = OnceLock::new();
    TOOLS
            .get_or_init(|| {
                vec![
                    definition::<Window>(
                        "inspect_project",
                        "Read the current project overview and a batch of page IDs. Follow pagination.next_offset until null to list every page. Layer content is read separately with inspect_page.",
                    ),
                    definition::<Window>("inspect_chapters", "List ordered chapter IDs, titles and page counts, including empty chapters. Follow pagination.next_offset. Page labels repeat across chapters: never use a label as an ID."),
                    definition::<InspectChapter>("inspect_chapter", "Read the ordered page IDs and 1-based page numbers of one chapter. Use this to resolve requests such as page 3 of Chapter 1. Follow pagination.next_offset."),
                    definition::<EditRange>("set_edit_scope", "Before editing, select the user's inclusive range with first_page and last_page IDs from inspection. Native rejects every mutation outside it. For the whole project use overview.first_page and overview.last_page; for a single page use the same ID twice. Never expand the range to work around an error."),
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
                    definition::<MovePage>("move_page", "Move a page to a zero-based GLOBAL project index. This can change its chapter membership. Inspect the returned page position after moving. Both source and destination pages must be in the edit scope."),
                    definition::<DeletePages>("delete_pages", "Delete pages and their contents."),
                    definition::<AddTextBox>("add_text_box", "Add a paragraph text box to a page."),
                    definition::<SetText>("set_source_text", "Replace an element's source text."),
                    definition::<SetTranslation>(
                        "set_translation",
                        "Set an element's translation, or pass null to remove it.",
                    ),
                    definition::<SetTypography>(
                        "set_typography",
                        "Patch only the supplied typography properties; omitted properties are preserved, explicit null clears an override. Example: element = an inspected text-layer ID, preferred_font = a family from list_fonts.",
                    ),
                    definition::<SetGeometry>(
                        "set_geometry",
                        "Move or resize visible text on the IMAGE using page-space polygon points. Requires element (text-layer ID from inspect_page) and points. Pass explicit null to reset an automatic layout; omitted points is invalid. Do not use reorder_element to move text on the image.",
                    ),
                    definition::<SetVisibility>(
                        "set_visibility",
                        "Change visibility and/or opacity for elements.",
                    ),
                    definition::<DeleteElements>("delete_elements", "Delete elements and descendants."),
                    definition::<ReorderElement>(
                        "reorder_element",
                        "Reorder the LAYER TREE only. Requires element (ID from inspect_page), parent (page/group ID), index (zero-based). This does not move text on the image: use set_geometry for that.",
                    ),
                    definition::<RunPipeline>(
                        "run_pipeline",
                        "Run the configured page pipeline on explicit pages OR text elements inside the edit scope. resume defaults to true to preserve completed work. This is page processing, not chapter summarization; use the Chapter workflow UI for summarize/wiki generation.",
                    ),
                ]
            })
            .clone()
}

fn definition<T: JsonSchema>(name: &str, description: &str) -> Tool {
    Tool::new(
        name,
        description,
        serde_json::to_value(schema_for!(T)).expect("tool argument schema must serialize"),
    )
}

fn arguments<T: for<'de> Deserialize<'de> + JsonSchema>(call: &ToolCall) -> Result<T> {
    serde_json::from_str(&call.arguments)
        .with_context(|| format!("Invalid arguments for {}. No edit was applied. Read the tool schema and retry with IDs from inspect_page/inspect_project. Required contract: {}", call.name, schema_for!(T).as_value()))
}

fn nullable<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<T>, D::Error> {
    Option::<T>::deserialize(d)
}

fn patch_field<'de, D: serde::Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> std::result::Result<Option<Option<T>>, D::Error> {
    Ok(Some(Option::<T>::deserialize(d)?))
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

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            call_id: "regression".into(),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    #[test]
    fn editor_contract_missing_text_or_unknown_fields_cannot_delete_translation() {
        assert!(
            arguments::<SetTranslation>(&call("set_translation", json!({"element":"layer"})))
                .is_err()
        );
        assert!(
            arguments::<SetTranslation>(&call(
                "set_translation",
                json!({"element":"layer", "text":null, "translation":"wrong field"})
            ))
            .is_err()
        );
        assert!(
            arguments::<SetTranslation>(&call(
                "set_translation",
                json!({"element":"layer", "text":null})
            ))
            .is_ok()
        );
    }

    #[test]
    fn editor_contract_small_typography_edit_does_not_require_replacement() {
        assert!(
            arguments::<SetTypography>(&call(
                "set_typography",
                json!({"element":"layer", "preferred_font":"Noto Sans"})
            ))
            .is_ok()
        );
    }

    #[test]
    fn editor_contract_missing_or_misspelled_scope_cannot_process_whole_project() {
        assert!(
            arguments::<RunPipeline>(&call(
                "run_pipeline",
                json!({"operation":"translation", "chapter":"not-a-supported-scope"})
            ))
            .is_err()
        );
        let parsed =
            arguments::<RunPipeline>(&call("run_pipeline", json!({"operation":"translation"})));
        assert!(parsed.is_err() || parsed.unwrap().scope().is_err());
    }

    #[test]
    fn editor_contract_geometry_can_explicitly_reset_to_automatic() {
        assert!(
            arguments::<SetGeometry>(&call(
                "set_geometry",
                json!({"element":"layer", "points":null})
            ))
            .is_ok()
        );
    }

    #[test]
    fn editor_contract_invalid_reorder_returns_schema_without_guessing_ids() {
        let error = arguments::<ReorderElement>(&call("reorder_element", json!({})))
            .err()
            .unwrap();
        let text = format!("{error:#}");
        assert!(
            text.contains("element")
                && text.contains("parent")
                && text.contains("index")
                && text.contains("No edit was applied")
        );
        let schema = serde_json::to_value(schema_for!(SetTranslation)).unwrap();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v == "text")
        );
    }

    #[tokio::test]
    async fn editor_contract_chapter_range_typography_and_selection_preserve_native_edits()
    -> Result<()> {
        use koharu_scene::{At, PageDraft};
        let directory = tempfile::tempdir()?;
        let mut project = Project::create(
            "chapter fixture".into(),
            directory.path().join("fixture.khrproj"),
        )
        .await?;
        let mut pages = Vec::new();
        let patch = project.snapshot().patch(|edit| {
            for _ in 0..5 {
                pages.push(edit.add_page(PageDraft::new("003", 1000.0, 1600.0), At::End)?);
            }
            Ok(())
        })?;
        project.session.commit(patch).await?;
        let (_, chapter1) = project.create_chapter("Chapter 1".into(), true).await?;
        let (_, chapter2) = project.create_chapter("Chapter 2".into(), true).await?;
        project
            .create_chapter("Empty chapter".into(), false)
            .await?;
        for (index, page) in pages.iter().copied().enumerate() {
            project
                .assign_page(page, if index < 3 { chapter1 } else { chapter2 })
                .await?;
        }
        let (_, layer) = project
            .add_text_box(
                pages[2],
                Frame {
                    x: 10.0,
                    y: 10.0,
                    width: 80.0,
                    height: 80.0,
                    angle_degrees: 0.0,
                },
            )
            .await?;
        let snapshot = project.snapshot();
        let roster = context::chapters(&snapshot, &Window::default())?;
        assert_eq!(roster["chapters"][2]["page_count"], 0);
        let chapter = context::chapter(
            &snapshot,
            &InspectChapter {
                chapter: chapter1.to_string(),
                offset: 0,
                limit: 20,
            },
        )?;
        assert_eq!(chapter["pages"][2]["id"], pages[2].to_string());
        assert_eq!(chapter["pages"][2]["chapter_page_number"], 3);
        let range = EditRange {
            first_page: pages[2].to_string(),
            last_page: pages[4].to_string(),
        }
        .resolve(&snapshot)?;
        assert_eq!(range, pages[2..]);
        assert!(scope::check(&snapshot, Some(&range), &[layer]).is_ok());
        assert!(scope::check(&snapshot, Some(&range), &[pages[0]]).is_err());
        assert!(scope::check(&snapshot, None, &[layer]).is_err());
        assert!(
            EditRange {
                first_page: pages[3].to_string(),
                last_page: pages[0].to_string()
            }
            .resolve(&snapshot)
            .is_err()
        );
        let initial = Typography {
            preferred_font: Some("Old font".into()),
            font_weight: Some(700),
            font_style: Some(koharu_scene::FontStyle::Italic),
            size: Some(24.0),
            auto_fit: false,
            color: Some([1, 2, 3, 255]),
            stroke_color: Some([4, 5, 6, 255]),
            stroke_width: Some(2.0),
            alignment: Some(koharu_scene::TextAlignment::Center),
            writing_mode: Some(koharu_scene::WritingMode::Horizontal),
        };
        project
            .set_typography(vec![TypographyUpdate {
                layer,
                typography: initial.clone(),
            }])
            .await?;
        let before = project.snapshot();
        let args: SetTypography = arguments(&call(
            "set_typography",
            json!({"element":layer,"preferred_font":"New font"}),
        ))?;
        let changed = args.merge(&before, layer)?;
        let mut expected = initial;
        expected.preferred_font = Some("New font".into());
        assert_eq!(
            serde_json::to_value(&changed)?,
            serde_json::to_value(&expected)?
        );
        let commit = project
            .set_typography(vec![TypographyUpdate {
                layer,
                typography: changed,
            }])
            .await?;
        project.record_commit(&commit);
        assert_eq!(
            project
                .snapshot()
                .component::<koharu_scene::Typography>(layer)?
                .unwrap()
                .preferred_font
                .as_deref(),
            Some("New font")
        );
        project.undo().await?;
        assert_eq!(
            project
                .snapshot()
                .component::<koharu_scene::Typography>(layer)?,
            before.component::<koharu_scene::Typography>(layer)?
        );
        let clear: SetTypography = arguments(&call(
            "set_typography",
            json!({"element":layer,"stroke_width":null}),
        ))?;
        assert!(clear.merge(&before, layer)?.stroke_width.is_none());
        project.select_page(pages[2])?;
        let selection = super::super::AgentSelection {
            page: Some(pages[2]),
            elements: vec![layer],
        };
        selection.validate(&project)?;
        project.select_page(pages[3])?;
        assert!(selection.validate(&project).is_err());
        Ok(())
    }

    #[test]
    #[ignore = "Manual contract export for the installed AGY test; requires KOHARU_EDITOR_CONTRACT_FILE"]
    fn export_editor_contracts_for_installed_cli() -> Result<()> {
        let path = std::env::var("KOHARU_EDITOR_CONTRACT_FILE")?;
        std::fs::write(path, serde_json::to_vec_pretty(&editor_tools())?)?;
        Ok(())
    }

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
            pages: Arc::new(parking_lot::Mutex::new(None)),
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
            pages: Arc::new(parking_lot::Mutex::new(None)),
        };
        drop(project);
        let reopened = Project::open("fixture".into(), path).await?;
        assert!(old.check(&reopened).is_err());
        Ok(())
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ViewPage {
    page: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenamePage {
    page: String,
    label: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MovePage {
    page: String,
    index: usize,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeletePages {
    pages: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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
#[serde(deny_unknown_fields)]
struct SetText {
    element: String,
    text: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetTranslation {
    element: String,
    #[serde(deserialize_with = "nullable")]
    #[schemars(required)]
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
#[serde(deny_unknown_fields)]
struct SetTypography {
    element: String,
    #[serde(default, deserialize_with = "patch_field")]
    preferred_font: Option<Option<String>>,
    #[serde(default, deserialize_with = "patch_field")]
    font_weight: Option<Option<u16>>,
    #[serde(default, deserialize_with = "patch_field")]
    font_style: Option<Option<AgentFontStyle>>,
    #[serde(default, deserialize_with = "patch_field")]
    size: Option<Option<f32>>,
    auto_fit: Option<bool>,
    #[serde(default, deserialize_with = "patch_field")]
    color: Option<Option<[u8; 4]>>,
    #[serde(default, deserialize_with = "patch_field")]
    stroke_color: Option<Option<[u8; 4]>>,
    #[serde(default, deserialize_with = "patch_field")]
    stroke_width: Option<Option<f32>>,
    #[serde(default, deserialize_with = "patch_field")]
    alignment: Option<Option<AgentTextAlignment>>,
    #[serde(default, deserialize_with = "patch_field")]
    writing_mode: Option<Option<AgentWritingMode>>,
}

impl SetTypography {
    fn merge(self, snapshot: &Snapshot, element: EntityId) -> Result<Typography> {
        let current = snapshot.component::<koharu_scene::Typography>(element)?;
        let mut value: Typography = if let Some(current) = current {
            serde_json::from_value(serde_json::to_value(current)?)?
        } else {
            serde_json::from_value(json!({"auto_fit":true}))?
        };
        if self.preferred_font.is_none()
            && self.font_weight.is_none()
            && self.font_style.is_none()
            && self.size.is_none()
            && self.auto_fit.is_none()
            && self.color.is_none()
            && self.stroke_color.is_none()
            && self.stroke_width.is_none()
            && self.alignment.is_none()
            && self.writing_mode.is_none()
        {
            bail!("Provide at least one typography property to change");
        }
        if let Some(v) = self.preferred_font {
            value.preferred_font = v;
        }
        if let Some(v) = self.font_weight {
            value.font_weight = v;
        }
        if let Some(v) = self.font_style {
            value.font_style = v.map(Into::into);
        }
        if let Some(v) = self.size {
            anyhow::ensure!(
                v.is_none_or(|n| n.is_finite() && n > 0.0),
                "size must be positive or null"
            );
            value.size = v;
        }
        if let Some(v) = self.auto_fit {
            value.auto_fit = v;
        }
        if let Some(v) = self.color {
            value.color = v;
        }
        if let Some(v) = self.stroke_color {
            value.stroke_color = v;
        }
        if let Some(v) = self.stroke_width {
            value.stroke_width = v;
        }
        if let Some(v) = self.alignment {
            value.alignment = v.map(Into::into);
        }
        if let Some(v) = self.writing_mode {
            value.writing_mode = v.map(Into::into);
        }
        Ok(value)
    }
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetGeometry {
    element: String,
    #[serde(deserialize_with = "nullable")]
    #[schemars(required)]
    points: Option<Vec<AgentPoint>>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct AgentPoint {
    x: f64,
    y: f64,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetVisibility {
    elements: Vec<String>,
    visible: Option<bool>,
    opacity: Option<f32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct DeleteElements {
    elements: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReorderElement {
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
#[serde(deny_unknown_fields)]
struct RunPipeline {
    operation: AgentPipelineOperation,
    #[serde(default)]
    pages: Vec<String>,
    #[serde(default)]
    elements: Vec<String>,
    #[serde(default = "resume_default")]
    resume: bool,
}

fn resume_default() -> bool {
    true
}

impl RunPipeline {
    fn scope(&self) -> Result<Scope> {
        match (self.pages.is_empty(), self.elements.is_empty()) {
            (true, true) => bail!(
                "Provide explicit page IDs or element IDs from inspection; an empty scope never means the whole project"
            ),
            (false, true) => Ok(Scope::Pages(entities(&self.pages)?)),
            (true, false) => Ok(Scope::Entities(entities(&self.elements)?)),
            (false, false) => bail!("pipeline scope cannot contain both pages and elements"),
        }
    }
}
