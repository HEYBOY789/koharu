//! Ordered chapter roster, per-chapter translation setting, and the evolving wiki.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{ChapterId, Error, PageId, Project, Result, Uuid};

const MAX_TITLE_BYTES: usize = 512;
const MAX_DOCUMENT_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct Chapter {
    pub id: ChapterId,
    pub title: String,
    pub pages: Vec<PageId>,
    pub summary: Option<String>,
    pub setting: Option<String>,
    pub summarize: bool,
    #[serde(default)]
    pub translated: bool,
}

impl Chapter {
    #[must_use]
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            id: ChapterId(Uuid::now_v7()),
            title: title.into(),
            pages: Vec::new(),
            summary: None,
            setting: None,
            summarize: true,
            translated: false,
        }
    }
}

/// Project-level chapter roster. Entry order is chapter order; page order
/// inside an entry is the project page order filtered by membership.
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

    pub fn chapter_for_page(&self, page: PageId) -> Option<&Chapter> {
        self.entries
            .iter()
            .find(|chapter| chapter.pages.contains(&page))
    }

    pub fn remove_page(&mut self, page: PageId) {
        for chapter in &mut self.entries {
            chapter.pages.retain(|candidate| *candidate != page);
        }
    }

    pub fn prune_empty(&mut self) {
        self.entries.retain(|chapter| !chapter.pages.is_empty());
    }
}

impl Chapters {
    pub(super) fn validate(&self, project: &Project) -> Result<()> {
        let mut ids = HashSet::with_capacity(self.entries.len());
        let mut pages = HashSet::new();
        for chapter in &self.entries {
            if !ids.insert(chapter.id) {
                return Err(Error::rejected("chapter ids contain duplicates"));
            }
            validate_title(&chapter.title)?;
            if chapter
                .summary
                .as_ref()
                .is_some_and(|summary| summary.len() > MAX_DOCUMENT_BYTES)
            {
                return Err(Error::rejected("chapter summary exceeds the size limit"));
            }
            if chapter
                .setting
                .as_ref()
                .is_some_and(|setting| setting.len() > MAX_DOCUMENT_BYTES)
            {
                return Err(Error::rejected("chapter setting exceeds the size limit"));
            }
            for page in &chapter.pages {
                if !pages.insert(*page) {
                    return Err(Error::rejected("a page belongs to multiple chapters"));
                }
                if project.page(*page).is_err() {
                    return Err(Error::rejected("chapter references a missing page"));
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
pub const DEFAULT_WIKI_SLUGS: [&str; 5] = ["world", "characters", "glossary", "timeline", "style"];

const MAX_WIKI_SLUG_BYTES: usize = 64;
const MAX_WIKI_FILES: usize = 64;

/// Wiki pages are kept whole so a summarizer can rewrite one file at a time
/// without inventing a second document format.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct Wiki {
    pub files: Vec<WikiFile>,
}

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
                    return Err(Error::rejected("the wiki has too many files"));
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
            return Err(Error::rejected("the wiki already has that file"));
        }
        if self.files.len() >= MAX_WIKI_FILES {
            return Err(Error::rejected("the wiki has too many files"));
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

impl Wiki {
    pub(super) fn validate(&self) -> Result<()> {
        let mut slugs = HashSet::with_capacity(self.files.len());
        for file in &self.files {
            validate_slug(&file.slug)?;
            if !slugs.insert(file.slug.as_str()) {
                return Err(Error::rejected("wiki slugs contain duplicates"));
            }
            if file.body.len() > MAX_DOCUMENT_BYTES {
                return Err(Error::rejected("wiki file exceeds the size limit"));
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
        Err(Error::rejected(
            "a wiki slug is lowercase ASCII letters, digits, or underscores",
        ))
    }
}

fn validate_title(title: &str) -> Result<()> {
    if title.trim().is_empty() || title.len() > MAX_TITLE_BYTES || title.contains('\0') {
        Err(Error::rejected("chapter title is empty or too long"))
    } else {
        Ok(())
    }
}
