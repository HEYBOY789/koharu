use anyhow::{Result, bail};
use koharu_scene::{EntityId, Snapshot};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::commands::project::Project;

const MAX_BYTES: usize = 64 * 1024;
const MAX_ITEMS: usize = 50;

fn default_limit() -> usize {
    20
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct Window {
    #[serde(default)]
    pub offset: usize,
    /// Number of entries to read, from 1 to 50. Large entries may shorten the batch.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

impl Default for Window {
    fn default() -> Self {
        Self {
            offset: 0,
            limit: default_limit(),
        }
    }
}

#[derive(Deserialize, JsonSchema)]
pub(super) struct InspectPage {
    pub page: String,
    #[serde(default)]
    pub offset: usize,
    /// Number of layer/region entries to read, from 1 to 50.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

pub(super) fn overview(project: &Project, configuration: Value, query: &Window) -> Result<Value> {
    validate(query.limit)?;
    let snapshot = project.snapshot();
    let total = snapshot.pages().len();
    let pages = snapshot
        .pages()
        .skip(query.offset)
        .take(query.limit)
        .map(|page| {
            let value = page.page()?;
            Ok(json!({
                "id": page.id(),
                "label": value.label,
                "chapter": snapshot.chapter_for(page.id())?.map(|chapter| chapter.id),
                "size": {"width": value.width, "height": value.height},
            }))
        })
        .collect::<Result<Vec<_>>>()?;
    window(
        json!({"project": project.info(), "configuration": configuration}),
        "pages",
        pages,
        total,
        query.offset,
    )
}

pub(super) fn page(
    snapshot: &Snapshot,
    page: EntityId,
    offset: usize,
    limit: usize,
) -> Result<Value> {
    validate(limit)?;
    let page = Project::page(snapshot, page)?;
    let total = page.layers.len() + page.regions.len();
    let metadata = json!({
        "id": page.id, "label": page.label, "chapter": page.chapter, "size": page.size,
        "layer_count": page.layers.len(), "region_count": page.regions.len(),
    });
    let entries = page
        .layers
        .into_iter()
        .map(|layer| json!({"layer": layer}))
        .chain(
            page.regions
                .into_iter()
                .map(|region| json!({"region": region})),
        )
        .skip(offset)
        .take(limit)
        .collect();
    window(
        json!({"page": metadata, "revision": snapshot.revision()}),
        "entries",
        entries,
        total,
        offset,
    )
}

pub(super) fn fonts(fonts: Vec<String>, query: &Window) -> Result<Value> {
    validate(query.limit)?;
    let total = fonts.len();
    let entries = fonts
        .into_iter()
        .skip(query.offset)
        .take(query.limit)
        .map(Value::String)
        .collect();
    window(json!({}), "fonts", entries, total, query.offset)
}

fn validate(limit: usize) -> Result<()> {
    if !(1..=MAX_ITEMS).contains(&limit) {
        bail!("Inspection limit must be from 1 to {MAX_ITEMS}");
    }
    Ok(())
}

fn window(
    mut response: Value,
    field: &str,
    mut entries: Vec<Value>,
    total: usize,
    offset: usize,
) -> Result<Value> {
    loop {
        let end = offset.saturating_add(entries.len());
        response["pagination"] = json!({
            "offset": offset, "returned": entries.len(), "total": total,
            "next_offset": if end < total { Some(end) } else { None },
        });
        response[field] = Value::Array(entries);
        if serde_json::to_vec(&response)?.len() <= MAX_BYTES {
            return Ok(response);
        }
        entries = match response[field].take() {
            Value::Array(entries) => entries,
            _ => unreachable!("inspection entries are an array"),
        };
        if entries.len() <= 1 {
            bail!(
                "An inspection entry or its metadata exceeds the {MAX_BYTES}-byte response budget and cannot be returned without truncation"
            );
        }
        entries.pop();
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use koharu_scene::{At, Authored, Origin, PageDraft, SourceText, TextLayout, TextLayoutKind};

    use super::*;

    #[tokio::test]
    async fn large_projects_have_bounded_overviews_and_lossless_page_inspection() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let mut project = Project::create(
            "large fixture".into(),
            directory.path().join("large.khrproj"),
        )
        .await?;
        let mut page_ids = Vec::new();
        let text = "Tôi đang kiểm tra font. ".repeat(500);
        let patch = project.snapshot().patch(|edit| {
            for index in 0..80 {
                let page = edit.add_page(
                    PageDraft::new(format!("page {index}"), 1000.0, 1600.0),
                    At::End,
                )?;
                page_ids.push(page);
                for _ in 0..12 {
                    let content = edit.add_text_content(page, At::End)?;
                    edit.set(
                        content,
                        &SourceText {
                            text: Authored::user(text.clone()),
                            language: None,
                        },
                    )?;
                    edit.add_text_layer(
                        page,
                        At::End,
                        content,
                        &TextLayout {
                            origin: Origin::User,
                            kind: TextLayoutKind::Paragraph,
                            angle_degrees: Some(0.0),
                        },
                    )?;
                }
            }
            Ok(())
        })?;
        project.session.commit(patch).await?;
        project.select_page(page_ids[0])?;
        let snapshot = project.snapshot();
        let old_pages = page_ids
            .iter()
            .map(|id| Project::page(&snapshot, *id))
            .collect::<Result<Vec<_>>>()?;
        assert!(serde_json::to_vec(&old_pages)?.len() > 9 * 1024 * 1024);
        drop(old_pages);

        let first = overview(&project, json!({}), &Window::default())?;
        assert!(serde_json::to_vec(&first)?.len() < 16 * 1024);
        assert_eq!(
            first["project"]["active_page"],
            serde_json::to_value(page_ids[0])?
        );
        assert_eq!(first["pagination"]["total"], 80);
        assert_eq!(first["pages"].as_array().unwrap().len(), 20);
        let mut seen_pages = HashSet::new();
        let mut offset = 0;
        loop {
            let response = overview(&project, json!({}), &Window { offset, limit: 20 })?;
            assert!(serde_json::to_vec(&response)?.len() <= MAX_BYTES);
            for item in response["pages"].as_array().unwrap() {
                assert!(item.get("layers").is_none());
                assert!(seen_pages.insert(item["id"].as_str().unwrap().to_owned()));
            }
            let Some(next) = response["pagination"]["next_offset"].as_u64() else {
                break;
            };
            assert!(next as usize > offset);
            offset = next as usize;
        }
        assert_eq!(seen_pages.len(), 80);

        let expected = Project::page(&snapshot, page_ids[0])?;
        let mut entries = Vec::new();
        offset = 0;
        loop {
            let response = page(&snapshot, page_ids[0], offset, 20)?;
            assert!(serde_json::to_vec(&response)?.len() <= MAX_BYTES);
            entries.extend(response["entries"].as_array().unwrap().iter().cloned());
            let Some(next) = response["pagination"]["next_offset"].as_u64() else {
                break;
            };
            assert!(next as usize > offset);
            offset = next as usize;
        }
        assert!(
            offset > 0,
            "fixture must exercise the byte budget rather than the count limit"
        );
        assert_eq!(
            entries,
            expected
                .layers
                .into_iter()
                .map(|layer| json!({"layer": layer}))
                .collect::<Vec<_>>()
        );
        assert!(
            overview(
                &project,
                json!({}),
                &Window {
                    offset: 0,
                    limit: 0
                }
            )
            .is_err()
        );
        assert!(page(&snapshot, page_ids[0], 0, 51).is_err());
        assert_eq!(
            page(&snapshot, page_ids[0], 500, 20)?["pagination"]["next_offset"],
            Value::Null
        );
        Ok(())
    }

    #[test]
    fn font_windows_preserve_names_and_never_exceed_the_response_budget() -> Result<()> {
        let names = (0..70)
            .map(|index| format!("Font {index}"))
            .collect::<Vec<_>>();
        let mut actual = Vec::new();
        let mut offset = 0;
        loop {
            let response = fonts(names.clone(), &Window { offset, limit: 20 })?;
            actual.extend(
                response["fonts"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| name.as_str().unwrap().to_owned()),
            );
            let Some(next) = response["pagination"]["next_offset"].as_u64() else {
                break;
            };
            offset = next as usize;
        }
        assert_eq!(actual, names);
        assert!(fonts(vec!["x".repeat(MAX_BYTES)], &Window::default()).is_err());
        Ok(())
    }
}
