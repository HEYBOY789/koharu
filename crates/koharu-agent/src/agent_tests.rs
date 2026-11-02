use super::*;
use crate::{Invocation, Tool, ToolCall};
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Default)]
struct PendingHost {
    pending: Arc<AtomicUsize>,
    edits: Arc<AtomicUsize>,
    reads: Arc<AtomicUsize>,
}

#[async_trait]
impl Host for PendingHost {
    async fn bind(&self, _: RunId, _: Control) -> Result<Arc<dyn Host>> {
        Ok(Arc::new(self.clone()))
    }
    async fn context(&self) -> Result<Value> {
        Ok(json!({"project":"isolated completion regression"}))
    }
    fn tools(&self) -> Vec<Tool> {
        vec![
            Tool::new("edit", "Edit page 016", json!({"type":"object"})),
            Tool::new("read_other", "Read page 017", json!({"type":"object"})),
        ]
    }
    async fn invoke(&self, call: ToolCall, _: &Control) -> Result<Invocation> {
        let args: Value = serde_json::from_str(&call.arguments)?;
        if call.name == "read_other" {
            self.reads.fetch_add(1, Ordering::SeqCst);
            return Invocation::read(json!({"page":"017"}));
        }
        if args["fail"] == true {
            self.pending.store(1, Ordering::SeqCst);
            anyhow::bail!("Native rejected the edit on page 016");
        }
        self.pending.store(0, Ordering::SeqCst);
        self.edits.fetch_add(1, Ordering::SeqCst);
        Invocation::changed(json!({"page":"016"}))
    }
    async fn ensure_complete(&self) -> Result<()> {
        anyhow::ensure!(
            self.pending.load(Ordering::SeqCst) == 0,
            "retry_of=failure-016: page 016 remains unresolved"
        );
        Ok(())
    }
}

async fn completion_probe(ignore: bool) -> Result<(bool, Vec<Event>, PendingHost)> {
    let directory = tempfile::tempdir()?;
    let script = directory.path().join("model.mjs");
    let source=r#"
      import {createInterface} from 'node:readline';
      let turn=0;
      async function call(name,args) {
        return fetch(process.env.KOHARU_EDITOR_TOOLS,{method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify({jsonrpc:'2.0',id:1,method:'tools/call',params:{name,arguments:args}})});
      }
      createInterface({input:process.stdin}).on('line',async line=>{
        const input=JSON.parse(line); turn++;
        if (turn===1) { await call('edit',{fail:true}); await call('read_other',{}); }
        else if (IGNORE) { await call('read_other',{}); }
        else { if (!input.message.includes('failure-016')) throw Error('missing native reminder'); await call('edit',{fail:false}); }
        console.log(JSON.stringify({type:'message_end',message:{role:'assistant',stopReason:'stop',content:[{type:'text',text:'All pages fixed'}]}}));
        console.log(JSON.stringify({type:'agent_settled'}));
      });
    "#.replace("IGNORE",if ignore {"true"} else {"false"});
    tokio::fs::write(&script, source).await?;
    let host = PendingHost::default();
    let agent = Agent::with_configuration(
        Codex::new()?,
        host.clone(),
        koharu_config::Config::memory(Config {
            backend: Backend::Pi,
            pi: crate::CliConfig {
                binary: Some(script.display().to_string()),
                ..Default::default()
            },
            ..Default::default()
        }),
    );
    let mut events = Vec::new();
    let control = Control::default();
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        agent.run(
            RunId::new(),
            "Repair page 016 onward".into(),
            control.clone(),
            |event| events.push(event),
        ),
    )
    .await;
    agent.clear().await;
    assert!(
        !control.is_cancelled(),
        "completion recovery must not cancel the user's other work"
    );
    Ok((result?.is_ok(), events, host))
}

#[tokio::test]
async fn a_model_that_moves_on_after_a_failed_page_is_continued_until_native_recovery() -> Result<()>
{
    let (ok, events, host) = completion_probe(false).await?;
    assert!(ok);
    assert_eq!(host.reads.load(Ordering::SeqCst), 1);
    assert_eq!(host.edits.load(Ordering::SeqCst), 1);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Completed { .. }))
            .count(),
        1
    );
    let edit = events
        .iter()
        .position(|event| matches!(event, Event::ToolFinished { changed: true, .. }))
        .unwrap();
    let complete = events
        .iter()
        .position(|event| matches!(event, Event::Completed { .. }))
        .unwrap();
    assert!(complete > edit);
    Ok(())
}

#[tokio::test]
async fn a_model_that_ignores_pending_work_fails_after_bounded_continuations() -> Result<()> {
    let (ok, events, host) = completion_probe(true).await?;
    assert!(!ok);
    assert_eq!(host.pending.load(Ordering::SeqCst), 1);
    assert_eq!(host.reads.load(Ordering::SeqCst), 3);
    assert_eq!(host.edits.load(Ordering::SeqCst), 0);
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, Event::Completed { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event,Event::Failed{message,..} if message.contains("016")))
    );
    Ok(())
}
