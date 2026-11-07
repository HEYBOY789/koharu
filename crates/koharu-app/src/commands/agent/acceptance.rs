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
async fn native_layout_candidates_enlarge_cramped_text_atomically_and_reject_stale_choices()
-> Result<()> {
    let mut f = Fixture::new().await?;
    f.edit(
        "set_geometry",
        json!({"element":f.layer,"points":[
            {"x":10,"y":20},{"x":210,"y":20},{"x":210,"y":60},{"x":10,"y":60}
        ]}),
    )
    .await?;
    f.edit(
        "set_typography",
        json!({"element":f.layer,"size":12,"auto_fit":false,"color":[0,0,0,255]}),
    )
    .await?;
    let snapshot = f.project.snapshot();
    let rotated = snapshot.patch(|edit| {
        let mut layout = snapshot
            .component::<koharu_scene::TextLayout>(f.layer)?
            .unwrap();
        layout.angle_degrees = Some(18.0);
        edit.set(f.layer, &layout)
    })?;
    f.project.commit(rotated).await?;
    let _directory = f.directory;
    let page = f.page;
    let element = f.layer;
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
            json!({"first_page":page,"last_page":page,"readability":true}),
        ),
        &control,
    )
    .await?;
    host.invoke(call("audit_readability", json!({"page":page})), &control)
        .await?;
    host.invoke(call("view_page", json!({"page":page})), &control)
        .await?;
    let initial = project.lock().await.snapshot();
    let mut report = raw.renderer.render(&initial, page).await?.readability();
    let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
    report.sample_background(
        &output::frame_image(
            rasterizer.clone(),
            &raw.renderer.render(&initial, page).await?.without_text()?,
        )
        .await?,
    );
    let fixed_frame =
        readability::repair_batch(&raw.renderer, rasterizer, &initial, &[element], &report).await?;
    assert!(
        fixed_frame
            .report
            .layers
            .iter()
            .find(|l| l.element == element)
            .unwrap()
            .display_font_px[0]
            < 12.0,
        "size-only fitting must reproduce cramped text before layout search"
    );
    let before = serde_json::to_value(Project::page(&initial, page)?)?;
    let revision = initial.revision();
    let proposals = host
        .invoke(
            call(
                "repair_text",
                json!({"page":page,"layout":{
                    "element":element,"area":[10,20,350,360],"avoid":[[220,20,140,360]]
                }}),
            ),
            &control,
        )
        .await?;
    assert!(!proposals.changed);
    assert_eq!(
        project.lock().await.revision(),
        revision,
        "preview must not save"
    );
    let candidates = proposals.value["candidates"]
        .as_array()
        .context("layout candidates")?;
    assert!(!candidates.is_empty() && candidates.len() <= 3);
    assert_eq!(
        proposals.images.len(),
        candidates.len() * 2,
        "phone and desktop for each choice"
    );
    for candidate in candidates {
        for key in ["bounds", "frame_bounds"] {
            let bounds: [f32; 4] = serde_json::from_value(candidate[key].clone())?;
            assert!(
                bounds[0] >= 9.99 && bounds[1] >= 19.99,
                "rotated {key} crossed permitted space: {bounds:?}"
            );
            assert!(
                bounds[0] + bounds[2] <= 220.01 && bounds[1] + bounds[3] <= 380.01,
                "rotated candidate {key} crosses protected artwork: {bounds:?}"
            );
        }
    }
    let chosen = candidates
        .iter()
        .find(|c| c["display_font_px"][0].as_f64().unwrap_or(0.0) >= 12.0)
        .with_context(||format!("cramped text must reach the phone minimum in the taller permitted frame: {candidates:?}"))?;
    let candidate = chosen["candidate"]
        .as_str()
        .context("candidate ID")?
        .to_owned();
    let applied = host
        .invoke(
            call("repair_text", json!({"page":page,"candidate":candidate})),
            &control,
        )
        .await?;
    assert!(applied.changed);
    let mut locked = project.lock().await;
    let after = locked.snapshot();
    assert_eq!(
        initial.text_layer(element)?.content()?.translation()?,
        after.text_layer(element)?.content()?.translation()?
    );
    assert_ne!(
        initial.component::<koharu_scene::Geometry>(element)?,
        after.component::<koharu_scene::Geometry>(element)?
    );
    locked.undo().await?;
    assert_eq!(
        serde_json::to_value(Project::page(&locked.snapshot(), page)?)?,
        before,
        "one undo must restore geometry and typography together"
    );
    drop(locked);
    host.context().await?;
    host.invoke(call("audit_readability", json!({"page":page})), &control)
        .await?;
    host.invoke(call("view_page", json!({"page":page})), &control)
        .await?;
    let revision = project.lock().await.revision();
    assert!(
        host.invoke(
            call("repair_text", json!({"page":page,"candidate":candidate})),
            &control
        )
        .await
        .is_err(),
        "a visually identical page after undo still has a different base revision"
    );
    assert_eq!(project.lock().await.revision(), revision);
    Ok(())
}

