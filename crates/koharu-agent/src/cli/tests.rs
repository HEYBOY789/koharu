use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::*;
use crate::{Invocation, Tool, ToolCall};

#[derive(Clone, Default)]
struct Fixture(Arc<AtomicUsize>);

#[async_trait]
impl Host for Fixture {
    async fn bind(&self, _: RunId, _: Control) -> Result<Arc<dyn Host>> {
        Ok(Arc::new(self.clone()))
    }
    async fn context(&self) -> Result<Value> {
        Ok(json!({"project":"isolated fixture"}))
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::new(
                "edit",
                "Edit the fixture",
                json!({"type":"object","properties":{}}),
            ),
            Tool::new(
                "view",
                "View the fixture",
                json!({"type":"object","properties":{}}),
            ),
        ]
    }
    async fn invoke(&self, call: ToolCall, control: &Control) -> Result<Invocation> {
        control.ensure_running()?;
        match call.name.as_str() {
            "edit" => { self.0.fetch_add(1, Ordering::SeqCst); Invocation::changed(json!({"text":"changed"})) }
            "view" => Ok(Invocation::read(json!({"page":"fixture"}))?.with_image("Fixture", "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+j/l8AAAAASUVORK5CYII=")),
            _ => bail!("Unknown fixture tool"),
        }
    }
}

