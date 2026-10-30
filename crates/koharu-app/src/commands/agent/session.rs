use koharu_agent::{Event, Role, RunId};
use parking_lot::Mutex;
use serde::Serialize;
use specta::Type;
use tokio::sync::watch;

const TEXT_BYTES: usize = 32 * 1024;
const MESSAGE_LIMIT: usize = 20;
const PROJECT_LIMIT: usize = 4;

#[derive(Clone, Debug, Default, Serialize, Type)]
pub struct AgentSnapshot {
    #[specta(type = f64)]
    pub sequence: u64,
    pub conversations: Vec<AgentConversation>,
    pub run: Option<AgentRun>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentConversation {
    pub project: String,
    pub messages: Vec<AgentMessage>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentMessage {
    pub id: String,
    pub role: Role,
    pub text: String,
    pub reasoning: String,
    pub error: bool,
    pub state: Option<AgentRunState>,
}

#[derive(Clone, Copy, Debug, Serialize, Type, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunState {
    Running,
    Finished,
    Failed,
    Stopped,
}

#[derive(Clone, Copy, Debug, Serialize, Type, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    Starting,
    Thinking,
    Tool,
    Waiting,
    Pipeline,
    Responding,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentRun {
    pub id: RunId,
    pub project: String,
    pub state: AgentRunState,
    pub phase: AgentPhase,
    pub tool: Option<String>,
    pub page: Option<String>,
    pub element: Option<String>,
    pub stage: Option<koharu_pipeline::Stage>,
    pub changes: u32,
    pub error: Option<String>,
}

pub(super) struct SessionStore {
    snapshot: Mutex<AgentSnapshot>,
    pub changed: watch::Sender<u64>,
}

impl Default for SessionStore {
    fn default() -> Self {
        let (changed, _) = watch::channel(0);
        Self {
            snapshot: Mutex::new(AgentSnapshot::default()),
            changed,
        }
    }
}

fn capped(text: &str) -> String {
    let mut end = text.len().min(TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if end < text.len() {
        format!("{}…", &text[..end])
    } else {
        text.to_owned()
    }
}

impl SessionStore {
    pub fn reference(&self, project: &str) -> String {
        let snapshot = self.snapshot.lock();
        let messages = snapshot
            .conversations
            .iter()
            .find(|chat| chat.project == project)
            .map(|chat| {
                chat.messages
                    .iter()
                    .rev()
                    .filter(|message| !message.text.is_empty())
                    .take(4)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let value = messages.into_iter().rev().map(|message| serde_json::json!({"role": message.role, "text": message.text.chars().take(1500).collect::<String>()})).collect::<Vec<_>>();
        if value.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&value).expect("conversation reference serializes")
        }
    }

    pub fn snapshot(&self) -> AgentSnapshot {
        self.snapshot.lock().clone()
    }

    fn update(&self, update: impl FnOnce(&mut AgentSnapshot)) {
        let mut snapshot = self.snapshot.lock();
        update(&mut snapshot);
        snapshot.sequence = snapshot.sequence.saturating_add(1);
        self.changed.send_replace(snapshot.sequence);
    }

    pub fn begin(&self, id: RunId, project: String, prompt: &str) {
        self.update(|snapshot| {
            let existing = snapshot
                .conversations
                .iter()
                .position(|chat| chat.project == project);
            let mut chat = existing
                .map(|i| snapshot.conversations.remove(i))
                .unwrap_or_else(|| AgentConversation {
                    project: project.clone(),
                    messages: vec![],
                });
            chat.messages.push(AgentMessage {
                id: format!("{id}-user"),
                role: Role::User,
                text: capped(prompt),
                reasoning: String::new(),
                error: false,
                state: None,
            });
            chat.messages.push(AgentMessage {
                id: id.to_string(),
                role: Role::Assistant,
                text: String::new(),
                reasoning: String::new(),
                error: false,
                state: Some(AgentRunState::Running),
            });
            if chat.messages.len() > MESSAGE_LIMIT {
                chat.messages.drain(..chat.messages.len() - MESSAGE_LIMIT);
            }
            while chat.messages.len() > 2
                && chat
                    .messages
                    .iter()
                    .map(|message| message.text.len() + message.reasoning.len())
                    .sum::<usize>()
                    > 192 * 1024
            {
                chat.messages.drain(..2);
            }
            snapshot.conversations.push(chat);
            if snapshot.conversations.len() > PROJECT_LIMIT {
                snapshot.conversations.remove(0);
            }
            snapshot.run = Some(AgentRun {
                id,
                project,
                state: AgentRunState::Running,
                phase: AgentPhase::Starting,
                tool: None,
                page: None,
                element: None,
                stage: None,
                changes: 0,
                error: None,
            });
        });
    }

    pub fn event(&self, event: Event) {
        self.update(|snapshot| {
            let id = match &event {
                Event::Started { run }
                | Event::TextDelta { run, .. }
                | Event::ReasoningDelta { run, .. }
                | Event::ToolStarted { run, .. }
                | Event::ToolFinished { run, .. }
                | Event::Completed { run, .. }
                | Event::Failed { run, .. }
                | Event::Cancelled { run } => *run,
            };
            let Some(run) = snapshot
                .run
                .as_mut()
                .filter(|run| run.id == id && run.state == AgentRunState::Running)
            else {
                return;
            };
            let Some(message) = snapshot
                .conversations
                .iter_mut()
                .find(|chat| chat.project == run.project)
                .and_then(|chat| chat.messages.last_mut())
            else {
                return;
            };
            match event {
                Event::Started { .. } => {}
                Event::TextDelta { delta, .. } => {
                    if message.text.len() < TEXT_BYTES {
                        message.text = capped(&format!("{}{delta}", message.text));
                    }
                    run.phase = AgentPhase::Responding;
                }
                Event::ReasoningDelta { delta, .. } => {
                    if message.reasoning.len() < TEXT_BYTES {
                        message.reasoning = capped(&format!("{}{delta}", message.reasoning));
                    }
                    run.phase = AgentPhase::Thinking;
                }
                Event::ToolStarted { name, .. } => {
                    run.phase = AgentPhase::Tool;
                    run.tool = Some(name);
                    run.page = None;
                    run.element = None;
                    run.stage = None;
                    run.error = None;
                }
                Event::ToolFinished {
                    changed, output, ..
                } => {
                    run.phase = AgentPhase::Tool;
                    run.error = match serde_json::from_str::<serde_json::Value>(&output) {
                        Ok(value) if value["ok"] == false => value["error"].as_str().map(capped),
                        Err(_) => Some(capped(&output)),
                        _ => None,
                    };
                    if changed {
                        run.changes = run.changes.saturating_add(1);
                    }
                }
                Event::Completed { message: text, .. } => {
                    message.text = capped(&text);
                    run.state = AgentRunState::Finished;
                    run.error = None;
                    message.state = Some(AgentRunState::Finished);
                }
                Event::Failed { message: text, .. } => {
                    message.text = capped(&text);
                    message.error = true;
                    run.error = Some(capped(&text));
                    run.state = AgentRunState::Failed;
                    message.state = Some(AgentRunState::Failed);
                }
                Event::Cancelled { .. } => {
                    run.state = AgentRunState::Stopped;
                    message.state = Some(AgentRunState::Stopped);
                }
            }
        });
    }

    pub fn progress(
        &self,
        id: RunId,
        phase: AgentPhase,
        page: Option<String>,
        element: Option<String>,
        stage: Option<koharu_pipeline::Stage>,
    ) {
        self.update(|snapshot| {
            if let Some(run) = snapshot
                .run
                .as_mut()
                .filter(|run| run.id == id && run.state == AgentRunState::Running)
            {
                run.phase = phase;
                run.page = page;
                run.element = element;
                run.stage = stage;
            }
        });
    }

    pub fn clear(&self) {
        self.update(|snapshot| {
            snapshot.conversations.clear();
            snapshot.run = None;
        });
    }
    pub fn dismiss(&self) {
        self.update(|snapshot| {
            if snapshot
                .run
                .as_ref()
                .is_some_and(|run| run.state != AgentRunState::Running)
            {
                snapshot.run = None;
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_failures_are_visible_without_counting_edits_and_replay_is_bounded() {
        let store = SessionStore::default();
        let id = RunId::new();
        store.begin(id, "A".into(), &"x".repeat(100_000));
        store.event(Event::ToolStarted {
            run: id,
            call_id: "1".into(),
            name: "set_translation".into(),
        });
        store.event(Event::ToolFinished {
            run: id,
            call_id: "1".into(),
            name: "set_translation".into(),
            changed: false,
            output: r#"{"ok":false,"error":"stale revision"}"#.into(),
        });
        let snapshot = store.snapshot();
        assert_eq!(
            snapshot.run.as_ref().unwrap().error.as_deref(),
            Some("stale revision")
        );
        assert_eq!(snapshot.run.unwrap().changes, 0);
        store.event(Event::Completed {
            run: id,
            message: "Recovered after a fresh read".into(),
        });
        assert!(store.snapshot().run.unwrap().error.is_none());
        let reference = store.reference("A");
        assert!(reference.len() < 16 * 1024);
        assert!(reference.contains("Recovered after a fresh read"));
        assert!(store.reference("B").is_empty());
    }

    #[test]
    fn reconnect_snapshots_keep_project_history_and_ignore_late_events() {
        let store = SessionStore::default();
        let a = RunId::new();
        store.begin(a, "A".into(), "edit A");
        for _ in 0..2000 {
            store.event(Event::ReasoningDelta {
                run: a,
                delta: "summary ".into(),
            });
        }
        store.event(Event::ToolStarted {
            run: a,
            call_id: "1".into(),
            name: "inspect_page".into(),
        });
        store.progress(a, AgentPhase::Tool, Some("007".into()), None, None);
        let before = store.snapshot();
        assert_eq!(before.run.unwrap().page.as_deref(), Some("007"));
        store.event(Event::Completed {
            run: a,
            message: "A edited".into(),
        });
        let b = RunId::new();
        store.begin(b, "B".into(), "edit B");
        store.event(Event::TextDelta {
            run: a,
            delta: "late".into(),
        });
        let snapshot = store.snapshot();
        assert!(snapshot.sequence > before.sequence);
        assert_eq!(
            snapshot.conversations[0].messages.last().unwrap().text,
            "A edited"
        );
        assert!(
            snapshot.conversations[1]
                .messages
                .last()
                .unwrap()
                .text
                .is_empty()
        );
        store.event(Event::Cancelled { run: b });
        store.event(Event::ToolFinished {
            run: b,
            call_id: "2".into(),
            name: "edit".into(),
            changed: true,
            output: "{}".into(),
        });
        assert_eq!(store.snapshot().run.unwrap().changes, 0);
    }
}