#[tokio::test]
async fn free_text_layout_preserves_original_lettering_footprint_on_artwork() -> Result<()> {
    let mut f = Fixture::new().await?;
    f.edit(
        "set_geometry",
        json!({"element":f.layer,"points":[
            {"x":100,"y":100},{"x":200,"y":100},{"x":200,"y":240},{"x":100,"y":240}
        ]}),
    )
    .await?;
    f.edit(
        "set_typography",
        json!({"element":f.layer,"size":16,"auto_fit":false,"color":[0,0,0,255]}),
    )
    .await?;
    let snapshot = f.project.snapshot();
    let patch = snapshot.patch(|edit| {
        let content = snapshot.text_layer(f.layer)?.content()?.id();
        edit.set(
            content,
            &koharu_scene::TextRole {
                origin: koharu_scene::Origin::User,
                role: "dev.koharu.text.free-text".into(),
            },
        )?;
        let region = edit.add_analysis_region::<koharu_scene::TextRegion>(
            f.page,
            At::End,
            &koharu_scene::Geometry::rectangle(100.0, 100.0, 100.0, 140.0),
            None,
        )?;
        edit.relate::<koharu_scene::RecognizedFrom>(content, region)?;
        Ok(())
    })?;
    let initial = f.project.commit(patch).await?.snapshot;
    let renderer = koharu_renderer::Renderer::new()?;
    let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
    let frame = renderer.render(&initial, f.page).await?;
    let mut before = frame.readability();
    before
        .sample_background(&output::frame_image(rasterizer.clone(), &frame.without_text()?).await?);
    let proposals = readability::search_layouts(
        &renderer,
        rasterizer.clone(),
        &initial,
        f.page,
        serde_json::from_value(
            json!({"element":f.layer,"area":[70,80,300,300],"avoid":[[240,80,140,300]]}),
        )?,
        &before,
        &Control::default(),
    )
    .await?;
    let result = proposals.invocation(rasterizer).await?;
    let choices = result.value["candidates"]
        .as_array()
        .context("candidates")?;
    assert!(
        !choices.is_empty(),
        "a modest improvement still fits the original lettering area"
    );
    for choice in choices {
        for key in ["bounds", "frame_bounds"] {
            let b: [f32; 4] = serde_json::from_value(choice[key].clone())?;
            assert!(
                b[0] >= 89.99 && b[1] >= 85.99 && b[0] + b[2] <= 210.01 && b[1] + b[3] <= 254.01,
                "free lettering expanded away from the source composition to reach a generic phone threshold: {b:?}"
            );
        }
    }
    let oversized = initial.patch(|edit| {
        edit.set(
            f.layer,
            &koharu_scene::Geometry::rectangle(80.0, 80.0, 240.0, 300.0),
        )?;
        let mut style = initial
            .component::<koharu_scene::Typography>(f.layer)?
            .unwrap();
        style.size = Some(30.0);
        style.auto_fit = false;
        edit.set(f.layer, &style)
    })?;
    assert!(
        readability::checked_candidate(
            &renderer,
            Arc::new(koharu_rasterizer::Rasterizer::new()?),
            &initial,
            &oversized,
            f.page,
            &before
        )
        .await
        .is_err(),
        "separate geometry/typography edits cannot bypass original-composition protection"
    );
    let zoomed = initial.preview([&oversized])?;
    let zoomed_frame = renderer.render(&zoomed, f.page).await?;
    let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
    let mut zoomed_report = zoomed_frame.readability();
    zoomed_report.sample_background(
        &output::frame_image(rasterizer.clone(), &zoomed_frame.without_text()?).await?,
    );
    let restore = zoomed.patch(|edit| {
        edit.set(
            f.layer,
            &initial
                .component::<koharu_scene::Geometry>(f.layer)?
                .unwrap(),
        )?;
        edit.set(
            f.layer,
            &initial
                .component::<koharu_scene::Typography>(f.layer)?
                .unwrap(),
        )
    })?;
    readability::checked_candidate(
        &renderer,
        rasterizer,
        &zoomed,
        &restore,
        f.page,
        &zoomed_report,
    )
    .await
    .context("returning an oversized edit to source scale must allow its smaller font")?;
    Ok(())
}

