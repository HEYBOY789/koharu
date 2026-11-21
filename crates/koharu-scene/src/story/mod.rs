//! Chapters and publishing data, stored with the pages they describe.

mod chapters;
mod imported_source;
mod publication;

use std::collections::BTreeMap;

use derive_more::{Display, FromStr};
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{Error, Hash, PageId, Project, Result, Uuid};

pub use chapters::{Chapter, Chapters, DEFAULT_WIKI_SLUGS, Wiki, WikiFile};
pub use imported_source::{ImportedSource, ImportedSourceChapter};
pub use publication::{PublicationGenre, PublicationMetadata};

#[derive(
    Clone,
    Copy,
    Debug,
    Display,
    FromStr,
    Eq,
    Hash,
    Ord,
    PartialEq,
    PartialOrd,
    Deserialize,
    Serialize,
    Type,
)]
#[serde(transparent)]
pub struct ChapterId(pub Uuid);

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize, Type)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectMetadata {
    pub chapters: Chapters,
    pub wiki: Wiki,
    pub page_labels: BTreeMap<PageId, String>,
    pub publication: Option<PublicationMetadata>,
    pub source: Option<ImportedSource>,
    pub cover: Option<Hash>,
}

impl ProjectMetadata {
    pub(crate) fn validate(&self, project: &Project) -> Result<()> {
        self.chapters.validate(project)?;
        self.wiki.validate()?;
        if self.page_labels.iter().any(|(page, label)| {
            project.page(*page).is_err() || label.len() > 1024 || label.contains('\0')
        }) {
            return Err(Error::rejected("invalid page label or missing page"));
        }
        if let Some(publication) = &self.publication {
            publication.validate()?;
        }
        if let Some(source) = &self.source {
            source.validate()?;
        }
        Ok(())
    }

    pub(crate) fn remove_page(&mut self, page: PageId) {
        self.page_labels.remove(&page);
        self.chapters.remove_page(page);
        self.chapters.prune_empty();
        if let Some(source) = &mut self.source {
            source
                .chapters
                .retain(|entry| self.chapters.chapter(entry.chapter).is_some());
        }
    }
}
