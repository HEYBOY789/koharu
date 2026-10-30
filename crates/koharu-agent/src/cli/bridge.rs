use std::sync::Arc;

use anyhow::Result;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use tokio::{
    net::TcpListener,
    sync::{RwLock, mpsc, oneshot},
    task::JoinHandle,
};
use uuid::Uuid;

use crate::{Control, Event, Host, RunId, ToolCall};

pub(super) struct Bridge {
    pub url: String,
    stop: Option<oneshot::Sender<()>>,
    task: JoinHandle<()>,
    context: Arc<RwLock<Option<Context>>>,
}

struct Context {
    host: Arc<dyn Host>,
    control: Control,
    events: mpsc::Sender<Event>,
    run: RunId,
}

impl Bridge {
    pub async fn start(
        host: Arc<dyn Host>,
        control: Control,
        events: mpsc::Sender<Event>,
        run: RunId,
    ) -> Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        // A fresh capability URL is scoped to one run; no project-wide listener exists.
        let path = format!("/{}", Uuid::new_v4().simple());
        let url = format!(
            "http://{}/{}",
            listener.local_addr()?,
            path.trim_start_matches('/')
        );
        let context = Arc::new(RwLock::new(Some(Context {
            host,
            control,
            events,
            run,
        })));
        let app = Router::new()
            .route(&path, post(dispatch))
            .layer(DefaultBodyLimit::max(4 * 1024 * 1024))
            .with_state(context.clone());
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stopped.await;
                })
                .await;
        });
        Ok(Self {
            url,
            stop: Some(stop),
            task,
            context,
        })
    }

    pub async fn activate(
        &self,
        host: Arc<dyn Host>,
        control: Control,
        events: mpsc::Sender<Event>,
        run: RunId,
    ) {
        *self.context.write().await = Some(Context {
            host,
            control,
            events,
            run,
        });
    }

    pub async fn deactivate(&self) {
        self.context.write().await.take();
    }

    pub async fn close(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        // Drain admitted host operations before the editor may replace its project.
        let _ = (&mut self.task).await;
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}

async fn dispatch(
    State(state): State<Arc<RwLock<Option<Context>>>>,
    Json(request): Json<Value>,
) -> Response {
    let Some(id) = request.get("id").cloned() else {
        return StatusCode::ACCEPTED.into_response();
    };
    // Hold the lease through the entire invocation; deactivation drains admitted edits.
    let lease = state.read().await;
    let Some(context) = lease.as_ref() else {
        return StatusCode::GONE.into_response();
    };
    if context.control.is_cancelled() {
        return StatusCode::GONE.into_response();
    }
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let result = match method {
        "initialize" => {
            json!({"protocolVersion":"2025-11-25","capabilities":{"tools":{}},"serverInfo":{"name":"koharu-editor","version":"1"}})
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": context.host.tools().into_iter().map(|tool| json!({
            "name":tool.name,"description":tool.description,"inputSchema":tool.parameters
        })).collect::<Vec<_>>()}),
        "tools/call" => {
            let params = &request["params"];
            let name = params["name"].as_str().unwrap_or_default().to_owned();
            if !context.host.tools().iter().any(|tool| tool.name == name) {
                return Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32602,"message":"Unknown Koharu tool"}})).into_response();
            }
            let call_id = Uuid::new_v4().to_string();
            let _ = context
                .events
                .send(Event::ToolStarted {
                    run: context.run,
                    call_id: call_id.clone(),
                    name: name.clone(),
                })
                .await;
            let result = context
                .host
                .invoke(
                    ToolCall {
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: params
                            .get("arguments")
                            .cloned()
                            .unwrap_or_else(|| json!({}))
                            .to_string(),
                    },
                    &context.control,
                )
                .await;
            let (content, changed, error, output) = match result {
                Ok(invocation) => {
                    let output = invocation.value.to_string();
                    let mut content = vec![json!({"type":"text","text":output})];
                    for image in invocation.images {
                        if let Some((header, data)) = image.data_url.split_once(";base64,") {
                            if let Some(mime) = header.strip_prefix("data:") {
                                content.push(json!({"type":"image","mimeType":mime,"data":data}));
                            }
                        }
                    }
                    (content, invocation.changed, false, output)
                }
                Err(error) => {
                    let message = format!("{error:#}");
                    (
                        vec![json!({"type":"text","text":message})],
                        false,
                        true,
                        message,
                    )
                }
            };
            let _ = context
                .events
                .send(Event::ToolFinished {
                    run: context.run,
                    call_id,
                    name,
                    changed,
                    output,
                })
                .await;
            json!({"content":content,"isError":error})
        }
        _ => return Json(
            json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
        )
        .into_response(),
    };
    Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response()
}
