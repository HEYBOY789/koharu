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
    failures::{Attempt, Failures, Target},
    readability::{self, Review, VerifyReadability},
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
    failures: Arc<parking_lot::Mutex<Failures>>,
    review: Arc<parking_lot::Mutex<Review>>,
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

    fn ensure_complete(&self, project: &Project) -> Result<()> {
        anyhow::ensure!(
            project.instance_id == self.instance,
            "Agent project has changed; start a new request"
        );
        // Completion diagnostics remain readable after internal cancellation;
        // otherwise cancellation hides the unresolved tool that caused it.
        self.failures.lock().ensure_complete()?;
        let review = self.review.lock();
        review.ensure_complete()?;
        review.ensure_fresh(&project.snapshot())
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
        let snapshot = project.snapshot();
        let pages = targets
            .iter()
            .map(|id| scope::page_for(&snapshot, *id))
            .collect::<Result<std::collections::HashSet<_>>>()?;
        for page in &pages {
            self.binding
                .as_ref()
                .unwrap()
                .review
                .lock()
                .before_edit(*page, readability::stamp(&snapshot, *page)?)?;
        }
        let (commit, value) = mutation(&mut project).await?;
        if commit.changes.from != commit.changes.to {
            for page in pages {
                self.binding.as_ref().unwrap().review.lock().changed(page);
            }
        }
        project.record_commit(&commit);
        project.reconcile_page();
        self.observe(&project);
        let result = mutation_result(&project, &commit, value)?;
        if visible {
            self.synchronize(commit, project.active_page()).await?;
        }
        Ok(result)
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
        let mut result = Invocation::read(json!({
            "base_revision": report.base,
            "final_revision": report.final_revision,
            "completed": report.completed,
            "total": report.total,
            "elapsed_ms": report.elapsed.as_millis(),
        }))?;
        result.changed = report.base != report.final_revision;
        Ok(result)
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
                failures: Arc::default(),
                review: Arc::default(),
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
        let binding = self.binding.as_ref().context("Agent host is unbound")?;
        let (attempt, parse_error) = match Attempt::parse(call.clone()) {
            Ok(attempt) => (attempt, None),
            Err(error) => (Attempt::raw(call), Some(error)),
        };
        let captured = async {
            let projects = self.handle.state::<CurrentProject>();
            let (project, _) = projects.project.target(self.target()?).await?;
            self.check_project(&project)?;
            Target::capture(&project.snapshot(), &attempt.call)
        }
        .await;
        let (target, capture_error) = match captured {
            Ok(target) => (target, None),
            Err(error) => (Target::default(), Some(error)),
        };
        let args: Value = serde_json::from_str(&attempt.call.arguments).unwrap_or(Value::Null);
        let element = args["element"]
            .as_str()
            .or_else(|| args["elements"][0].as_str())
            .map(str::to_owned);
        self.progress(super::AgentPhase::Tool, target.page.clone(), element, None);
        let ready = match parse_error.or(capture_error) {
            Some(error) => Err(error),
            None => binding.failures.lock().validate(&attempt, &target),
        };
        let result = match ready {
            Ok(()) => self.execute_tool(attempt.call.clone(), control).await,
            Err(error) => Err(error),
        };
        let (result, pending, exhausted) = {
            let mut failures = binding.failures.lock();
            let result = failures.finish(&attempt, target, result);
            (result, failures.snapshot(), failures.exhausted())
        };
        self.handle
            .state::<super::AgentState>()
            .store
            .failures(binding.run, pending);
        self.handle
            .state::<super::AgentState>()
            .store
            .readability(binding.run, binding.review.lock().progress());
        if exhausted {
            binding.control.cancel();
        }
        result
    }

    async fn ensure_complete(&self) -> Result<()> {
        let binding = self.binding.as_ref().context("Agent host is unbound")?;
        let projects = self.handle.state::<CurrentProject>();
        let (project, _) = projects.project.target(self.target()?).await?;
        binding.ensure_complete(&project)
    }
}

impl KoharuHost {
    async fn measured(
        &self,
        snapshot: &Snapshot,
        page: EntityId,
    ) -> Result<(
        koharu_renderer::ReadabilityReport,
        Vec<super::redundancy::RedundantText>,
    )> {
        let desktop = self.handle.state::<Desktop>();
        let frame = desktop.renderer().render(snapshot, page).await?;
        let background =
            output::frame_image(desktop.rasterizer().await?, &frame.without_text()?).await?;
        let mut report = frame.readability();
        let duplicates = readability::source_duplicates(snapshot, &mut report)?;
        report.sample_background(&background);
        Ok((report, duplicates))
    }

