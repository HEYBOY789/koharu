use std::{
    collections::HashMap,
    io::Write as _,
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::Url;
use tokio::sync::Mutex;

use super::MangaDexCacheMode;

const MAX_BYTES: usize = 32 * 1024 * 1024;
const RETENTION: u64 = 30 * 24 * 60 * 60;

pub(super) fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Serialize, Deserialize)]
struct Cached {
    checked: u64,
    value: Value,
}

#[derive(Default)]
struct Store {
    loaded: bool,
    entries: HashMap<String, Cached>,
}

impl Store {
    fn read(&self, key: &str, ttl: u64, mode: MangaDexCacheMode) -> Option<Read> {
        if mode == MangaDexCacheMode::Refresh {
            return None;
        }
        let time = now();
        let ttl = if mode == MangaDexCacheMode::Offline {
            RETENTION
        } else {
            ttl
        };
        self.entries
            .get(key)
            .filter(|entry| entry.checked <= time && time - entry.checked <= ttl)
            .map(|entry| Read {
                value: entry.value.clone(),
                checked: entry.checked,
                network: false,
            })
    }
}

#[derive(Debug)]
pub(super) struct Read {
    pub value: Value,
    pub checked: u64,
    pub network: bool,
}

// Serial network admission includes a second cache check, so identical reads coalesce.
pub(super) struct Api {
    pub origin: Url,
    file: Option<PathBuf>,
    store: Mutex<Store>,
    admission: Mutex<()>,
    next_request: Mutex<Instant>,
}

impl Api {
    pub fn new(file: Option<PathBuf>) -> Self {
        Self {
            origin: Url::parse("https://api.mangadex.org/").expect("constant API URL"),
            file,
            store: Mutex::default(),
            admission: Mutex::default(),
            next_request: Mutex::new(Instant::now()),
        }
    }

    pub async fn get(
        &self,
        path: &str,
        query: &[(String, String)],
        mode: MangaDexCacheMode,
    ) -> Result<Read> {
        let mut url = self.origin.join(path)?;
        let mut query = query.to_vec();
        query.sort();
        query.dedup();
        url.query_pairs_mut().extend_pairs(
            query
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str())),
        );
        let key = url.to_string();
        let ttl = if path == "manga/tag" {
            7 * 86400
        } else if path == "author" {
            86400
        } else {
            900
        };
        let mut store = self.store.lock().await;
        if !store.loaded {
            if let Some(file) = self.file.clone() {
                store.entries = tokio::task::spawn_blocking(move || {
                    let metadata = std::fs::metadata(&file).ok()?;
                    if metadata.len() > MAX_BYTES as u64 {
                        return None;
                    }
                    let bytes = std::fs::read(file).ok()?;
                    serde_json::from_slice::<HashMap<String, Cached>>(&bytes).ok()
                })
                .await?
                .unwrap_or_default();
                store.entries.retain(|_, entry| {
                    entry.checked <= now() && now() - entry.checked <= RETENTION
                });
            }
            store.loaded = true;
        }
        if let Some(read) = store.read(&key, ttl, mode) {
            return Ok(read);
        }
        ensure!(
            mode != MangaDexCacheMode::Offline,
            "No saved MangaDex response for these filters/page. Search online once first."
        );
        drop(store);
        let _admission = self.admission.lock().await;
        if let Some(read) = self.store.lock().await.read(&key, ttl, mode) {
            return Ok(read);
        }
        {
            let mut next = self.next_request.lock().await;
            let delay = next.saturating_duration_since(Instant::now());
            ensure!(
                delay <= Duration::from_secs(2),
                "MangaDex rate limit; retry after at least {} seconds",
                delay.as_secs() + 1
            );
            tokio::time::sleep(delay).await;
            *next = Instant::now() + Duration::from_millis(500);
        }
        let mut response = koharu_runtime::http_client()?
            .get(url)
            .header("User-Agent", "Koharu/0.1 (MangaDex search)")
            .timeout(Duration::from_secs(20))
            .send()
            .await
            .context("could not reach MangaDex")?;
        let status = response.status().as_u16();
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        let reset = response
            .headers()
            .get("x-ratelimit-retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(|timestamp| timestamp.saturating_sub(now()));
        let exhausted = response
            .headers()
            .get("x-ratelimit-remaining")
            .is_some_and(|v| v == "0");
        if status == 429 || exhausted {
            let seconds = retry
                .into_iter()
                .chain(reset)
                .max()
                .unwrap_or(60)
                .clamp(1, 86400);
            let mut next = self.next_request.lock().await;
            *next = (*next).max(Instant::now() + Duration::from_secs(seconds));
            if status == 429 {
                bail!("MangaDex rate limit; retry after at least {seconds} seconds");
            }
        }
        ensure!(
            (200..300).contains(&status),
            "MangaDex API returned HTTP {status}; retry later"
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|length| length <= 8 * 1024 * 1024),
            "MangaDex response is too large"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= 8 * 1024 * 1024,
                "MangaDex response is too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).context("invalid MangaDex JSON")?;
        ensure!(
            value["result"] == "ok",
            "MangaDex did not return a successful result"
        );
        let checked = now();
        let mut store = self.store.lock().await;
        store
            .entries
            .retain(|_, entry| entry.checked <= checked && checked - entry.checked <= RETENTION);
        store.entries.insert(
            key,
            Cached {
                checked,
                value: value.clone(),
            },
        );
        while store.entries.len() > 1024 {
            evict(&mut store.entries);
        }
        if let Some(file) = self.file.clone() {
            let mut bytes = serde_json::to_vec(&store.entries)?;
            while bytes.len() > MAX_BYTES && store.entries.len() > 1 {
                evict(&mut store.entries);
                bytes = serde_json::to_vec(&store.entries)?;
            }
            tokio::task::spawn_blocking(move || -> Result<()> {
                let parent = file.parent().context("MangaDex cache has no parent")?;
                std::fs::create_dir_all(parent)?;
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                temporary.write_all(&bytes)?;
                temporary.as_file().sync_all()?;
                temporary.persist(file)?;
                Ok(())
            })
            .await?
            .context("could not save MangaDex cache")?;
        }
        Ok(Read {
            value,
            checked,
            network: true,
        })
    }
}

