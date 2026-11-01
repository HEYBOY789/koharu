use std::{collections::BTreeSet, io::Write as _, path::PathBuf, time::Duration};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::State;
use tokio::sync::Mutex;

use super::{
    mangadex::MangaDex,
    publication::PublicationConfig,
    source_import::{self, SourceImportOptions},
};
use tauri::AppHandle;
use tauri_runtime_cef::CefRuntime;

#[derive(Clone, Debug, Deserialize, Type)]
pub struct FollowInput {
    pub source: FollowSource,
    pub title: String,
    pub language: String,
    pub cover_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, Type)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FollowSource {
    MangaDex {
        id: String,
        chapter_numbers_reset: bool,
    },
    Website {
        url: String,
    },
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct FollowedManga {
    pub key: String,
    pub source: FollowSource,
    pub title: String,
    pub language: String,
    pub cover_url: Option<String>,
    pub latest: Option<String>,
    pub chapters: u32,
    pub new_chapters: u32,
    pub checked_at: Option<u32>,
    pub error: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct ChapterSnapshot {
    keys: BTreeSet<String>,
    latest: Option<String>,
}

#[cfg(test)]
impl ChapterSnapshot {
    fn parse(value: &serde_json::Value, reset: bool) -> Result<Self> {
        let (keys, latest) = super::mangadex::inventory_from_aggregate(value, reset)?;
        Ok(Self { keys, latest })
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct SavedFollow {
    key: String,
    source: FollowSource,
    title: String,
    language: String,
    cover_url: Option<String>,
    seen: Option<BTreeSet<String>>,
    snapshot: Option<ChapterSnapshot>,
    checked_at: Option<u32>,
    error: Option<String>,
}

impl SavedFollow {
    fn view(&self) -> FollowedManga {
        let keys = self.snapshot.as_ref().map(|snapshot| &snapshot.keys);
        FollowedManga {
            key: self.key.clone(),
            source: self.source.clone(),
            title: self.title.clone(),
            language: self.language.clone(),
            cover_url: self.cover_url.clone(),
            latest: self.snapshot.as_ref().and_then(|s| s.latest.clone()),
            chapters: keys.map_or(0, |keys| keys.len() as u32),
            new_chapters: keys
                .zip(self.seen.as_ref())
                .map_or(0, |(keys, seen)| keys.difference(seen).count() as u32),
            checked_at: self.checked_at,
            error: self.error.clone(),
        }
    }

    fn checked(&mut self, result: Result<(ChapterSnapshot, u32)>) {
        match result {
            Ok((snapshot, checked)) => {
                if self.seen.is_none() {
                    self.seen = Some(snapshot.keys.clone());
                }
                self.snapshot = Some(snapshot);
                self.checked_at = Some(checked);
                self.error = None;
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
    }
}

pub(crate) struct Following {
    file: PathBuf,
    entries: Mutex<Option<Vec<SavedFollow>>>,
}

impl Following {
    pub fn persistent() -> Result<Self> {
        Ok(Self {
            file: dirs::data_local_dir()
                .context("local data directory is unavailable")?
                .join("Koharu/following.json"),
            entries: Mutex::new(None),
        })
    }

    async fn loaded(&self, entries: &mut Option<Vec<SavedFollow>>) -> Result<()> {
        if entries.is_some() {
            return Ok(());
        }
        let file = self.file.clone();
        *entries = Some(
            tokio::task::spawn_blocking(move || -> Result<Vec<SavedFollow>> {
                match std::fs::metadata(&file) {
                    Ok(metadata) => {
                        ensure!(
                            metadata.len() <= 16 * 1024 * 1024,
                            "following file is too large"
                        );
                        let mut entries: Vec<SavedFollow> =
                            serde_json::from_slice(&std::fs::read(&file)?)
                                .context("could not read saved following list")?;
                        ensure!(entries.len() <= 1000, "following list is too large");
                        for entry in &mut entries {
                            ensure!(
                                entry.source.key(&entry.language)? == entry.key,
                                "invalid saved follow identity"
                            );
                        }
                        Ok(entries)
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
                    Err(error) => Err(error.into()),
                }
            })
            .await??,
        );
        Ok(())
    }

    async fn list(&self) -> Result<Vec<SavedFollow>> {
        let mut entries = self.entries.lock().await;
        self.loaded(&mut entries).await?;
        Ok(entries.as_ref().unwrap().clone())
    }

    async fn edit(
        &self,
        edit: impl FnOnce(&mut Vec<SavedFollow>) -> Result<()>,
    ) -> Result<Vec<FollowedManga>> {
        let mut entries = self.entries.lock().await;
        self.loaded(&mut entries).await?;
        let mut next = entries.as_ref().unwrap().clone();
        edit(&mut next)?;
        let bytes = serde_json::to_vec(&next)?;
        ensure!(
            bytes.len() <= 16 * 1024 * 1024,
            "following file is too large"
        );
        let file = self.file.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let parent = file.parent().context("following file has no parent")?;
            std::fs::create_dir_all(parent)?;
            let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
            temporary.write_all(&bytes)?;
            temporary.as_file().sync_all()?;
            temporary.persist(file)?;
            Ok(())
        })
        .await??;
        let views = next.iter().map(SavedFollow::view).collect();
        *entries = Some(next);
        Ok(views)
    }
}

fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as u32
}

impl FollowSource {
    fn key(&mut self, language: &str) -> Result<String> {
        ensure!(
            language != "auto"
                && (2..=5).contains(&language.len())
                && language
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'-'),
            "invalid chapter language"
        );
        match self {
            Self::MangaDex { id, .. } => {
                *id = uuid::Uuid::parse_str(id)?.to_string();
                Ok(format!("mangadex:{id}:{language}"))
            }
            Self::Website { url } => {
                let mut address = tauri::Url::parse(url.trim())?;
                ensure!(
                    matches!(address.scheme(), "http" | "https")
                        && address.username().is_empty()
                        && address.password().is_none(),
                    "enter an HTTP(S) manga URL without embedded credentials"
                );
                ensure!(
                    !matches!(
                        address.host_str(),
                        Some("mangadex.org" | "www.mangadex.org")
                    ),
                    "add MangaDex titles from the MangaDex search tab"
                );
                address.set_fragment(None);
                *url = address.to_string();
                Ok(format!("website:{url}:{language}"))
            }
        }
    }
}

async fn snapshot(
    client: &MangaDex,
    entry: &SavedFollow,
    refresh: bool,
    handle: Option<&AppHandle<CefRuntime>>,
) -> Result<Option<(ChapterSnapshot, u32, Option<String>)>> {
    match &entry.source {
        FollowSource::MangaDex {
            id,
            chapter_numbers_reset,
        } => {
            let (keys, latest, checked) = tokio::time::timeout(
                Duration::from_secs(25),
                client.inventory(id, &entry.language, *chapter_numbers_reset, refresh),
            )
            .await
            .context("MangaDex update check timed out")??;
            Ok(Some((ChapterSnapshot { keys, latest }, checked, None)))
        }
        FollowSource::Website { url } => {
            let handle = handle.context("website checks require the source importer")?;
            let config = koharu_config::load::<PublicationConfig>("publication")?
                .read()?
                .clone();
            let preview = source_import::preview_source_url(
                url.clone(),
                config,
                SourceImportOptions {
                    language: Some(entry.language.clone()),
                    download_url: None,
                    project_name: None,
                    summarize: false,
                    translate: false,
                    chapters: None,
                },
                handle.clone(),
            )
            .await
            .map_err(|error| error.0)?;
            let Some(preview) = preview else {
                return Ok(None);
            };
            let snapshot = website_snapshot(&preview)?;
            Ok(Some((snapshot, now(), Some(preview.title))))
        }
    }
}

fn website_snapshot(preview: &source_import::SourcePreview) -> Result<ChapterSnapshot> {
    let mut keys = BTreeSet::new();
    for chapter in &preview.chapters {
        let mut url = tauri::Url::parse(&chapter.url)?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none(),
            "invalid chapter URL in source survey"
        );
        url.set_fragment(None);
        keys.insert(url.to_string());
    }
    ensure!(
        !keys.is_empty(),
        "source survey returned no chapters; the last successful check is kept"
    );
    let latest = preview
        .chapters
        .iter()
        .filter(|c| c.number.is_finite())
        .max_by(|a, b| a.number.total_cmp(&b.number))
        .map(|c| c.title.chars().take(200).collect());
    Ok(ChapterSnapshot { keys, latest })
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_following(
    following: State<'_, Following>,
) -> Result<Vec<FollowedManga>, super::Error> {
    Ok(following
        .list()
        .await?
        .iter()
        .map(SavedFollow::view)
        .collect())
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn follow_manga(
    mut input: FollowInput,
    following: State<'_, Following>,
    client: State<'_, MangaDex>,
    handle: AppHandle<CefRuntime>,
) -> Result<Vec<FollowedManga>, super::Error> {
    let key = input.source.key(&input.language)?;
    if input.title.trim().is_empty() || input.title.chars().count() > 500 {
        return Err(anyhow::anyhow!("invalid manga title").into());
    }
    if let Some(cover) = &input.cover_url {
        let cover = tauri::Url::parse(cover)?;
        if !(cover.scheme() == "https" && cover.host_str() == Some("uploads.mangadex.org")) {
            return Err(anyhow::anyhow!("invalid MangaDex cover URL").into());
        }
    }
    let current = following.list().await?;
    if current.iter().any(|entry| entry.key == key) {
        return Ok(current.iter().map(SavedFollow::view).collect());
    }
    let mut entry = SavedFollow {
        key,
        source: input.source,
        title: input.title,
        language: input.language,
        cover_url: input.cover_url,
        seen: None,
        snapshot: None,
        checked_at: None,
        error: None,
    };
    match snapshot(&client, &entry, false, Some(&handle)).await {
        Ok(Some((snapshot, checked, title))) => {
            if let Some(title) = title {
                entry.title = title.chars().take(500).collect();
            }
            entry.checked(Ok((snapshot, checked)));
        }
        Ok(None) => return Ok(current.iter().map(SavedFollow::view).collect()),
        Err(error) => entry.checked(Err(error)),
    }
    Ok(following
        .edit(move |entries| {
            if !entries.iter().any(|other| other.key == entry.key) {
                ensure!(
                    entries.len() < 1000,
                    "following list is full (1000 entries)"
                );
                entries.push(entry);
            }
            Ok(())
        })
        .await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn check_follow(
    key: String,
    refresh: bool,
    following: State<'_, Following>,
    client: State<'_, MangaDex>,
    handle: AppHandle<CefRuntime>,
) -> Result<Vec<FollowedManga>, super::Error> {
    let entry = following
        .list()
        .await?
        .into_iter()
        .find(|entry| entry.key == key)
        .context("this manga is no longer followed")?;
    let result = snapshot(&client, &entry, refresh, Some(&handle)).await;
    Ok(following
        .edit(move |entries| {
            // A concurrent remove must never be undone by a late network result.
            if let Some(entry) = entries.iter_mut().find(|entry| entry.key == key) {
                match result {
                    Ok(Some((snapshot, checked, title))) => {
                        if let Some(title) = title {
                            entry.title = title.chars().take(500).collect();
                        }
                        entry.checked(Ok((snapshot, checked)));
                    }
                    Ok(None) => {}
                    Err(error) => entry.checked(Err(error)),
                }
            }
            Ok(())
        })
        .await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn mark_follow_seen(
    key: String,
    following: State<'_, Following>,
) -> Result<Vec<FollowedManga>, super::Error> {
    Ok(following
        .edit(move |entries| {
            let entry = entries
                .iter_mut()
                .find(|entry| entry.key == key)
                .context("this manga is no longer followed")?;
            if let Some(snapshot) = &entry.snapshot {
                entry
                    .seen
                    .get_or_insert_default()
                    .extend(snapshot.keys.iter().cloned());
            }
            Ok(())
        })
        .await?)
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn unfollow_manga(
    key: String,
    following: State<'_, Following>,
) -> Result<Vec<FollowedManga>, super::Error> {
    Ok(following
        .edit(move |entries| {
            entries.retain(|entry| entry.key != key);
            Ok(())
        })
        .await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn aggregate(numbers: &[&str]) -> serde_json::Value {
        let chapters = numbers
            .iter()
            .map(|number| {
                (
                    number.to_string(),
                    json!({
                        "id": "00000000-0000-0000-0000-000000000002", "chapter": number,
                        "isUnavailable": false, "others": ["another-group"], "count": 2
                    }),
                )
            })
            .collect::<serde_json::Map<_, _>>();
        json!({"result":"ok", "volumes":{"none":{"volume":"none", "chapters":chapters}}})
    }

    fn entry() -> SavedFollow {
        SavedFollow {
            key: "mangadex:00000000-0000-0000-0000-000000000001:en".into(),
            source: FollowSource::MangaDex {
                id: "00000000-0000-0000-0000-000000000001".into(),
                chapter_numbers_reset: false,
            },
            title: "Book".into(),
            language: "en".into(),
            cover_url: None,
            seen: None,
            snapshot: None,
            checked_at: None,
            error: None,
        }
    }

    #[test]
    fn website_surveys_find_new_chapter_urls_and_reject_empty_or_invalid_surveys() {
        let preview = |numbers: &[u32]| source_import::SourcePreview {
            title: "Website series".into(),
            source_url: "https://reader.example/book".into(),
            chapters: numbers
                .iter()
                .map(|number| source_import::SourceChapterPreview {
                    url: format!("https://reader.example/chapter/{number}"),
                    title: format!("Chapter {number}"),
                    number: *number as f64,
                })
                .collect(),
        };
        let mut saved = entry();
        saved.checked(Ok((website_snapshot(&preview(&[1, 3])).unwrap(), 10)));
        saved.checked(Ok((website_snapshot(&preview(&[1, 2, 3])).unwrap(), 20)));
        assert_eq!(saved.view().new_chapters, 1);
        assert_eq!(saved.view().latest.as_deref(), Some("Chapter 3"));
        assert!(website_snapshot(&preview(&[])).is_err());
        let mut invalid = preview(&[1]);
        invalid.chapters[0].url = "file:///private".into();
        assert!(website_snapshot(&invalid).is_err());
        let mut source = FollowSource::Website {
            url: "https://reader.example/book#chapter".into(),
        };
        assert_eq!(
            source.key("en").unwrap(),
            "website:https://reader.example/book:en"
        );
        assert!(source.key("auto").is_err());
    }

    #[test]
    fn detects_gap_fills_and_specials_without_counting_other_groups_or_deleted_chapters() {
        let mut entry = entry();
        entry.checked(Ok((
            ChapterSnapshot::parse(&aggregate(&["0", "1", "3.5"]), false).unwrap(),
            10,
        )));
        assert_eq!(entry.view().new_chapters, 0);
        assert_eq!(entry.view().chapters, 3);
        entry.checked(Ok((
            ChapterSnapshot::parse(&aggregate(&["0", "1", "2", "3.5", "none"]), false).unwrap(),
            20,
        )));
        assert_eq!(entry.view().latest.as_deref(), Some("3.5"));
        assert_eq!(entry.view().new_chapters, 2);
        entry.checked(Err(anyhow::anyhow!("temporary network failure")));
        assert_eq!(entry.view().checked_at, Some(20));
        assert_eq!(entry.view().new_chapters, 2);
        assert!(entry.view().error.is_some());
        entry
            .seen
            .as_mut()
            .unwrap()
            .extend(entry.snapshot.as_ref().unwrap().keys.iter().cloned());
        entry.checked(Ok((
            ChapterSnapshot::parse(&aggregate(&["0", "2", "3.50", "none"]), false).unwrap(),
            30,
        )));
        assert_eq!(entry.view().new_chapters, 0);
    }

    #[tokio::test]
    async fn saved_follows_and_acknowledgements_survive_reopening_and_failed_writes_do_not_replace_memory()
     {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("following.json");
        let store = Following {
            file: file.clone(),
            entries: Mutex::new(None),
        };
        let mut saved = entry();
        saved.checked(Ok((
            ChapterSnapshot::parse(&aggregate(&["1"]), false).unwrap(),
            10,
        )));
        store
            .edit(|entries| {
                entries.push(saved);
                Ok(())
            })
            .await
            .unwrap();
        let reopened = Following {
            file: file.clone(),
            entries: Mutex::new(None),
        };
        assert_eq!(reopened.list().await.unwrap()[0].view().chapters, 1);
        reopened
            .edit(|entries| {
                entries[0].checked(Ok((
                    ChapterSnapshot::parse(&aggregate(&["1", "2"]), false)?,
                    20,
                )));
                Ok(())
            })
            .await
            .unwrap();
        assert_eq!(reopened.list().await.unwrap()[0].view().new_chapters, 1);
        reopened
            .edit(|entries| {
                let snapshot = entries[0].snapshot.as_ref().unwrap().keys.clone();
                entries[0].seen.as_mut().unwrap().extend(snapshot);
                Ok(())
            })
            .await
            .unwrap();
        let reloaded = Following {
            file,
            entries: Mutex::new(None),
        };
        assert_eq!(reloaded.list().await.unwrap()[0].view().new_chapters, 0);
        let failed = Following {
            file: directory.path().to_owned(),
            entries: Mutex::new(Some(reloaded.list().await.unwrap())),
        };
        assert!(
            failed
                .edit(|entries| {
                    entries.clear();
                    Ok(())
                })
                .await
                .is_err()
        );
        assert_eq!(failed.list().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn update_checks_reuse_the_search_cache_and_only_refresh_on_request() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for numbers in [vec!["1"], vec!["1", "2"]] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = String::from_utf8_lossy(&buffer[..size]);
                assert!(request.contains("translatedLanguage%5B%5D=en"));
                assert!(request.contains("includeUnavailable=0"));
                let body = aggregate(&numbers).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let client =
            MangaDex::with_origin(tauri::Url::parse(&format!("http://{address}/")).unwrap());
        let entry = entry();
        assert_eq!(
            snapshot(&client, &entry, false, None)
                .await
                .unwrap()
                .unwrap()
                .0
                .keys
                .len(),
            1
        );
        assert_eq!(
            snapshot(&client, &entry, false, None)
                .await
                .unwrap()
                .unwrap()
                .0
                .keys
                .len(),
            1
        );
        assert_eq!(
            snapshot(&client, &entry, true, None)
                .await
                .unwrap()
                .unwrap()
                .0
                .keys
                .len(),
            2
        );
        server.await.unwrap();
    }
}
