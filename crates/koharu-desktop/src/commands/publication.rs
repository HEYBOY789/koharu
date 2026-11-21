//! Prepares the website's local editorial bundle using the native exports.

use std::{path::PathBuf, process::Stdio, sync::Arc};

use anyhow::{Context as _, ensure};
use koharu_psd::Labels;
use koharu_scene::{Edit, Op, PublicationMetadata, Uuid};
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{State, Window};
use tauri_runtime_cef::CefRuntime;
use tokio::process::Command;

use super::{
    Result,
    export::{self, ExportFormat},
    source_import::{self, PublicationConfig},
};
use crate::app::App;

#[derive(Deserialize, Serialize, Type)]
pub struct PublicationSettings {
    pub metadata: PublicationMetadata,
    pub config: PublicationConfig,
}

#[tauri::command]
#[specta::specta]
pub fn get_publication_settings(app: State<'_, Arc<App>>) -> Result<PublicationSettings> {
    let project = app.project.lock();
    let open = project.as_ref().context("no project is open")?;
    let project = open.session.lock().project();
    let metadata = project
        .metadata()
        .publication
        .clone()
        .unwrap_or_else(|| PublicationMetadata {
            title: open.name.clone(),
            slug: open
                .name
                .split(|letter: char| !letter.is_ascii_alphanumeric())
                .filter(|word| !word.is_empty())
                .collect::<Vec<_>>()
                .join("-")
                .to_ascii_lowercase(),
            ..Default::default()
        });
    Ok(PublicationSettings {
        metadata,
        config: koharu_config::get()?,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn prepare_publication(
    app: State<'_, Arc<App>>,
    window: Window<CefRuntime>,
    settings: PublicationSettings,
    labels: Labels,
) -> Result<Option<String>> {
    let session = app.session()?;
    let config = &settings.config;
    let script =
        source_import::toolkit_script(config).context("choose the publishing toolkit script")?;
    let node =
        source_import::executable(&config.node, "node").context("Node executable is missing")?;
    let catalog =
        tauri::Url::parse(&config.catalog_url).context("enter the website catalog URL")?;
    if !matches!(catalog.scheme(), "http" | "https")
        || !catalog.username().is_empty()
        || catalog.password().is_some()
    {
        return Err(anyhow::anyhow!("enter an HTTP(S) catalog URL without credentials").into());
    }
    let Some(folder) = rfd::AsyncFileDialog::new()
        .set_parent(&window)
        .pick_folder()
        .await
    else {
        return Ok(None);
    };
    let output = folder.path().canonicalize().map_err(anyhow::Error::from)?;
    let renderer = app.renderer.clone();
    let settings = settings;
    let session_for_export = Arc::clone(&session);
    let (staging, revision, config, slug) =
        tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let PublicationSettings {
                metadata: publication,
                config,
            } = settings;
            let mut writer = session_for_export.lock();
            let mut metadata = writer.project().metadata().clone();
            metadata.publication = Some(publication.clone());
            writer.apply(Edit {
                label: "publication.save".into(),
                ops: vec![Op::SetProjectMetadata {
                    metadata: Box::new(metadata.clone()),
                }],
            })?;
            let revision = writer.revision();
            let project = writer.project();
            let cover = metadata.cover.map(|hash| writer.file(&hash)).transpose()?;
            drop(writer);

            let staging = tempfile::tempdir_in(&output)?;
            let root = staging.path();
            let source = root.join("source");
            fs_err::create_dir(&source)?;
            let chapters = metadata.chapters.entries;
            ensure!(
                chapters.iter().any(|chapter| !chapter.pages.is_empty()),
                "create at least one chapter first"
            );
            let mut chapter_folders = std::collections::BTreeSet::new();
            for chapter in chapters {
                if chapter.pages.is_empty() {
                    continue;
                }
                let name = sanitize_filename::sanitize(&chapter.title);
                ensure!(
                    !name.is_empty() && chapter_folders.insert(name.to_lowercase()),
                    "chapter names must give distinct folders"
                );
                let destination = source.join(name);
                fs_err::create_dir(&destination)?;
                let pages = project
                    .pages()
                    .filter(|page| chapter.pages.contains(&page.id))
                    .map(|page| page.id)
                    .collect::<Vec<_>>();
                export::write(
                    &session_for_export,
                    &renderer,
                    &pages,
                    ExportFormat::Png,
                    &labels,
                    &destination,
                )?;
            }
            let wiki = source.join("wiki");
            fs_err::create_dir(&wiki)?;
            for file in metadata.wiki.files {
                fs_err::write(wiki.join(format!("{}.md", file.slug)), file.body)?;
            }
            let mut data = serde_json::to_value(&publication)?;
            if let Some(cover) = cover {
                fs_err::write(root.join("cover.image"), cover)?;
                data["coverFile"] = serde_json::json!("cover.image");
            }
            fs_err::write(root.join("metadata.json"), serde_json::to_vec(&data)?)?;
            Ok((staging, revision, config, publication.slug))
        })
        .await
        .map_err(anyhow::Error::from)??;
    let root = staging.path();
    let bundle = root.join("bundle");
    let cache = dirs::cache_dir()
        .context("local cache folder is unavailable")?
        .join("Koharu/publication/catalog.json");
    let mut command = Command::new(node);
    command
        .current_dir(
            script
                .ancestors()
                .nth(3)
                .context("invalid publisher script location")?,
        )
        .args(["--import", "tsx"])
        .arg(&script)
        .arg("build")
        .arg("--source")
        .arg(root.join("source"))
        .arg("--metadata")
        .arg(root.join("metadata.json"))
        .arg("--output")
        .arg(&bundle)
        .arg("--url")
        .arg(&config.catalog_url)
        .arg("--cache")
        .arg(cache)
        .arg("--backend")
        .arg(&config.backend)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    if config.offline {
        command.arg("--offline");
    }
    if let Some(binary) = &config.binary {
        command.arg("--binary").arg(binary);
    }
    if let Some(model) = &config.model {
        command.arg("--model").arg(model);
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000);
    let result = command.output().await.map_err(anyhow::Error::from)?;
    let destination = (|| -> anyhow::Result<PathBuf> {
        ensure!(
            result.status.success(),
            "publisher exited with {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr[result.stderr.len().saturating_sub(2048)..])
        );
        ensure!(
            session.lock().revision() == revision,
            "the project changed during export"
        );
        ensure!(
            bundle.join("editorial.json").is_file(),
            "the publisher did not produce editorial.json"
        );
        let directory = root
            .parent()
            .context("output folder is missing")?
            .canonicalize()?;
        let published = directory.join(format!("{slug}-{}", Uuid::new_v4()));
        let bundle = bundle.canonicalize()?;
        ensure!(
            bundle.starts_with(&directory) && !published.exists(),
            "the output folder changed"
        );
        fs_err::rename(bundle, &published)?;
        Ok(published)
    })()?;
    koharu_config::update(|saved| *saved = config)?;
    Ok(Some(destination.to_string_lossy().into_owned()))
}
