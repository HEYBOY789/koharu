use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Type, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MangaDexFilter {
    #[default]
    Any,
    Either,
    NoTarget,
    Behind,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, Type, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MangaDexCacheMode {
    #[default]
    Online,
    Refresh,
    Offline,
}

#[derive(Clone, Copy, Debug, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum MangaDexOptionsKind {
    Tags,
    People,
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct MangaDexSearch {
    pub title: String,
    pub sort: String,
    pub reference: String,
    pub target: String,
    pub filter: MangaDexFilter,
    pub original_languages: Vec<String>,
    pub translated_languages: Vec<String>,
    pub content_ratings: Vec<String>,
    pub demographics: Vec<String>,
    pub statuses: Vec<String>,
    pub authors: Vec<String>,
    pub artists: Vec<String>,
    pub included_tags: Vec<String>,
    pub excluded_tags: Vec<String>,
    pub include_all: bool,
    pub exclude_all: bool,
    pub year: Option<u32>,
    pub has_chapters: bool,
    pub offset: u32,
    pub cache: MangaDexCacheMode,
}

impl Default for MangaDexSearch {
    fn default() -> Self {
        Self {
            title: String::new(),
            sort: String::new(),
            reference: "en".into(),
            target: "vi".into(),
            filter: MangaDexFilter::Any,
            original_languages: vec![],
            translated_languages: vec![],
            content_ratings: vec![],
            demographics: vec![],
            statuses: vec![],
            authors: vec![],
            artists: vec![],
            included_tags: vec![],
            excluded_tags: vec![],
            include_all: false,
            exclude_all: false,
            year: None,
            has_chapters: false,
            offset: 0,
            cache: MangaDexCacheMode::Online,
        }
    }
}

pub(crate) fn language(value: &str) -> bool {
    (2..=5).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')
}

pub(super) fn comparison_languages(reference: &str, target: &str) -> Result<()> {
    ensure!(
        language(target) && (reference == "auto" || reference == "original" || language(reference)),
        "invalid MangaDex comparison languages"
    );
    Ok(())
}

impl MangaDexSearch {
    pub fn query(&self) -> Result<(u32, Vec<(String, String)>)> {
        comparison_languages(&self.reference, &self.target)?;
        let batch = if self.filter == MangaDexFilter::Any {
            100
        } else {
            10
        };
        ensure!(
            self.title.chars().count() <= 200 && u64::from(self.offset) + u64::from(batch) <= 10000,
            "invalid MangaDex search range"
        );
        ensure!(
            self.year.is_none_or(|year| (1000..=9999).contains(&year)),
            "year must have four digits"
        );
        ensure!(
            self.included_tags
                .iter()
                .all(|tag| !self.excluded_tags.contains(tag)),
            "a tag cannot be included and excluded"
        );
        let mut query = vec![
            ("limit".into(), batch.to_string()),
            ("offset".into(), self.offset.to_string()),
            ("includes[]".into(), "author".into()),
            ("includes[]".into(), "artist".into()),
            ("includes[]".into(), "cover_art".into()),
        ];
        if !self.title.trim().is_empty() {
            query.push(("title".into(), self.title.trim().into()));
        }
        if !self.sort.is_empty() {
            let (field, direction) = self
                .sort
                .split_once('.')
                .ok_or_else(|| anyhow::anyhow!("invalid MangaDex sort"))?;
            ensure!(
                [
                    "title",
                    "year",
                    "createdAt",
                    "latestUploadedChapter",
                    "followedCount",
                    "relevance",
                    "rating"
                ]
                .contains(&field)
                    && ["asc", "desc"].contains(&direction),
                "invalid MangaDex sort"
            );
            ensure!(
                field != "relevance" || !self.title.trim().is_empty(),
                "best match requires a title"
            );
            query.push((format!("order[{field}]"), direction.into()));
        }
        for (key, values, choices) in [
            (
                "contentRating[]",
                &self.content_ratings,
                &["safe", "suggestive", "erotica", "pornographic"][..],
            ),
            (
                "publicationDemographic[]",
                &self.demographics,
                &["shounen", "shoujo", "seinen", "josei", "none"][..],
            ),
            (
                "status[]",
                &self.statuses,
                &["ongoing", "completed", "hiatus", "cancelled"][..],
            ),
        ] {
            ensure!(
                values.len() <= choices.len()
                    && values.iter().all(|value| choices.contains(&value.as_str())),
                "invalid {key}"
            );
            if key == "contentRating[]" && values.is_empty() {
                query.extend(choices.iter().map(|value| (key.into(), (*value).into())));
            }
            for value in values {
                query.push((key.into(), value.clone()));
            }
        }
        for (key, values) in [
            ("originalLanguage[]", &self.original_languages),
            ("availableTranslatedLanguage[]", &self.translated_languages),
        ] {
            ensure!(
                values.len() <= 100
                    && values.iter().all(|value| (2..=5).contains(&value.len())
                        && value.bytes().all(|b| b.is_ascii_lowercase() || b == b'-')),
                "invalid {key}"
            );
            if key == "availableTranslatedLanguage[]" && !self.has_chapters {
                continue;
            }
            for value in values {
                query.push((key.into(), value.clone()));
            }
        }
        for (key, values) in [
            ("authors[]", &self.authors),
            ("artists[]", &self.artists),
            ("includedTags[]", &self.included_tags),
            ("excludedTags[]", &self.excluded_tags),
        ] {
            ensure!(
                values.len() <= 100
                    && values
                        .iter()
                        .all(|value| uuid::Uuid::parse_str(value).is_ok()),
                "invalid {key}"
            );
            for value in values {
                query.push((key.into(), value.clone()));
            }
        }
        if !self.included_tags.is_empty() {
            query.push((
                "includedTagsMode".into(),
                if self.include_all { "AND" } else { "OR" }.into(),
            ));
        }
        if !self.excluded_tags.is_empty() {
            query.push((
                "excludedTagsMode".into(),
                if self.exclude_all { "AND" } else { "OR" }.into(),
            ));
        }
        if let Some(year) = self.year {
            query.push(("year".into(), year.to_string()));
        }
        if self.has_chapters {
            query.push(("hasAvailableChapters".into(), "true".into()));
        }
        Ok((batch, query))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mangadex_query_validates_and_encodes_advanced_filters_without_extra_reads() {
        let id = "00000000-0000-0000-0000-000000000001".to_string();
        let mut input = MangaDexSearch {
            title: "  A & B  ".into(),
            sort: "rating.desc".into(),
            year: Some(2023),
            original_languages: vec!["ja".into(), "ko".into()],
            translated_languages: vec!["en".into()],
            content_ratings: vec!["safe".into()],
            demographics: vec!["none".into()],
            statuses: vec!["completed".into()],
            authors: vec![id.clone()],
            included_tags: vec![id.clone()],
            has_chapters: true,
            include_all: true,
            ..Default::default()
        };
        let (batch, query) = input.query().unwrap();
        assert_eq!(batch, 100);
        for pair in [
            ("title", "A & B"),
            ("order[rating]", "desc"),
            ("publicationDemographic[]", "none"),
            ("includedTagsMode", "AND"),
            ("hasAvailableChapters", "true"),
            ("year", "2023"),
            ("availableTranslatedLanguage[]", "en"),
        ] {
            assert!(query.contains(&(pair.0.into(), pair.1.into())));
        }
        input.has_chapters = false;
        assert!(
            !input
                .query()
                .unwrap()
                .1
                .iter()
                .any(|(key, _)| key == "availableTranslatedLanguage[]")
        );
        input.excluded_tags.push(id);
        assert!(input.query().is_err());
        input.excluded_tags.clear();
        input.sort = "relevance.desc".into();
        input.title.clear();
        assert!(input.query().is_err());
        input.sort.clear();
        input.offset = 9950;
        assert!(input.query().is_err());
        input.filter = MangaDexFilter::Either;
        assert_eq!(input.query().unwrap().0, 10);
    }
}