    async fn audit_readability(&self, query: InspectPage) -> Result<Invocation> {
        context::validate(query.limit)?;
        let page = entity(&query.page)?;
        let projects = self.handle.state::<CurrentProject>();
        let (project, _) = projects.project.target(self.target()?).await?;
        self.check_project(&project)?;
        let binding = self.binding.as_ref().unwrap();
        let pages = binding
            .pages
            .lock()
            .clone()
            .context("Choose set_edit_scope before reviewing")?;
        scope::check(&project.snapshot(), Some(&pages), &[page])?;
        binding.review.lock().begin(&pages)?;
        let (report, duplicates) = self.measured(&project.snapshot(), page).await?;
        let entries = report.layers.iter().skip(query.offset).take(query.limit).map(|layer| json!({
            "layer":layer,"issues":report.issues.iter().filter(|i| i.element==layer.element).collect::<Vec<_>>(),
            "ocr_duplicate":duplicates.iter().find(|duplicate| duplicate.element==layer.element)
        })).collect();
        let result = context::window(
            json!({"page":context::location(&project.snapshot(),page)?, "revision":project.revision(), "reader_widths":report.reader_widths, "minimum_font_px":report.minimum_font_px,"issue_count":report.issues.len(), "note":"Measurements are triage. Inspect crops; a dense balloon that cannot fit readable text needs review, not silent shrinking."}),
            "entries",
            entries,
            report.layers.len(),
            query.offset,
        )?;
        binding
            .review
            .lock()
            .audit(readability::stamp(&project.snapshot(), page)?, report);
        self.observe(&project);
        Invocation::read(result)
    }

    async fn edit_readability(&self, edit: LayerEdit) -> Result<Invocation> {
        let projects = self.handle.state::<CurrentProject>();
        let (mut project, visible) = projects.project.target(self.target()?).await?;
        self.check_project(&project)?;
        self.check_revision(&project)?;
        let snapshot = project.snapshot();
        let binding = self.binding.as_ref().unwrap();
        scope::check(&snapshot, binding.pages.lock().as_deref(), &edit.targets())?;
        if !binding.review.lock().active() {
            let pages = binding
                .pages
                .lock()
                .clone()
                .context("Choose an edit scope")?;
            binding.review.lock().begin(&pages)?;
        }
        let page = scope::page_for(&snapshot, edit.targets()[0])?;
        let before = binding
            .review
            .lock()
            .before_edit(page, readability::stamp(&snapshot, page)?)?;
        let (patch, value) = edit
            .preview_patch(&snapshot)?
            .context("expected typography/geometry edit")?;
        let quality = if let Some(before) = before {
            let desktop = self.handle.state::<Desktop>();
            let candidate = readability::checked_candidate(
                &desktop.renderer(),
                desktop.rasterizer().await?,
                &snapshot,
                &patch,
                page,
                &before,
            )
            .await;
            match candidate {
                Ok(report) => Some(report),
                Err(error) if error.is::<readability::CandidateRegression>() => {
                    return Invocation::read(
                        json!({"page":page,"changed":false,"candidate_rejected":format!("{error}"),"remaining_issues":before.issues,"next":"Inspect a crop and propose a different layout, or verify the remaining issue as needs_review."}),
                    );
                }
                Err(error) => return Err(error),
            }
        } else {
            None
        };
        self.check_project(&project)?;
        let commit = project.commit(patch).await?;
        project.record_commit(&commit);
        project.reconcile_page();
        if commit.changes.from != commit.changes.to {
            if let Some(report) = quality.as_ref() {
                binding
                    .review
                    .lock()
                    .audit(readability::stamp(&commit.snapshot, page)?, report.clone());
            } else {
                binding.review.lock().changed(page);
            }
        }
        self.observe(&project);
        let result = mutation_result(
            &project,
            &commit,
            json!({"edit":value,"quality":quality,"verification_required":true}),
        )?;
        if visible {
            self.synchronize(commit, project.active_page()).await?;
        }
        Ok(result)
    }

