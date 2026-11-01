use anyhow::{Context as _, Result, ensure};
use koharu_scene::{EntityId, Snapshot};
use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct EditRange {
    /// First page ID, inclusive, obtained from inspection. Not a page label or ordinal.
    pub first_page: String,
    /// Last page ID, inclusive. Use the same ID for a single page.
    pub last_page: String,
}

impl EditRange {
    pub fn resolve(&self, snapshot: &Snapshot) -> Result<Vec<EntityId>> {
        let pages = snapshot.pages().map(|p| p.id()).collect::<Vec<_>>();
        let first = pages
            .iter()
            .position(|id| id.to_string() == self.first_page)
            .context("first_page is not a page in this project; inspect chapters/pages for IDs")?;
        let last = pages
            .iter()
            .position(|id| id.to_string() == self.last_page)
            .context("last_page is not a page in this project; inspect chapters/pages for IDs")?;
        ensure!(first <= last, "edit range is reversed");
        Ok(pages[first..=last].to_vec())
    }
}

pub(super) fn page_for(snapshot: &Snapshot, mut entity: EntityId) -> Result<EntityId> {
    loop {
        if snapshot.page(entity).is_ok() {
            return Ok(entity);
        }
        entity = snapshot
            .parent(entity)?
            .context("entity has no owning page")?;
    }
}

pub(super) fn check(
    snapshot: &Snapshot,
    allowed: Option<&[EntityId]>,
    entities: &[EntityId],
) -> Result<()> {
    let allowed = allowed.context("Choose the user's requested range with set_edit_scope before editing. Use first_page and last_page IDs from inspection; never guess IDs")?;
    ensure!(
        !entities.is_empty(),
        "an edit must target at least one page or layer"
    );
    for id in entities {
        ensure!(
            allowed.contains(&page_for(snapshot, *id)?),
            "target {id} is outside the selected edit scope; no edit was applied"
        );
    }
    Ok(())
}