#[tokio::test]
async fn missing_readability_view_is_recoverable_without_exhausting_tool_failures() -> Result<()> {
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
    let mut ledger = Failures::default();
    let attempt = Attempt::parse(call("delete_elements", json!({"elements":[f.layer]})))?;
    let edit = LayerEdit::parse(&attempt.call)?.unwrap();
    for _ in 0..3 {
        let rejected = edit.validate_review(&snapshot, &review);
        assert!(rejected.is_err(), "unviewed page must not be edited");
        let feedback = ledger.finish(
            &attempt,
            Target::capture(&snapshot, &attempt.call)?,
            rejected.and_then(|_| anyhow::bail!("unviewed edit reached the commit seam")),
        );
        assert!(
            feedback.is_ok(),
            "missing review prerequisites must return actionable feedback: {feedback:?}"
        );
        let feedback = feedback?;
        assert!(!feedback.changed);
        assert_eq!(feedback.value["review_required"]["tool"], "view_page");
        assert_eq!(
            feedback.value["review_required"]["arguments"],
            json!({"page":f.page})
        );
        assert!(!ledger.exhausted());
        assert_eq!(snapshot.revision(), revision);
        assert!(snapshot.text_layer(f.layer).is_ok());
    }
    ledger.ensure_complete()?;
    assert!(
        review.ensure_complete().is_err(),
        "a refused edit never verifies the page"
    );
    review.viewed(f.page, stamp, None);
    review.before_edit(f.page, stamp)?;
    review.verify(f.page, stamp, vec![])?;
    review.ensure_complete()?;
    Ok(())
}