    async fn repair_text(&self, query: RepairText) -> Result<Invocation> {
        query.validate()?;
        let page = entity(&query.page)?;
        if let Some(layout) = query.layout {
            let desktop = self.handle.state::<Desktop>();
            let rasterizer = desktop.rasterizer().await?;
            let projects = self.handle.state::<CurrentProject>();
            let (project, _) = projects.project.target(self.target()?).await?;
            self.check_project(&project)?;
            self.check_revision(&project)?;
            let snapshot = project.snapshot();
            scope::check(
                &snapshot,
                self.binding.as_ref().unwrap().pages.lock().as_deref(),
                &[page, entity(&layout.element)?],
            )?;
            let binding = self.binding.as_ref().unwrap();
            let before = binding
                .review
                .lock()
                .before_edit(page, readability::stamp(&snapshot, page)?)?
                .context("Call audit_readability and view_page before layout search")?;
            let proposals = readability::search_layouts(
                &desktop.renderer(),
                rasterizer.clone(),
                &snapshot,
                page,
                layout,
                &before,
                &binding.control,
            )
            .await?;
            let result = proposals.invocation(rasterizer).await?;
            anyhow::ensure!(
                !binding.control.is_cancelled(),
                "Layout search cancelled; no edit was saved"
            );
            binding.review.lock().propose_layouts(proposals);
            self.observe(&project);
            return Ok(result);
        }
        let elements = entities(&query.elements)?;
        let targets = std::iter::once(page)
            .chain(elements.iter().copied())
            .collect();
        let desktop = self.handle.state::<Desktop>();
        let renderer = desktop.renderer();
        let rasterizer = desktop.rasterizer().await?;
        let review = self.binding.as_ref().unwrap().review.clone();
        let control = self.binding.as_ref().unwrap().control.clone();
        self.mutate(targets, move |project| {
            Box::pin(async move {
                let snapshot = project.snapshot();
                let before = review
                    .lock()
                    .before_edit(page, readability::stamp(&snapshot, page)?)?
                    .context("Call audit_readability and view_page before repair_text")?;
                if let Some(candidate) = query.candidate {
                    let (patch,value) = review.lock().layout_choice(&snapshot,page,&candidate)?;
                    anyhow::ensure!(!control.is_cancelled(), "Agent request was cancelled; no candidate edit was saved");
                    return Ok((project.commit(patch).await?, value));
                }
                let batch =
                    readability::repair_batch(&renderer, rasterizer, &snapshot, &elements, &before).await?;
                anyhow::ensure!(
                    !control.is_cancelled(),
                    "Agent request was cancelled; no candidate edit was saved"
                );
                let commit = project.commit(batch.patch).await?;
                Ok((
                    commit,
                    json!({"page":page,"elements":batch.elements,"removed_duplicates":batch.removed_duplicates,"balanced_groups":batch.balanced_groups,"remaining_issues":batch.report.issues,"declined":batch.declined,"verification_required":true}),
                ))
            })
        })
        .await
    }

