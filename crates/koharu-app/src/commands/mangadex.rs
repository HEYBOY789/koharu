mod cache;
mod metadata;
mod query;
pub use metadata::{MangaDexMetadataPage, MangaDexMetadataRequest};
pub(super) use query::language as valid_language;
pub use query::{MangaDexCacheMode, MangaDexFilter, MangaDexOptionsKind, MangaDexSearch};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use specta::Type;
use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};
use tauri::State;

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexEntry {
    pub id: String,
    pub title: String,
    pub url: String,
    pub author: String,
    pub artist: String,
    pub original_language: String,
    pub year: Option<u32>,
    pub created_at: Option<String>,
    pub latest_uploaded_chapter: Option<String>,
    pub publication_status: String,
    pub content_rating: String,
    pub description: String,
    pub tags: Vec<String>,
    pub cover_url: Option<String>,
    pub chapter_numbers_reset: bool,
    pub comparison: Option<MangaDexComparison>,
}

#[derive(Clone, Debug, Deserialize, Type)]
pub struct MangaDexChapterRequest {
    pub id: String,
    pub original_language: String,
    pub reference: String,
    pub target: String,
    pub chapter_numbers_reset: bool,
    pub cache: MangaDexCacheMode,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexChapterSummary {
    pub language: String,
    pub latest: Option<String>,
    pub count: u32,
    pub unnumbered: u32,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexComparison {
    pub reference_language: String,
    pub target_language: String,
    pub chapters: Vec<MangaDexChapterSummary>,
    pub total_available: Option<u32>,
    pub latest_available: Option<String>,
    pub missing: u32,
    pub status: Option<MangaDexFilter>,
    pub checked_at: u32,
    pub network_requests: u32,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexPage {
    pub manga: Vec<MangaDexEntry>,
    pub scanned: u32,
    pub skipped: u32,
    pub total: u32,
    pub next_offset: Option<u32>,
    pub capped: bool,
    pub checked_at: u32,
    pub network_requests: u32,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexOption {
    pub id: String,
    pub name: String,
    pub group: String,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexOptions {
    pub options: Vec<MangaDexOption>,
    pub checked_at: u32,
}

pub(crate) struct MangaDex {
    api: cache::Api,
}

impl Default for MangaDex {
    fn default() -> Self {
        Self {
            api: cache::Api::new(None),
        }
    }
}

impl MangaDex {
    pub fn persistent() -> Result<Self> {
        let file = dirs::cache_dir()
            .context("local cache directory is unavailable")?
            .join("Koharu/mangadex/v1.json");
        Ok(Self {
            api: cache::Api::new(Some(file)),
        })
    }

    async fn options(
        &self,
        kind: MangaDexOptionsKind,
        name: String,
        mode: MangaDexCacheMode,
    ) -> Result<MangaDexOptions> {
        let query = match kind {
            MangaDexOptionsKind::Tags => vec![],
            MangaDexOptionsKind::People => {
                ensure!(
                    (2..=100).contains(&name.trim().chars().count()),
                    "enter at least two characters for an author/artist"
                );
                vec![
                    ("name".into(), name.trim().into()),
                    ("limit".into(), "20".into()),
                    ("order[name]".into(), "asc".into()),
                ]
            }
        };
        let path = match kind {
            MangaDexOptionsKind::Tags => "manga/tag",
            MangaDexOptionsKind::People => "author",
        };
        let read = self.api.get(path, &query, mode).await?;
        let data = read.value["data"]
            .as_array()
            .context("invalid MangaDex options")?;
        let mut options = Vec::new();
        for item in data {
            let id = item["id"].as_str().context("MangaDex option has no ID")?;
            uuid::Uuid::parse_str(id)?;
            let attributes = &item["attributes"];
            let name = attributes["name"]["en"]
                .as_str()
                .or_else(|| attributes["name"].as_str())
                .context("MangaDex option has no name")?;
            options.push(MangaDexOption {
                id: id.into(),
                name: name.into(),
                group: attributes["group"].as_str().unwrap_or("").into(),
            });
        }
        options.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(MangaDexOptions {
            options,
            checked_at: read.checked as u32,
        })
    }

    async fn chapters(
        &self,
        id: &str,
        language: Option<&str>,
        reset: bool,
        mode: MangaDexCacheMode,
        checked: &mut u64,
        requests: &mut u32,
    ) -> Result<Chapters> {
        let mut query = vec![("includeUnavailable".into(), "0".into())];
        if let Some(language) = language {
            query.push(("translatedLanguage[]".into(), language.into()));
        }
        let read = self
            .api
            .get(&format!("manga/{id}/aggregate"), &query, mode)
            .await?;
        *checked = (*checked).min(read.checked);
        *requests += u32::from(read.network);
        Chapters::parse(&read.value, reset)
    }

    async fn compare(
        &self,
        input: MangaDexChapterRequest,
        filter: MangaDexFilter,
    ) -> Result<(Option<MangaDexComparison>, u64, u32)> {
        uuid::Uuid::parse_str(&input.id).context("invalid MangaDex manga ID")?;
        query::comparison_languages(&input.reference, &input.target)?;
        ensure!(
            query::language(&input.original_language),
            "invalid original language"
        );
        let reset = input.chapter_numbers_reset;
        let mut checked = cache::now();
        let mut requests = 0;
        let mut languages = BTreeMap::new();
        languages.insert(
            input.target.clone(),
            self.chapters(
                &input.id,
                Some(&input.target),
                reset,
                input.cache,
                &mut checked,
                &mut requests,
            )
            .await?,
        );
        let target_available = languages[&input.target].available;
        if (filter == MangaDexFilter::NoTarget && target_available > 0)
            || (filter == MangaDexFilter::Behind && target_available == 0)
        {
            return Ok((None, checked, requests));
        }
        let references = match input.reference.as_str() {
            "auto" => vec!["en", input.original_language.as_str()],
            "original" => vec![input.original_language.as_str()],
            language => vec![language],
        };
        let mut selected = None;
        for language in &references {
            if !languages.contains_key(*language) {
                languages.insert(
                    (*language).to_string(),
                    self.chapters(
                        &input.id,
                        Some(language),
                        reset,
                        input.cache,
                        &mut checked,
                        &mut requests,
                    )
                    .await?,
                );
            }
            if input.reference != "auto"
                || classify(&languages[*language], &languages[&input.target]).is_some()
            {
                selected = Some((*language).to_string());
                break;
            }
        }
        let reference_language = selected.unwrap_or_else(|| {
            references
                .iter()
                .find(|language| {
                    languages
                        .get(**language)
                        .is_some_and(|chapters| chapters.available > 0)
                })
                .unwrap_or(&references[0])
                .to_string()
        });
        let source = &languages[&reference_language];
        let target = &languages[&input.target];
        let missing = source.numbered.difference(&target.numbered).count() as u32;
        let status = classify(source, target);
        if filter != MangaDexFilter::Any
            && (status.is_none() || (filter != MangaDexFilter::Either && status != Some(filter)))
        {
            return Ok((None, checked, requests));
        }
        if !languages.contains_key(&input.original_language) {
            languages.insert(
                input.original_language.clone(),
                self.chapters(
                    &input.id,
                    Some(&input.original_language),
                    reset,
                    input.cache,
                    &mut checked,
                    &mut requests,
                )
                .await?,
            );
        }
        let all = if languages[&input.original_language].available == 0 {
            Some(
                self.chapters(
                    &input.id,
                    None,
                    reset,
                    input.cache,
                    &mut checked,
                    &mut requests,
                )
                .await?,
            )
        } else {
            None
        };
        Ok((
            Some(MangaDexComparison {
                reference_language,
                target_language: input.target,
                total_available: all
                    .as_ref()
                    .map(|chapters| chapters.numbered.len() as u32 + chapters.unnumbered),
                latest_available: all.as_ref().and_then(|chapters| chapters.latest(reset)),
                chapters: languages
                    .into_iter()
                    .map(|(language, chapters)| MangaDexChapterSummary {
                        language,
                        latest: chapters.latest(reset),
                        count: chapters.numbered.len() as u32 + chapters.unnumbered,
                        unnumbered: chapters.unnumbered,
                    })
                    .collect(),
                missing,
                status,
                checked_at: checked as u32,
                network_requests: requests,
            }),
            checked,
            requests,
        ))
    }

    async fn search(&self, input: MangaDexSearch) -> Result<MangaDexPage> {
        let (batch, query) = input.query()?;
        let read = self.api.get("manga", &query, input.cache).await?;
        let mut checked = read.checked;
        let mut requests = u32::from(read.network);
        let value = read.value;
        let data = value["data"]
            .as_array()
            .context("MangaDex returned no manga collection")?;
        ensure!(
            data.len() <= batch as usize,
            "MangaDex returned an unexpected page size"
        );
        let total = value["total"]
            .as_u64()
            .context("MangaDex returned no collection size")?
            .min(u64::from(u32::MAX)) as u32;
        let mut manga = Vec::new();
        for item in data {
            let id = item["id"].as_str().context("MangaDex manga has no ID")?;
            uuid::Uuid::parse_str(id).context("invalid MangaDex manga ID")?;
            let a = &item["attributes"];
            let original = a["originalLanguage"]
                .as_str()
                .context("MangaDex manga has no original language")?;
            ensure!(
                (2..=5).contains(&original.len())
                    && original
                        .bytes()
                        .all(|b| b.is_ascii_alphabetic() || b == b'-'),
                "invalid original language code"
            );
            let reset = a["chapterNumbersResetOnNewVolume"]
                .as_bool()
                .unwrap_or(false);
            let comparison = if input.filter == MangaDexFilter::Any {
                None
            } else {
                let (comparison, comparison_checked, comparison_requests) = self
                    .compare(
                        MangaDexChapterRequest {
                            id: id.into(),
                            original_language: original.into(),
                            reference: input.reference.clone(),
                            target: input.target.clone(),
                            chapter_numbers_reset: reset,
                            cache: input.cache,
                        },
                        input.filter,
                    )
                    .await?;
                checked = checked.min(comparison_checked);
                requests += comparison_requests;
                let Some(comparison) = comparison else {
                    continue;
                };
                Some(comparison)
            };
            let titles = a["title"]
                .as_object()
                .context("MangaDex manga has no title")?;
            let title = ["en", "vi", "ja-ro", "ko-ro", original]
                .iter()
                .find_map(|lang| titles.get(*lang).and_then(Value::as_str))
                .or_else(|| titles.values().find_map(Value::as_str))
                .context("MangaDex title is empty")?;
            let people = |kind: &str| {
                item["relationships"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|entry| entry["type"] == kind)
                    .filter_map(|entry| entry["attributes"]["name"].as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            manga.push(MangaDexEntry {
                id: id.into(),
                title: title.into(),
                url: format!("https://mangadex.org/title/{id}"),
                author: people("author"),
                artist: people("artist"),
                original_language: original.into(),
                year: a["year"].as_u64().and_then(|v| u32::try_from(v).ok()),
                created_at: a["createdAt"].as_str().map(String::from),
                latest_uploaded_chapter: a["latestUploadedChapter"]
                    .as_str()
                    .filter(|id| uuid::Uuid::parse_str(id).is_ok())
                    .map(String::from),
                publication_status: a["status"].as_str().unwrap_or("").into(),
                content_rating: a["contentRating"].as_str().unwrap_or("").into(),
                description: a["description"]["vi"]
                    .as_str()
                    .or_else(|| a["description"]["en"].as_str())
                    .unwrap_or("")
                    .chars()
                    .take(700)
                    .collect(),
                tags: a["tags"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|tag| tag["attributes"]["name"]["en"].as_str().map(String::from))
                    .collect(),
                cover_url: cover_url(item, id),
                chapter_numbers_reset: reset,
                comparison,
            });
        }
        let next = input.offset + data.len() as u32;
        Ok(MangaDexPage {
            skipped: data.len() as u32 - manga.len() as u32,
            manga,
            scanned: data.len() as u32,
            total,
            next_offset: (!data.is_empty() && next < total && next + batch <= 10000)
                .then_some(next),
            capped: next + batch > 10000 && total > next,
            checked_at: checked as u32,
            network_requests: requests,
        })
    }
}

fn cover_url(item: &Value, id: &str) -> Option<String> {
    let filename = item["relationships"]
        .as_array()?
        .iter()
        .find(|entry| entry["type"] == "cover_art")?["attributes"]["fileName"]
        .as_str()?;
    if filename.is_empty()
        || filename.len() > 200
        || !filename
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
    {
        return None;
    }
    Some(format!(
        "https://uploads.mangadex.org/covers/{id}/{filename}.256.jpg"
    ))
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_mangadex_metadata(
    input: Vec<MangaDexMetadataRequest>,
    cache: MangaDexCacheMode,
    client: State<'_, MangaDex>,
) -> std::result::Result<MangaDexMetadataPage, super::Error> {
    Ok(
        tokio::time::timeout(Duration::from_secs(45), client.metadata(input, cache))
            .await
            .context("MangaDex metadata lookup timed out")??,
    )
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_mangadex_chapters(
    input: MangaDexChapterRequest,
    client: State<'_, MangaDex>,
) -> std::result::Result<MangaDexComparison, super::Error> {
    Ok(tokio::time::timeout(
        Duration::from_secs(65),
        client.compare(input, MangaDexFilter::Any),
    )
    .await
    .context("MangaDex chapter comparison timed out")??
    .0
    .context("MangaDex returned no chapter comparison")?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn search_mangadex(
    input: MangaDexSearch,
    client: State<'_, MangaDex>,
) -> std::result::Result<MangaDexPage, super::Error> {
    Ok(
        tokio::time::timeout(Duration::from_secs(60), client.search(input))
            .await
            .context("MangaDex search timed out; try again or narrow the search")??,
    )
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_mangadex_options(
    kind: MangaDexOptionsKind,
    name: String,
    cache: MangaDexCacheMode,
    client: State<'_, MangaDex>,
) -> std::result::Result<MangaDexOptions, super::Error> {
    Ok(
        tokio::time::timeout(Duration::from_secs(25), client.options(kind, name, cache))
            .await
            .context("MangaDex lookup timed out")??,
    )
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Number {
    integer: String,
    fraction: String,
}
impl Number {
    fn parse(value: &str) -> Option<Self> {
        if value.len() > 64 {
            return None;
        }
        let (integer, fraction) = value.split_once('.').unwrap_or((value, ""));
        if integer.is_empty()
            || !integer.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        let integer = integer.trim_start_matches('0');
        Some(Self {
            integer: if integer.is_empty() { "0" } else { integer }.into(),
            fraction: fraction.trim_end_matches('0').into(),
        })
    }
    fn label(&self) -> String {
        if self.fraction.is_empty() {
            self.integer.clone()
        } else {
            format!("{}.{}", self.integer, self.fraction)
        }
    }
}
impl Ord for Number {
    fn cmp(&self, other: &Self) -> Ordering {
        self.integer
            .len()
            .cmp(&other.integer.len())
            .then_with(|| self.integer.cmp(&other.integer))
            .then_with(|| {
                let length = self.fraction.len().max(other.fraction.len());
                self.fraction
                    .bytes()
                    .chain(std::iter::repeat(b'0'))
                    .take(length)
                    .cmp(
                        other
                            .fraction
                            .bytes()
                            .chain(std::iter::repeat(b'0'))
                            .take(length),
                    )
            })
    }
}
impl PartialOrd for Number {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Chapter {
    volume: Number,
    chapter: Number,
}
#[derive(Default)]
struct Chapters {
    available: u32,
    unnumbered: u32,
    numbered: BTreeSet<Chapter>,
}

// MangaDex can encode numerically indexed maps (including chapter 0) as JSON arrays.
fn aggregate_entries(value: &Value) -> Option<Box<dyn Iterator<Item = &Value> + '_>> {
    match value {
        Value::Object(entries) => Some(Box::new(entries.values())),
        Value::Array(entries) => Some(Box::new(entries.iter())),
        _ => None,
    }
}

impl Chapters {
    fn parse(value: &Value, reset: bool) -> Result<Self> {
        let mut result = Self::default();
        let volumes =
            aggregate_entries(&value["volumes"]).context("invalid MangaDex aggregate volumes")?;
        for volume in volumes {
            let entries = aggregate_entries(&volume["chapters"])
                .context("invalid MangaDex aggregate chapters")?;
            for entry in entries {
                if entry["isUnavailable"]
                    .as_bool()
                    .context("MangaDex chapter has no availability flag")?
                {
                    continue;
                }
                result.available += 1;
                let number = entry["chapter"].as_str().and_then(Number::parse);
                let volume = if reset {
                    volume["volume"].as_str().and_then(Number::parse)
                } else {
                    Number::parse("0")
                };
                match (volume, number) {
                    (Some(volume), Some(chapter)) => {
                        result.numbered.insert(Chapter { volume, chapter });
                    }
                    _ => result.unnumbered += 1,
                }
            }
        }
        Ok(result)
    }
    fn latest(&self, reset: bool) -> Option<String> {
        self.numbered.last().map(|entry| {
            if reset {
                format!("V{} · C{}", entry.volume.label(), entry.chapter.label())
            } else {
                entry.chapter.label()
            }
        })
    }
}
fn classify(source: &Chapters, vietnamese: &Chapters) -> Option<MangaDexFilter> {
    if source.available == 0 {
        return None;
    }
    if vietnamese.available == 0 {
        return Some(MangaDexFilter::NoTarget);
    }
    match (source.numbered.last(), vietnamese.numbered.last()) {
        (Some(source), Some(vietnamese)) if source > vietnamese => Some(MangaDexFilter::Behind),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tauri::Url;

    fn aggregate(entries: &[(&str, &str, bool)], reset: bool) -> Chapters {
        let mut volumes = serde_json::Map::new();
        for (volume, chapter, unavailable) in entries {
            let value = volumes
                .entry(volume.to_string())
                .or_insert_with(|| json!({"volume":volume,"chapters":{}}));
            value["chapters"][chapter] = json!({"chapter":chapter,"isUnavailable":unavailable,"others":["duplicate-group-id"],"count":2});
        }
        Chapters::parse(&json!({"volumes":volumes}), reset).unwrap()
    }

    #[test]
    fn mangadex_compares_chapters_without_counting_duplicate_groups_or_deleted_releases() {
        let source = aggregate(
            &[
                ("none", "1", false),
                ("none", "10", false),
                ("none", "10.50", false),
                ("none", "999", true),
            ],
            false,
        );
        let vi = aggregate(&[("none", "1", false), ("none", "10", false)], false);
        assert_eq!(classify(&source, &vi), Some(MangaDexFilter::Behind));
        assert_eq!(source.latest(false).as_deref(), Some("10.5"));
        assert_eq!(source.numbered.difference(&vi.numbered).count(), 1);
        let empty = Chapters::parse(&json!({"volumes":[]}), false).unwrap();
        assert_eq!(classify(&source, &empty), Some(MangaDexFilter::NoTarget));
        assert_eq!(classify(&source, &source), None);
        assert_eq!(classify(&empty, &vi), None);
    }

    #[test]
    fn mangadex_accepts_chapter_zero_encoded_as_an_array() {
        // Hunter x Hunter's Vietnamese aggregate has an indexed chapter-zero volume.
        let value = json!({"result":"ok","volumes":{
            "none":{"volume":"none","chapters":{
                "410":{"chapter":"410","isUnavailable":false}
            }},
            "0":{"volume":"0","chapters":[
                {"chapter":"0","isUnavailable":false}
            ]}
        }});
        let chapters = Chapters::parse(&value, false).unwrap();
        assert_eq!(chapters.available, 2);
        assert_eq!(chapters.numbered.len(), 2);
        assert_eq!(chapters.unnumbered, 0);
        assert_eq!(chapters.latest(false).as_deref(), Some("410"));
        let indexed_volumes = json!({"volumes":[
            {"volume":"0","chapters":[{"chapter":"0","isUnavailable":false}]},
            {"volume":"1","chapters":[]}
        ]});
        let chapters = Chapters::parse(&indexed_volumes, true).unwrap();
        assert_eq!(chapters.available, 1);
        assert_eq!(chapters.latest(true).as_deref(), Some("V0 · C0"));
        assert!(Chapters::parse(&json!({"volumes":{"0":{"chapters":null}}}), false).is_err());
        assert!(Chapters::parse(&json!({"volumes":{"0":{"chapters":[{}]}}}), false).is_err());
    }

    #[test]
    fn mangadex_handles_volume_resets_decimal_numbers_and_unnumbered_extras() {
        let source = aggregate(
            &[
                ("1", "20", false),
                ("2", "1.2", false),
                ("2", "1.10", false),
                ("2", "none", false),
            ],
            true,
        );
        let vi = aggregate(&[("1", "20", false)], true);
        assert_eq!(classify(&source, &vi), Some(MangaDexFilter::Behind));
        assert_eq!(source.latest(true).as_deref(), Some("V2 · C1.2"));
        assert_eq!(source.unnumbered, 1);
        let unnumbered = aggregate(&[("none", "none", false)], false);
        assert_eq!(classify(&source, &unnumbered), None);
        assert_eq!(Number::parse("0010.500"), Number::parse("10.5"));
        assert!(Number::parse("2").unwrap() > Number::parse("1.9999").unwrap());
        assert!(Chapters::parse(&json!({"volumes":[1]}), false).is_err());
        assert!(
            Chapters::parse(&json!({"volumes":{"none":{"chapters":{"1":{}}}}}), false).is_err()
        );
    }

    #[tokio::test]
    async fn mangadex_compares_selected_languages_deduplicates_original_and_reuses_cache() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut languages = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = std::str::from_utf8(&buffer[..size]).unwrap();
                let url = Url::parse(&format!(
                    "http://localhost{}",
                    request.split_whitespace().nth(1).unwrap()
                ))
                .unwrap();
                let language = url
                    .query_pairs()
                    .find(|(key, _)| key == "translatedLanguage[]")
                    .unwrap()
                    .1
                    .to_string();
                assert!(
                    url.query_pairs()
                        .any(|(key, value)| key == "includeUnavailable" && value == "0")
                );
                let numbers = if language == "en" {
                    vec!["1", "2", "3.5"]
                } else {
                    vec!["1"]
                };
                let chapters = numbers.into_iter().map(|number| (number.to_string(), json!({"chapter":number,"isUnavailable":false,"count":3,"others":["duplicate"]}))).collect::<serde_json::Map<_, _>>();
                let body =
                    json!({"result":"ok","volumes":{"none":{"volume":"none","chapters":chapters}}})
                        .to_string();
                languages.push(language);
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            languages
        });
        let mut client = MangaDex::default();
        client.api.origin = Url::parse(&format!("http://{address}/")).unwrap();
        let mut input = MangaDexChapterRequest {
            id: "00000000-0000-0000-0000-000000000001".into(),
            original_language: "en".into(),
            reference: "en".into(),
            target: "fr".into(),
            chapter_numbers_reset: false,
            cache: MangaDexCacheMode::Online,
        };
        let result = client
            .compare(input.clone(), MangaDexFilter::Any)
            .await
            .unwrap()
            .0
            .unwrap();
        assert_eq!(result.reference_language, "en");
        assert_eq!(result.target_language, "fr");
        assert_eq!(result.chapters.len(), 2);
        assert_eq!(result.chapters[0].count, 3);
        assert_eq!(result.chapters[0].latest.as_deref(), Some("3.5"));
        assert_eq!(result.missing, 2);
        assert_eq!(result.network_requests, 2);
        assert_eq!(result.status, Some(MangaDexFilter::Behind));
        assert_eq!(
            client
                .compare(input.clone(), MangaDexFilter::Any)
                .await
                .unwrap()
                .0
                .unwrap()
                .network_requests,
            0
        );
        input.target = "en".into();
        let same = client
            .compare(input.clone(), MangaDexFilter::Any)
            .await
            .unwrap()
            .0
            .unwrap();
        assert_eq!(same.chapters.len(), 1);
        assert_eq!(same.missing, 0);
        assert_eq!(same.network_requests, 0);
        input.target = "../../bad".into();
        assert!(client.compare(input, MangaDexFilter::Any).await.is_err());
        assert_eq!(server.await.unwrap(), vec!["fr", "en"]);
    }

    #[tokio::test]
    async fn mangadex_missing_original_uses_unique_all_language_aggregate_totals() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requested = Vec::new();
            for _ in 0..4 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = std::str::from_utf8(&buffer[..size]).unwrap();
                let url = Url::parse(&format!(
                    "http://localhost{}",
                    request.split_whitespace().nth(1).unwrap()
                ))
                .unwrap();
                let language = url
                    .query_pairs()
                    .find(|(key, _)| key == "translatedLanguage[]")
                    .map(|(_, value)| value.to_string());
                let numbers = match language.as_deref() {
                    Some("ja") => vec![],
                    Some("en" | "vi") => vec!["1", "2"],
                    None => vec!["1", "2", "3.5"],
                    _ => panic!("unexpected language"),
                };
                requested.push(language);
                let chapters = numbers.into_iter().map(|number| (number.to_string(), json!({"chapter":number,"isUnavailable":false,"count":4,"others":["duplicate-language-or-group"]}))).collect::<serde_json::Map<_, _>>();
                let body = if chapters.is_empty() {
                    json!({"result":"ok","volumes":[]})
                } else {
                    json!({"result":"ok","volumes":{"none":{"volume":"none","chapters":chapters}}})
                }
                .to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            requested
        });
        let mut client = MangaDex::default();
        client.api.origin = Url::parse(&format!("http://{address}/")).unwrap();
        let input = MangaDexChapterRequest {
            id: "00000000-0000-0000-0000-000000000001".into(),
            original_language: "ja".into(),
            reference: "en".into(),
            target: "vi".into(),
            chapter_numbers_reset: false,
            cache: MangaDexCacheMode::Online,
        };
        let result = client
            .compare(input.clone(), MangaDexFilter::Any)
            .await
            .unwrap()
            .0
            .unwrap();
        assert_eq!(result.total_available, Some(3));
        assert_eq!(result.latest_available.as_deref(), Some("3.5"));
        assert_eq!(
            result
                .chapters
                .iter()
                .find(|chapter| chapter.language == "ja")
                .unwrap()
                .count,
            0
        );
        assert_eq!(result.missing, 0);
        assert_eq!(result.network_requests, 4);
        assert_eq!(
            client
                .compare(input, MangaDexFilter::Any)
                .await
                .unwrap()
                .0
                .unwrap()
                .network_requests,
            0
        );
        assert_eq!(
            server.await.unwrap(),
            vec![
                Some("vi".into()),
                Some("en".into()),
                Some("ja".into()),
                None
            ]
        );
    }

    #[tokio::test]
    async fn mangadex_http_cache_refresh_and_rate_limit_use_the_same_request_owner() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering as AtomicOrder},
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = calls.clone();
        let server = tokio::spawn(async move {
            for index in 0..3 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0; 4096];
                socket.read(&mut request).await.unwrap();
                observed.fetch_add(1, AtomicOrder::SeqCst);
                let (status, body, extra) = if index == 2 {
                    (429, "{}", "Retry-After: 5\r\n")
                } else {
                    (200, r#"{"result":"ok","data":[],"total":0}"#, "")
                };
                socket.write_all(format!("HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut client = MangaDex::default();
        client.api.origin = Url::parse(&format!("http://{address}/")).unwrap();
        client
            .api
            .get("manga", &[], MangaDexCacheMode::Online)
            .await
            .unwrap();
        client
            .api
            .get("manga", &[], MangaDexCacheMode::Online)
            .await
            .unwrap();
        assert_eq!(calls.load(AtomicOrder::SeqCst), 1);
        client
            .api
            .get("manga", &[], MangaDexCacheMode::Refresh)
            .await
            .unwrap();
        assert_eq!(calls.load(AtomicOrder::SeqCst), 2);
        assert!(
            client
                .api
                .get("manga", &[], MangaDexCacheMode::Refresh)
                .await
                .unwrap_err()
                .to_string()
                .contains("rate limit")
        );
        assert!(
            client
                .api
                .get("manga", &[], MangaDexCacheMode::Refresh)
                .await
                .unwrap_err()
                .to_string()
                .contains("rate limit")
        );
        assert_eq!(calls.load(AtomicOrder::SeqCst), 3);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mangadex_search_prefers_english_then_checks_original_when_english_is_not_ahead() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let ids = [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000003",
        ];
        let data = ids.iter().enumerate().map(|(index,id)| json!({"id":id,"attributes":{"title":{"en":format!("Book {index}")},"originalLanguage":if index==2 {"ja"} else {"ko"},"chapterNumbersResetOnNewVolume":false},"relationships":[]})).collect::<Vec<_>>();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let mut requested = Vec::new();
            for _ in 0..11 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = std::str::from_utf8(&buffer[..size]).unwrap();
                let path = request.split_whitespace().nth(1).unwrap();
                let url = Url::parse(&format!("http://localhost{path}")).unwrap();
                let body = if url.path()=="/manga" { json!({"result":"ok","data":data,"total":3}) } else {
                    let language = url.query_pairs().find(|(key,_)| key=="translatedLanguage[]").map(|(_,value)|value.to_string()).unwrap_or_else(||"all".into());
                    let book = ids.iter().position(|id|url.path().contains(id)).unwrap();
                    requested.push((book,language.clone()));
                    let chapter = match (book,language.as_str()) { (0,"en"|"all")=>Some("20"),(1,"en"|"vi")=>Some("5"),(1,"ko")=>Some("7"),(2,"vi")=>Some("2"),(2,"ja")=>Some("3"),_=>None };
                    match chapter {Some(number)=>json!({"result":"ok","volumes":{"none":{"volume":"none","chapters":{number:{"chapter":number,"isUnavailable":false}}}}}),None=>json!({"result":"ok","volumes":[]})}
                }.to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            requested
        });
        let mut client = MangaDex::default();
        client.api.origin = Url::parse(&format!("http://{address}/")).unwrap();
        let input = MangaDexSearch {
            title: String::new(),
            reference: "auto".into(),
            filter: MangaDexFilter::Either,
            ..Default::default()
        };
        let page = client.search(input.clone()).await.unwrap();
        assert_eq!(
            page.manga
                .iter()
                .map(|book| book
                    .comparison
                    .as_ref()
                    .unwrap()
                    .reference_language
                    .as_str())
                .collect::<Vec<_>>(),
            vec!["en", "ko", "ja"]
        );
        assert_eq!(
            page.manga[0].comparison.as_ref().and_then(|c| c.status),
            Some(MangaDexFilter::NoTarget)
        );
        assert_eq!(
            page.manga[1].comparison.as_ref().and_then(|c| c.status),
            Some(MangaDexFilter::Behind)
        );
        assert_eq!(client.search(input).await.unwrap().manga.len(), 3);
        assert_eq!(
            server.await.unwrap(),
            vec![
                (0, "vi".into()),
                (0, "en".into()),
                (0, "ko".into()),
                (0, "all".into()),
                (1, "vi".into()),
                (1, "en".into()),
                (1, "ko".into()),
                (2, "vi".into()),
                (2, "en".into()),
                (2, "ja".into())
            ]
        );
    }

    #[tokio::test]
    async fn mangadex_general_listing_uses_one_request_and_reuses_it_after_filter_order_changes() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 8192];
            let size = socket.read(&mut buffer).await.unwrap();
            let request = std::str::from_utf8(&buffer[..size]).unwrap();
            let url = Url::parse(&format!(
                "http://localhost{}",
                request.split_whitespace().nth(1).unwrap()
            ))
            .unwrap();
            let pairs = url.query_pairs().collect::<Vec<_>>();
            assert!(
                pairs
                    .iter()
                    .any(|(key, value)| key == "limit" && value == "100")
            );
            assert!(
                pairs
                    .iter()
                    .any(|(key, value)| key == "includes[]" && value == "cover_art")
            );
            let body = json!({"result":"ok","total":201,"data":[{"id":"00000000-0000-0000-0000-000000000001","attributes":{"title":{"en":"A book"},"originalLanguage":"ko","year":2023,"status":"ongoing","contentRating":"safe","description":{"en":"Summary"},"tags":[]},"relationships":[{"type":"cover_art","attributes":{"fileName":"cover.jpg"}}]}]}).to_string();
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        });
        let mut client = MangaDex::default();
        client.api.origin = Url::parse(&format!("http://{address}/")).unwrap();
        let mut input = MangaDexSearch {
            original_languages: vec!["ko".into(), "ja".into()],
            ..Default::default()
        };
        let page = client.search(input.clone()).await.unwrap();
        assert_eq!(page.network_requests, 1);
        assert_eq!(page.manga[0].year, Some(2023));
        assert!(
            page.manga[0]
                .cover_url
                .as_ref()
                .unwrap()
                .ends_with("/cover.jpg.256.jpg")
        );
        assert!(
            page.manga[0]
                .comparison
                .as_ref()
                .and_then(|c| c.status)
                .is_none()
        );
        input.original_languages.reverse();
        assert_eq!(client.search(input).await.unwrap().network_requests, 0);
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "read-only live MangaDex Animan TLS regression; run with the desktop package feature graph"]
    async fn mangadex_live_animan_connect() {
        let page = MangaDex::default()
            .search(MangaDexSearch {
                title: "animan".into(),
                content_ratings: vec!["safe".into(), "suggestive".into(), "erotica".into()],
                ..Default::default()
            })
            .await
            .expect("desktop HTTP client must reach the Animan MangaDex search");
        assert!(page.scanned > 0);
        assert_eq!(page.network_requests, 1);
        println!(
            "Animan HTTPS search: {} titles, {} API request",
            page.manga.len(),
            page.network_requests
        );
    }

    #[tokio::test]
    #[ignore = "read-only live MangaDex API smoke check"]
    async fn mangadex_live_search() {
        let client = MangaDex::default();
        let options = client
            .options(
                MangaDexOptionsKind::Tags,
                String::new(),
                MangaDexCacheMode::Online,
            )
            .await
            .unwrap();
        assert!(!options.options.is_empty());
        let list = client
            .search(MangaDexSearch {
                title: "Solo Leveling".into(),
                sort: "followedCount.desc".into(),
                original_languages: vec!["ko".into()],
                has_chapters: true,
                translated_languages: vec!["en".into()],
                content_ratings: vec!["safe".into(), "suggestive".into(), "erotica".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(list.scanned > 0);
        assert_eq!(list.network_requests, 1);
        assert!(list.manga.iter().all(|book| book.comparison.is_none()));
        println!(
            "MangaDex advanced list: {} titles, {} API request; {} tags",
            list.manga.len(),
            list.network_requests,
            options.options.len()
        );
        let page = client
            .search(MangaDexSearch {
                title: "Solo Leveling".into(),
                reference: "auto".into(),
                filter: MangaDexFilter::Either,
                original_languages: vec!["ko".into()],
                ..Default::default()
            })
            .await
            .unwrap();
        assert!(page.scanned > 0);
        println!(
            "MangaDex live: scanned {}, matched {}, skipped {}",
            page.scanned,
            page.manga.len(),
            page.skipped
        );
        for manga in page.manga.iter().take(3) {
            println!(
                "{} | {:?} | {:?} | VI {:?}",
                manga.title,
                manga
                    .comparison
                    .as_ref()
                    .map(|value| &value.reference_language),
                manga.comparison.as_ref().map(|value| &value.chapters),
                manga.comparison.as_ref().map(|value| value.missing)
            );
        }
    }
}
