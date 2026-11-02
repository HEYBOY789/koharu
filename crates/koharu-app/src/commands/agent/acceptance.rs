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
        let directory = tempfile::tempdir()?;
        let mut project = Project::create(
            "native editor acceptance".into(),
            directory.path().join("fixture.khrproj"),
        )
        .await?;
        let mut pages = vec![];
        let patch = project.snapshot().patch(|edit| {
            for _ in 0..4 {
                pages.push(edit.add_page(PageDraft::new("003", 1000.0, 1600.0), At::End)?);
            }
            Ok(())
        })?;
        project.session.commit(patch).await?;
        let (_, chapter) = project.create_chapter("Chapter 1".into(), false).await?;
        for page in &pages[..3] {
            project.assign_page(*page, chapter).await?;
        }
        let (_, chapter) = project.create_chapter("Chapter 2".into(), false).await?;
        project.assign_page(pages[3], chapter).await?;
        let page = pages[2];
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
        anyhow::ensure!(!control.is_cancelled(), "Agent request was cancelled");
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
            let (commit, value) = edit.apply(&mut project).await?;
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
                    *binding.pages.lock() = Some(pages);
                    Invocation::read(json!({"page_count":count}))?
                }
                "list_fonts" => Invocation::read(context::fonts(
                    self.renderer.available_fonts().await?,
                    &arguments(&call)?,
                )?)?,
                "view_page" => {
                    let a: ViewPage = arguments(&call)?;
                    let page = entity(&a.page)?;
                    let rasterizer = Arc::new(koharu_rasterizer::Rasterizer::new()?);
                    let bytes = output::rendered_preview(
                        &self.renderer,
                        rasterizer,
                        &project.snapshot(),
                        page,
                    )
                    .await?;
                    Invocation::read(context::location(&project.snapshot(), page)?)?.with_image(
                        "Native rendered fixture",
                        format!("data:image/webp;base64,{}", STANDARD.encode(bytes)),
                    )
                }
                _ => bail!(
                    "This property acceptance fixture does not allow {}",
                    call.name
                ),
            }
        };
        *binding.observed.lock() = Some(project.revision());
        self.calls.lock().push(json!({"tool":call.name,"arguments":serde_json::from_str::<Value>(&call.arguments)?,"changed":result.changed}));
        Ok(result)
    }
}

#[tokio::test]
#[ignore = "Manual installed AGY/Gemini acceptance on an isolated native project, reads saved agent setup"]
async fn installed_agy_applies_real_native_layer_settings() -> Result<()> {
    let store = std::env::var("KOHARU_NATIVE_ACCEPTANCE_STORE")
        .context("provide an isolated copy of the font package cache for this manual test")?;
    koharu_runtime::Store::configure(std::path::PathBuf::from(store))?;
    let f = Fixture::new().await?;
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
    let _directory = f.directory;
    let calls = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let project = Arc::new(tokio::sync::Mutex::new(f.project));
    let host = NativeHost {
        project: project.clone(),
        binding: None,
        renderer: renderer.clone(),
        calls: calls.clone(),
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
        "Native editor property acceptance test on an ISOLATED temporary project. Resolve Chapter 1 page 3 through inspect_chapters/inspect_chapter; edit ONLY that one page. Inspect its existing text layer. Apply exactly these typography properties: {expected}. Set that layer's geometry to page-space points [{{\"x\":40,\"y\":60}},{{\"x\":280,\"y\":60}},{{\"x\":280,\"y\":190}},{{\"x\":40,\"y\":190}}]. Set visibility=true and opacity=0.4. Preserve all original words and all other pages. Use the real layer tools, no pipeline. Inspect the page before and after, including its native rendered image, and verify the saved properties. You must call view_page again AFTER the last edit, not just twice before editing. Reply briefly in Vietnamese."
    );
    let control = Control::default();
    let future = agent.run(RunId::new(), prompt, control.clone(), |_| {});
    let result = match tokio::time::timeout(std::time::Duration::from_secs(180), future).await {
        Ok(result) => result?,
        Err(_) => {
            control.cancel();
            agent.clear().await;
            bail!("AGY native acceptance timed out");
        }
    };
    agent.clear().await;
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
                && call["arguments"]["last_page"] == page.to_string()),
        "model must select only Chapter 1 page 3"
    );
    if let Ok(path) = std::env::var("KOHARU_NATIVE_ACCEPTANCE_DIR") {
        std::fs::create_dir_all(&path)?;
        std::fs::write(
            std::path::Path::new(&path).join("native-acceptance.json"),
            serde_json::to_vec_pretty(
                &json!({"model":config.agy.model,"layer":value,"calls":*calls,"message":result.message}),
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