    async fn execute_tool(&self, call: ToolCall, control: &Control) -> Result<Invocation> {
        if control.is_cancelled() {
            bail!("Agent request was cancelled");
        }
        if let Some(edit) = LayerEdit::parse(&call)? {
            let snapshot = self
                .handle
                .state::<CurrentProject>()
                .project
                .snapshot(self.target()?)
                .await?;
            edit.validate_review(&snapshot, &self.binding.as_ref().unwrap().review.lock())?;
            if matches!(edit, LayerEdit::Typography(..) | LayerEdit::Geometry(..))
                || (matches!(edit, LayerEdit::Translation(..))
                    && self.binding.as_ref().unwrap().review.lock().active())
            {
                return self.edit_readability(edit).await;
            }
            return self
                .mutate(edit.targets(), |project| Box::pin(edit.apply(project)))
                .await;
        }
        anyhow::ensure!(
            !self.binding.as_ref().unwrap().review.lock().active()
                || !matches!(call.name.as_str(), "delete_pages" | "move_page"),
            "Readability repair cannot delete pages or move them between chapters"
        );
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
                if query.readability || self.binding.as_ref().unwrap().review.lock().active() {
                    self.binding.as_ref().unwrap().review.lock().begin(&pages)?;
                }
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
            "audit_readability" => self.audit_readability(arguments(&call)?).await,
            "repair_text" => self.repair_text(arguments(&call)?).await,
            "verify_readability" => {
                let query: VerifyReadability = arguments(&call)?;
                let page = entity(&query.page)?;
                let projects = self.handle.state::<CurrentProject>();
                let (project, _) = projects.project.target(self.target()?).await?;
                self.check_project(&project)?;
                self.binding.as_ref().unwrap().review.lock().verify(
                    page,
                    readability::stamp(&project.snapshot(), page)?,
                    query.issues,
                )?;
                Invocation::read(
                    json!({"page":page,"coverage":self.binding.as_ref().unwrap().review.lock().progress()}),
                )
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
                Invocation::read(context::fonts(fonts, &query)?)
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
                let frame = renderer.render(&snapshot, page).await?;
                let review_active = self.binding.as_ref().unwrap().review.lock().active();
                let review_report = if review_active && !arguments.original {
                    let mut report = frame.readability();
                    readability::source_duplicates(&snapshot, &mut report)?;
                    report.sample_background(
                        &output::frame_image(rasterizer.clone(), &frame.without_text()?).await?,
                    );
                    let summary = json!({"issues":report.issues,"font_sizes":report.layers.iter().map(|l|json!({"element":l.element,"source_font_px":l.source_font_px,"display_font_px":l.display_font_px})).collect::<Vec<_>>()});
                    let in_scope = self
                        .binding
                        .as_ref()
                        .unwrap()
                        .pages
                        .lock()
                        .as_ref()
                        .is_some_and(|pages| pages.contains(&page));
                    let mut review = self.binding.as_ref().unwrap().review.lock();
                    if in_scope {
                        review.audit(readability::stamp(&snapshot, page)?, report);
                    }
                    Some(summary)
                } else {
                    None
                };
                let crop = readability::view_crop(
                    &snapshot,
                    &frame,
                    arguments.element.as_deref().map(entity).transpose()?,
                    arguments.original,
                )?;
                let frame = if arguments.original {
                    frame.original_artwork()?
                } else {
                    frame
                };
                let dual = crop.is_none()
                    && !arguments.original
                    && arguments.display_width.is_none()
                    && self.binding.as_ref().unwrap().review.lock().active();
                let width = if dual {
                    Some(960)
                } else {
                    arguments.display_width
                };
                let bytes = output::frame_preview(rasterizer.clone(), &frame, crop, width).await?;
                let mut result = Invocation::read(json!({ "page": page, "label": label, "original":arguments.original,"crop":crop,"display_width":width,"phone_and_desktop":dual,"readability":review_report }))?.with_image(
                    format!("{} page {label} ({page}), width {width:?}", if arguments.original { "Original artwork" } else { "Rendered" }), format!("data:image/webp;base64,{}", STANDARD.encode(bytes)));
                if dual {
                    let bytes = output::frame_preview(rasterizer, &frame, None, Some(390)).await?;
                    result = result.with_image(
                        format!("Phone view {label} ({page}), 390px wide"),
                        format!("data:image/webp;base64,{}", STANDARD.encode(bytes)),
                    );
                }
                if arguments.original {
                    self.binding
                        .as_ref()
                        .unwrap()
                        .review
                        .lock()
                        .source_viewed(page, readability::stamp(&snapshot, page)?);
                } else if crop.is_none() {
                    self.binding.as_ref().unwrap().review.lock().viewed(
                        page,
                        readability::stamp(&snapshot, page)?,
                        if dual { None } else { width },
                    );
                }
                Ok(result)
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
            "run_pipeline" => {
                anyhow::ensure!(
                    !self.binding.as_ref().unwrap().review.lock().active(),
                    "Do not restart processing during readability repair"
                );
                self.run_pipeline(arguments(&call)?, control).await
            }
            name => bail!("unknown Koharu tool {name}"),
        }
    }
}

struct AgentCommitter {
    host: KoharuHost,
}

fn mutation_result(
    project: &Project,
    commit: &Commit,
    value: impl serde::Serialize,
) -> Result<Invocation> {
    let mut result = Invocation::read(json!({
        "revision": commit.revision, "project": project.info(), "result": value,
    }))?;
    result.changed = commit.changes.from != commit.changes.to;
    Ok(result)
}

#[cfg(test)]
#[path = "acceptance.rs"]
mod acceptance;

// The CLI and the native acceptance fixture execute the same parsed layer commands.
enum LayerEdit {
    Add(EntityId, Frame),
    Source(EntityId, String),
    Translation(EntityId, Option<String>),
    Typography(EntityId, SetTypography),
    Geometry(EntityId, Option<Vec<Point>>),
    Visibility(Vec<EntityId>, Option<bool>, Option<f32>),
    Delete(Vec<EntityId>),
    Reorder(EntityId, EntityId, usize),
}