#[tokio::test]
async fn native_review_prerequisite_recovers_after_edits_without_authorizing_dialogue_deletion()
-> Result<()> {
    let mut f = Fixture::new().await?;
    f.edit(
        "set_geometry",
        json!({"element":f.layer,"points":[
            {"x":10,"y":20},{"x":410,"y":20},{"x":410,"y":220},{"x":10,"y":220}
        ]}),
    )
    .await?;
    f.edit(
        "set_typography",
        json!({"element":f.layer,"size":32,"auto_fit":false,"color":[0,0,0,255]}),
    )
    .await?;
    let _directory = f.directory;
    let page = f.page;
    let layer = f.layer;
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
            json!({"first_page":page,"last_page":page,"readability":true}),
        ),
        &control,
    )
    .await?;
    host.invoke(call("audit_readability", json!({"page":page})), &control)
        .await?;
    let revision = project.lock().await.revision();
    for _ in 0..3 {
        let feedback = host
            .invoke(
                call("delete_elements", json!({"elements":[layer]})),
                &control,
            )
            .await?;
        assert!(!feedback.changed);
        assert_eq!(
            feedback.value["review_required"]["arguments"],
            json!({"page":page})
        );
        assert!(!control.is_cancelled());
    }
    assert_eq!(project.lock().await.revision(), revision);
    assert!(host.ensure_complete().await.is_err());
    for args in [
        json!({"page":page,"element":layer}),
        json!({"page":page,"original":true}),
    ] {
        host.invoke(call("view_page", args), &control).await?;
        let feedback = host
            .invoke(
                call("delete_elements", json!({"elements":[layer]})),
                &control,
            )
            .await?;
        assert_eq!(
            feedback.value["review_required"]["tool"], "view_page",
            "a crop or original image is not current full-page review"
        );
    }
    let view = host
        .invoke(call("view_page", json!({"page":page})), &control)
        .await?;
    assert_eq!(
        view.images.len(),
        2,
        "refresh supplies phone and desktop images"
    );
    let edit = host
        .invoke(
            call(
                "set_typography",
                json!({"element":layer,"color":[20,20,20,255]}),
            ),
            &control,
        )
        .await?;
    assert!(edit.changed);
    let feedback = host
        .invoke(
            call("delete_elements", json!({"elements":[layer]})),
            &control,
        )
        .await?;
    assert_eq!(
        feedback.value["review_required"]["tool"], "view_page",
        "a saved edit requires a fresh view"
    );
    let view = host
        .invoke(call("view_page", json!({"page":page})), &control)
        .await?;
    let declined = host
        .invoke(
            call("delete_elements", json!({"elements":[layer]})),
            &control,
        )
        .await?;
    assert!(!declined.changed);
    assert!(
        declined.value["candidate_rejected"].is_string(),
        "fresh review still cannot authorize ordinary dialogue deletion"
    );
    assert!(project.lock().await.snapshot().text_layer(layer).is_ok());
    let issues = view.value["readability"]["issues"].as_array().context("current view issues")?.iter().map(|issue|json!({
        "issue":issue["id"],"disposition":"needs_review",
        "reason":"This fixture checks review recovery and preserves the existing lettering for separate visual assessment."
    })).collect::<Vec<_>>();
    host.invoke(
        call("verify_readability", json!({"page":page,"issues":issues})),
        &control,
    )
    .await?;
    host.ensure_complete().await?;
    assert!(!control.is_cancelled());
    Ok(())
}

