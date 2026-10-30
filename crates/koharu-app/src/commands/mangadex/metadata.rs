use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context as _, Result, ensure};
use serde::{Deserialize, Serialize};
use specta::Type;

use super::{MangaDex, MangaDexCacheMode, cache};

#[derive(Clone, Debug, Deserialize, Type)]
pub struct MangaDexMetadataRequest {
    pub id: String,
    pub latest_uploaded_chapter: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexMetadata {
    pub id: String,
    pub rating: Option<f64>,
    pub follows: Option<u32>,
    pub latest_upload_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct MangaDexMetadataPage {
    pub manga: Vec<MangaDexMetadata>,
    pub checked_at: u32,
    pub network_requests: u32,
}

impl MangaDex {
    pub(super) async fn metadata(
        &self,
        input: Vec<MangaDexMetadataRequest>,
        mode: MangaDexCacheMode,
    ) -> Result<MangaDexMetadataPage> {
        ensure!(
            input.len() <= 100,
            "MangaDex metadata batch exceeds 100 titles"
        );
        let mut titles = BTreeMap::new();
        let mut chapters = BTreeSet::new();
        for item in input {
            uuid::Uuid::parse_str(&item.id).context("invalid MangaDex manga ID")?;
            if let Some(chapter) = &item.latest_uploaded_chapter {
                uuid::Uuid::parse_str(chapter).context("invalid MangaDex chapter ID")?;
                chapters.insert(chapter.clone());
            }
            ensure!(
                titles
                    .insert(item.id, item.latest_uploaded_chapter)
                    .is_none(),
                "duplicate MangaDex metadata title"
            );
        }
        let mut checked = cache::now();
        let mut requests = 0;
        if titles.is_empty() {
            return Ok(MangaDexMetadataPage {
                manga: vec![],
                checked_at: checked as u32,
                network_requests: 0,
            });
        }
        let query = titles
            .keys()
            .map(|id| ("manga[]".into(), id.clone()))
            .collect::<Vec<_>>();
        let read = self.api.get("statistics/manga", &query, mode).await?;
        checked = checked.min(read.checked);
        requests += u32::from(read.network);
        let statistics = read.value["statistics"]
            .as_object()
            .context("MangaDex returned no manga statistics")?;
        let mut uploads = BTreeMap::new();
        if !chapters.is_empty() {
            let mut query = vec![("limit".into(), "100".into())];
            query.extend(chapters.iter().map(|id| ("ids[]".into(), id.clone())));
            query.extend(
                ["safe", "suggestive", "erotica", "pornographic"]
                    .map(|rating| ("contentRating[]".into(), rating.into())),
            );
            let read = self.api.get("chapter", &query, mode).await?;
            checked = checked.min(read.checked);
            requests += u32::from(read.network);
            let data = read.value["data"]
                .as_array()
                .context("MangaDex returned no chapter collection")?;
            for chapter in data {
                if let (Some(id), Some(created)) = (
                    chapter["id"].as_str(),
                    chapter["attributes"]["createdAt"].as_str(),
                ) && chapters.contains(id)
                {
                    uploads.insert(id.to_owned(), created.to_owned());
                }
            }
        }
        let manga = titles
            .into_iter()
            .map(|(id, chapter)| {
                let stats = statistics.get(&id);
                MangaDexMetadata {
                    rating: stats
                        .and_then(|s| s["rating"]["bayesian"].as_f64())
                        .filter(|value| value.is_finite() && (0.0..=10.0).contains(value)),
                    follows: stats
                        .and_then(|s| s["follows"].as_u64())
                        .and_then(|v| u32::try_from(v).ok()),
                    latest_upload_at: chapter.and_then(|id| uploads.get(&id).cloned()),
                    id,
                }
            })
            .collect();
        Ok(MangaDexMetadataPage {
            manga,
            checked_at: checked as u32,
            network_requests: requests,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tauri::Url;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn mangadex_metadata_batches_ratings_uploads_and_reuses_cached_responses() {
        let ids = [
            "00000000-0000-0000-0000-000000000001",
            "00000000-0000-0000-0000-000000000002",
            "00000000-0000-0000-0000-000000000003",
        ];
        let chapter_id = "00000000-0000-0000-0000-000000000004";
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 16384];
                let size = socket.read(&mut buffer).await.unwrap();
                let request = std::str::from_utf8(&buffer[..size]).unwrap();
                let url = Url::parse(&format!(
                    "http://localhost{}",
                    request.split_whitespace().nth(1).unwrap()
                ))
                .unwrap();
                let body = if index == 0 {
                    assert_eq!(url.path(), "/statistics/manga");
                    assert_eq!(
                        url.query_pairs()
                            .filter(|(key, _)| key == "manga[]")
                            .count(),
                        3
                    );
                    json!({"result":"ok","statistics":{
                        ids[0]:{"rating":{"average":9.5,"bayesian":8.125},"follows":123456},
                        ids[1]:{"rating":{"average":null,"bayesian":null},"follows":0}
                    }})
                } else {
                    assert_eq!(url.path(), "/chapter");
                    assert_eq!(
                        url.query_pairs().filter(|(key, _)| key == "ids[]").count(),
                        1
                    );
                    assert!(
                        url.query_pairs()
                            .any(|(key, value)| key == "ids[]" && value == chapter_id)
                    );
                    assert!(url.query_pairs().any(
                            |(key, value)| key == "contentRating[]" && value == "pornographic"
                        ));
                    json!({"result":"ok","data":[{"id":chapter_id,"attributes":{
                        "createdAt":"2026-10-27T03:05:27+00:00",
                        "updatedAt":"2026-10-30T10:00:00+00:00"
                    }}]})
                }
                .to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut client = MangaDex::default();
        client.api.origin = origin;
        let input = ids
            .into_iter()
            .enumerate()
            .map(|(index, id)| MangaDexMetadataRequest {
                id: id.into(),
                latest_uploaded_chapter: (index < 2).then(|| chapter_id.into()),
            })
            .collect::<Vec<_>>();
        let result = client
            .metadata(input.clone(), MangaDexCacheMode::Online)
            .await
            .unwrap();
        assert_eq!(result.network_requests, 2);
        assert_eq!(result.manga[0].rating, Some(8.125));
        assert_eq!(result.manga[0].follows, Some(123456));
        assert_eq!(
            result.manga[0].latest_upload_at.as_deref(),
            Some("2026-10-27T03:05:27+00:00")
        );
        assert_eq!(result.manga[1].rating, None);
        assert_eq!(result.manga[1].follows, Some(0));
        assert_eq!(result.manga[2].follows, None);
        assert_eq!(result.manga[2].latest_upload_at, None);
        let mut reversed = input.clone();
        reversed.reverse();
        assert_eq!(
            client
                .metadata(reversed, MangaDexCacheMode::Online)
                .await
                .unwrap()
                .network_requests,
            0
        );
        assert_eq!(
            client
                .metadata(input.clone(), MangaDexCacheMode::Offline)
                .await
                .unwrap()
                .network_requests,
            0
        );
        assert_eq!(
            client
                .metadata(vec![], MangaDexCacheMode::Online)
                .await
                .unwrap()
                .network_requests,
            0
        );
        assert!(
            client
                .metadata(vec![input[0].clone(); 101], MangaDexCacheMode::Online)
                .await
                .is_err()
        );
        assert!(
            client
                .metadata(vec![input[0].clone(); 2], MangaDexCacheMode::Online)
                .await
                .is_err()
        );
        server.await.unwrap();
    }
}
