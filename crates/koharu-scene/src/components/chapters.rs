//! Ordered chapter roster, per-chapter translation setting, and the evolving wiki.

use std::collections::HashSet;

use revision::revisioned;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{
    ChapterId, EntityId, Error, Result,
    component::{Component, ValidationContext},
};

const MAX_TITLE_BYTES: usize = 512;
const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

#[revisioned(revision = 1)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct Chapter {
    pub id: ChapterId,
    pub title: String,
    pub pages: Vec<EntityId>,
    pub summary: Option<String>,
    pub setting: Option<String>,
    pub summarize: bool,
}

impl Chapter {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            id: ChapterId::new(),
            title: title.into(),
            pages: Vec::new(),
            summary: None,
            setting: None,
            summarize: true,
        }
    }
}

/// Project-level chapter roster. Entry order is chapter order; page order
/// inside an entry is the project page order filtered by membership.
#[revisioned(revision = 1)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct Chapters {
    pub entries: Vec<Chapter>,
}

impl Chapters {
    pub fn chapter(&self, id: ChapterId) -> Option<&Chapter> {
        self.entries.iter().find(|chapter| chapter.id == id)
    }

    pub fn chapter_mut(&mut self, id: ChapterId) -> Option<&mut Chapter> {
        self.entries.iter_mut().find(|chapter| chapter.id == id)
    }

    pub fn chapter_for_page(&self, page: EntityId) -> Option<&Chapter> {
        self.entries
            .iter()
            .find(|chapter| chapter.pages.contains(&page))
    }

    pub fn remove_page(&mut self, page: EntityId) {
        for chapter in &mut self.entries {
            chapter.pages.retain(|candidate| *candidate != page);
        }
    }

    pub fn prune_empty(&mut self) {
        self.entries.retain(|chapter| !chapter.pages.is_empty());
    }
}

impl Component for Chapters {
    const KIND: &'static str = "dev.koharu.chapters";

    fn record_refs(&self) -> Vec<EntityId> {
        self.entries
            .iter()
            .flat_map(|chapter| chapter.pages.iter().copied())
            .collect()
    }

    fn validate(&self, context: &ValidationContext<'_>) -> Result<()> {
        let mut ids = HashSet::with_capacity(self.entries.len());
        let mut pages = HashSet::new();
        for chapter in &self.entries {
            if !ids.insert(chapter.id) {
                return Err(Error::invalid("chapter ids contain duplicates"));
            }
            validate_title(&chapter.title)?;
            if chapter
                .summary
                .as_ref()
                .is_some_and(|summary| summary.len() > MAX_DOCUMENT_BYTES)
            {
                return Err(Error::invalid("chapter summary exceeds the size limit"));
            }
            if chapter
                .setting
                .as_ref()
                .is_some_and(|setting| setting.len() > MAX_DOCUMENT_BYTES)
            {
                return Err(Error::invalid("chapter setting exceeds the size limit"));
            }
            for page in &chapter.pages {
                if !pages.insert(*page) {
                    return Err(Error::invalid("a page belongs to multiple chapters"));
                }
                if !context.contains_entity(*page) {
                    return Err(Error::invalid("chapter references a missing page"));
                }
            }
        }
        Ok(())
    }
}

/// The categories a new series starts with. `style` carries the translation
/// conventions a series settles on (voice, profanity, honorifics, SFX), which
/// no other file owns and which must survive between chapters. A series may grow
/// more: the roster is a list, not a fixed record, because a long run
/// accumulates factions, relationship maps, or power systems that do not fit.
pub const DEFAULT_WIKI_SLUGS: [&str; 5] = [
    "world",
    "characters",
    "glossary",
    "timeline",
    "style",
];

const MAX_WIKI_SLUG_BYTES: usize = 64;
const MAX_WIKI_FILES: usize = 64;

/// Wiki pages are kept whole so a summarizer can rewrite one file at a time
/// without inventing a second document format.
#[revisioned(revision = 1)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct Wiki {
    pub files: Vec<WikiFile>,
}

#[revisioned(revision = 1)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct WikiFile {
    pub slug: String,
    pub body: String,
}

impl Default for Wiki {
    fn default() -> Self {
        Self {
            files: DEFAULT_WIKI_SLUGS
                .iter()
                .map(|slug| WikiFile {
                    slug: (*slug).to_owned(),
                    body: String::new(),
                })
                .collect(),
        }
    }
}