impl LayerEdit {
    fn validate_review(&self, snapshot: &Snapshot, review: &readability::Review) -> Result<()> {
        if !review.active() {
            return Ok(());
        }
        anyhow::ensure!(
            !matches!(
                self,
                LayerEdit::Add(..) | LayerEdit::Source(..) | LayerEdit::Reorder(..)
            ),
            "Readability repair preserves artwork, source text and the layer tree"
        );
        if let LayerEdit::Translation(element, text) = self {
            let text = text
                .as_ref()
                .context("Readability repair cannot remove the translation")?;
            let original = snapshot
                .text_layer(*element)?
                .content()?
                .translation()?
                .context("readability needs an existing translation")?;
            anyhow::ensure!(
                original
                    .text
                    .value
                    .split_whitespace()
                    .eq(text.split_whitespace()),
                "Readability edits may change line breaks, but must preserve every word"
            );
        }
        if matches!(self, LayerEdit::Delete(..) | LayerEdit::Visibility(..)) {
            for target in self.targets() {
                let is_text = snapshot.text_layer(target).is_ok();
                let cleanup = snapshot
                    .component::<koharu_scene::RasterLayer>(target)?
                    .is_some_and(|r| r.kind == koharu_scene::RasterLayerKind::Cleanup);
                anyhow::ensure!(
                    is_text || (cleanup && matches!(self, LayerEdit::Visibility(..))),
                    "Readability repair cannot delete or hide artwork or raster layers"
                );
                let destructive = matches!(self, LayerEdit::Delete(..))
                    || matches!(&self, LayerEdit::Visibility(_, Some(false), _))
                    || matches!(&self,LayerEdit::Visibility(_,_,Some(v)) if *v<=0.0);
                if cleanup {
                    anyhow::ensure!(
                        !destructive,
                        "Readability repair must preserve cleanup; hiding it exposes original lettering"
                    );
                } else if destructive {
                    let page = scope::page_for(&snapshot, target)?;
                    let report = review
                        .before_edit(page, readability::stamp(&snapshot, page)?)?
                        .context("Audit before removing duplicates")?;
                    let targets = self.targets();
                    let groups = super::redundancy::OriginalText::read(snapshot, &report)?.groups();
                    let covered = if let Some(group) =
                        groups.iter().find(|group| group.element == target)
                    {
                        anyhow::ensure!(
                            !group.requires_source_review
                                || review
                                    .source_was_viewed(page, readability::stamp(snapshot, page)?),
                            "Conflicting OCR clock digits require view_page with original=true before removing this candidate. Compare the original numbers with the surviving layer; keep uncertain readings as needs_review."
                        );
                        if group.restore.is_some() {
                            return Err(readability::DeclinedEdit("Use repair_text on this page to preserve the leading word in its surviving dialogue before removing the merged OCR.".into()).into());
                        }
                        group.survivors.iter().all(|id| !targets.contains(id))
                    } else {
                        report
                            .issues
                            .iter()
                            .filter(|issue| {
                                issue.kind == koharu_renderer::ReadabilityKind::DuplicateCandidate
                                    && !issue.id.starts_with("source_coverage:")
                            })
                            .any(|issue| {
                                let other = if issue.element == target {
                                    issue.related
                                } else if issue.related == Some(target) {
                                    Some(issue.element)
                                } else {
                                    None
                                };
                                other.is_some_and(|other| {
                                    !targets.contains(&other)
                                        && report.layers.iter().any(|layer| layer.element == other)
                                })
                            })
                    };
                    if !covered {
                        return Err(readability::DeclinedEdit("Only an audited duplicate can be removed. Keep all listed survivors visible and preserve any distinct dialogue.".into()).into());
                    }
                }
                if let LayerEdit::Visibility(_, _, Some(opacity)) = &self {
                    let current = snapshot
                        .component::<koharu_scene::Visibility>(target)?
                        .map_or(1.0, |v| v.opacity);
                    anyhow::ensure!(
                        *opacity >= current || destructive,
                        "Readability repair cannot fade text or cleanup to conceal a problem"
                    );
                }
            }
        }
        Ok(())
    }
    fn preview_patch(&self, snapshot: &Snapshot) -> Result<Option<(koharu_scene::Patch, Value)>> {
        Ok(Some(match self {
            Self::Translation(element, text) => (
                Project::translation_patch(snapshot, *element, text.clone())?,
                json!({"element":element}),
            ),
            Self::Typography(element, a) => (
                Project::typography_patch(
                    snapshot,
                    vec![TypographyUpdate {
                        layer: *element,
                        typography: a.clone().merge(snapshot, *element)?,
                    }],
                )?,
                json!({"element":element}),
            ),
            Self::Geometry(element, points) => (
                Project::geometry_patch(
                    snapshot,
                    vec![GeometryUpdate {
                        layer: *element,
                        points: points.clone(),
                    }],
                )?,
                json!({"element":element}),
            ),
            _ => return Ok(None),
        }))
    }
    fn parse(call: &ToolCall) -> Result<Option<Self>> {
        Ok(Some(match call.name.as_str() {
            "add_text_box" => {
                let a: AddTextBox = arguments(call)?;
                Self::Add(
                    entity(&a.page)?,
                    Frame {
                        x: a.x,
                        y: a.y,
                        width: a.width,
                        height: a.height,
                        angle_degrees: a.angle_degrees,
                    },
                )
            }
            "set_source_text" => {
                let a: SetText = arguments(call)?;
                Self::Source(entity(&a.element)?, a.text)
            }
            "set_translation" => {
                let a: SetTranslation = arguments(call)?;
                Self::Translation(entity(&a.element)?, a.text)
            }
            "set_typography" => {
                let a: SetTypography = arguments(call)?;
                Self::Typography(entity(&a.element)?, a)
            }
            "set_geometry" => {
                let a: SetGeometry = arguments(call)?;
                Self::Geometry(
                    entity(&a.element)?,
                    a.points.map(|points| {
                        points
                            .into_iter()
                            .map(|p| Point { x: p.x, y: p.y })
                            .collect()
                    }),
                )
            }
            "set_visibility" => {
                let a: SetVisibility = arguments(call)?;
                Self::Visibility(entities(&a.elements)?, a.visible, a.opacity)
            }
            "delete_elements" => {
                let a: DeleteElements = arguments(call)?;
                Self::Delete(entities(&a.elements)?)
            }
            "reorder_element" => {
                let a: ReorderElement = arguments(call)?;
                Self::Reorder(entity(&a.element)?, entity(&a.parent)?, a.index)
            }
            _ => return Ok(None),
        }))
    }

