use anyhow::{Context as _, Result, bail, ensure};
use koharu_agent::{Invocation, ToolCall, ToolRejection};
use koharu_scene::{EntityId, Snapshot};
use serde::Serialize;
use serde_json::{Value, json};
use specta::Type;

use super::{context, scope};

const MAX_PENDING: usize = 32;
const MAX_ATTEMPTS: u32 = 3;

#[derive(Clone, Debug, Serialize, Type)]
pub struct AgentFailure {
    pub id: String,
    pub tool: String,
    pub page: Option<String>,
    pub error: String,
    pub attempts: u32,
}

#[cfg(test)]
mod tests {
    use super::*;
    use koharu_scene::{At, PageDraft, Session};
    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            call_id: "test".into(),
            name: name.into(),
            arguments: args.to_string(),
        }
    }
    async fn pages() -> Result<(Snapshot, EntityId, EntityId)> {
        let mut session = Session::memory().await?;
        let mut ids = Vec::new();
        let patch = session.snapshot().patch(|edit| {
            for label in ["016", "017"] {
                ids.push(edit.add_page(PageDraft::new(label, 100.0, 100.0), At::End)?);
            }
            Ok(())
        })?;
        let snapshot = session.commit(patch).await?.snapshot;
        Ok((snapshot, ids[0], ids[1]))
    }
    fn failed(ledger: &mut Failures, snapshot: &Snapshot, attempt: &Attempt) -> String {
        let target = Target::capture(snapshot, &attempt.call).unwrap();
        let error = ledger
            .finish(
                attempt,
                target,
                Err(anyhow::anyhow!("native rejected this request")),
            )
            .unwrap_err();
        error
            .downcast_ref::<ToolRejection>()
            .unwrap()
            .failure_id
            .clone()
    }

    #[tokio::test]
    async fn other_page_success_does_not_clear_failure_and_retry_cannot_retarget() -> Result<()> {
        let (snapshot, p16, p17) = pages().await?;
        let mut ledger = Failures::default();
        let bad = Attempt::parse(call("inspect_page", json!({"page_id":p16,"limit":51})))?;
        assert!(
            serde_json::from_str::<super::super::context::InspectPage>(&bad.call.arguments)
                .is_err()
        );
        let id = failed(&mut ledger, &snapshot, &bad);
        let other = Attempt::parse(call("inspect_page", json!({"page":p17})))?;
        let target = Target::capture(&snapshot, &other.call)?;
        ledger.validate(&other, &target)?;
        ledger.finish(&other, target, Invocation::read(json!({"page":p17})))?;
        assert!(ledger.ensure_complete().is_err());
        assert!(ledger.snapshot()[0].page.as_ref().unwrap().contains("016"));
        let wrong = Attempt::parse(call("inspect_page", json!({"page":p17,"retry_of":id})))?;
        assert!(
            ledger
                .validate(&wrong, &Target::capture(&snapshot, &wrong.call)?)
                .is_err()
        );
        let right = Attempt::parse(call(
            "inspect_page",
            json!({"page":p16,"limit":20,"retry_of":id}),
        ))?;
        let target = Target::capture(&snapshot, &right.call)?;
        ledger.validate(&right, &target)?;
        ledger.finish(&right, target, Invocation::read(json!({"page":p16})))?;
        ledger.ensure_complete()?;
        Ok(())
    }

    #[tokio::test]
    async fn retries_retain_known_scope_boundaries_windows_and_pipeline_operations() -> Result<()> {
        let (snapshot, p16, p17) = pages().await?;
        for (name, bad, wrong, right) in [
            (
                "set_edit_scope",
                json!({"first_page":p16}),
                json!({"first_page":p17,"last_page":p16}),
                json!({"first_page":p16,"last_page":p17}),
            ),
            (
                "inspect_page",
                json!({"page":p16,"offset":20,"limit":51}),
                json!({"page":p16,"offset":0,"limit":20}),
                json!({"page":p16,"offset":20,"limit":20}),
            ),
            (
                "run_pipeline",
                json!({"pages":[p16],"operation":"translation"}),
                json!({"pages":[p16],"operation":"ocr"}),
                json!({"pages":[p16],"operation":"translation","resume":true}),
            ),
        ] {
            let mut ledger = Failures::default();
            let bad = Attempt::parse(call(name, bad))?;
            let id = failed(&mut ledger, &snapshot, &bad);
            let mut wrong = wrong;
            wrong["retry_of"] = json!(id);
            let wrong = Attempt::parse(call(name, wrong))?;
            assert!(
                ledger
                    .validate(&wrong, &Target::capture(&snapshot, &wrong.call)?)
                    .is_err()
            );
            let mut right = right;
            right["retry_of"] = json!(id);
            let right = Attempt::parse(call(name, right))?;
            ledger.validate(&right, &Target::capture(&snapshot, &right.call)?)?;
        }
        Ok(())
    }

    #[tokio::test]
    async fn repeated_rejections_are_bounded_and_retained_for_attention() -> Result<()> {
        let (snapshot, p16, _) = pages().await?;
        let mut ledger = Failures::default();
        let bad = Attempt::parse(call("inspect_page", json!({"page":p16,"limit":51})))?;
        let id = failed(&mut ledger, &snapshot, &bad);
        for _ in 1..MAX_ATTEMPTS {
            assert_eq!(failed(&mut ledger, &snapshot, &bad), id);
        }
        assert!(ledger.exhausted());
        assert_eq!(ledger.snapshot().len(), 1);
        assert_eq!(ledger.snapshot()[0].attempts, MAX_ATTEMPTS);
        assert!(ledger.ensure_complete().is_err());
        assert!(
            ledger
                .validate(&bad, &Target::capture(&snapshot, &bad.call)?)
                .is_err()
        );
        Ok(())
    }
}