#[tokio::test]
async fn native_exhausted_tool_failures_keep_their_diagnostic_after_internal_cancellation()
-> Result<()> {
    let f = Fixture::new().await?;
    let _directory = f.directory;
    let layer = f.layer;
    let project = Arc::new(tokio::sync::Mutex::new(f.project));
    let revision = project.lock().await.revision();
    let raw = NativeHost {
        project: project.clone(),
        binding: None,
        renderer: koharu_renderer::Renderer::new()?,
        calls: Default::default(),
        argument_faults: Default::default(),
    };
    let control = Control::default();
    let host = raw.bind(RunId::new(), control.clone()).await?;
    for _ in 0..3 {
        assert!(
            host.invoke(
                call(
                    "set_typography",
                    json!({"element":layer,"font_weight":"bold"})
                ),
                &control
            )
            .await
            .is_err()
        );
    }
    assert!(control.is_cancelled(), "malformed calls remain bounded");
    let error = host.ensure_complete().await.unwrap_err().to_string();
    assert!(
        error.contains("set_typography") && error.contains("unresolved tool"),
        "internal cancellation must preserve the tool diagnostic: {error}"
    );
    assert!(!error.contains("Agent request was cancelled"));
    assert_eq!(project.lock().await.revision(), revision);
    Ok(())
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
    assert!(
        readability::source_duplicates(&snapshot, &mut report)?.is_empty(),
        "different translations of one OCR line require visual review"
    );
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
async fn merged_ocr_requires_visible_survivors_and_preserves_translation_order() -> Result<()> {
    use koharu_scene::{
        Authored, BubbleRegion, FlowsIn, Geometry, Origin, RecognizedFrom, SourceText, TextLayout,
        TextLayoutKind, TextRegion, Translation,
    };
    let mut f = Fixture::new().await?;
    let mut layers = vec![];
    let patch = f.project.snapshot().patch(|edit| {
        let balloon = edit.add_analysis_region::<BubbleRegion>(
            f.page,
            At::End,
            &Geometry::rectangle(0.0, 0.0, 600.0, 400.0),
            None,
        )?;
        for (source, translated, x, width) in [
            (
                "Please call me tomorrow. Thank you for your help.",
                "Hãy gọi tôi ngày mai. Cảm ơn bạn đã giúp.",
                0.0,
                500.0,
            ),
            (
                "Please call me tomorrow.",
                "Hãy gọi tôi ngày mai.",
                10.0,
                200.0,
            ),
            (
                "Thank you for your help.",
                "Cảm ơn bạn đã giúp.",
                260.0,
                200.0,
            ),
            (
                "Please call me tomorrow. Thank you for your help. Bring medicine.",
                "Hãy gọi tôi ngày mai. Cảm ơn bạn đã giúp. Mang thuốc.",
                0.0,
                500.0,
            ),
            (
                "Please call me tomorrow. Please call me tomorrow. Thank you for your help.",
                "Hãy gọi tôi ngày mai. Hãy gọi tôi ngày mai. Cảm ơn bạn đã giúp.",
                0.0,
                500.0,
            ),
            (
                "Please call me tomorrow. Thank you for your help.",
                "Cảm ơn bạn đã giúp. Hãy gọi tôi ngày mai.",
                0.0,
                500.0,
            ),
        ] {
            let region = edit.add_analysis_region::<TextRegion>(
                f.page,
                At::End,
                &Geometry::rectangle(x, 20.0, width, 100.0),
                None,
            )?;
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
                    text: Authored::user(translated.into()),
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
            edit.relate::<FlowsIn>(layer, balloon)?;
            layers.push(layer);
        }
        Ok(())
    })?;
    let snapshot = f.project.session.commit(patch).await?.snapshot;
    let renderer = koharu_renderer::Renderer::new()?;
    let mut report = renderer.render(&snapshot, f.page).await?.readability();
    let groups = readability::source_duplicates(&snapshot, &mut report)?;
    assert_eq!(groups.len(), 2);
    let matched = groups
        .iter()
        .find(|group| group.element == layers[0])
        .context("exact ordered match")?;
    assert!(matched.automatic);
    assert_eq!(matched.survivors.len(), 2);
    assert!(layers[1..3].iter().all(|id| matched.survivors.contains(id)));
    let reordered = groups
        .iter()
        .find(|group| group.element == layers[5])
        .context("reordered candidate")?;
    assert!(
        !reordered.automatic,
        "word order cannot be ignored in an automatic repair"
    );
    let stamp = readability::stamp(&snapshot, f.page)?;
    let mut review = readability::Review::default();
    review.begin(&[f.page])?;
    review.audit(stamp, report);
    review.viewed(f.page, stamp, None);
    LayerEdit::Delete(vec![layers[0]]).validate_review(&snapshot, &review)?;
    LayerEdit::Delete(vec![layers[5]]).validate_review(&snapshot, &review)?;
    assert!(
        LayerEdit::Delete(vec![layers[0], layers[1]])
            .validate_review(&snapshot, &review)
            .is_err()
    );
    assert!(
        LayerEdit::Delete(vec![layers[3]])
            .validate_review(&snapshot, &review)
            .is_err(),
        "unique words must survive"
    );
    assert!(
        LayerEdit::Delete(vec![layers[4]])
            .validate_review(&snapshot, &review)
            .is_err(),
        "one occurrence cannot cover repeated dialogue"
    );
    let hide = snapshot.patch(|edit| {
        edit.set(
            layers[2],
            &koharu_scene::Visibility {
                origin: Origin::User,
                visible: false,
                opacity: 1.0,
            },
        )
    })?;
    let hidden = snapshot.preview([&hide])?;
    let mut report = renderer.render(&hidden, f.page).await?.readability();
    assert!(
        readability::source_duplicates(&hidden, &mut report)?
            .iter()
            .all(|group| group.element != layers[0] && group.element != layers[5])
    );
    Ok(())
}

#[tokio::test]
async fn conflicting_clock_duplicate_requires_original_artwork_before_native_removal() -> Result<()>
{
    use koharu_scene::{
        Authored, FitsTo, Geometry, Origin, RecognizedFrom, SourceText, TextLayout, TextLayoutKind,
        TextRegion, Translation,
    };
    let mut fixture = Fixture::new().await?;
    let mut ids = vec![];
    let patch = fixture.project.snapshot().patch(|edit| {
        for (source, translated, geometry) in [
            (
                "9:50-14\n10:55-\n13:10-",
                "9:50-14\n10:55-\n13:10-",
                Geometry::rectangle(230.0, 140.0, 90.0, 130.0),
            ),
            (
                "Midterm exams\nJapanese 9:50 - 16\nEnglish 10:55 -\nMath 13:10 -",
                "Kỳ thi giữa kỳ\nQuốc ngữ 9:50 - 16\nTiếng Anh 10:55 -\nToán 13:10 -",
                Geometry::rectangle(100.0, 100.0, 240.0, 180.0),
            ),
        ] {
            let region =
                edit.add_analysis_region::<TextRegion>(fixture.page, At::End, &geometry, None)?;
            let content = edit.add_text_content(fixture.page, At::End)?;
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
                    text: Authored::user(translated.into()),
                    language: None,
                },
            )?;
            edit.relate::<RecognizedFrom>(content, region)?;
            let layer = edit.add_text_layer(
                fixture.page,
                At::End,
                content,
                &TextLayout {
                    origin: Origin::User,
                    kind: TextLayoutKind::Paragraph,
                    angle_degrees: None,
                },
            )?;
            edit.relate::<FitsTo>(layer, region)?;
            ids.push(layer);
        }
        Ok(())
    })?;
    let snapshot = fixture.project.session.commit(patch).await?.snapshot;
    let mut report = koharu_renderer::Renderer::new()?
        .render(&snapshot, fixture.page)
        .await?
        .readability();
    let groups = readability::source_duplicates(&snapshot, &mut report)?;
    assert!(
        groups.iter().any(|group| group.element == ids[0]
            && group.requires_source_review
            && !group.automatic)
    );
    let mut review = readability::Review::default();
    let version = readability::stamp(&snapshot, fixture.page)?;
    review.begin(&[fixture.page])?;
    review.audit(version, report);
    review.viewed(fixture.page, version, None);
    let removal = LayerEdit::Delete(vec![ids[0]]);
    assert!(removal.validate_review(&snapshot, &review).is_err());
    review.source_viewed(fixture.page, version);
    removal.validate_review(&snapshot, &review)?;
    assert!(
        LayerEdit::Delete(ids)
            .validate_review(&snapshot, &review)
            .is_err()
    );
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
        let project = self.project.lock().await;
        binding.ensure_complete(&project)
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
                    let duplicates =
                        readability::source_duplicates(&project.snapshot(), &mut report)?;
                    let background = output::frame_image(
                        Arc::new(koharu_rasterizer::Rasterizer::new()?),
                        &frame.without_text()?,
                    )
                    .await?;
                    report.sample_background(&background);
                    let entries=report.layers.iter().skip(a.offset).take(a.limit).map(|l|json!({"layer":l,"issues":report.issues.iter().filter(|i|i.element==l.element).collect::<Vec<_>>(),"ocr_duplicate":duplicates.iter().find(|duplicate| duplicate.element==l.element)})).collect();
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
                    a.validate()?;
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
                    if let Some(layout) = a.layout {
                        scope::check(
                            &snapshot,
                            binding.pages.lock().as_deref(),
                            &[page, entity(&layout.element)?],
                        )?;
                        let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
                        let proposals = readability::search_layouts(
                            &self.renderer,
                            rasterizer.clone(),
                            &snapshot,
                            page,
                            layout,
                            &before,
                            control,
                        )
                        .await?;
                        let result = proposals.invocation(rasterizer).await?;
                        anyhow::ensure!(
                            !control.is_cancelled(),
                            "Layout search cancelled; no edit was saved"
                        );
                        binding.review.lock().propose_layouts(proposals);
                        result
                    } else {
                        let (patch, value) = if let Some(candidate) = a.candidate {
                            binding
                                .review
                                .lock()
                                .layout_choice(&snapshot, page, &candidate)?
                        } else {
                            let batch = readability::repair_batch(
                                &self.renderer,
                                Arc::new(koharu_rasterizer::Rasterizer::new()?),
                                &snapshot,
                                &elements,
                                &before,
                            )
                            .await?;
                            (
                                batch.patch,
                                json!({"page":page,"elements":batch.elements,"removed_duplicates":batch.removed_duplicates,"balanced_groups":batch.balanced_groups,"remaining_issues":batch.report.issues,"declined":batch.declined}),
                            )
                        };
                        anyhow::ensure!(
                            !control.is_cancelled(),
                            "Agent request cancelled; no edit saved"
                        );
                        let commit = project.commit(patch).await?;
                        project.record_commit(&commit);
                        if commit.changes.from != commit.changes.to {
                            binding.review.lock().changed(page);
                        }
                        mutation_result(&project, &commit, value)?
                    }
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
                    let review_report = if binding.review.lock().active() && !a.original {
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
                    let crop = readability::view_crop(
                        &project.snapshot(),
                        &frame,
                        a.element.as_deref().map(entity).transpose()?,
                        a.original,
                    )?;
                    let frame = if a.original {
                        frame.original_artwork()?
                    } else {
                        frame
                    };
                    let dual = crop.is_none()
                        && !a.original
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
                        Invocation::read(json!({"page":context::location(&project.snapshot(), page)?,"original":a.original,"readability":review_report}))?
                            .with_image(
                                if a.original { "Original source artwork" } else { "Native rendered fixture" },
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
                    if a.original {
                        binding
                            .review
                            .lock()
                            .source_viewed(page, readability::stamp(&project.snapshot(), page)?);
                    } else if crop.is_none() {
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
        let mut recorded = json!({"tool":call.name,"arguments":serde_json::from_str::<Value>(&call.arguments)?,"changed":result.changed,"coverage":binding.review.lock().progress()});
        if result.value.get("candidates").is_some() {
            recorded["layout_preview"] = result.value.clone();
        }
        self.calls.lock().push(recorded);
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
    let request = std::env::var("KOHARU_READABILITY_REQUEST").unwrap_or_else(|_| {
        "Sửa chữ nhỏ, chữ chồng và viền quá dày để dễ đọc trên điện thoại và desktop.".into()
    });
    let prompt = format!(
        "{request} CHỈ làm các vị trí chapter/trang (1-based): {:?}, tức phạm vi inclusive first_page={} last_page={}. Đây là bản sao thử, không sửa ngoài phạm vi. Giữ nguyên lời thoại và hình vẽ; chỉ được bỏ một bản OCR trùng đã được audit xác nhận và còn một bản hiển thị. Không chạy lại dịch/OCR/pipeline. Dùng audit_readability, ảnh toàn trang và ảnh cận cảnh để chọn cách sửa phù hợp từng lớp, ưu tiên repair_text theo batch; nới khung chữ trong vùng đặt chữ hợp lệ theo ảnh gốc khi chữ vẫn quá nhỏ. Kiểm tra lại cả hai kích thước sau sửa và verify_readability từng trang. Vấn đề không sửa an toàn được phải đánh dấu needs_review và nói rõ. Làm trực tiếp bằng tool, không chỉ trả lời hướng dẫn.",
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
        let duplicates = readability::source_duplicates(&before, &mut report)?;
        let surviving = renderer.render(&after, *page).await?.readability();
        let visible = |id| surviving.layers.iter().any(|layer| layer.element == id);
        for entity in before.descendants(*page)? {
            if let Ok(layer) = before.text_layer(entity.id()) {
                let Ok(new) = after.text_layer(entity.id()) else {
                    anyhow::ensure!(
                        duplicates.iter().any(|duplicate| {
                            duplicate.element == entity.id()
                                && duplicate.survivors.iter().copied().all(visible)
                        }) || report.issues.iter().any(|issue| {
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
                            survivor.is_some_and(visible)
                        }),
                        "agent deleted dialogue without a surviving audited copy"
                    );
                    continue;
                };
                let mut expected = layer.content()?.translation()?;
                if let Some(restore) = duplicates
                    .iter()
                    .filter(|group| after.text_layer(group.element).is_err())
                    .filter_map(|group| group.restore.as_ref())
                    .find(|restore| restore.element == entity.id())
                    && let Some(translation) = &mut expected
                {
                    translation.text.value =
                        format!("{} {}", restore.word, translation.text.value.trim_start());
                }
                anyhow::ensure!(
                    expected.map(|v| v
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