    fn targets(&self) -> Vec<EntityId> {
        match self {
            Self::Add(id, _)
            | Self::Source(id, _)
            | Self::Translation(id, _)
            | Self::Typography(id, _)
            | Self::Geometry(id, _) => vec![*id],
            Self::Visibility(ids, ..) | Self::Delete(ids) => ids.clone(),
            Self::Reorder(id, parent, _) => vec![*id, *parent],
        }
    }

    async fn apply(self, project: &mut Project) -> Result<(Commit, Value)> {
        Ok(match self {
            Self::Add(page, frame) => {
                let (commit, element) = project.add_text_box(page, frame).await?;
                (commit, json!({ "page": page, "element": element }))
            }
            Self::Source(element, text) => (
                project.set_source_text(element, text).await?,
                json!({ "element": element }),
            ),
            Self::Translation(element, text) => (
                project.set_translation(element, text).await?,
                json!({ "element": element }),
            ),
            Self::Typography(element, a) => {
                let typography = a.merge(&project.snapshot(), element)?;
                (
                    project
                        .set_typography(vec![TypographyUpdate {
                            layer: element,
                            typography,
                        }])
                        .await?,
                    json!({ "element": element }),
                )
            }
            Self::Geometry(element, points) => (
                project
                    .set_geometry(vec![GeometryUpdate {
                        layer: element,
                        points,
                    }])
                    .await?,
                json!({ "element": element }),
            ),
            Self::Visibility(elements, visible, opacity) => (
                project
                    .set_visibility(elements.clone(), visible, opacity)
                    .await?,
                json!({ "elements": elements }),
            ),
            Self::Delete(elements) => (
                project.delete_layers(elements.clone()).await?,
                json!({ "elements": elements }),
            ),
            Self::Reorder(element, parent, index) => (
                project.move_layer(element, parent, index).await?,
                json!({ "element": element, "parent": parent }),
            ),
        })
    }
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
                    definition::<InspectChapter>("inspect_chapter", "Read ordered page IDs and 1-based positions of one chapter. Required argument: chapter = an ID from inspect_chapters.chapters[].id. Optional offset and limit control pagination. Use this to resolve requests such as page 16 of Chapter 1. Follow pagination.next_offset."),
                    definition::<EditRange>("set_edit_scope", "Before editing, supply BOTH required arguments first_page and last_page: actual page IDs read through inspection. Empty arguments are invalid. The range is inclusive. Native rejects mutations outside it. For the whole project use overview.first_page and overview.last_page; for a single page use the same ID twice. Never expand the range to work around an error."),
                    definition::<InspectPage>(
                        "inspect_page",
                        "Read one page's semantic layer and analysis-region entries, including text, font and geometry. Follow pagination.next_offset until null for all entries. No image rendering is performed.",
                    ),
                    definition::<Window>(
                        "list_fonts",
                        "List installed font families with available faces (numeric weight, weight_range, style) and script/language metadata, using the same representation as Properties. Choose supported faces rather than guessing. Follow pagination.next_offset until null.",
                    ),
                    definition::<ViewPage>(
                        "view_page",
                        "Render the actual page. Optional element crops around a text layer, preserving nearby artwork. Set original=true to inspect unmodified source artwork without cleanup or translated text; element then crops its ORIGINAL OCR region. Use original=true for conflicting OCR numbers before deleting a duplicate. Original views do not verify the translated page. Omit element for the whole page; display_width=390 simulates phone width, 960 desktop. View the translated full page after edits before verify_readability.",
                    ),
                    definition::<InspectPage>("audit_readability", "Measure rendered text at phone (390px) and desktop (960px) widths. Requires an edit scope and activates review for the scope. Returns fitted font sizes, overflow, overlap candidates, outline ratios and background contrast. An ocr_duplicate entry lists required survivors. If automatic=false, inspect the candidate and all survivors. requires_source_review=true means conflicting OCR clock digits: use view_page original=true and compare them before choosing a copy. Keep uncertain readings needs_review. Follow pagination.next_offset."),
                    definition::<RepairText>("repair_text", "Repair audited, visually inspected text. page alone or elements (up to 50) repairs typography and proven duplicates in one undo step. For cramped text, supply layout={element,area:[x,y,width,height],avoid:[[x,y,width,height],...]} after inspecting original artwork. Native tries joint frames/font sizes/contrast, protects text/avoid regions, and limits free narration to its original footprint plus a small allowance (source_placement). Phone size is a warning; preserve the source composition and visual scale. Returns up to 3 read-only phone/desktop previews. Compare them, then call with page and candidate (chosen ID) to save one atomic patch. A changed project invalidates candidates. Omit elements when using layout or candidate. Re-audit/view saved results and verify; explain remaining warnings honestly."),
                    definition::<VerifyReadability>("verify_readability", "Finish one page only after a fresh audit and a full view_page of the current result. Supply one issue decision for every remaining issue ID. keep is ONLY for visual collision/duplicate false positives; measured small text/overflow/contrast/thick outline must be fixed or marked needs_review with a concrete reason. Native completion requires every scoped page verified; do not claim needs_review pages repaired."),
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
                        "Patch existing text typography. Omitted fields are preserved; null clears an override. font_weight is an INTEGER 1–1000 (400 regular, 700 bold), never a word. font_style is normal/italic/oblique. Choose a supported face from list_fonts. Colors are numeric RGBA arrays, each channel 0–255. Fixed size: size > 0 in pixels with auto_fit=false. Auto size: size=null with auto_fit=true. stroke_width >= 0; 0 disables the outline. alignment=Start/Center/End/Justify; writing_mode=Horizontal/Vertical; null restores automatic direction/alignment. Preserve text and artwork for readability repairs.",
                    ),
                    definition::<SetGeometry>(
                        "set_geometry",
                        "Move or resize visible text on the IMAGE using page-space polygon points. Requires element (text-layer ID from inspect_page) and points. Pass explicit null to reset an automatic layout; omitted points is invalid. Do not use reorder_element to move text on the image.",
                    ),
                    definition::<SetVisibility>(
                        "set_visibility",
                        "Change visibility (boolean) and/or opacity (number from 0 to 1, not 0 to 100). Omitted properties are preserved. Target inspected layer IDs.",
                    ),
                    definition::<DeleteElements>("delete_elements", "Delete elements and descendants. During readability review, only remove an audited duplicate after checking the rendered text. Keep every listed survivor visible and preserve all distinct dialogue. On candidate_rejected, keep the text and fix its layout or mark the issue needs_review."),
                    definition::<ReorderElement>(
                        "reorder_element",
                        "Reorder the LAYER TREE only. Requires element (ID from inspect_page), parent (page/group ID), index (zero-based). This does not move text on the image: use set_geometry for that.",
                    ),
                    definition::<RunPipeline>(
                        "run_pipeline",
                        "Only for explicitly requested detection/OCR/translation/inpainting, never for readability, font, outline, size, rendering or canvas refresh. Requires operation (full/detection/ocr/translation/inpainting) and nonempty pages OR elements within edit scope; resume defaults to true. Direct layer edits already save/render. Chapter summary/wiki uses the Chapter workflow UI.",
                    ),
                ]
            })
            .clone()
}