#[derive(Default, Clone)]
pub(super) struct Target {
    entities: Vec<EntityId>,
    pub page: Option<String>,
    facets: Vec<(String, Value)>,
}

impl Target {
    // Capture only existing native targets. This never repairs or accepts invalid arguments.
    pub fn capture(snapshot: &Snapshot, call: &ToolCall) -> Result<Self> {
        let args: Value = serde_json::from_str(&call.arguments).unwrap_or(Value::Null);
        let mut ids = Vec::new();
        let mut pages = Vec::new();
        let mut fields = vec!["page", "pages", "element", "elements"];
        if call.name == "set_edit_scope" {
            fields.extend(["first_page", "last_page"]);
        }
        for field in fields {
            let values = match &args[field] {
                Value::Array(values) => values.clone(),
                Value::String(_) => vec![args[field].clone()],
                _ => vec![],
            };
            for value in values {
                let Ok(id) = serde_json::from_value::<EntityId>(value) else {
                    continue;
                };
                let Ok(page) = scope::page_for(snapshot, id) else {
                    continue;
                };
                // A layer supplied in a page-only field is invalid input, not
                // a layer mutation target. Recovery must retain its owning page
                // without requiring the same mistaken layer ID in that field.
                let target = if matches!(field, "page" | "pages" | "first_page" | "last_page") {
                    page
                } else {
                    id
                };
                for entity in [target, page] {
                    if !ids.contains(&entity) {
                        ids.push(entity);
                    }
                }
                if !pages.contains(&page) {
                    pages.push(page);
                }
            }
        }
        if call.name == "repair_text"
            && let Ok(id) = serde_json::from_value::<EntityId>(args["layout"]["element"].clone())
            && let Ok(page) = scope::page_for(snapshot, id)
        {
            for entity in [id, page] {
                if !ids.contains(&entity) {
                    ids.push(entity);
                }
            }
            if !pages.contains(&page) {
                pages.push(page);
            }
        }
        // IDs under unknown keys are diagnostic only; native argument validation still rejects them.
        if ids.is_empty() {
            for value in args
                .as_object()
                .into_iter()
                .flat_map(|fields| fields.values())
            {
                let values = match value {
                    Value::Array(values) => values.clone(),
                    _ => vec![value.clone()],
                };
                for value in values {
                    let Ok(id) = serde_json::from_value::<EntityId>(value) else {
                        continue;
                    };
                    let Ok(page) = scope::page_for(snapshot, id) else {
                        continue;
                    };
                    for entity in [id, page] {
                        if !ids.contains(&entity) {
                            ids.push(entity);
                        }
                    }
                    if !pages.contains(&page) {
                        pages.push(page);
                    }
                }
            }
        }
        let mut facets = Vec::new();
        if matches!(
            call.name.as_str(),
            "inspect_project"
                | "inspect_chapters"
                | "inspect_chapter"
                | "inspect_page"
                | "audit_readability"
                | "list_fonts"
        ) {
            if args.get("offset").is_none() {
                facets.push(("offset".into(), json!(0)));
            } else if let Some(offset) = args["offset"].as_u64() {
                facets.push(("offset".into(), json!(offset)));
            }
        }
        if call.name == "inspect_chapter" {
            if let Some(chapter) = serde_json::from_value(args["chapter"].clone())
                .ok()
                .and_then(|id| snapshot.chapter(id).ok().flatten())
            {
                facets.push(("chapter".into(), json!(chapter.id)));
            }
        }
        if call.name == "set_edit_scope" {
            let valid = ["first_page", "last_page"]
                .into_iter()
                .filter(|field| {
                    serde_json::from_value::<EntityId>(args[*field].clone())
                        .ok()
                        .is_some_and(|id| snapshot.page(id).is_ok())
                })
                .collect::<Vec<_>>();
            for field in valid {
                facets.push((field.into(), args[field].clone()));
            }
        }
        if call.name == "run_pipeline"
            && serde_json::from_value::<super::host::AgentPipelineOperation>(
                args["operation"].clone(),
            )
            .is_ok()
        {
            facets.push(("operation".into(), args["operation"].clone()));
        }
        let page = if let Some(first) = pages.first() {
            let label = context::Locations::new(snapshot)?.label(*first)?;
            Some(if pages.len() > 1 {
                format!("{label} · +{}", pages.len() - 1)
            } else {
                label
            })
        } else {
            None
        };
        Ok(Self {
            entities: ids,
            page,
            facets,
        })
    }
}

