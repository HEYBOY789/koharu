//! Exercises the production layer-command parser and native project mutations without a WebView.
use super::*;
use koharu_scene::{At, PageDraft};

struct Fixture {
    directory: tempfile::TempDir,
    project: Project,
    page: EntityId,
    other: EntityId,
    layer: EntityId,
}

fn call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        call_id: uuid::Uuid::new_v4().to_string(),
        name: name.into(),
        arguments: args.to_string(),
    }
}

impl Fixture {
    async fn new() -> Result<Self> {
        Self::with_chapter_pages(3).await
    }

    async fn with_chapter_pages(count: usize) -> Result<Self> {
        let directory = tempfile::tempdir()?;
        let mut project = Project::create(
            "native editor acceptance".into(),
            directory.path().join("fixture.khrproj"),
        )
        .await?;
        let mut pages = vec![];
        let patch = project.snapshot().patch(|edit| {
            for index in 0..=count {
                pages.push(edit.add_page(
                    PageDraft::new(format!("{:03}", index % count + 1), 1000.0, 1600.0),
                    At::End,
                )?);
            }
            Ok(())
        })?;
        project.session.commit(patch).await?;
        let (_, chapter) = project.create_chapter("Chapter 1".into(), false).await?;
        for page in &pages[..count] {
            project.assign_page(*page, chapter).await?;
        }
        let (_, chapter) = project.create_chapter("Chapter 2".into(), false).await?;
        project.assign_page(pages[count], chapter).await?;
        let page = pages[count - 1];
        let add = LayerEdit::parse(&call(
            "add_text_box",
            json!({"page":page,"x":10,"y":20,"width":200,"height":100}),
        ))?
        .unwrap();
        let (_, output) = add.apply(&mut project).await?;
        let layer = serde_json::from_value(output["element"].clone())?;
        project
            .set_source_text(layer, "Original text".into())
            .await?;
        project
            .set_translation(layer, Some("Chữ tiếng Việt cần dễ đọc".into()))
            .await?;
        Ok(Self {
            directory,
            project,
            page,
            other: pages[0],
            layer,
        })
    }

    async fn edit(&mut self, name: &str, args: Value) -> Result<Invocation> {
        let edit = LayerEdit::parse(&call(name, args))?.context("layer command")?;
        scope::check(
            &self.project.snapshot(),
            Some(&[self.page]),
            &edit.targets(),
        )?;
        let (commit, value) = edit.apply(&mut self.project).await?;
        self.project.record_commit(&commit);
        mutation_result(&self.project, &commit, value)
    }