fn definition<T: JsonSchema>(name: &str, description: &str) -> Tool {
    let mut parameters =
        serde_json::to_value(schema_for!(T)).expect("tool argument schema must serialize");
    parameters["properties"]["retry_of"] = json!({"type":"string","description":"When retrying a rejected call, use its exact retry_of ID. Keep the original tool and page/layer targets; unrelated successes do not resolve failures."});
    Tool::new(name, description, parameters)
}

fn arguments<T: for<'de> Deserialize<'de> + JsonSchema>(call: &ToolCall) -> Result<T> {
    serde_json::from_str(&call.arguments).map_err(|error| {
        let schema = schema_for!(T);
        let required = schema.as_value()["required"]
            .as_array()
            .map(|fields| fields.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        let mut accepted = schema.as_value()["properties"].as_object().map(|fields| fields.keys().map(String::as_str).collect::<Vec<_>>()).unwrap_or_default();
        accepted.push("retry_of");
        let accepted = accepted.join(", ");
        let requirement = if required.is_empty() {
            "All fields are optional; {} is valid".to_owned()
        } else {
            format!("Required fields: {required}")
        };
        anyhow!("Invalid arguments for {}: {error}. No edit was applied. {requirement}. Accepted fields: {accepted}. Correct the arguments using the tool definition and retry.", call.name)
    })
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

    #[test]
    fn editor_contract_missing_pipeline_operation_has_a_compact_recovery_message() {
        let error = arguments::<RunPipeline>(&call("run_pipeline", json!({})))
            .err()
            .unwrap();
        let message = format!("{error:#}");
        assert!(message.contains("operation"));
        assert!(message.contains("No edit was applied"));
        assert!(
            message.len() < 500,
            "validation error leaked the full schema: {message}"
        );
        assert!(!message.contains("$defs") && !message.contains("$schema"));
    }

    #[test]
    fn editor_contract_optional_inspection_arguments_have_consistent_recovery_guidance() {
        let error = arguments::<Window>(&call("inspect_project", json!({"unknown":true})))
            .err()
            .unwrap();
        let message = format!("{error:#}");
        assert!(message.contains("offset") && message.contains("limit"));
        assert!(
            message.contains("{} is valid"),
            "optional inspection recovery must allow its valid empty query: {message}"
        );
        assert!(!message.contains("do not repeat an empty call"));
        assert!(arguments::<Window>(&call("inspect_project", json!({}))).is_ok());
        assert!(arguments::<RunPipeline>(&call("run_pipeline", json!({}))).is_err());
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
            readability: false,
        }
        .resolve(&snapshot)?;
        assert_eq!(range, pages[2..]);
        assert!(scope::check(&snapshot, Some(&range), &[layer]).is_ok());
        assert!(scope::check(&snapshot, Some(&range), &[pages[0]]).is_err());
        assert!(scope::check(&snapshot, None, &[layer]).is_err());
        assert!(
            EditRange {
                first_page: pages[3].to_string(),
                last_page: pages[0].to_string(),
                readability: false,
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
        std::fs::write(&path, serde_json::to_vec_pretty(&editor_tools())?)?;
        let error = arguments::<Window>(&call("inspect_project", json!({"unknown":true})))
            .err()
            .unwrap();
        std::fs::write(
            std::path::Path::new(&path).with_extension("errors.json"),
            serde_json::to_vec(&json!({"inspect_project_unknown":format!("{error:#}")}))?,
        )?;
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
            failures: Arc::default(),
            review: Arc::default(),
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
            failures: Arc::default(),
            review: Arc::default(),
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
    element: Option<String>,
    display_width: Option<u32>,
    /// Show unmodified source artwork. Use it to resolve OCR conflicts; it does not verify the translated page.
    #[serde(default)]
    original: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RepairText {
    page: String,
    /// Omit to repair the eligible flagged layers on the page.
    #[serde(default)]
    elements: Vec<String>,
    /// Preview complete layouts in explicitly permitted space; saves nothing.
    layout: Option<readability::LayoutSearch>,
    /// Apply one ID from the most recent layout preview, at the same revision.
    candidate: Option<String>,
}

impl RepairText {
    fn validate(&self) -> Result<()> {
        anyhow::ensure!(
            !(self.layout.is_some() && self.candidate.is_some())
                && (self.elements.is_empty() || self.layout.is_none() && self.candidate.is_none()),
            "Use page/elements for ordinary repair, page/layout for previews, or page/candidate for selection"
        );
        Ok(())
    }
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

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetTypography {
    element: String,
    #[serde(default, deserialize_with = "patch_field")]
    preferred_font: Option<Option<String>>,
    #[serde(default, deserialize_with = "patch_field")]
    /// Numeric font weight, 1–1000. Use 400 for regular, 700 for bold. Never pass a word.
    font_weight: Option<Option<u16>>,
    #[serde(default, deserialize_with = "patch_field")]
    /// Supported face style from list_fonts: normal, italic or oblique.
    font_style: Option<Option<AgentFontStyle>>,
    #[serde(default, deserialize_with = "patch_field")]
    /// Positive pixel size; pair with auto_fit=false. For Auto use size=null, auto_fit=true.
    size: Option<Option<f32>>,
    auto_fit: Option<bool>,
    #[serde(default, deserialize_with = "patch_field")]
    /// Numeric RGBA channels, each 0–255.
    color: Option<Option<[u8; 4]>>,
    #[serde(default, deserialize_with = "patch_field")]
    /// Numeric RGBA outline color; alpha 0 hides the border.
    stroke_color: Option<Option<[u8; 4]>>,
    #[serde(default, deserialize_with = "patch_field")]
    /// Nonnegative outline width in pixels; 0 disables the border.
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
pub(super) enum AgentPipelineOperation {
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
    /// Required processing operation: full, detection, ocr, translation or inpainting.
    /// This generates page content; it does not render, refresh or format existing layers.
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