#[tokio::test]
async fn bridge_streams_changes_and_images_and_rejects_revoked_or_unknown_tools() -> Result<()> {
    let host = Arc::new(Fixture::default());
    let control = Control::default();
    let (events, mut receiver) = mpsc::channel(32);
    let bridge = bridge::Bridge::start(host.clone(), control.clone(), events, RunId::new()).await?;
    let client = reqwest::Client::builder().no_proxy().build()?;
    let list: Value = client
        .post(&bridge.url)
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 2);
    for (id, name) in [(2, "edit"), (3, "view")] {
        let result: Value = client.post(&bridge.url).json(&json!({"jsonrpc":"2.0","id":id,"method":"tools/call","params":{"name":name,"arguments":{}}})).send().await?.json().await?;
        assert_eq!(result["result"]["isError"], false);
        if name == "view" {
            assert_eq!(result["result"]["content"][1]["type"], "image");
            assert_eq!(result["result"]["content"][1]["mimeType"], "image/png");
        }
    }
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    assert!(matches!(
        receiver.recv().await,
        Some(Event::ToolStarted { .. })
    ));
    assert!(matches!(
        receiver.recv().await,
        Some(Event::ToolFinished { changed: true, .. })
    ));
    let invalid: Value = client.post(&bridge.url).json(&json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"not_registered"}})).send().await?.json().await?;
    assert_eq!(invalid["error"]["code"], -32602);
    bridge.deactivate().await;
    let idle = client
        .post(&bridge.url)
        .json(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"edit"}}))
        .send()
        .await?;
    assert_eq!(idle.status(), reqwest::StatusCode::GONE);
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    control.cancel();
    let next_control = Control::default();
    let (next_events, _next_receiver) = mpsc::channel(32);
    bridge
        .activate(
            host.clone(),
            next_control.clone(),
            next_events,
            RunId::new(),
        )
        .await;
    let next: Value = client
        .post(&bridge.url)
        .json(&json!({"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"edit"}}))
        .send()
        .await?
        .json()
        .await?;
    assert_eq!(next["result"]["isError"], false);
    assert_eq!(host.0.load(Ordering::SeqCst), 2);
    next_control.cancel();
    let revoked = client
        .post(&bridge.url)
        .json(&json!({"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"edit"}}))
        .send()
        .await?;
    assert_eq!(revoked.status(), reqwest::StatusCode::GONE);
    assert_eq!(host.0.load(Ordering::SeqCst), 2);
    let url = bridge.url.clone();
    bridge.close().await;
    assert!(client.post(&url).send().await.is_err());
    Ok(())
}

#[test]
fn streams_require_terminal_success_and_surface_model_errors() -> Result<()> {
    let run = RunId::new();
    let mut events = vec![];
    let mut publish = |event| events.push(event);
    let mut agy = Stream::default();
    assert!(
        agy.accept(
            Backend::Agy,
            json!({"event":"init","conversation_id":"session"}),
            run,
            &mut publish
        )?
        .is_none()
    );
    agy.accept(
        Backend::Agy,
        json!({"event":"step_update","step_update":{"text_delta":"hello"}}),
        run,
        &mut publish,
    )?;
    assert_eq!(
        agy.accept(
            Backend::Agy,
            json!({"event":"result","result":{"status":"SUCCESS","response":"hello"}}),
            run,
            &mut publish
        )?,
        Some("hello".into())
    );
    assert_eq!(agy.conversation.as_deref(), Some("session"));
    assert!(
        agy.accept(
            Backend::Agy,
            json!({"event":"result","result":{"status":"ERROR","error":"quota"}}),
            run,
            &mut publish
        )
        .unwrap_err()
        .to_string()
        .contains("quota")
    );
    let mut pi = Stream::default();
    assert!(
        pi.accept(
            Backend::Pi,
            json!({"type":"response","success":true}),
            run,
            &mut publish
        )?
        .is_none()
    );
    pi.accept(Backend::Pi, json!({"type":"message_end","message":{"role":"assistant","stopReason":"error","errorMessage":"missing credentials"}}), run, &mut publish)?;
    assert!(
        pi.accept(
            Backend::Pi,
            json!({"type":"agent_settled"}),
            run,
            &mut publish
        )
        .unwrap_err()
        .to_string()
        .contains("missing credentials")
    );
    Ok(())
}

#[tokio::test]
async fn public_progress_streams_report_thinking_without_inventing_agy_content() -> Result<()> {
    let run = RunId::new();
    let mut events = Vec::new();
    let mut stream = Stream::default();
    // AGY's documented ACTIVE agent_response is a phase signal, not reasoning text.
    stream.accept(Backend::Agy, json!({"event":"step_update","step_update":{"step_type":"agent_response","state":"ACTIVE"}}), run, &mut |event| events.push(event))?;
    assert!(matches!(&events[0], Event::ReasoningDelta { delta, .. } if delta.is_empty()));
    stream.accept(Backend::Pi, json!({"type":"message_update","assistantMessageEvent":{"type":"thinking_delta","delta":"Public summary"}}), run, &mut |event| events.push(event))?;
    assert!(matches!(&events[1], Event::ReasoningDelta { delta, .. } if delta == "Public summary"));
    Ok(())
}

#[tokio::test]
async fn cli_sends_the_complete_user_request_before_project_data() -> Result<()> {
    let source = tempfile::tempdir()?;
    let script = source.path().join("request.mjs");
    let prompt =
        "Chỉnh lỗi font và xuống dòng cho toàn bộ project. Giữ nguyên nội dung. TASK_END_6817";
    tokio::fs::write(&script, format!(r#"
        import {{ createInterface }} from 'node:readline';
        createInterface({{ input: process.stdin }}).on('line', line => {{
            const input = JSON.parse(line);
            const text = input.message?.content ?? input.message;
            const task = {task};
            const success = text.startsWith('User request:\n' + task + '\n\n')
                && text.indexOf('TASK_END_6817') < text.indexOf('<koharu_project_context>');
            if (input.event === 'user') {{
                console.log(JSON.stringify({{event:'result',result:{{status:'SUCCESS',response:success ? 'task preserved' : 'task hidden behind project data'}}}}));
            }} else {{
                console.log(JSON.stringify({{type:'message_end',message:{{role:'assistant',stopReason:'stop',content:[{{type:'text',text:success ? 'task preserved' : 'task hidden behind project data'}}]}}}}));
                console.log('{{"type":"agent_settled"}}');
            }}
        }});
    "#, task = serde_json::to_string(prompt)?)).await?;
    let config = CliConfig {
        binary: Some(script.display().to_string()),
        ..CliConfig::default()
    };
    for backend in [Backend::Agy, Backend::Pi] {
        let mut session = Session::default();
        let result = run(
            &mut session,
            backend,
            &config,
            Arc::new(Fixture::default()),
            RunId::new(),
            prompt,
            &Control::default(),
            &mut |_| {},
        )
        .await?;
        session.close().await;
        assert_eq!(result, "task preserved", "{backend:?}");
    }
    Ok(())
}

#[tokio::test]
async fn discovery_reads_both_help_streams_and_requires_a_successful_capability_check() -> Result<()>
{
    let source = tempfile::tempdir()?;
    let script = source.path().join("discovery.mjs");
    let config = CliConfig {
        binary: Some(script.display().to_string()),
        ..CliConfig::default()
    };
    for (stdout, stderr, exit, supported) in [
        (
            "--input-format --output-format --agent --settings",
            "",
            0,
            true,
        ),
        (
            "",
            "--input-format --output-format --agent --settings",
            0,
            true,
        ),
        (
            "--input-format --output-format",
            "--agent --settings",
            0,
            true,
        ),
        (
            "--input-format --output-format --agent --settings",
            "CLI startup failed",
            2,
            false,
        ),
        ("", "--input-format --output-format --agent", 0, false),
    ] {
        let stdout = serde_json::to_string(stdout)?;
        let stderr = serde_json::to_string(stderr)?;
        tokio::fs::write(
            &script,
            format!(
                r#"
            if (process.argv.includes('--help')) {{
                process.stdout.write({stdout});
                process.stderr.write({stderr});
                process.exitCode = {exit};
            }} else if (process.argv.includes('models')) {{
                console.log('fixture-model\tFixture model');
            }} else {{ process.exitCode = 3; }}
        "#
            ),
        )
        .await?;
        let result = inspect(Backend::Agy, &config).await;
        if supported {
            let (_, models) = result?;
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, "fixture-model");
        } else {
            assert!(
                result.is_err(),
                "accepted an unsuccessful or incomplete capability check"
            );
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Manual installed CLI discovery; makes no model calls"]
async fn installed_agy_discovers_editor_models() -> Result<()> {
    let config = CliConfig {
        binary: std::env::var("KOHARU_EDITOR_PROBE_BINARY").ok(),
        ..CliConfig::default()
    };
    let (executable, models) = inspect(Backend::Agy, &config).await?;
    assert!(!models.is_empty());
    println!(
        "Discovered {} editor models using {executable}",
        models.len()
    );
    Ok(())
}

#[tokio::test]
async fn cli_preserves_partial_records_while_host_events_arrive_and_can_be_cancelled() -> Result<()>
{
    let source = tempfile::tempdir()?;
    let script = source.path().join("fixture.mjs");
    tokio::fs::write(&script, r#"
      process.stdin.once('data', async () => {
        process.stdout.write('{"type":"message_');
        await fetch(process.env.KOHARU_EDITOR_TOOLS, {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name:'edit',arguments:{}}})});
        setTimeout(() => {
          console.log('end","message":{"role":"assistant","stopReason":"stop","content":[{"type":"text","text":"done"}]}}');
          console.log('{"type":"agent_settled"}');
        }, 30);
      });
    "#).await?;
    let config = CliConfig {
        binary: Some(script.display().to_string()),
        ..CliConfig::default()
    };
    let host = Arc::new(Fixture::default());
    let mut events = vec![];
    let message = run(
        &mut Session::default(),
        Backend::Pi,
        &config,
        host.clone(),
        RunId::new(),
        "edit fixture",
        &Control::default(),
        &mut |event| events.push(event),
    )
    .await?;
    assert_eq!(message, "done");
    assert_eq!(host.0.load(Ordering::SeqCst), 1);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolFinished { changed: true, .. }))
    );

    tokio::fs::write(
        &script,
        "process.stdin.once('data',()=>setInterval(()=>{},1000));",
    )
    .await?;
    let control = Control::default();
    let cancel = control.clone();
    let watcher = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run(
            &mut Session::default(),
            Backend::Pi,
            &config,
            host,
            RunId::new(),
            "wait",
            &control,
            &mut |_| {},
        ),
    )
    .await?;
    watcher.await?;
    assert!(result.is_err());
    Ok(())
}

#[tokio::test]
#[ignore = "Manual installed CLI verification; uses its signed-in model on an isolated fixture, never a user project"]
async fn installed_cli_keeps_conversation_and_reconnects_project_tools() -> Result<()> {
    let backend = match std::env::var("KOHARU_EDITOR_PROBE_BACKEND").as_deref() {
        Ok("agy") => Backend::Agy,
        Ok("pi") => Backend::Pi,
        _ => bail!("Set KOHARU_EDITOR_PROBE_BACKEND to agy or pi"),
    };
    let config = CliConfig {
        binary: std::env::var("KOHARU_EDITOR_PROBE_BINARY").ok(),
        node: None,
        model: std::env::var("KOHARU_EDITOR_PROBE_MODEL").ok(),
        reasoning: None,
    };
    let mut session = Session::default();
    let host = Arc::new(Fixture::default());
    let mut events = vec![];
    let message = run(&mut session, backend, &config, host.clone(), RunId::new(), "This is an isolated tool protocol fixture, not a real project. Call the Koharu MCP tool named edit once and the tool named view once, both with empty arguments. Remember the word KOHARU_PRISM_6817 for the next user turn. Report success only after both tools succeed.", &Control::default(), &mut |event| events.push(event)).await?;
    assert_eq!(host.0.load(Ordering::SeqCst), 1, "{message}");
    println!("First turn: {:?}", session.conversation);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, Event::ToolFinished { name, .. } if name == "view")),
        "{message}"
    );
    let message = run(&mut session, backend, &config, host.clone(), RunId::new(), "Call the Koharu tool edit once again. Then repeat the exact word I asked you to remember in my previous turn. Use the currently connected tools and do not use files or shell commands.", &Control::default(), &mut |_| {}).await?;
    println!("Second turn: {:?}", session.conversation);
    assert_eq!(host.0.load(Ordering::SeqCst), 2, "{message}");
    assert!(message.contains("KOHARU_PRISM_6817"), "{message}");
    session.close().await;
    println!("Installed {backend:?}: image/edit tools and conversation continuity passed");
    Ok(())
}