    fn layer_view(&self) -> Result<Value> {
        let page = serde_json::to_value(Project::page(&self.project.snapshot(), self.page)?)?;
        page["layers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|layer| layer["id"] == self.layer.to_string())
            .cloned()
            .context("text layer")
    }
}

#[tokio::test]
async fn native_layer_settings_roundtrip_preserve_text_reject_invalid_edits_and_ignore_noops()
-> Result<()> {
    let mut fixture = Fixture::new().await?;
    let layer = fixture.layer;
    let before = fixture.layer_view()?;
    let settings = json!({"element":layer,"preferred_font":"Noto Sans","font_weight":700,
        "font_style":"italic","size":32.0,"auto_fit":false,"color":[10,20,30,255],
        "stroke_color":[240,241,242,220],"stroke_width":1.5,"alignment":"Center","writing_mode":"Horizontal"});
    assert!(
        fixture
            .edit("set_typography", settings.clone())
            .await?
            .changed
    );
    let after = fixture.layer_view()?;
    for (key, value) in settings.as_object().unwrap() {
        if key != "element" {
            assert_eq!(&after["typography"][key], value, "{key}");
        }
    }
    assert_eq!(before["content"], after["content"]);
    assert!(
        !fixture
            .edit("set_typography", settings.clone())
            .await?
            .changed,
        "a no-op must not count as a saved edit"
    );
    fixture
        .edit("set_typography", json!({"element":layer,"font_weight":400}))
        .await?;
    let mut expected = after["typography"].clone();
    expected["font_weight"] = json!(400);
    assert_eq!(
        fixture.layer_view()?["typography"],
        expected,
        "a weight-only patch must preserve every other setting"
    );
    fixture.project.undo().await?;
    assert_eq!(fixture.layer_view()?["typography"], after["typography"]);

    for properties in [
        json!({"font_weight":"bold"}),
        json!({"font_weight":0}),
        json!({"font_weight":1001}),
        json!({"font_style":"bold"}),
        json!({"size":0}),
        json!({"size":null,"auto_fit":false}),
        json!({"stroke_width":-1}),
        json!({"color":"#000000"}),
        json!({"color":[256,0,0,255]}),
        json!({"alignment":"left"}),
        json!({"writing_mode":"diagonal"}),
        json!({"unknown":true}),
    ] {
        let mut args = properties;
        args["element"] = json!(layer);
        let revision = fixture.project.revision();
        let before = fixture.layer_view()?;
        assert!(fixture.edit("set_typography", args).await.is_err());
        assert_eq!(fixture.project.revision(), revision);
        assert_eq!(fixture.layer_view()?, before);
    }
    let points = json!([{"x":40.0,"y":60.0},{"x":280.0,"y":60.0},{"x":280.0,"y":190.0},{"x":40.0,"y":190.0}]);
    fixture
        .edit("set_geometry", json!({"element":layer,"points":points}))
        .await?;
    assert_eq!(fixture.layer_view()?["geometry"]["points"], points);
    assert!(
        fixture
            .edit("set_geometry", json!({"element":layer,"points":null}))
            .await
            .is_err(),
        "manual frames cannot reset to automatic"
    );
    let patch = fixture.project.snapshot().patch(|edit| {
        let geometry = koharu_scene::Geometry {
            origin: koharu_scene::Origin::User,
            points: vec![
                koharu_scene::Point { x: 40.0, y: 60.0 },
                koharu_scene::Point { x: 280.0, y: 60.0 },
                koharu_scene::Point { x: 280.0, y: 190.0 },
                koharu_scene::Point { x: 40.0, y: 190.0 },
            ],
        };
        let region = edit.add_analysis_region::<koharu_scene::TextRegion>(
            fixture.page,
            At::End,
            &geometry,
            None,
        )?;
        edit.relate::<koharu_scene::FitsTo>(layer, region)?;
        Ok(())
    })?;
    fixture.project.session.commit(patch).await?;
    fixture
        .edit("set_geometry", json!({"element":layer,"points":null}))
        .await?;
    assert!(fixture.layer_view()?["geometry"].is_null());
    assert!(!fixture.layer_view()?["automatic_region"].is_null());
    fixture
        .edit(
            "set_visibility",
            json!({"elements":[layer],"visible":false,"opacity":0.4}),
        )
        .await?;
    assert_eq!(
        fixture.layer_view()?["visibility"],
        json!({"visible":false,"opacity":0.4f32})
    );
    fixture
        .edit("set_visibility", json!({"elements":[layer],"visible":true}))
        .await?;
    assert_eq!(
        fixture.layer_view()?["visibility"],
        json!({"visible":true,"opacity":0.4f32})
    );
    let revision = fixture.project.revision();
    assert!(
        fixture
            .edit("set_visibility", json!({"elements":[layer],"opacity":50}))
            .await
            .is_err()
    );
    assert_eq!(fixture.project.revision(), revision);
    assert!(
        fixture
            .edit(
                "set_visibility",
                json!({"elements":[fixture.other],"visible":false})
            )
            .await
            .is_err()
    );

    // All enum choices and explicit clearing travel through the exact same command parser.
    for style in ["normal", "italic", "oblique"] {
        for alignment in ["Start", "Center", "End", "Justify"] {
            for direction in ["Horizontal", "Vertical"] {
                fixture.edit("set_typography",json!({"element":layer,"font_style":style,"alignment":alignment,"writing_mode":direction})).await?;
                let value = fixture.layer_view()?;
                assert_eq!(value["typography"]["font_style"], style);
                assert_eq!(value["typography"]["alignment"], alignment);
                assert_eq!(value["typography"]["writing_mode"], direction);
            }
        }
    }
    fixture.edit("set_typography",json!({"element":layer,"preferred_font":null,"font_weight":null,"font_style":null,"size":null,"auto_fit":true,"color":null,"stroke_color":null,"stroke_width":null,"alignment":null,"writing_mode":null})).await?;
    let cleared = fixture.layer_view()?;
    for key in [
        "preferred_font",
        "font_weight",
        "font_style",
        "size",
        "color",
        "stroke_color",
        "stroke_width",
        "alignment",
        "writing_mode",
    ] {
        assert!(cleared["typography"][key].is_null(), "{key}");
    }
    assert_eq!(cleared["typography"]["auto_fit"], true);
    assert_eq!(before["content"], cleared["content"]);

    let path = fixture.directory.path().join("fixture.khrproj");
    let persisted = fixture.layer_view()?;
    drop(fixture.project);
    fixture.project = Project::open("native editor acceptance".into(), path).await?;
    assert_eq!(
        fixture.layer_view()?,
        persisted,
        "read durable settings back after reopening the isolated project"
    );
    assert_eq!(
        context::Locations::new(&fixture.project.snapshot())?.label(fixture.page)?,
        "Chapter 1 · 3/3 · 003"
    );
    Ok(())
}

#[tokio::test]
async fn native_layer_tree_and_text_commands_apply_and_undo() -> Result<()> {
    let mut f = Fixture::new().await?;
    let layer = f.layer;
    f.edit(
        "set_source_text",
        json!({"element":layer,"text":"New source"}),
    )
    .await?;
    f.edit(
        "set_translation",
        json!({"element":layer,"text":"Chữ dễ đọc\nhơn"}),
    )
    .await?;
    assert_eq!(f.layer_view()?["content"]["source"]["text"], "New source");
    assert_eq!(
        f.layer_view()?["content"]["translation"]["text"],
        "Chữ dễ đọc\nhơn"
    );
    let added = f
        .edit(
            "add_text_box",
            json!({"page":f.page,"x":300,"y":200,"width":100,"height":80,"angle_degrees":15}),
        )
        .await?;
    let second = entity(added.value["result"]["element"].as_str().unwrap())?;
    let parent = f.project.snapshot().parent(layer)?.context("parent")?;
    f.edit(
        "reorder_element",
        json!({"element":second,"parent":parent,"index":0}),
    )
    .await?;
    assert_eq!(f.project.snapshot().children(parent)?.next(), Some(second));
    let saved = serde_json::to_value(Project::page(&f.project.snapshot(), f.page)?)?;
    f.edit("delete_elements", json!({"elements":[second]}))
        .await?;
    assert!(f.project.snapshot().entity(second).is_err());
    f.project.undo().await?;
    assert_eq!(
        serde_json::to_value(Project::page(&f.project.snapshot(), f.page)?)?,
        saved
    );
    f.edit("set_translation", json!({"element":layer,"text":null}))
        .await?;
    assert!(f.layer_view()?["content"]["translation"].is_null());
    Ok(())
}

#[derive(Clone)]
struct NativeHost {
    project: Arc<tokio::sync::Mutex<Project>>,
    binding: Option<Binding>,
    renderer: koharu_renderer::Renderer,
    calls: Arc<parking_lot::Mutex<Vec<Value>>>,
    argument_faults: Arc<parking_lot::Mutex<Vec<&'static str>>>,
}

#[tokio::test]
async fn declined_dialogue_removal_does_not_poison_the_readability_run() -> Result<()> {
    let f = Fixture::new().await?;
    let snapshot = f.project.snapshot();
    let revision = snapshot.revision();
    let stamp = readability::stamp(&snapshot, f.page)?;
    let mut review = readability::Review::default();
    review.begin(&[f.page])?;
    review.audit(
        stamp,
        koharu_renderer::ReadabilityReport {
            page: f.page,
            page_width: 1000,
            page_height: 1600,
            reader_widths: vec![390, 960],
            minimum_font_px: 12.0,
            layers: vec![],
            issues: vec![],
        },
    );
    review.viewed(f.page, stamp, None);
    let mut ledger = Failures::default();
    for (tool, arguments) in [
        ("delete_elements", json!({"elements":[f.layer]})),
        (
            "set_visibility",
            json!({"elements":[f.layer],"visible":false}),
        ),
    ] {
        let attempt = Attempt::parse(call(tool, arguments))?;
        let edit = LayerEdit::parse(&attempt.call)?.unwrap();
        let target = Target::capture(&snapshot, &attempt.call)?;
        let rejected = edit.validate_review(&snapshot, &review);
        assert!(rejected.is_err(), "ordinary dialogue must remain protected");
        let feedback = ledger.finish(
            &attempt,
            target,
            rejected.and_then(|_| anyhow::bail!("an unsafe edit must never reach the commit seam")),
        );
        assert!(
            feedback.is_ok(),
            "a rejected visual proposal must be actionable feedback, not require successfully deleting dialogue to clear the failure: {feedback:?}"
        );
        let feedback = feedback?;
        assert!(!feedback.changed);
        assert_eq!(snapshot.revision(), revision);
        assert!(snapshot.text_layer(f.layer).is_ok());
    }
    ledger.ensure_complete()?;
    review.verify(f.page, stamp, vec![])?;
    review.ensure_complete()?;
    Ok(())
}

#[tokio::test]
async fn original_ocr_duplicates_survive_translation_and_layout_differences_but_keep_one_copy()
-> Result<()> {
    use koharu_scene::{
        Authored, BubbleRegion, FlowsIn, Geometry, Origin, RecognizedFrom, SourceText, TextLayout,
        TextLayoutKind, TextRegion, Translation,
    };
    let mut f = Fixture::new().await?;
    let geometry = |x: f64| Geometry {
        origin: Origin::User,
        points: vec![
            koharu_scene::Point { x, y: 100.0 },
            koharu_scene::Point {
                x: x + 100.0,
                y: 100.0,
            },
            koharu_scene::Point {
                x: x + 100.0,
                y: 160.0,
            },
            koharu_scene::Point { x, y: 160.0 },
        ],
    };
    let mut layers = vec![];
    let patch = f.project.snapshot().patch(|edit| {
        let balloon =
            edit.add_analysis_region::<BubbleRegion>(f.page, At::End, &geometry(100.0), None)?;
        let other_balloon =
            edit.add_analysis_region::<BubbleRegion>(f.page, At::End, &geometry(100.0), None)?;
        for (i, (source, x, other)) in [
            ("Did you hear my conversation just now?", 100.0, false),
            ("Did you hear my conversation just now?", 102.0, false),
            ("Did you hear my conversation just now?", 100.0, true),
            ("Did you hear my conversation just now?", 400.0, false),
            ("A different sentence in the same balloon.", 100.0, false),
            ("Yes!", 100.0, false),
            ("Yes!", 100.0, false),
        ]
        .into_iter()
        .enumerate()
        {
            let region =
                edit.add_analysis_region::<TextRegion>(f.page, At::End, &geometry(x), None)?;
            let content = edit.add_text_content(f.page, At::End)?;
            edit.set(
                content,
                &SourceText {
                    text: Authored::user(source.into()),
                    language: None,
                },
            )?;
            edit.set(
                content,
                &Translation {
                    text: Authored::user(format!("different translation {i}")),
                    language: None,
                },
            )?;
            edit.relate::<RecognizedFrom>(content, region)?;
            let layer = edit.add_text_layer(
                f.page,
                At::End,
                content,
                &TextLayout {
                    origin: Origin::User,
                    kind: TextLayoutKind::Paragraph,
                    angle_degrees: None,
                },
            )?;
            edit.relate::<FlowsIn>(layer, if other { other_balloon } else { balloon })?;
            layers.push(layer);
        }
        Ok(())
    })?;
    let snapshot = f.project.session.commit(patch).await?.snapshot;
    let mut report = koharu_renderer::ReadabilityReport {
        page: f.page,
        page_width: 1000,
        page_height: 1600,
        reader_widths: vec![390, 960],
        minimum_font_px: 12.0,
        layers: layers
            .iter()
            .enumerate()
            .map(|(i, id)| koharu_renderer::ReadableText {
                element: *id,
                text: format!("different translation {i}"),
                source_font_px: 9.0,
                display_font_px: vec![3.51, 8.64],
                fonts: vec![],
                bounds: [i as f32 * 150.0, 0.0, 100.0, 20.0],
                layout_bounds: [i as f32 * 150.0, 0.0, 100.0, 20.0],
                color: [0, 0, 0, 255],
                opacity: 1.0,
                stroke_width: 0.0,
                stroke_color: None,
                background: None,
            })
            .collect(),
        issues: vec![],
    };
    readability::source_duplicates(&snapshot, &mut report)?;
    assert_eq!(
        report.issues.len(),
        1,
        "Different balloons, nonoverlapping source regions, different source words and repeated short utterances are not duplicate proof"
    );
    assert_eq!(report.issues[0].element, layers[0]);
    assert_eq!(report.issues[0].related, Some(layers[1]));
    let stamp = readability::stamp(&snapshot, f.page)?;
    let mut review = readability::Review::default();
    review.begin(&[f.page])?;
    review.audit(stamp, report.clone());
    review.viewed(f.page, stamp, None);
    LayerEdit::Delete(vec![layers[0]]).validate_review(&snapshot, &review)?;
    assert!(
        LayerEdit::Delete(vec![layers[0], layers[1]])
            .validate_review(&snapshot, &review)
            .is_err()
    );
    assert!(
        LayerEdit::Delete(vec![layers[4]])
            .validate_review(&snapshot, &review)
            .is_err()
    );
    let (_, value) = LayerEdit::Delete(vec![layers[0]])
        .apply(&mut f.project)
        .await?;
    let _ = value;
    assert!(f.project.snapshot().text_layer(layers[1]).is_ok());
    Ok(())
}

#[tokio::test]
async fn native_failed_layer_is_not_cleared_by_other_page_edits_or_wrong_target_retry() -> Result<()>
{
    let mut f = Fixture::new().await?;
    let (_, other_layer) = f
        .project
        .add_text_box(
            f.other,
            Frame {
                x: 10.0,
                y: 10.0,
                width: 100.0,
                height: 100.0,
                angle_degrees: 0.0,
            },
        )
        .await?;
    let _directory = f.directory;
    let project = Arc::new(tokio::sync::Mutex::new(f.project));
    let raw = NativeHost {
        project: project.clone(),
        binding: None,
        renderer: koharu_renderer::Renderer::new()?,
        calls: Default::default(),
        argument_faults: Default::default(),
    };
    let control = Control::default();
    let host = raw.bind(RunId::new(), control.clone()).await?;
    host.context().await?;
    host.invoke(
        call(
            "set_edit_scope",
            json!({"first_page":f.other,"last_page":f.page}),
        ),
        &control,
    )
    .await?;
    let error = host
        .invoke(
            call(
                "set_typography",
                json!({"element":f.layer,"font_weight":"bold"}),
            ),
            &control,
        )
        .await
        .unwrap_err();
    let id = error
        .downcast_ref::<koharu_agent::ToolRejection>()
        .context("native rejection ID")?
        .failure_id
        .clone();
    host.invoke(call("inspect_page", json!({"page":f.other})), &control)
        .await?;
    host.invoke(
        call(
            "set_typography",
            json!({"element":other_layer,"size":28,"auto_fit":false}),
        ),
        &control,
    )
    .await?;
    assert!(host.ensure_complete().await.is_err());
    let before = project.lock().await.revision();
    assert!(
        host.invoke(
            call(
                "set_typography",
                json!({"element":other_layer,"font_weight":700,"retry_of":id})
            ),
            &control
        )
        .await
        .is_err()
    );
    assert_eq!(
        project.lock().await.revision(),
        before,
        "wrong-target retry must not reach the native mutation"
    );
    host.invoke(
        call(
            "set_typography",
            json!({"element":f.layer,"font_weight":700}),
        ),
        &control,
    )
    .await?;
    assert!(
        host.ensure_complete().await.is_err(),
        "unlinked successful calls cannot erase a failed request"
    );
    let retry = host
        .invoke(
            call(
                "set_typography",
                json!({"element":f.layer,"font_weight":700,"retry_of":id}),
            ),
            &control,
        )
        .await?;
    assert!(
        !retry.changed,
        "a confirmed no-op can resolve a retry without inflating edits"
    );
    host.ensure_complete().await?;
    Ok(())
}

#[async_trait]
impl Host for NativeHost {
    async fn bind(&self, run: RunId, control: Control) -> Result<Arc<dyn Host>> {
        let project = self.project.lock().await;
        let binding = Binding {
            instance: project.instance_id,
            run,
            control,
            observed: Arc::new(parking_lot::Mutex::new(None)),
            pages: Arc::new(parking_lot::Mutex::new(None)),
            failures: Arc::default(),
            review: Arc::default(),
        };
        Ok(Arc::new(Self {
            binding: Some(binding),
            ..self.clone()
        }))
    }

