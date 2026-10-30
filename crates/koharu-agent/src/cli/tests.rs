use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;

use super::*;
use crate::{Invocation, Tool, ToolCall};

#[derive(Clone, Default)]
struct Fixture(Arc<AtomicUsize>);

#[async_trait]
impl Host for Fixture {
    async fn bind(&self, _: Control) -> Result<Arc<dyn Host>> {
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
async fn discovery_reads_both_help_streams_and_requires_a_successful_capability_check() -> Result<()> {
    let source = tempfile::tempdir()?;
    let script = source.path().join("discovery.mjs");
    let config = CliConfig {
        binary: Some(script.display().to_string()),
        ..CliConfig::default()
    };
    for (stdout, stderr, exit, supported) in [
        ("--input-format --output-format --agent --settings", "", 0, true),
        ("", "--input-format --output-format --agent --settings", 0, true),
        ("--input-format --output-format", "--agent --settings", 0, true),
        ("--input-format --output-format --agent --settings", "CLI startup failed", 2, false),
        ("", "--input-format --output-format --agent", 0, false),
    ] {
        let stdout = serde_json::to_string(stdout)?;
        let stderr = serde_json::to_string(stderr)?;
        tokio::fs::write(&script, format!(r#"
            if (process.argv.includes('--help')) {{
                process.stdout.write({stdout});
                process.stderr.write({stderr});
                process.exitCode = {exit};
            }} else if (process.argv.includes('models')) {{
                console.log('fixture-model\tFixture model');
            }} else {{ process.exitCode = 3; }}
        "#)).await?;
        let result = inspect(Backend::Agy, &config).await;
        if supported {
            let (_, models) = result?;
            assert_eq!(models.len(), 1);
            assert_eq!(models[0].id, "fixture-model");
        } else {
            assert!(result.is_err(), "accepted an unsuccessful or incomplete capability check");
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
    println!("Discovered {} editor models using {executable}", models.len());
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