impl Wiki {
    pub fn file(&self, slug: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|file| file.slug == slug)
            .map(|file| file.body.as_str())
    }

    pub fn file_mut(&mut self, slug: &str) -> Option<&mut String> {
        self.files
            .iter_mut()
            .find(|file| file.slug == slug)
            .map(|file| &mut file.body)
    }

    /// Replaces the body of an existing category or appends a new one, which is
    /// how a summarizer adds a category the series grew into.
    pub fn upsert(&mut self, slug: &str, body: String) -> Result<()> {
        validate_slug(slug)?;
        match self.file_mut(slug) {
            Some(existing) => *existing = body,
            None => {
                if self.files.len() >= MAX_WIKI_FILES {
                    return Err(Error::invalid("the wiki has too many files"));
                }
                self.files.push(WikiFile {
                    slug: slug.to_owned(),
                    body,
                });
            }
        }
        Ok(())
    }

    pub fn insert(&mut self, slug: &str) -> Result<()> {
        validate_slug(slug)?;
        if self.file(slug).is_some() {
            return Err(Error::invalid("the wiki already has that file"));
        }
        if self.files.len() >= MAX_WIKI_FILES {
            return Err(Error::invalid("the wiki has too many files"));
        }
        self.files.push(WikiFile {
            slug: slug.to_owned(),
            body: String::new(),
        });
        Ok(())
    }

    pub fn remove(&mut self, slug: &str) -> bool {
        let before = self.files.len();
        self.files.retain(|file| file.slug != slug);
        self.files.len() != before
    }

    /// Adds any default category this wiki is missing. A wiki saved before a
    /// default existed would otherwise reach the summarizer without it.
    pub fn ensure_defaults(&mut self) {
        for slug in DEFAULT_WIKI_SLUGS {
            if self.file(slug).is_none() {
                self.files.push(WikiFile {
                    slug: slug.to_owned(),
                    body: String::new(),
                });
            }
        }
    }
}

impl Component for Wiki {
    const KIND: &'static str = "dev.koharu.wiki";

    fn validate(&self, _context: &ValidationContext<'_>) -> Result<()> {
        let mut slugs = HashSet::with_capacity(self.files.len());
        for file in &self.files {
            validate_slug(&file.slug)?;
            if !slugs.insert(file.slug.as_str()) {
                return Err(Error::invalid("wiki slugs contain duplicates"));
            }
            if file.body.len() > MAX_DOCUMENT_BYTES {
                return Err(Error::invalid("wiki file exceeds the size limit"));
            }
        }
        Ok(())
    }
}

fn validate_slug(slug: &str) -> Result<()> {
    let valid = !slug.is_empty()
        && slug.len() <= MAX_WIKI_SLUG_BYTES
        && slug
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(Error::invalid(
            "a wiki slug is lowercase ASCII letters, digits, or underscores",
        ))
    }
}

fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES || title.contains('\0') {
        Err(Error::invalid("chapter title is empty or too long"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn validate(chapters: &Chapters, page: EntityId) -> Result<()> {
        let exists = |id: EntityId| id == page;
        let blob = |_: crate::BlobId| true;
        chapters.validate(&ValidationContext::new(&exists, &blob))
    }

    #[test]
    fn a_page_cannot_belong_to_two_chapters() {
        let page = EntityId::new();
        let mut first = Chapter::new("1");
        first.pages.push(page);
        let mut second = Chapter::new("2");
        second.pages.push(page);
        let chapters = Chapters {
            entries: vec![first, second],
        };
        assert!(validate(&chapters, page).is_err());
    }

    #[test]
    fn deleting_a_page_prunes_empty_chapters() {
        let page = EntityId::new();
        let mut chapter = Chapter::new("1");
        chapter.pages.push(page);
        let mut chapters = Chapters {
            entries: vec![chapter],
        };
        chapters.remove_page(page);
        chapters.prune_empty();
        assert!(chapters.entries.is_empty());
    }

    #[test]
    fn a_summarizer_can_add_a_wiki_category_beyond_the_defaults() {
        let mut wiki = Wiki::default();
        assert_eq!(wiki.files.len(), DEFAULT_WIKI_SLUGS.len());
        wiki.upsert("factions", "Guilds".to_owned()).unwrap();
        wiki.upsert("world", "Rewritten".to_owned()).unwrap();
        assert_eq!(wiki.file("factions"), Some("Guilds"));
        assert_eq!(wiki.file("world"), Some("Rewritten"));
        assert!(wiki.upsert("Not A Slug", String::new()).is_err());
        assert!(wiki.insert("factions").is_err());
        assert!(wiki.remove("factions"));
    }

    #[test]
    fn a_wiki_saved_before_style_existed_gains_it() {
        let mut wiki = Wiki {
            files: vec![WikiFile {
                slug: "world".to_owned(),
                body: "The city".to_owned(),
            }],
        };
        wiki.ensure_defaults();
        assert_eq!(wiki.files.len(), DEFAULT_WIKI_SLUGS.len());
        assert_eq!(wiki.file("world"), Some("The city"));
        assert_eq!(wiki.file("style"), Some(""));
    }
}