#[tokio::test]
#[ignore = "Manual AGY Gemini acceptance using exported native editor contracts and an isolated in-memory fixture"]
async fn installed_agy_edits_layer_with_native_contracts_and_chapter_range() -> Result<()> {
    struct EditorFixture {
        tools: Vec<Tool>,
        recovery_error: Option<String>,
        inspection_attempts: AtomicUsize,
        readability: bool,
        images: Option<(String, String)>,
        calls: std::sync::Mutex<Vec<(String, Value)>>,
        scoped: std::sync::atomic::AtomicBool,
        edited: AtomicUsize,
    }
    const FIRST: &str = "00000000-0000-4000-8000-000000000003";
    const LAST: &str = "00000000-0000-4000-8000-000000000005";
    const LAYER: &str = "00000000-0000-4000-8000-000000000013";
    const CHAPTER: &str = "00000000-0000-4000-8000-000000000021";
    #[async_trait]
    impl Host for EditorFixture {
        async fn bind(&self, _: RunId, _: Control) -> Result<Arc<dyn Host>> {
            bail!("already bound fixture")
        }
        async fn context(&self) -> Result<Value> {
            if self.readability {
                return Ok(
                    json!({"project":{"name":"isolated readability fixture","active_page":FIRST},"first_page":FIRST,"last_page":FIRST,"chapter_count":1,"pages":[{"id":FIRST,"label":"003","position":{"chapter_title":"Chapter 1","chapter_page_number":1,"chapter_page_count":1}}],"pagination":{"next_offset":null}}),
                );
            }
            Ok(
                json!({"project":{"name":"isolated chapter tool test","active_page":FIRST},"first_page":"00000000-0000-4000-8000-000000000001","last_page":LAST,"chapter_count":2,"pages":[],"pagination":{"next_offset":null}}),
            )
        }
        fn tools(&self) -> Vec<Tool> {
            self.tools.clone()
        }
        async fn invoke(&self, call: ToolCall, _: &Control) -> Result<Invocation> {
            let args: Value = serde_json::from_str(&call.arguments)?;
            self.calls
                .lock()
                .unwrap()
                .push((call.name.clone(), args.clone()));
            match call.name.as_str() {
                "inspect_project" => {
                    let attempt = self.inspection_attempts.fetch_add(1, Ordering::SeqCst);
                    if let Some(error) = &self.recovery_error {
                        if attempt == 0 {
                            println!(
                                "Fixture injected inspect_project arguments: {{\"unknown\":true}}"
                            );
                            bail!("{error}");
                        }
                        anyhow::ensure!(
                            args.as_object()
                                .is_some_and(|v| v.keys().all(|k| k == "offset" || k == "limit")),
                            "{error}"
                        );
                    }
                    Invocation::read(self.context().await?)
                }
                "inspect_chapters" if self.readability => Invocation::read(
                    json!({"chapters":[{"id":CHAPTER,"title":"Chapter 1","page_count":1,"first_page":FIRST,"last_page":FIRST}],"pagination":{"next_offset":null}}),
                ),
                "inspect_chapter" if self.readability => Invocation::read(
                    json!({"pages":[{"id":FIRST,"label":"003","chapter_page_number":1}],"pagination":{"next_offset":null}}),
                ),
                "inspect_page" if self.readability => Invocation::read(
                    json!({"page":{"id":FIRST,"label":"003","size":{"width":1000,"height":1400}},"entries":[{"layer":{"kind":"text","id":LAYER,"geometry":{"points":[{"x":150,"y":200},{"x":850,"y":200},{"x":850,"y":700},{"x":150,"y":700}]},"typography":{"preferred_font":"Segoe UI","size":70,"auto_fit":false,"color":[0,0,0,255],"stroke_color":[0,0,0,255],"stroke_width":8},"translation":{"text":"Tôi sẽ tiếp tục bước về phía trước. Không ai có thể ngăn cản tôi."}}}],"pagination":{"next_offset":null}}),
                ),
                "view_page" if self.readability => {
                    anyhow::ensure!(args["page"] == FIRST, "unknown fixture page");
                    let images = self.images.as_ref().unwrap();
                    let image = if self.edited.load(Ordering::SeqCst) == 0 {
                        &images.0
                    } else {
                        &images.1
                    };
                    Ok(Invocation::read(json!({"page":FIRST}))?
                        .with_image("Readability fixture", image.clone()))
                }
                "set_typography" if self.readability => {
                    anyhow::ensure!(
                        self.scoped.load(Ordering::SeqCst) && args["element"] == LAYER,
                        "read the layer ID and select its page scope first"
                    );
                    anyhow::ensure!(
                        args.as_object().is_some_and(|v| v.len() > 1),
                        "provide typography properties to change"
                    );
                    self.edited.fetch_add(1, Ordering::SeqCst);
                    Invocation::changed(json!({"element":LAYER,"saved":true}))
                }
                "set_geometry" if self.readability => {
                    anyhow::ensure!(
                        self.scoped.load(Ordering::SeqCst)
                            && args["element"] == LAYER
                            && args["points"].is_array(),
                        "provide inspected layer ID and page-space points"
                    );
                    self.edited.fetch_add(1, Ordering::SeqCst);
                    Invocation::changed(json!({"element":LAYER,"saved":true}))
                }

                "inspect_chapters" => Invocation::read(
                    json!({"chapters":[{"id":CHAPTER,"title":"Chapter 1","chapter_number":1,"page_count":3,"first_page":"00000000-0000-4000-8000-000000000001","last_page":FIRST},{"id":"00000000-0000-4000-8000-000000000022","title":"Chapter 2","chapter_number":2,"page_count":2,"last_page":LAST}],"pagination":{"next_offset":null}}),
                ),
                "inspect_chapter" => {
                    anyhow::ensure!(args["chapter"] == CHAPTER, "inspect Chapter 1");
                    Invocation::read(
                        json!({"pages":[{"id":"00000000-0000-4000-8000-000000000001","label":"001","chapter_page_number":1},{"id":"00000000-0000-4000-8000-000000000002","label":"002","chapter_page_number":2},{"id":FIRST,"label":"003","chapter_page_number":3}],"pagination":{"next_offset":null}}),
                    )
                }
                "inspect_page" => {
                    anyhow::ensure!(
                        args["page"] == FIRST,
                        "only the requested page needs inspection"
                    );
                    Invocation::read(
                        json!({"page":{"id":FIRST,"label":"003","position":{"chapter_title":"Chapter 1","chapter_page_number":3}},"entries":[{"layer":{"kind":"text","id":LAYER,"typography":{"preferred_font":"Old font","size":24,"auto_fit":false,"color":[1,2,3,255]},"translation":{"text":"Kiểm tra font chữ"}}}],"pagination":{"next_offset":null}}),
                    )
                }
                "list_fonts" => Invocation::read(
                    json!({"fonts":["Noto Sans"],"pagination":{"next_offset":null}}),
                ),
                "set_edit_scope" => {
                    anyhow::ensure!(
                        args["first_page"] == FIRST
                            && args["last_page"] == if self.readability { FIRST } else { LAST },
                        "range must start at Chapter 1 page 3 and end at the project's last page"
                    );
                    self.scoped.store(true, Ordering::SeqCst);
                    Invocation::read(json!({"page_count":3}))
                }
                "run_pipeline" => {
                    anyhow::ensure!(
                        self.scoped.load(Ordering::SeqCst),
                        "select edit scope before editing"
                    );
                    anyhow::ensure!(
                        args["operation"] == "ocr",
                        "run_pipeline requires operation=ocr for this task"
                    );
                    anyhow::ensure!(
                        args["pages"] == json!([FIRST]),
                        "the page pipeline must target only the requested page ID"
                    );
                    self.edited.fetch_add(1, Ordering::SeqCst);
                    Invocation::changed(json!({"completed":1,"total":1,"operation":"ocr"}))
                }
                "set_typography" => {
                    anyhow::ensure!(
                        self.scoped.load(Ordering::SeqCst),
                        "select edit scope before editing"
                    );
                    anyhow::ensure!(
                        args["element"] == LAYER && args["preferred_font"] == "Noto Sans",
                        "missing or incorrect layer/font argument; read inspect_page for the element ID"
                    );
                    anyhow::ensure!(
                        args.get("size").is_none()
                            && args.get("auto_fit").is_none()
                            && args.get("color").is_none(),
                        "omit typography properties the user did not ask to change"
                    );
                    self.edited.fetch_add(1, Ordering::SeqCst);
                    Invocation::changed(
                        json!({"element":LAYER,"preferred_font":"Noto Sans","size":24,"auto_fit":false,"color":[1,2,3,255]}),
                    )
                }
                _ => bail!("this isolated font task does not need {}", call.name),
            }
        }
    }
    let contract_path = std::path::PathBuf::from(std::env::var("KOHARU_EDITOR_CONTRACT_FILE")?);
    let contracts: Vec<Value> = serde_json::from_slice(&std::fs::read(&contract_path)?)?;
    let recovery = std::env::var("KOHARU_EDITOR_PROBE_TASK").as_deref() == Ok("recovery");
    let recovery_error = if recovery {
        let errors: Value =
            serde_json::from_slice(&std::fs::read(contract_path.with_extension("errors.json"))?)?;
        Some(
            errors["inspect_project_unknown"]
                .as_str()
                .context("exported native recovery error missing")?
                .to_owned(),
        )
    } else {
        None
    };
    let tools = contracts
        .into_iter()
        .map(|v| {
            Tool::new(
                v["name"].as_str().unwrap(),
                v["description"].as_str().unwrap(),
                v["parameters"].clone(),
            )
        })
        .collect();
    let readability = std::env::var("KOHARU_EDITOR_PROBE_TASK").as_deref() == Ok("readability");
    let images = if readability {
        use base64::Engine as _;
        let directory = std::path::PathBuf::from(std::env::var("KOHARU_EDITOR_PROBE_IMAGES")?);
        let read = |name: &str| -> Result<String> {
            Ok(format!(
                "data:image/png;base64,{}",
                base64::engine::general_purpose::STANDARD
                    .encode(std::fs::read(directory.join(name))?)
            ))
        };
        Some((read("before.png")?, read("after.png")?))
    } else {
        None
    };
    let host = Arc::new(EditorFixture {
        tools,
        recovery_error,
        inspection_attempts: AtomicUsize::new(0),
        readability,
        images,
        calls: Default::default(),
        scoped: Default::default(),
        edited: AtomicUsize::new(0),
    });
    let config = CliConfig {
        binary: std::env::var("KOHARU_EDITOR_PROBE_BINARY").ok(),
        model: Some(std::env::var("KOHARU_EDITOR_PROBE_MODEL")?),
        node: None,
        reasoning: None,
    };
    let mut session = Session::default();
    let control = Control::default();
    let task = "Đây là fixture kiểm tra tool, không phải truyện thật. Phạm vi cho phép chỉnh là từ trang thứ 3 của Chapter 1 tới hết project. Chỉ đổi font của layer chữ tại trang thứ 3 Chapter 1 sang Noto Sans; giữ nguyên cỡ chữ, màu, auto_fit và mọi thuộc tính khác. Dùng tool của Koharu và ID đọc từ tool, không dùng shell hay file. Không cần xem ảnh để đổi font. Hoàn tất sau khi tool sửa thành công.";
    let pipeline_task = "Đây là fixture kiểm tra hợp đồng tool, không phải truyện thật. Chọn phạm vi từ trang thứ 3 Chapter 1 đến hết project, sau đó chạy OCR bằng run_pipeline chỉ cho trang thứ 3 Chapter 1. Chỉ OCR, không dịch, không inpaint hay đổi font. Đọc ID trang qua tool Koharu và gửi tham số đúng schema. Không chạy shell/file. Hoàn tất sau khi tool pipeline thành công.";
    let task = if std::env::var("KOHARU_EDITOR_PROBE_TASK").as_deref() == Ok("pipeline") {
        pipeline_task
    } else {
        task
    };
    let task = if readability {
        "chỉnh lại các layer bị lỗi chồng chữ sai format chữ, sao cho nó readable đi. làm nguyên project này luôn. Sửa border chữ và font size cho dễ đọc, giữ nguyên nội dung."
    } else {
        task
    };
    let recovery_task = format!("Trước tiên đọc lại tổng quan bằng inspect_project. {task}");
    let task = if recovery {
        recovery_task.as_str()
    } else {
        task
    };
    let mut events = Vec::new();
    let result = tokio::time::timeout(
        Duration::from_secs(180),
        run(
            &mut session,
            Backend::Agy,
            &config,
            host.clone(),
            RunId::new(),
            task,
            &control,
            &mut |event| events.push(event),
        ),
    )
    .await;
    if result.is_err() {
        control.cancel();
    }
    let diagnostic = session.close().await;
    if (result.is_err() || host.edited.load(Ordering::SeqCst) == 0) && !diagnostic.is_empty() {
        println!("AGY fixture failure: {}", redact(&diagnostic));
    }
    let message = result??;
    for (name, args) in host.calls.lock().unwrap().iter() {
        println!("Fixture tool {name}: {args}");
    }
    if readability {
        let calls = host.calls.lock().unwrap();
        assert!(host.edited.load(Ordering::SeqCst) > 0, "{message}");
        assert!(
            !calls.iter().any(|(name, _)| name == "run_pipeline"),
            "layout repair must not reprocess page content: {calls:?}"
        );
        assert!(
            calls.iter().filter(|(name, _)| name == "view_page").count() >= 2,
            "inspect before/after readability edits"
        );
    } else {
        assert_eq!(host.edited.load(Ordering::SeqCst), 1, "{message}");
    }
    if recovery {
        assert!(
            host.inspection_attempts.load(Ordering::SeqCst) >= 2,
            "agent did not retry rejected inspection"
        );
        let rejected=events.iter().position(|event|matches!(event,Event::ToolFinished{name,changed:false,output,..} if name=="inspect_project" && output.contains("unknown field") && output.contains("{} is valid"))).context("native validation feedback did not reach the model bridge")?;
        let read=events.iter().enumerate().skip(rejected+1).find(|(_,event)|matches!(event,Event::ToolFinished{name,changed:false,output,..} if name=="inspect_project" && serde_json::from_str::<Value>(output).is_ok())).map(|(i,_)|i).context("corrected inspection did not succeed")?;
        assert!(
            events
                .iter()
                .skip(read + 1)
                .any(|event| matches!(event, Event::ToolFinished { changed: true, .. })),
            "no edit after recovery"
        );
    }
    assert!(host.scoped.load(Ordering::SeqCst));
    println!("AGY native-contract scoped editor task passed: {message}");
    Ok(())
}
