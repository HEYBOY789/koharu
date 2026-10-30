use revision::revisioned;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;

use crate::{ChapterId, Component, Error, Result, ValidationContext};

#[revisioned(revision = 1)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct ImportedSourceChapter {
    pub chapter: ChapterId,
    pub url: String,
    pub title: String,
    pub number: f64,
}

#[revisioned(revision = 1)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
pub struct ImportedSource {
    pub url: String,
    pub description: String,
    pub fields: Vec<String>,
    pub chapters: Vec<ImportedSourceChapter>,
}

impl Component for ImportedSource {
    const KIND: &'static str = "dev.koharu.imported-source";

    fn validate(&self, _context: &ValidationContext<'_>) -> Result<()> {
        let valid_url = |url: &str| {
            url.len() <= 8000 && (url.starts_with("https://") || url.starts_with("http://"))
        };
        let mut urls = HashSet::new();
        let mut chapters = HashSet::new();
        if !valid_url(&self.url)
            || self.description.len() > 120_000
            || self.fields.len() > 100
            || self.fields.iter().any(|field| field.len() > 12_000)
            || self.chapters.len() > 20_000
            || self.chapters.iter().any(|chapter| {
                !valid_url(&chapter.url)
                    || chapter.title.len() > 4000
                    || !chapter.number.is_finite()
                    || chapter.number < 0.0
                    || !urls.insert(&chapter.url)
                    || !chapters.insert(chapter.chapter)
            })
        {
            return Err(Error::invalid("invalid imported source metadata"));
        }
        Ok(())
    }
}
