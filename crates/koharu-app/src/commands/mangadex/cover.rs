use std::{
    collections::VecDeque,
    io::Cursor,
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, ensure};
use tauri::{State, Url};
use tokio::sync::Mutex;

use super::MangaDex;
use crate::commands::{Error, ImageBytes};

const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
const MAX_CACHE_BYTES: usize = 16 * 1024 * 1024;

#[derive(Default)]
struct Store {
    entries: VecDeque<(String, Vec<u8>)>,
    bytes: usize,
}

impl Store {
    fn get(&mut self, url: &str) -> Option<Vec<u8>> {
        let index = self.entries.iter().position(|(key, _)| key == url)?;
        let entry = self.entries.remove(index)?;
        let bytes = entry.1.clone();
        self.entries.push_back(entry);
        Some(bytes)
    }
    fn put(&mut self, url: String, bytes: Vec<u8>) {
        self.bytes += bytes.len();
        self.entries.push_back((url, bytes));
        while self.bytes > MAX_CACHE_BYTES || self.entries.len() > 128 {
            if let Some((_, bytes)) = self.entries.pop_front() {
                self.bytes -= bytes.len();
            }
        }
    }
}

pub(super) struct Covers {
    store: Mutex<Store>,
    admission: Mutex<Instant>,
}

impl Default for Covers {
    fn default() -> Self {
        Self {
            store: Mutex::default(),
            admission: Mutex::new(Instant::now()),
        }
    }
}

fn address(value: &str) -> Result<Url> {
    let url = Url::parse(value)?;
    ensure!(
        url.scheme() == "https"
            && url.host_str() == Some("uploads.mangadex.org")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "invalid MangaDex cover URL"
    );
    let parts = url
        .path_segments()
        .context("invalid MangaDex cover path")?
        .collect::<Vec<_>>();
    ensure!(
        parts.len() == 3 && parts[0] == "covers",
        "invalid MangaDex cover path"
    );
    uuid::Uuid::parse_str(parts[1])?;
    ensure!(
        parts[2].ends_with(".256.jpg")
            && parts[2].len() <= 220
            && parts[2]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
        "invalid MangaDex thumbnail filename"
    );
    Ok(url)
}

impl Covers {
    async fn read(&self, url: Url) -> Result<Vec<u8>> {
        let key = url.to_string();
        if let Some(bytes) = self.store.lock().await.get(&key) {
            return Ok(bytes);
        }
        // One bounded CDN lane coalesces identical misses and caps traffic at 2/s.
        let mut next = self.admission.lock().await;
        if let Some(bytes) = self.store.lock().await.get(&key) {
            return Ok(bytes);
        }
        tokio::time::sleep(next.saturating_duration_since(Instant::now())).await;
        *next = Instant::now() + Duration::from_millis(500);
        let mut response = koharu_runtime::http_client()?
            .get(url.clone())
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .context("could not download MangaDex cover")?
            .error_for_status()?;
        ensure!(
            response.url().origin() == url.origin(),
            "MangaDex cover redirected to another origin"
        );
        ensure!(
            response
                .content_length()
                .is_none_or(|size| size <= MAX_IMAGE_BYTES as u64),
            "MangaDex cover is too large"
        );
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                bytes.len() + chunk.len() <= MAX_IMAGE_BYTES,
                "MangaDex cover is too large"
            );
            bytes.extend_from_slice(&chunk);
        }
        let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            ensure!(
                image::guess_format(&bytes)? == image::ImageFormat::Jpeg,
                "MangaDex thumbnail is not JPEG"
            );
            let mut reader = image::ImageReader::new(Cursor::new(&bytes)).with_guessed_format()?;
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(1024);
            limits.max_image_height = Some(8192);
            limits.max_alloc = Some(32 * 1024 * 1024);
            reader.limits(limits);
            reader.decode().context("invalid MangaDex cover image")?;
            Ok(bytes)
        })
        .await??;
        self.store.lock().await.put(key, bytes.clone());
        Ok(bytes)
    }
}

#[tauri::command]
#[specta::specta]
pub(crate) async fn get_mangadex_cover(
    url: String,
    client: State<'_, MangaDex>,
) -> Result<ImageBytes, Error> {
    let url = address(&url)?;
    Ok(ImageBytes(
        tokio::time::timeout(Duration::from_secs(25), client.covers.read(url))
            .await
            .context("MangaDex cover lookup timed out")??,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_accepts_mangadex_thumbnail_addresses() {
        let valid = "https://uploads.mangadex.org/covers/a4c0c579-f0a6-4ff7-bc2e-1c0d3f614d03/e69c9ff2.jpg.256.jpg";
        assert!(address(valid).is_ok());
        for invalid in [
            valid.replace("https:", "http:"),
            valid.replace("uploads.mangadex.org", "example.com"),
            format!("{valid}?redirect=example.com"),
            valid.replace("/covers/", "/data/"),
            valid.replace(".256.jpg", ".jpg"),
            valid.replace("e69c9ff2", "..%2Fsecret"),
        ] {
            assert!(address(&invalid).is_err(), "{invalid}");
        }
        let mut store = Store::default();
        for i in 0..140 {
            store.put(i.to_string(), vec![0; 1024]);
        }
        assert_eq!(store.entries.len(), 128);
        assert!(store.get("0").is_none());
        assert!(store.get("139").is_some());
        store.put("big".into(), vec![0; MAX_CACHE_BYTES]);
        assert!(store.bytes <= MAX_CACHE_BYTES);
    }

    #[tokio::test]
    async fn failed_images_are_not_cached_and_concurrent_reads_share_a_successful_download() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = Url::parse(&format!("http://{}/cover", listener.local_addr().unwrap())).unwrap();
        let mut jpeg = Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 3)
            .write_to(&mut jpeg, image::ImageFormat::Jpeg)
            .unwrap();
        let jpeg = jpeg.into_inner();
        let expected = jpeg.clone();
        let server = tokio::spawn(async move {
            for body in [b"not an image".to_vec(), jpeg] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buffer = [0; 2048];
                socket.read(&mut buffer).await.unwrap();
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                socket.write_all(&body).await.unwrap();
            }
        });
        let covers = Covers::default();
        assert!(covers.read(url.clone()).await.is_err());
        let (first, second) = tokio::join!(covers.read(url.clone()), covers.read(url));
        assert_eq!(first.unwrap(), expected);
        assert_eq!(second.unwrap(), expected);
        server.await.unwrap();
    }

    #[tokio::test]
    #[ignore = "manual acceptance: fetches the user's reported public MangaDex thumbnail with the native TLS client"]
    async fn live_atarashii_cover_decodes_through_native_tls() {
        let url = address("https://uploads.mangadex.org/covers/a4c0c579-f0a6-4ff7-bc2e-1c0d3f614d03/e69c9ff2-ae1d-435f-84c3-03b3e69b114c.jpg.256.jpg").unwrap();
        let bytes = Covers::default().read(url).await.unwrap();
        let image = image::load_from_memory(&bytes).unwrap();
        assert_eq!(image.width(), 256);
        assert!(image.height() > 0);
        println!(
            "Native MangaDex cover decoded: {} bytes, {}x{}",
            bytes.len(),
            image.width(),
            image.height()
        );
    }
}