pub(super) struct Attempt {
    pub call: ToolCall,
    pub retry_of: Option<String>,
}

impl Attempt {
    pub fn raw(call: ToolCall) -> Self {
        Self {
            call,
            retry_of: None,
        }
    }

    pub fn parse(mut call: ToolCall) -> Result<Self> {
        let mut value: Value = serde_json::from_str(&call.arguments)?;
        let fields = value
            .as_object_mut()
            .context("tool arguments must be an object")?;
        let retry_of = fields
            .remove("retry_of")
            .map(|id| {
                id.as_str()
                    .filter(|id| !id.is_empty())
                    .map(str::to_owned)
                    .context("retry_of must be the nonempty ID of an unresolved tool call")
            })
            .transpose()?;
        call.arguments = value.to_string();
        Ok(Self { call, retry_of })
    }
}

struct Entry {
    failure: AgentFailure,
    arguments: String,
    target: Target,
}

#[derive(Default)]
pub(super) struct Failures {
    entries: Vec<Entry>,
    exhausted: bool,
}

fn compact(message: &str) -> String {
    let text = message.chars().take(1600).collect::<String>();
    if text.len() < message.len() {
        format!("{text}…")
    } else {
        text
    }
}

impl Failures {
    pub fn snapshot(&self) -> Vec<AgentFailure> {
        self.entries
            .iter()
            .map(|entry| entry.failure.clone())
            .collect()
    }

    pub fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub fn validate(&self, attempt: &Attempt, target: &Target) -> Result<()> {
        ensure!(
            !self.exhausted,
            "Agent needs attention after repeated unresolved tool failures"
        );
        if let Some(id) = &attempt.retry_of {
            let entry = self
                .entries
                .iter()
                .find(|entry| entry.failure.id == *id)
                .context("retry_of does not identify an unresolved call in this run")?;
            ensure!(
                entry.failure.tool == attempt.call.name,
                "Retry the original tool {} for failure {id}",
                entry.failure.tool
            );
            ensure!(
                entry
                    .target
                    .entities
                    .iter()
                    .all(|id| target.entities.contains(id)),
                "Retry must retain the original page and any explicit element targets for failure {id}; correct a layer used as a page ID to its owning page, never a different page"
            );
            let complete_scope = entry
                .target
                .facets
                .iter()
                .any(|(field, _)| field == "first_page")
                && entry
                    .target
                    .facets
                    .iter()
                    .any(|(field, _)| field == "last_page");
            ensure!(
                entry.target.facets.iter().all(|facet| (complete_scope
                    && matches!(facet.0.as_str(), "first_page" | "last_page"))
                    || target.facets.contains(facet)),
                "Retry must retain the original chapter, pagination position, scope boundary and pipeline operation for failure {id}"
            );
        }
        Ok(())
    }