    async fn context(&self) -> Result<Value> {
        let project = self.project.lock().await;
        *self.binding.as_ref().unwrap().observed.lock() = Some(project.revision());
        context::overview(&project, json!({}), &Window::default())
    }

    fn tools(&self) -> Vec<Tool> {
        editor_tools()
    }

    async fn invoke(&self, call: ToolCall, control: &Control) -> Result<Invocation> {
        let binding = self.binding.as_ref().context("bound test host")?;
        let (attempt, parse_error) = match Attempt::parse(call.clone()) {
            Ok(attempt) => (attempt, None),
            Err(error) => (Attempt::raw(call), Some(error)),
        };
        let target = Target::capture(&self.project.lock().await.snapshot(), &attempt.call)?;
        let ready = match parse_error {
            Some(error) => Err(error),
            None => binding.failures.lock().validate(&attempt, &target),
        };
        let result = match ready {
            Ok(()) => self.execute_tool(attempt.call.clone(), control).await,
            Err(error) => Err(error),
        };
        let (result, exhausted) = {
            let mut failures = binding.failures.lock();
            let result = failures.finish(&attempt, target, result);
            (result, failures.exhausted())
        };
        if exhausted {
            binding.control.cancel();
        }
        result
    }

    async fn ensure_complete(&self) -> Result<()> {
        let binding = self.binding.as_ref().context("bound test host")?;
        binding.failures.lock().ensure_complete()?;
        let project = self.project.lock().await;
        let review = binding.review.lock();
        review.ensure_complete()?;
        review.ensure_fresh(&project.snapshot())
    }
}

impl NativeHost {
    async fn execute_tool(&self, call: ToolCall, control: &Control) -> Result<Invocation> {
        anyhow::ensure!(!control.is_cancelled(), "Agent request was cancelled");
        let inject = {
            let mut faults = self.argument_faults.lock();
            faults
                .iter()
                .position(|name| *name == call.name)
                .map(|index| faults.remove(index))
        };
        if let Some(name) = inject {
            let error = match name {
                "inspect_chapter" => {
                    arguments::<InspectChapter>(&self::call(name, json!({"chapter_id":"fixture"})))
                        .err()
                }
                "set_edit_scope" => arguments::<EditRange>(&self::call(name, json!({}))).err(),
                _ => unreachable!(),
            }
            .context("expected native validation rejection")?;
            bail!(
                "Controlled native validation fault in this isolated fixture; the submitted call was not executed. Retry this same tool using its input_schema. Sample rejected payload error: {error:#}"
            );
        }
        let mut project = self.project.lock().await;
        let binding = self.binding.as_ref().context("bound test host")?;
        binding.check(&project)?;
        let result = if let Some(edit) = LayerEdit::parse(&call)? {
            binding.check_revision(&project)?;
            scope::check(
                &project.snapshot(),
                binding.pages.lock().as_deref(),
                &edit.targets(),
            )?;
            let snapshot = project.snapshot();
            edit.validate_review(&snapshot, &binding.review.lock())?;
            let pages = edit
                .targets()
                .iter()
                .map(|id| scope::page_for(&snapshot, *id))
                .collect::<Result<std::collections::HashSet<_>>>()?;
            let mut baseline = None;
            for page in &pages {
                baseline = binding
                    .review
                    .lock()
                    .before_edit(*page, readability::stamp(&snapshot, *page)?)?;
            }
            let (commit, value) = if let Some(before) = baseline {
                if let Some((patch, value)) = edit.preview_patch(&snapshot)? {
                    let candidate = readability::checked_candidate(
                        &self.renderer,
                        Arc::new(koharu_rasterizer::Rasterizer::new()?),
                        &snapshot,
                        &patch,
                        before.page,
                        &before,
                    )
                    .await;
                    match candidate {
                        Ok(_) => {}
                        Err(error) if error.is::<readability::CandidateRegression>() => {
                            return Invocation::read(
                                json!({"page":before.page,"changed":false,"candidate_rejected":format!("{error}"),"remaining_issues":before.issues,"next":"Inspect a crop and propose a different layout, or verify remaining issues as needs_review."}),
                            );
                        }
                        Err(error) => return Err(error),
                    }
                    (project.commit(patch).await?, value)
                } else {
                    edit.apply(&mut project).await?
                }
            } else {
                edit.apply(&mut project).await?
            };
            if commit.changes.from != commit.changes.to {
                for page in pages {
                    binding.review.lock().changed(page);
                }
            }
            project.record_commit(&commit);
            mutation_result(&project, &commit, value)?
        } else {
            match call.name.as_str() {
                "inspect_project" => {
                    Invocation::read(context::overview(&project, json!({}), &arguments(&call)?)?)?
                }
                "inspect_chapters" => {
                    Invocation::read(context::chapters(&project.snapshot(), &arguments(&call)?)?)?
                }
                "inspect_chapter" => {
                    Invocation::read(context::chapter(&project.snapshot(), &arguments(&call)?)?)?
                }
                "inspect_page" => {
                    let a: InspectPage = arguments(&call)?;
                    Invocation::read(context::page(
                        &project.snapshot(),
                        entity(&a.page)?,
                        a.offset,
                        a.limit,
                    )?)?
                }
                "set_edit_scope" => {
                    let range: EditRange = arguments(&call)?;
                    let pages = range.resolve(&project.snapshot())?;
                    let count = pages.len();
                    if range.readability {
                        binding.review.lock().begin(&pages)?;
                    }
                    *binding.pages.lock() = Some(pages);
                    Invocation::read(json!({"page_count":count}))?
                }
                "list_fonts" => Invocation::read(context::fonts(
                    self.renderer.available_fonts().await?,
                    &arguments(&call)?,
                )?)?,
                "audit_readability" => {
                    let a: InspectPage = arguments(&call)?;
                    context::validate(a.limit)?;
                    let page = entity(&a.page)?;
                    let scope = binding.pages.lock().clone().context("choose scope")?;
                    scope::check(&project.snapshot(), Some(&scope), &[page])?;
                    binding.review.lock().begin(&scope)?;
                    let frame = self.renderer.render(&project.snapshot(), page).await?;
                    let mut report = frame.readability();
                    readability::source_duplicates(&project.snapshot(), &mut report)?;
                    let background = output::frame_image(
                        Arc::new(koharu_rasterizer::Rasterizer::new()?),
                        &frame.without_text()?,
                    )
                    .await?;
                    report.sample_background(&background);
                    let entries=report.layers.iter().skip(a.offset).take(a.limit).map(|l|json!({"layer":l,"issues":report.issues.iter().filter(|i|i.element==l.element).collect::<Vec<_>>()})).collect();
                    let response = context::window(
                        json!({"page":context::location(&project.snapshot(),page)?,"reader_widths":report.reader_widths,"minimum_font_px":report.minimum_font_px,"issue_count":report.issues.len()}),
                        "entries",
                        entries,
                        report.layers.len(),
                        a.offset,
                    )?;
                    binding
                        .review
                        .lock()
                        .audit(readability::stamp(&project.snapshot(), page)?, report);
                    Invocation::read(response)?
                }
                "repair_text" => {
                    let a: RepairText = arguments(&call)?;
                    let page = entity(&a.page)?;
                    let elements = entities(&a.elements)?;
                    scope::check(
                        &project.snapshot(),
                        binding.pages.lock().as_deref(),
                        &[page],
                    )?;
                    binding.check_revision(&project)?;
                    let snapshot = project.snapshot();
                    let before = binding
                        .review
                        .lock()
                        .before_edit(page, readability::stamp(&snapshot, page)?)?
                        .context("audit and view first")?;
                    let batch = readability::repair_batch(
                        &self.renderer,
                        Arc::new(koharu_rasterizer::Rasterizer::new()?),
                        &snapshot,
                        &elements,
                        &before,
                    )
                    .await?;
                    let commit = project.commit(batch.patch).await?;
                    project.record_commit(&commit);
                    if commit.changes.from != commit.changes.to {
                        binding.review.lock().changed(page);
                    }
                    mutation_result(
                        &project,
                        &commit,
                        json!({"page":page,"elements":batch.elements,"remaining_issues":batch.report.issues,"declined":batch.declined}),
                    )?
                }
                "verify_readability" => {
                    let a: VerifyReadability = arguments(&call)?;
                    let page = entity(&a.page)?;
                    binding.review.lock().verify(
                        page,
                        readability::stamp(&project.snapshot(), page)?,
                        a.issues,
                    )?;
                    Invocation::read(json!({"coverage":binding.review.lock().progress()}))?
                }
                "view_page" => {
                    let a: ViewPage = arguments(&call)?;
                    let page = entity(&a.page)?;
                    let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
                    let frame = self.renderer.render(&project.snapshot(), page).await?;
                    let review_report = if binding.review.lock().active() {
                        let mut report = frame.readability();
                        readability::source_duplicates(&project.snapshot(), &mut report)?;
                        let background =
                            output::frame_image(rasterizer.clone(), &frame.without_text()?).await?;
                        report.sample_background(&background);
                        let summary = json!({"issues":report.issues,"font_sizes":report.layers.iter().map(|l|json!({"element":l.element,"source_font_px":l.source_font_px,"display_font_px":l.display_font_px})).collect::<Vec<_>>()});
                        let in_scope = binding
                            .pages
                            .lock()
                            .as_ref()
                            .is_some_and(|pages| pages.contains(&page));
                        if in_scope {
                            binding
                                .review
                                .lock()
                                .audit(readability::stamp(&project.snapshot(), page)?, report);
                        }
                        Some(summary)
                    } else {
                        None
                    };
                    let crop = a
                        .element
                        .as_deref()
                        .map(entity)
                        .transpose()?
                        .map(|id| {
                            let b = frame.layer(id).context("rendered layer")?.bounds();
                            Ok::<_, anyhow::Error>([
                                b.x - 32.0,
                                b.y - 32.0,
                                b.width + 64.0,
                                b.height + 64.0,
                            ])
                        })
                        .transpose()?;
                    let dual = crop.is_none()
                        && a.display_width.is_none()
                        && binding.review.lock().active();
                    let bytes = output::frame_preview(
                        rasterizer.clone(),
                        &frame,
                        crop,
                        if dual { Some(960) } else { a.display_width },
                    )
                    .await?;
                    let mut output =
                        Invocation::read(json!({"page":context::location(&project.snapshot(), page)?,"readability":review_report}))?
                            .with_image(
                                "Native rendered fixture",
                                format!("data:image/webp;base64,{}", STANDARD.encode(bytes)),
                            );
                    if dual {
                        output = output.with_image(
                            "Phone view, 390px",
                            format!(
                                "data:image/webp;base64,{}",
                                STANDARD.encode(
                                    output::frame_preview(rasterizer, &frame, None, Some(390))
                                        .await?
                                )
                            ),
                        );
                    }
                    if crop.is_none() {
                        binding.review.lock().viewed(
                            page,
                            readability::stamp(&project.snapshot(), page)?,
                            if dual { None } else { a.display_width },
                        );
                    }
                    output
                }
                _ => bail!(
                    "This property acceptance fixture does not allow {}",
                    call.name
                ),
            }
        };
        *binding.observed.lock() = Some(project.revision());
        self.calls.lock().push(json!({"tool":call.name,"arguments":serde_json::from_str::<Value>(&call.arguments)?,"changed":result.changed,"coverage":binding.review.lock().progress()}));
        Ok(result)
    }
}

#[tokio::test]
#[ignore = "Manual installed-agent readability integration on explicitly selected detached real manga pages"]
async fn installed_agent_reviews_readability_with_native_measurements_and_verification()
-> Result<()> {
    let path =
        std::path::PathBuf::from(std::env::var("KOHARU_READABILITY_SAMPLE")?).canonicalize()?;
    anyhow::ensure!(
        path.starts_with(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../temp")
                .canonicalize()?
        ),
        "only a detached temp copy may be edited"
    );
    koharu_runtime::Store::configure(std::path::PathBuf::from(std::env::var(
        "KOHARU_NATIVE_ACCEPTANCE_STORE",
    )?))?;
    let project = Arc::new(tokio::sync::Mutex::new(
        Project::open("Detached readability integration".into(), path).await?,
    ));
    let before = project.lock().await.snapshot();
    let positions: Vec<[usize; 2]> =
        serde_json::from_str(&std::env::var("KOHARU_READABILITY_POSITIONS")?)?;
    let chapters = before.chapters()?;
    let pages = positions
        .iter()
        .map(|[chapter, page]| {
            chapters
                .entries
                .get(chapter.saturating_sub(1))
                .and_then(|c| c.pages.get(page.saturating_sub(1)))
                .copied()
                .context("requested chapter/page")
        })
        .collect::<Result<Vec<_>>>()?;
    anyhow::ensure!(!pages.is_empty(), "choose at least one page");
    let renderer = koharu_renderer::Renderer::new()?;
    let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let host = NativeHost {
        project: project.clone(),
        binding: None,
        renderer: renderer.clone(),
        calls: calls.clone(),
        argument_faults: Arc::default(),
    };
    let agent = koharu_agent::Agent::new(koharu_agent::Codex::new()?, host)?;
    let control = Control::default();
    let prompt = format!(
        "Sửa chữ nhỏ, chữ chồng và viền quá dày để dễ đọc trên điện thoại và desktop. CHỈ làm các vị trí chapter/trang (1-based): {:?}, tức phạm vi inclusive first_page={} last_page={}. Đây là bản sao thử, không sửa ngoài phạm vi. Giữ nguyên lời thoại và hình vẽ; chỉ được bỏ một bản OCR trùng đã được audit xác nhận và còn một bản hiển thị. Không chạy lại dịch/OCR/pipeline. Dùng audit_readability, ảnh toàn trang và ảnh cận cảnh để chọn cách sửa phù hợp từng lớp, ưu tiên repair_text theo batch; nới khung trong vùng bóng thoại hợp lệ khi chữ vẫn quá nhỏ. Kiểm tra lại cả hai kích thước sau sửa và verify_readability từng trang. Vấn đề không sửa an toàn được phải đánh dấu needs_review và nói rõ. Làm trực tiếp bằng tool, không chỉ trả lời hướng dẫn.",
        positions,
        pages[0],
        pages[pages.len() - 1]
    );
    let mut events = Vec::new();
    let task = agent.run(RunId::new(), prompt, control.clone(), |event| {
        if let koharu_agent::Event::ToolFinished { name, changed, .. } = &event {
            println!("Editor tool {name}, saved={changed}");
        }
        events.push(event);
    });
    let result = match tokio::time::timeout(std::time::Duration::from_secs(900), task).await {
        Ok(result) => result,
        Err(_) => {
            control.cancel();
            agent.clear().await;
            Err(anyhow!("isolated agent review timed out"))
        }
    };
    agent.clear().await;
    let after = project.lock().await.snapshot();
    for page in &pages {
        let mut report = renderer.render(&before, *page).await?.readability();
        readability::source_duplicates(&before, &mut report)?;
        for entity in before.descendants(*page)? {
            if let Ok(layer) = before.text_layer(entity.id()) {
                let Ok(new) = after.text_layer(entity.id()) else {
                    anyhow::ensure!(
                        report.issues.iter().any(|issue| {
                            if issue.kind != koharu_renderer::ReadabilityKind::DuplicateCandidate {
                                return false;
                            }
                            let survivor = if issue.element == entity.id() {
                                issue.related
                            } else if issue.related == Some(entity.id()) {
                                Some(issue.element)
                            } else {
                                None
                            };
                            survivor.is_some_and(|id| after.text_layer(id).is_ok())
                        }),
                        "agent deleted dialogue without a surviving audited copy"
                    );
                    continue;
                };
                anyhow::ensure!(
                    layer.content()?.translation()?.map(|v| v
                        .text
                        .value
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "))
                        == new.content()?.translation()?.map(|v| v
                            .text
                            .value
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")),
                    "agent changed dialogue wording"
                );
            }
        }
    }
    let calls = calls.lock();
    let output = std::path::PathBuf::from(std::env::var("KOHARU_READABILITY_OUTPUT")?);
    std::fs::create_dir_all(&output)?;
    std::fs::write(
        output.join("installed-agent-review.json"),
        serde_json::to_vec_pretty(
            &json!({"backend":agent.config()?.backend,"model":agent.config()?.pi.model,"calls":*calls,"errors":events.iter().filter_map(|event| if let koharu_agent::Event::ToolFinished{name,output,..}=event { let value=serde_json::from_str::<Value>(output).ok()?; (value["ok"]==false).then(||json!({"tool":name,"feedback":value})) } else { None }).collect::<Vec<_>>(),"result":result.as_ref().map(|r|&r.message).map_err(|e|format!("{e:#}"))}),
        )?,
    )?;
    let result = result?;
    anyhow::ensure!(
        calls.last().is_some_and(
            |c| c["coverage"]["checked"].as_u64() == Some(pages.len() as u64)
                && c["coverage"]["total"].as_u64() == Some(pages.len() as u64)
        ),
        "agent did not verify every scoped page"
    );
    anyhow::ensure!(
        calls.iter().any(|c| c["changed"] == true),
        "agent made no saved edit"
    );
    println!(
        "Installed agent finished measured, rendered and verified native pages: {}",
        result.message
    );
    Ok(())
}

#[tokio::test]
#[ignore = "Manual installed AGY/Gemini acceptance on an isolated native project, reads saved agent setup"]
async fn installed_agy_applies_real_native_layer_settings() -> Result<()> {
    let store = std::env::var("KOHARU_NATIVE_ACCEPTANCE_STORE")
        .context("provide an isolated copy of the font package cache for this manual test")?;
    koharu_runtime::Store::configure(std::path::PathBuf::from(store))?;
    let recovery = std::env::var_os("KOHARU_NATIVE_ARGUMENT_RECOVERY").is_some();
    let page_number = if recovery { 16 } else { 3 };
    let f = Fixture::with_chapter_pages(page_number).await?;
    let renderer = koharu_renderer::Renderer::new()?;
    let families = renderer.available_fonts().await?;
    let family = families
        .iter()
        .find(|font| font.name == "CCWildWords")
        .context("copy the app's cached CCWildWords faces into the isolated font store")?;
    let face = family
        .faces
        .iter()
        .find(|face| face.weight == 700 && face.style == koharu_renderer::FontStyle::Normal)
        .or_else(|| family.faces.first())
        .context("font face")?;
    let style = serde_json::to_value(face.style)?;
    let expected = json!({"preferred_font":family.name,"font_weight":face.weight,"font_style":style,
        "size":32.0,"auto_fit":false,"color":[10,20,30,255],"stroke_color":[240,241,242,220],
        "stroke_width":1.5,"alignment":"Center","writing_mode":"Horizontal"});
    let page = f.page;
    let layer = f.layer;
    let last_page = if recovery {
        f.project.snapshot().pages().last().unwrap().id()
    } else {
        page
    };
    let _directory = f.directory;
    let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let project = Arc::new(tokio::sync::Mutex::new(f.project));
    let host = NativeHost {
        project: project.clone(),
        binding: None,
        renderer: renderer.clone(),
        calls: calls.clone(),
        argument_faults: Arc::new(parking_lot::Mutex::new(if recovery {
            vec!["inspect_chapter", "set_edit_scope"]
        } else {
            vec![]
        })),
    };
    let agent = koharu_agent::Agent::new(koharu_agent::Codex::new()?, host)?;
    let config = agent.config()?;
    anyhow::ensure!(
        config.backend == koharu_agent::Backend::Agy,
        "select AGY in the existing app setup before this manual test"
    );
    anyhow::ensure!(
        config
            .agy
            .model
            .as_deref()
            .is_some_and(|model| model.contains("flash")),
        "select the intended AGY Flash model before this manual test"
    );
    let prompt = format!(
        "Native editor property acceptance test on an ISOLATED temporary project. Resolve Chapter 1 page {page_number} through inspect_chapters/inspect_chapter; select the inclusive edit scope from that page to page ID {last_page}. Inspect its existing text layer and edit only that layer; other pages are blank. Apply exactly these typography properties: {expected}. Set that layer's geometry to page-space points [{{\"x\":40,\"y\":60}},{{\"x\":280,\"y\":60}},{{\"x\":280,\"y\":190}},{{\"x\":40,\"y\":190}}]. Set visibility=true and opacity=0.4. Preserve all original words and all other pages. Use the real layer tools, no pipeline. Inspect the page before and after, including its native rendered image, and verify the saved properties. You must call view_page again AFTER the last edit, not just twice before editing. Reply briefly in Vietnamese."
    );
    let prompt = if recovery {
        format!(
            "This is an explicit tool-error recovery test in a disposable fixture. You must call inspect_chapters, then inspect_chapter to resolve Chapter 1 page 16 even when the overview already contains its ID; next call set_edit_scope. The test host will inject ONE controlled native validation error on the first inspect_chapter and on the first set_edit_scope call. No mutation occurs for those rejected attempts. Read each returned error and input_schema and retry the same tool successfully before proceeding. Always send valid arguments obtained from inspection, do not simulate errors or stop at a warning. After both recoveries, complete this task: {prompt}"
        )
    } else {
        prompt
    };
    let control = Control::default();
    let mut events = Vec::new();
    let future = agent.run(RunId::new(), prompt, control.clone(), |event| {
        events.push(event)
    });
    let result = match tokio::time::timeout(std::time::Duration::from_secs(180), future).await {
        Ok(result) => result?,
        Err(_) => {
            control.cancel();
            agent.clear().await;
            bail!("AGY native acceptance timed out");
        }
    };
    agent.clear().await;
    let mut recoveries = Vec::new();
    if recovery {
        for tool in ["inspect_chapter", "set_edit_scope"] {
            let rejected = events.iter().position(|event| matches!(event,
                koharu_agent::Event::ToolFinished { name, changed: false, output, .. }
                if name == tool && serde_json::from_str::<Value>(output).is_ok_and(|value|value["ok"] == false)))
                .with_context(||format!("model did not reproduce the rejected {tool} call"))?;
            let koharu_agent::Event::ToolFinished { output, .. } = &events[rejected] else {
                unreachable!()
            };
            let feedback: Value = serde_json::from_str(output)?;
            assert_eq!(feedback["tool"], tool);
            assert!(feedback["input_schema"]["required"].is_array());
            if tool == "inspect_chapter" {
                assert!(feedback["error"].as_str().unwrap().contains("chapter_id"));
                assert!(feedback["received_arguments"]["chapter"].is_string());
            } else {
                assert!(feedback["error"].as_str().unwrap().contains("first_page"));
                assert!(feedback["received_arguments"]["first_page"].is_string());
                assert!(feedback["received_arguments"]["last_page"].is_string());
            }
            let success = events.iter().enumerate().skip(rejected + 1).find(|(_,event)| matches!(event,
                koharu_agent::Event::ToolFinished { name, changed: false, output, .. }
                if name == tool && serde_json::from_str::<Value>(output).is_ok_and(|value|value["ok"] != false)))
                .map(|(index,_)| index).with_context(||format!("no corrected {tool} invocation after rejection"))?;
            assert!(
                events.iter().skip(success + 1).any(|event| matches!(
                    event,
                    koharu_agent::Event::ToolFinished { changed: true, .. }
                )),
                "no saved edit after {tool} recovery"
            );
            recoveries.push(feedback);
        }
    }
    let snapshot = project.lock().await.snapshot();
    let view = serde_json::to_value(Project::page(&snapshot, page)?)?;
    let value = view["layers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|value| value["id"] == layer.to_string())
        .context("edited layer")?;
    for (key, expected) in expected.as_object().unwrap() {
        assert_eq!(
            &value["typography"][key], expected,
            "AGY did not apply {key}"
        );
    }
    assert_eq!(
        value["visibility"],
        json!({"visible":true,"opacity":0.4f32})
    );
    assert_eq!(
        value["geometry"]["points"],
        json!([{"x":40.0,"y":60.0},{"x":280.0,"y":60.0},{"x":280.0,"y":190.0},{"x":40.0,"y":190.0}])
    );
    assert_eq!(value["content"]["source"]["text"], "Original text");
    assert_eq!(
        value["content"]["translation"]["text"],
        "Chữ tiếng Việt cần dễ đọc"
    );
    let calls = calls.lock();
    let first_edit = calls
        .iter()
        .position(|call| call["changed"] == true)
        .context("a saved edit")?;
    let last_edit = calls
        .iter()
        .rposition(|call| call["changed"] == true)
        .unwrap();
    assert!(
        calls[..first_edit]
            .iter()
            .any(|call| call["tool"] == "view_page"),
        "render before edits"
    );
    assert!(
        calls[last_edit + 1..]
            .iter()
            .any(|call| call["tool"] == "view_page"),
        "render after the last edit"
    );
    assert!(
        calls
            .iter()
            .filter(|call| call["tool"] == "set_edit_scope")
            .all(|call| call["arguments"]["first_page"] == page.to_string()
                && call["arguments"]["last_page"] == last_page.to_string()),
        "model must select the requested chapter range"
    );
    if let Ok(path) = std::env::var("KOHARU_NATIVE_ACCEPTANCE_DIR") {
        std::fs::create_dir_all(&path)?;
        std::fs::write(
            std::path::Path::new(&path).join("native-acceptance.json"),
            serde_json::to_vec_pretty(
                &json!({"model":config.agy.model,"layer":value,"calls":*calls,"recoveries":recoveries,"message":result.message}),
            )?,
        )?;
        let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
        let bytes = output::rendered_preview(&renderer, rasterizer, &snapshot, page).await?;
        std::fs::write(std::path::Path::new(&path).join("native-after.webp"), bytes)?;
    }
    println!(
        "AGY Flash applied and native inspection verified every requested typography, geometry and visibility property."
    );
    Ok(())
}