fn evict(entries: &mut HashMap<String, Cached>) {
    if let Some(key) = entries
        .iter()
        .min_by_key(|(_, entry)| entry.checked)
        .map(|(key, _)| key.clone())
    {
        entries.remove(&key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn mangadex_cached_reads_do_not_wait_for_an_unrelated_network_request() {
        use std::sync::Arc;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let (started, waiting) = tokio::sync::oneshot::channel();
        let (release, resume) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            for index in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                socket.read(&mut buffer).await.unwrap();
                if index == 1 {
                    started.send(()).unwrap();
                    resume.await.unwrap();
                    let body = r#"{"result":"ok","data":[]}"#;
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                    break;
                }
                let body = r#"{"result":"ok","data":[]}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut api = Api::new(None);
        api.origin = origin;
        let api = Arc::new(api);
        api.get("manga", &[], MangaDexCacheMode::Online)
            .await
            .unwrap();
        let pending_api = api.clone();
        let pending = tokio::spawn(async move {
            pending_api
                .get("author", &[], MangaDexCacheMode::Online)
                .await
        });
        waiting.await.unwrap();
        let cached = tokio::time::timeout(
            Duration::from_millis(250),
            api.get("manga", &[], MangaDexCacheMode::Online),
        )
        .await;
        release.send(()).unwrap();
        pending.await.unwrap().unwrap();
        server.await.unwrap();
        assert!(
            !cached
                .expect("cached response waited behind the network request")
                .unwrap()
                .network
        );
    }

    #[tokio::test]
    async fn mangadex_success_with_exhausted_quota_honors_unix_reset_header() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = [0; 4096];
            socket.read(&mut buffer).await.unwrap();
            let body = r#"{"result":"ok","data":[]}"#;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nX-RateLimit-Remaining: 0\r\nX-RateLimit-Retry-After: {}\r\nConnection: close\r\n\r\n{body}",body.len(),now()+120).as_bytes()).await.unwrap();
        });
        let mut api = Api::new(None);
        api.origin = origin;
        api.get("manga", &[], MangaDexCacheMode::Online)
            .await
            .unwrap();
        assert!(
            !api.get("manga", &[], MangaDexCacheMode::Online)
                .await
                .unwrap()
                .network
        );
        let error = api
            .get("author", &[], MangaDexCacheMode::Online)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("rate limit"));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn mangadex_persistent_cache_coalesces_concurrent_reads_and_offline_survives_restart() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("cache.json");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            for version in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 4096];
                socket.read(&mut buffer).await.unwrap();
                let body = json!({"result":"ok","data":[],"total":0,"version":version}).to_string();
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let mut client = Api::new(Some(file.clone()));
        client.origin = origin.clone();
        let (first, second) = tokio::join!(
            client.get("manga", &[], MangaDexCacheMode::Online),
            client.get("manga", &[], MangaDexCacheMode::Online)
        );
        assert!(first.unwrap().network);
        assert!(!second.unwrap().network);
        let refreshed = client
            .get("manga", &[], MangaDexCacheMode::Refresh)
            .await
            .unwrap();
        assert!(refreshed.network);
        assert_eq!(refreshed.value["version"], 1);
        server.await.unwrap();
        drop(client);
        let mut entries: HashMap<String, Cached> =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        entries.values_mut().for_each(|entry| entry.checked -= 3600);
        std::fs::write(&file, serde_json::to_vec(&entries).unwrap()).unwrap();
        let mut reopened = Api::new(Some(file.clone()));
        reopened.origin = origin;
        let saved = reopened
            .get("manga", &[], MangaDexCacheMode::Offline)
            .await
            .unwrap();
        assert!(!saved.network);
        assert_eq!(saved.value["version"], 1);
        assert!(now() - saved.checked >= 3600);
        assert!(
            reopened
                .get(
                    "manga",
                    &[("title".into(), "other".into())],
                    MangaDexCacheMode::Offline
                )
                .await
                .is_err()
        );
        assert!(
            reopened
                .get("manga", &[], MangaDexCacheMode::Online)
                .await
                .is_err()
        );
        assert_eq!(
            reopened
                .get("manga", &[], MangaDexCacheMode::Offline)
                .await
                .unwrap()
                .value,
            saved.value
        );
        let mut expired: HashMap<String, Cached> =
            serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        expired
            .values_mut()
            .for_each(|entry| entry.checked = now() - RETENTION - 1);
        std::fs::write(&file, serde_json::to_vec(&expired).unwrap()).unwrap();
        let mut expired_client = Api::new(Some(file.clone()));
        expired_client.origin = reopened.origin.clone();
        assert!(
            expired_client
                .get("manga", &[], MangaDexCacheMode::Offline)
                .await
                .is_err()
        );
        std::fs::write(&file, "broken JSON").unwrap();
        assert!(
            Api::new(Some(file))
                .get("manga", &[], MangaDexCacheMode::Offline)
                .await
                .is_err()
        );
    }
}