    pub fn finish(
        &mut self,
        attempt: &Attempt,
        target: Target,
        result: Result<Invocation>,
    ) -> Result<Invocation> {
        let result = match result {
            Err(error) if error.is::<super::readability::ReviewRequired>() => error
                .downcast_ref::<super::readability::ReviewRequired>()
                .unwrap()
                .feedback(),
            Err(error) if error.is::<super::readability::DeclinedEdit>() => error
                .downcast_ref::<super::readability::DeclinedEdit>()
                .unwrap()
                .feedback(),
            result => result,
        };
        match result {
            Ok(result) => {
                if let Some(id) = &attempt.retry_of {
                    self.entries.retain(|entry| entry.failure.id != *id);
                }
                Ok(result)
            }
            Err(error) => {
                let message = format!("{error:#}");
                let existing = attempt
                    .retry_of
                    .as_ref()
                    .and_then(|id| {
                        self.entries.iter().position(|entry| {
                            entry.failure.id == *id && entry.failure.tool == attempt.call.name
                        })
                    })
                    .or_else(|| {
                        self.entries.iter().position(|entry| {
                            entry.failure.tool == attempt.call.name
                                && entry.arguments == attempt.call.arguments
                        })
                    });
                let index = if let Some(index) = existing {
                    index
                } else {
                    // Stop admission before bounded bookkeeping could silently drop a failed call.
                    if self.entries.len() >= MAX_PENDING {
                        self.exhausted = true;
                    }
                    if self.entries.len() > MAX_PENDING {
                        return Err(error
                            .context("Agent has too many unresolved calls and needs attention"));
                    }
                    self.entries.push(Entry {
                        failure: AgentFailure {
                            id: uuid::Uuid::new_v4().to_string(),
                            tool: attempt.call.name.clone(),
                            page: target.page.clone(),
                            error: String::new(),
                            attempts: 0,
                        },
                        arguments: attempt.call.arguments.clone(),
                        target,
                    });
                    self.entries.len() - 1
                };
                let entry = &mut self.entries[index];
                entry.failure.attempts = entry.failure.attempts.saturating_add(1);
                entry.failure.error = compact(&message);
                if entry.failure.attempts >= MAX_ATTEMPTS {
                    self.exhausted = true;
                }
                Err(ToolRejection {
                    failure_id: entry.failure.id.clone(),
                    message,
                }
                .into())
            }
        }
    }

    pub fn ensure_complete(&self) -> Result<()> {
        if self.entries.is_empty() {
            return Ok(());
        }
        let pending = self.entries.iter().take(8).map(|entry|json!({
            "retry_of":entry.failure.id,"tool":entry.failure.tool,"page":entry.failure.page,
            "error":entry.failure.error,"targets":entry.target.entities.iter().take(20).collect::<Vec<_>>(),
            "target_count":entry.target.entities.len(),
        })).collect::<Vec<_>>();
        bail!(
            "Agent has {} unresolved tool calls. {} Retry each ORIGINAL tool using its retry_of ID and corrected arguments, retaining the original page and any explicit element targets. Correct wrong entity types using the reported owning page ID. Other pages' successful calls do not resolve these failures. Inspect IDs/schema as needed; do not broaden the edit scope. Pending preview (resolve these to reveal any remainder): {}",
            self.entries.len(),
            if self.exhausted {
                "Recovery limit reached; user attention is required."
            } else {
                "The request is not complete."
            },
            json!(pending)
        );
    }
}
