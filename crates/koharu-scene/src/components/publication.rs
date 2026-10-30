use revision::revisioned;
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::{Component, Error, Result, ValidationContext};

#[revisioned(revision = 1)]
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct PublicationGenre {
    pub name: String,
    pub slug: String,
}

#[revisioned(revision = 1)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PublicationMetadata {
    pub title: String,
    pub slug: String,
    pub alternative_titles: String,
    pub author: String,
    pub release_year: Option<u16>,
    pub country: String,
    pub status: String,
    pub genres: Vec<PublicationGenre>,
}

impl Default for PublicationMetadata {
    fn default() -> Self {
        Self {
            title: String::new(),
            slug: String::new(),
            alternative_titles: String::new(),
            author: String::new(),
            release_year: None,
            country: String::new(),
            status: "ongoing".to_owned(),
            genres: Vec::new(),
        }
    }
}

impl Component for PublicationMetadata {
    const KIND: &'static str = "dev.koharu.publication-metadata";

    fn validate(&self, _context: &ValidationContext<'_>) -> Result<()> {
        let slug_valid = |value: &str| {
            !value.is_empty()
                && value.len() <= 120
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
                && !value.starts_with('-')
                && !value.ends_with('-')
                && !value.contains("--")
        };
        if self.title.trim().is_empty()
            || self.title.len() > 1200
            || !slug_valid(&self.slug)
            || self.author.len() > 1200
            || self.alternative_titles.len() > 8000
            || self.country.len() > 600
            || self
                .release_year
                .is_some_and(|year| !(1000..=9999).contains(&year))
            || !matches!(self.status.as_str(), "ongoing" | "completed" | "hiatus")
            || self.genres.len() > 30
            || self.genres.iter().any(|genre| {
                genre.name.trim().is_empty() || genre.name.len() > 400 || !slug_valid(&genre.slug)
            })
        {
            return Err(Error::invalid("invalid publication metadata"));
        }
        Ok(())
    }
}
